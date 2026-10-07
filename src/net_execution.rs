//! 一份不可变净目标与毛量父计划共享耐久事务；实际原币费用用于后续子单准入。
//! 预算超出只停止未来动作，不能撤销已成交费用，也不自动创建残量补单。
use crate::{
    decimal::{self, SCALE},
    execution::*,
    external_plan::{self, Phase},
    external_target::TargetRequest,
    paper::PaperError,
    types::Side,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Mandate {
    request: TargetRequest,
    initial_preview: serde_json::Value,
}
pub(crate) fn create_schema(c: &Connection) -> Result<(), PaperError> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS net_target(singleton INTEGER PRIMARY KEY CHECK(singleton=1),body TEXT NOT NULL,checksum TEXT NOT NULL);")?;
    Ok(())
}
pub(crate) fn has_target(c: &Connection) -> Result<bool, PaperError> {
    Ok(c.query_row("SELECT count(*) FROM net_target", [], |r| {
        r.get::<_, i64>(0)
    })? == 1)
}
fn hash(body: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"kaze-net-mandate-v1");
    h.update(body);
    crate::journal::hex(&h.finalize())
}
fn field(v: &serde_json::Value, n: &str) -> Result<i128, PaperError> {
    decimal::parse(v[n].as_str().ok_or("invalid target mandate decimal")?)
}
fn load(j: &ExecutionJournal) -> Result<Mandate, PaperError> {
    let (body, checksum): (String, String) = j.conn.query_row(
        "SELECT body,checksum FROM net_target WHERE singleton=1 AND length(CAST(body AS BLOB))<=65536",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if hash(&body) != checksum {
        return Err("net target checksum conflict".into());
    }
    let m: Mandate = serde_json::from_str(&body)?;
    m.request.validate()?;
    let p = &m.initial_preview;
    let plan: external_plan::PlanConfig = serde_json::from_value(p["plan"].clone())?;
    let status = j.plan_status()?;
    let net = field(p, "net_quantity")?;
    let target = decimal::parse(&m.request.target_quantity)?;
    if p["schema_version"] != 1
        || p["decision"] != "planned"
        || p["request"] != serde_json::to_value(&m.request)?
        || p["account_identity"]
            != serde_json::to_value(j.execution_identity()?.ok_or("target account absent")?)?
        || plan != status.config
        || plan != m.request.plan(plan.side, field(p, "gross_quantity")?)?
        || (plan.side == Side::Buy && target <= net)
        || (plan.side == Side::Sell && target >= net)
    {
        return Err("net target parent/provenance conflict".into());
    }
    let (base, quote): (String, String) = j.conn.query_row(
        "SELECT base,quote FROM instruments WHERE symbol=?1",
        [&m.request.symbol],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if p["base_asset"] != base || p["quote_asset"] != quote {
        return Err("net target asset identity conflict".into());
    }
    Ok(m)
}
pub(crate) fn validate(j: &ExecutionJournal) -> Result<(), PaperError> {
    load(j).map(|_| ())
}
fn fees(j: &ExecutionJournal, m: &Mandate) -> Result<BTreeMap<String, i128>, PaperError> {
    let mut q = j
        .conn
        .prepare("SELECT body FROM trades ORDER BY symbol,id")?;
    let mut fees = BTreeMap::new();
    for body in q.query_map([], |r| r.get::<_, String>(0))? {
        let t: TradeObservation = serde_json::from_str(&body?)?;
        if t.symbol != m.request.symbol {
            return Err("net target foreign trade symbol".into());
        }
        let amount = decimal::parse(&t.commission)?;
        let n = fees.entry(t.commission_asset).or_insert(0i128);
        *n = n.checked_add(amount).ok_or("target fee overflow")?;
    }
    Ok(fees)
}
fn violation(
    j: &ExecutionJournal,
    m: &Mandate,
    f: &BTreeMap<String, i128>,
) -> Result<Option<String>, PaperError> {
    let base = m.initial_preview["base_asset"]
        .as_str()
        .ok_or("target base absent")?;
    let quote = m.initial_preview["quote_asset"]
        .as_str()
        .ok_or("target quote absent")?;
    if f.iter().any(|(a, n)| a != base && a != quote && *n > 0) {
        return Ok(Some("net target unsupported fee asset".into()));
    }
    if f.get(base).copied().unwrap_or(0) > decimal::parse(&m.request.base_fee_reserve)?
        || f.get(quote).copied().unwrap_or(0) > decimal::parse(&m.request.quote_fee_reserve)?
    {
        return Ok(Some("net target fee reserve exceeded".into()));
    }
    let net = j
        .expected_balances()?
        .get(base)
        .map(|s| decimal::parse(s))
        .transpose()?
        .unwrap_or(0);
    let target = decimal::parse(&m.request.target_quantity)?;
    let side = j.plan_status()?.config.side;
    if (side == Side::Buy && net > target) || (side == Side::Sell && net < target) {
        return Ok(Some("net target directional bound exceeded".into()));
    }
    Ok(None)
}
pub(crate) fn budget_violation(j: &ExecutionJournal) -> Result<Option<String>, PaperError> {
    if !has_target(&j.conn)? {
        return Ok(None);
    }
    let m = load(j)?;
    violation(j, &m, &fees(j, &m)?)
}
pub(crate) fn verify_child(j: &ExecutionJournal, intent: &OrderIntent) -> Result<(), PaperError> {
    if !has_target(&j.conn)? {
        return Ok(());
    }
    j.require_reconciled()?;
    let m = load(j)?;
    let f = fees(j, &m)?;
    if let Some(reason) = violation(j, &m, &f)? {
        return Err(PaperError(reason));
    }
    let base = m.initial_preview["base_asset"]
        .as_str()
        .ok_or("target base absent")?;
    let quote = m.initial_preview["quote_asset"]
        .as_str()
        .ok_or("target quote absent")?;
    let body: String = j.conn.query_row(
        "SELECT body FROM accounts ORDER BY seq DESC LIMIT 1",
        [],
        |r| r.get(0),
    )?;
    let account: AccountObservation = serde_json::from_str(&body)?;
    let mut observed = crate::external_state::totals(&account)?;
    let balances = j.expected_balances()?;
    for asset in balances.keys() {
        observed.entry(asset.clone()).or_default();
    }
    for (asset, n) in observed {
        if n != balances
            .get(&asset)
            .map(|s| decimal::parse(s))
            .transpose()?
            .unwrap_or(0)
        {
            return Err("net target latest snapshot conflicts with ledger".into());
        }
    }
    let free = |asset: &str| -> Result<i128, PaperError> {
        account
            .balances
            .iter()
            .find(|b| b.asset == asset)
            .map(|b| decimal::parse(&b.free))
            .transpose()
            .map(|n| n.unwrap_or(0))
    };
    let current = balances
        .get(base)
        .map(|s| decimal::parse(s))
        .transpose()?
        .unwrap_or(0);
    let target = decimal::parse(&m.request.target_quantity)?;
    let qty = decimal::parse(&intent.quantity)?;
    let base_fee = decimal::parse(&m.request.base_fee_reserve)? - f.get(base).copied().unwrap_or(0);
    let quote_fee =
        decimal::parse(&m.request.quote_fee_reserve)? - f.get(quote).copied().unwrap_or(0);
    if intent.symbol != m.request.symbol
        || intent.price != m.request.limit_price
        || intent.side != j.plan_status()?.config.side
    {
        return Err("net target child identity conflicts".into());
    }
    match intent.side {
        Side::Buy => {
            let n = decimal::parse(&intent.price)?
                .checked_mul(qty)
                .ok_or("target child notional overflow")?;
            let cost = n / SCALE + i128::from(n % SCALE != 0);
            if qty > target - current
                || cost + quote_fee > free(quote)?
                || free(base)? + qty < base_fee
            {
                return Err("net target buy resource/position bound".into());
            }
        }
        Side::Sell => {
            if qty + base_fee > current - target
                || qty + base_fee > free(base)?
                || quote_fee > free(quote)?
            {
                return Err("net target sell resource/position bound".into());
            }
        }
    }
    Ok(())
}
impl ExecutionJournal {
    /// 核对之后一次性提交目标、预览来源和父计划；函数自身从不发送子单。
    pub fn init_net_target(
        &mut self,
        v: &mut impl ExecutionVenue,
        request: TargetRequest,
    ) -> Result<serde_json::Value, PaperError> {
        request.validate()?;
        let started = std::time::Instant::now();
        self.reconcile(v)?;
        if started.elapsed().as_millis() > u128::from(request.max_reconciliation_ms) {
            self.mark_execution_gap("net target init reconciliation stale", false)?;
            return Err("stale net target initialization".into());
        }
        if has_target(&self.conn)? {
            let m = load(self)?;
            if m.request != request {
                return Err("net target mandate immutable".into());
            }
            return self.net_target_status();
        }
        let initial_preview = self.preview_target(&request)?;
        if initial_preview["plan"].is_null() {
            return Err("net target not executable; preview hold/resources/dust first".into());
        }
        let plan = serde_json::from_value(initial_preview["plan"].clone())?;
        let m = Mandate {
            request,
            initial_preview,
        };
        let body = serde_json::to_string(&m)?;
        if body.len() > 65536 {
            return Err("net target mandate exceeds 64KiB".into());
        }
        if started.elapsed().as_millis() > u128::from(m.request.max_reconciliation_ms) {
            self.mark_execution_gap("net target init compilation stale", false)?;
            return Err("stale net target compilation".into());
        }
        let tx = self.conn.transaction()?;
        external_plan::init_plan_tx(&tx, plan)?;
        tx.execute(
            "INSERT INTO net_target VALUES(1,?1,?2)",
            params![body, hash(&body)],
        )?;
        tx.execute(
            "UPDATE execution_meta SET revision='binance-testnet-execution-v3-target'",
            [],
        )?;
        tx.commit()?;
        self.net_target_status()
    }
    pub fn net_target_status(&self) -> Result<serde_json::Value, PaperError> {
        let m = load(self)?;
        let f = fees(self, &m)?;
        let problem = violation(self, &m, &f)?;
        let plan = self.plan_status()?;
        let base = m.initial_preview["base_asset"]
            .as_str()
            .ok_or("target base absent")?;
        let quote = m.initial_preview["quote_asset"]
            .as_str()
            .ok_or("target quote absent")?;
        let current = self
            .expected_balances()?
            .get(base)
            .map(|s| decimal::parse(s))
            .transpose()?
            .unwrap_or(0);
        let gap = (decimal::parse(&m.request.target_quantity)? - current).abs();
        let within = gap <= decimal::parse(&m.request.tolerance_quantity)?;
        let phase = if problem.is_some() {
            "budget_or_bound_violation"
        } else {
            match plan.phase {
                Phase::NeedsReconciliation => "needs_reconciliation",
                Phase::SubmitUnknown => "submit_unknown",
                Phase::CancelPending => "cancel_pending",
                Phase::Paused => "paused",
                Phase::Completed => {
                    if within {
                        "satisfied"
                    } else {
                        "completed_with_residual"
                    }
                }
                Phase::Ready => "ready",
                Phase::Working => "working",
            }
        };
        let used_base = f.get(base).copied().unwrap_or(0);
        let used_quote = f.get(quote).copied().unwrap_or(0);
        let base_remaining = (decimal::parse(&m.request.base_fee_reserve)? - used_base).max(0);
        let quote_remaining = (decimal::parse(&m.request.quote_fee_reserve)? - used_quote).max(0);
        let fee_values = f
            .into_iter()
            .map(|(a, n)| Ok((a, decimal::format(n)?)))
            .collect::<Result<BTreeMap<_, _>, PaperError>>()?;
        Ok(
            serde_json::json!({"schema_version":1,"phase":phase,"execution_phase":plan.phase,"request":m.request,"initial_preview":m.initial_preview,
            "current_net_quantity":decimal::format(current)?,"residual_quantity":decimal::format(gap)?,"observed_within_tolerance":within,
            "confirmed_satisfied":phase=="satisfied","fees_by_original_asset":fee_values,"base_fee_remaining":decimal::format(base_remaining)?,"quote_fee_remaining":decimal::format(quote_remaining)?,"violation":problem,
            "scope":"one immutable bounded net target linked to one fixed-limit parent; new children guarded by reconciled net/free resources and cumulative fee reserves; detected violation pauses future work, not rollback of actual fills; no continuous strategy update/automatic residual repair/shared portfolio/mainnet/alpha"}),
        )
    }
}
