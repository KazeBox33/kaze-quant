//! 净仓位目标只编译为可审阅的毛量计划；快照不是下一次发送许可。
//! 费用预留由调用方声明，实际原币账本仍为权威，不能自动补齐费用尾差。
use crate::{
    decimal::{self, SCALE},
    execution::{AccountObservation, ExecutionJournal},
    external_plan::PlanConfig,
    paper::PaperError,
    types::Side,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetRequest {
    pub version: u32,
    pub plan_id: String,
    pub symbol: String,
    pub limit_price: String,
    pub target_quantity: String,
    pub tolerance_quantity: String,
    pub base_fee_reserve: String,
    pub quote_fee_reserve: String,
    pub max_gross_quantity: String,
    pub quantity_step: String,
    pub min_child_quantity: String,
    pub max_child_quantity: String,
    pub slices: u16,
    pub interval_ms: u64,
    pub max_working_ms: u64,
    pub max_reconciliation_ms: u64,
    pub max_children: u16,
}
/// 全部采用1e-8原币整数；不包含行情请求、SQL、十进制解析或发送操作。
#[derive(Clone, Copy, Debug)]
pub struct AllocationInput {
    pub net_base: i128,
    pub free_base: i128,
    pub free_quote: i128,
    pub target: i128,
    pub tolerance: i128,
    pub price: i128,
    pub step: i128,
    pub min_total: i128,
    pub max_gross: i128,
    pub base_fee_reserve: i128,
    pub quote_fee_reserve: i128,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Hold,
    Planned,
    BelowMinimum,
    InsufficientResources,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Allocation {
    pub decision: Decision,
    pub side: Option<Side>,
    pub gross: i128,
    pub quote_required: i128,
    pub base_required: i128,
    pub projected_net_low: i128,
    pub projected_net_high: i128,
    pub worst_residual: i128,
    pub within_tolerance_if_reserves_hold: bool,
}
fn ceil_div(n: i128, d: i128) -> i128 {
    n / d + i128::from(n % d != 0)
}
pub fn allocate(i: AllocationInput) -> Result<Allocation, PaperError> {
    let bound = 10i128.pow(30);
    if [
        i.net_base,
        i.free_base,
        i.free_quote,
        i.target,
        i.tolerance,
        i.price,
        i.step,
        i.min_total,
        i.max_gross,
        i.base_fee_reserve,
        i.quote_fee_reserve,
    ]
    .into_iter()
    .any(|n| !(0..=bound).contains(&n))
        || i.price == 0
        || i.step == 0
        || i.min_total == 0
        || i.max_gross == 0
        || i.free_base > i.net_base
        || i.min_total % i.step != 0
        || i.max_gross % i.step != 0
    {
        return Err("invalid target allocation bounds/grid/resources".into());
    }
    let gap = (i.target - i.net_base).abs();
    let side = if gap <= i.tolerance {
        None
    } else if i.target > i.net_base {
        Some(Side::Buy)
    } else {
        Some(Side::Sell)
    };
    let mut a = Allocation {
        decision: Decision::Hold,
        side,
        gross: 0,
        quote_required: 0,
        base_required: 0,
        projected_net_low: i.net_base,
        projected_net_high: i.net_base,
        worst_residual: gap,
        within_tolerance_if_reserves_hold: gap <= i.tolerance,
    };
    let Some(side) = side else {
        return Ok(a);
    };
    // 报价币费用全额先预留；不借用预计卖出收益，也不把locked资产当free。
    if i.free_quote < i.quote_fee_reserve {
        a.decision = Decision::InsufficientResources;
        return Ok(a);
    }
    let (directional, resource) = match side {
        Side::Buy => (
            gap,
            (i.free_quote - i.quote_fee_reserve)
                .checked_mul(SCALE)
                .ok_or("target quote resource overflow")?
                / i.price,
        ),
        Side::Sell => (
            (gap - i.base_fee_reserve).max(0),
            (i.free_base - i.base_fee_reserve).max(0),
        ),
    };
    let raw = directional.min(i.max_gross).min(resource);
    let qty = raw / i.step * i.step;
    if qty < i.min_total || (side == Side::Buy && qty < i.base_fee_reserve) {
        a.decision = if resource < i.min_total {
            Decision::InsufficientResources
        } else {
            Decision::BelowMinimum
        };
        return Ok(a);
    }
    a.gross = qty;
    a.decision = Decision::Planned;
    match side {
        Side::Buy => {
            let cost = ceil_div(
                i.price.checked_mul(qty).ok_or("target notional overflow")?,
                SCALE,
            );
            a.quote_required = cost
                .checked_add(i.quote_fee_reserve)
                .ok_or("target quote reserve overflow")?;
            a.projected_net_high = i
                .net_base
                .checked_add(qty)
                .ok_or("target position overflow")?;
            a.projected_net_low = a.projected_net_high - i.base_fee_reserve;
        }
        Side::Sell => {
            a.base_required = qty
                .checked_add(i.base_fee_reserve)
                .ok_or("target base reserve overflow")?;
            a.quote_required = i.quote_fee_reserve;
            a.projected_net_high = i.net_base - qty;
            a.projected_net_low = a.projected_net_high - i.base_fee_reserve;
        }
    }
    if a.projected_net_high > bound || a.projected_net_low < 0 {
        return Err("target projected position outside economic bound".into());
    }
    a.worst_residual = (i.target - a.projected_net_low)
        .abs()
        .max((i.target - a.projected_net_high).abs());
    a.within_tolerance_if_reserves_hold = a.worst_residual <= i.tolerance;
    Ok(a)
}
impl TargetRequest {
    pub(crate) fn plan(&self, side: Side, qty: i128) -> Result<PlanConfig, PaperError> {
        Ok(PlanConfig {
            version: 1,
            plan_id: self.plan_id.clone(),
            symbol: self.symbol.clone(),
            side,
            limit_price: self.limit_price.clone(),
            total_quantity: decimal::format(qty)?,
            quantity_step: self.quantity_step.clone(),
            min_child_quantity: self.min_child_quantity.clone(),
            max_child_quantity: decimal::format(
                decimal::parse(&self.max_child_quantity)?.min(qty),
            )?,
            slices: self.slices,
            interval_ms: self.interval_ms,
            max_working_ms: self.max_working_ms,
            max_reconciliation_ms: self.max_reconciliation_ms,
            max_children: self.max_children,
        })
    }
    pub fn validate(&self) -> Result<(), PaperError> {
        if self.version != 1 {
            return Err("unsupported target request version".into());
        }
        for v in [
            &self.target_quantity,
            &self.tolerance_quantity,
            &self.base_fee_reserve,
            &self.quote_fee_reserve,
        ] {
            decimal::parse(v)?;
        }
        if decimal::parse(&self.max_child_quantity)? > decimal::parse(&self.max_gross_quantity)? {
            return Err("child exceeds target gross cap".into());
        }
        self.plan(Side::Buy, decimal::parse(&self.max_gross_quantity)?)?
            .validate()
    }
}
fn digest(value: &impl Serialize) -> Result<String, PaperError> {
    Ok(crate::journal::hex(&Sha256::digest(serde_json::to_vec(
        value,
    )?)))
}
impl ExecutionJournal {
    /// Ready仅说明上次核对成功；调用方在预览前必须核对并检查其时间预算。
    /// 只读编译不会写意图，输出不能用于跳过tick_plan的重新核对。
    pub fn preview_target(&self, request: &TargetRequest) -> Result<serde_json::Value, PaperError> {
        request.validate()?;
        self.require_reconciled()?;
        let identity = self
            .execution_identity()?
            .ok_or("target requires bound account")?;
        if !self.has_history_recovery()?
            || !self.orders()?.is_empty()
            || crate::external_plan::has_plan(&self.conn)?
        {
            return Err(
                "target preview requires fresh history-enabled journal without plan/intents".into(),
            );
        }
        let (base, quote): (String, String) = self.conn.query_row(
            "SELECT base,quote FROM instruments WHERE symbol=?1",
            [&request.symbol],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let body: String = self.conn.query_row(
            "SELECT body FROM accounts ORDER BY seq DESC LIMIT 1",
            [],
            |r| r.get(0),
        )?;
        let account: AccountObservation = serde_json::from_str(&body)?;
        if !account.can_trade {
            return Err("target account cannot trade".into());
        }
        let balances = self.expected_balances()?;
        // Ready之后也拒绝本地新增但尚未核对的快照，不能借用旧Ready解释新资源。
        let mut observed = crate::external_state::totals(&account)?;
        for asset in balances.keys() {
            observed.entry(asset.clone()).or_default();
        }
        for (asset, actual) in observed {
            let expected = balances
                .get(&asset)
                .map(|s| decimal::parse(s))
                .transpose()?
                .unwrap_or(0);
            if actual != expected {
                return Err("target account snapshot conflicts with original-asset ledger".into());
            }
        }
        let net = balances
            .get(&base)
            .map(|s| decimal::parse(s))
            .transpose()?
            .unwrap_or(0);
        let free = |asset: &str| -> Result<i128, PaperError> {
            let found: Vec<_> = account
                .balances
                .iter()
                .filter(|b| b.asset == asset)
                .collect();
            if found.len() > 1 {
                return Err("duplicate snapshot asset".into());
            }
            found
                .first()
                .map(|b| decimal::parse(&b.free))
                .transpose()
                .map(|n| n.unwrap_or(0))
        };
        let input = AllocationInput {
            net_base: net,
            free_base: free(&base)?,
            free_quote: free(&quote)?,
            target: decimal::parse(&request.target_quantity)?,
            tolerance: decimal::parse(&request.tolerance_quantity)?,
            price: decimal::parse(&request.limit_price)?,
            step: decimal::parse(&request.quantity_step)?,
            min_total: decimal::parse(&request.min_child_quantity)?
                .checked_mul(i128::from(request.slices))
                .ok_or("minimum target overflow")?,
            max_gross: decimal::parse(&request.max_gross_quantity)?,
            base_fee_reserve: decimal::parse(&request.base_fee_reserve)?,
            quote_fee_reserve: decimal::parse(&request.quote_fee_reserve)?,
        };
        let a = allocate(input)?;
        let plan = if a.decision == Decision::Planned {
            let p = request.plan(a.side.ok_or("allocation side absent")?, a.gross)?;
            p.validate()?;
            Some(p)
        } else {
            None
        };
        Ok(serde_json::json!({
            "schema_version":1, "request":request, "request_sha256":digest(request)?,
            "account_identity":identity, "account_snapshot_sha256":digest(&account)?, "account_update_time":account.update_time,
            "history_cursors":self.history_cursors()?, "base_asset":base,"quote_asset":quote,
            "net_quantity":decimal::format(net)?,"free_base":decimal::format(input.free_base)?,"free_quote":decimal::format(input.free_quote)?,
            "decision":a.decision,"side":a.side,"gross_quantity":decimal::format(a.gross)?,
            "base_required":decimal::format(a.base_required)?,"quote_required":decimal::format(a.quote_required)?,
            "projected_net_low":decimal::format(a.projected_net_low)?,"projected_net_high":decimal::format(a.projected_net_high)?,
            "worst_residual":decimal::format(a.worst_residual)?,"within_tolerance_if_reserves_hold":a.within_tolerance_if_reserves_hold,
            "plan":plan,"submit_calls":0,
            "scope":"read-only net target compiler for one fresh isolated Spot account; fee reserves are caller assumptions, projection assumes full fills within reserves; snapshot is not send authorization; no automatic signal routing/net-target enforcement/mainnet/alpha"
        }))
    }
}
