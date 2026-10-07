//! 因果信号 → 目标仓位 → 过滤 → 单子单执行。计划只读账本，绝不直接改资金。
//! TWAP 用整数累计释放量，不按错过的定时次数爆发补单；取消回报前不替换子单。
use crate::{
    account::fee,
    engine::Engine,
    strategy::{ActionBuffer, RollingMean, Strategy, StrategyView},
    types::*,
};
use serde::{Deserialize, Serialize};
const MAX_TIME: u64 = 86_400_000_000_000;
const MAX_PARENTS: u64 = 1_000_000_000_000;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SignalConfig {
    Constant {
        exposure_bps: u32,
    },
    Threshold {
        buy_below: Price,
        sell_above: Price,
    },
    SmaCross {
        fast: usize,
        slow: usize,
        band_bps: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SizingConfig {
    FixedLots {
        lots: u64,
    },
    CashBudget {
        #[serde(with = "crate::config::money")]
        budget_minor: i128,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Schedule {
    Immediate,
    Twap {
        interval_ns: u64,
        slices: u16,
    },
    Iceberg {
        limit: Price,
        display_lots: u64,
        replenish_interval_ns: u64,
    },
    BestLimit {
        limit_guard: Price,
        min_reprice_ns: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionConfig {
    pub version: u32,
    pub signal: SignalConfig,
    pub sizing: SizingConfig,
    pub max_spread_bps: u32,
    pub cooldown_ns: u64,
    pub min_rebalance_lots: u64,
    pub max_child_lots: u64,
    pub max_working_ns: u64,
    pub schedule: Schedule,
}
impl CompositionConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !matches!(self.version, 1 | 2)
            || self.max_spread_bps > 10000
            || self.cooldown_ns > MAX_TIME
            || !(1..=MAX_TIME).contains(&self.max_working_ns)
            || !(1..=Quantity::MAX).contains(&self.min_rebalance_lots)
            || !(1..=Quantity::MAX).contains(&self.max_child_lots)
        {
            return Err("invalid composition version/limits");
        }
        match self.signal {
            SignalConfig::Constant { exposure_bps } if exposure_bps > 10000 => {
                return Err("invalid signal exposure");
            }
            SignalConfig::Threshold {
                buy_below,
                sell_above,
            } if buy_below >= sell_above => return Err("invalid threshold ordering"),
            SignalConfig::SmaCross {
                fast,
                slow,
                band_bps,
            } if fast == 0
                || fast >= slow
                || fast > 4096
                || slow > 4096
                || fast + slow > 4096
                || band_bps > 1000 =>
            {
                return Err("invalid composition SMA windows/band");
            }
            _ => {}
        }
        match self.sizing {
            SizingConfig::FixedLots { lots } if lots == 0 || lots > Quantity::MAX => {
                return Err("invalid fixed lots");
            }
            SizingConfig::CashBudget { budget_minor }
                if budget_minor <= 0 || budget_minor > 10i128.pow(30) =>
            {
                return Err("invalid cash sizing budget");
            }
            _ => {}
        }
        if let Schedule::Twap {
            interval_ns,
            slices,
        } = self.schedule
            && (!(1..=MAX_TIME).contains(&interval_ns)
                || !(1..=1024).contains(&slices)
                || interval_ns
                    .checked_mul(u64::from(slices - 1))
                    .is_none_or(|v| v > MAX_TIME))
        {
            return Err("invalid TWAP interval/horizon");
        }
        match self.schedule {
            Schedule::Iceberg {
                display_lots,
                replenish_interval_ns,
                ..
            } if !(1..=Quantity::MAX).contains(&display_lots)
                || replenish_interval_ns > MAX_TIME =>
            {
                return Err("invalid Iceberg display/interval");
            }
            Schedule::BestLimit { min_reprice_ns, .. } if min_reprice_ns > MAX_TIME => {
                return Err("invalid BestLimit reprice interval");
            }
            _ => (),
        }
        Ok(())
    }
    pub fn window_slots(&self) -> usize {
        match self.signal {
            SignalConfig::SmaCross { fast, slow, .. } => fast + slow,
            _ => 0,
        }
    }
    pub fn validate_market(&self, m: &crate::config::MarketConfig) -> Result<(), &'static str> {
        self.validate()?;
        if !self.max_child_lots.is_multiple_of(m.quantity_step)
            || !self.min_rebalance_lots.is_multiple_of(m.quantity_step)
        {
            return Err("composition sizes must match quantity grid");
        }
        if let SizingConfig::FixedLots { lots } = self.sizing
            && (lots > m.engine.max_position || !lots.is_multiple_of(m.quantity_step))
        {
            return Err("composition target outside market position/grid");
        }
        match self.schedule {
            Schedule::Iceberg {
                limit,
                display_lots,
                ..
            } => {
                if !limit.units().is_multiple_of(m.price_tick)
                    || !display_lots.is_multiple_of(m.quantity_step)
                {
                    return Err("Iceberg limit/display outside market grid");
                }
            }
            Schedule::BestLimit { limit_guard, .. }
                if !limit_guard.units().is_multiple_of(m.price_tick) =>
            {
                return Err("BestLimit guard outside market grid");
            }
            _ => (),
        }
        Ok(())
    }
}
/// PaperRuntime 在已验证市场边界提供这些约束；不改变旧 StrategyView 的公开构造方式。
#[derive(Clone, Copy)]
pub struct Constraints {
    pub price_tick: u64,
    pub price_collar_bps: u32,
    pub quantity_step: u64,
    pub max_position: u64,
    pub fee_bps: u32,
    pub max_order_notional: i128,
}
impl Constraints {
    pub fn from_market(m: &crate::config::MarketConfig) -> Self {
        Self {
            price_tick: m.price_tick,
            price_collar_bps: m.risk.price_collar_bps,
            quantity_step: m.quantity_step,
            max_position: m.engine.max_position,
            fee_bps: m.engine.fee_bps,
            max_order_notional: m.risk.max_order_notional,
        }
    }
    fn valid(self) -> bool {
        (1..=Quantity::MAX).contains(&self.quantity_step)
            && (1..=Price::MAX).contains(&self.price_tick)
            && self.price_collar_bps <= 10000
            && (1..=Quantity::MAX).contains(&self.max_position)
            && self.fee_bps <= 10000
            && self.max_order_notional > 0
            && self.max_order_notional <= i128::from(Price::MAX) * i128::from(Quantity::MAX)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    Warmup,
    Aligned,
    ForeignWorking,
    Working,
    CancelPending,
    TargetChanged,
    WorkingExpired,
    SpreadBlocked,
    Cooldown,
    SliceNotDue,
    Dust,
    Budget,
    Submitting,
    Accepted,
    RiskRejected(crate::paper::RiskReject),
    EngineRejected(RejectReason),
    InvalidContext,
    ParentCapacity,
    ReplenishWaiting,
    PriceMoved,
    PriceGuard,
    ForeignPosition,
    PriceBand,
    PriceGrid,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub quote_sequence: u64,
    pub timestamp_ns: u64,
    pub exposure_bps: u32,
    pub target_lots: u64,
    pub position_lots: u64,
    pub projected_lots: u64,
    pub authorized_lots: u64,
    pub parent_id: Option<u64>,
    pub action: Action,
    pub order_id: Option<OrderId>,
    pub reason: DecisionReason,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Child {
    id: OrderId,
    request: OrderRequest,
    remaining: u64,
    submitted_ns: u64,
    cancel_pending: bool,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Parent {
    id: u64,
    start_ns: u64,
    start_position: u64,
    target: u64,
    side: Side,
    total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    progress: Option<ParentProgress>,
}
/// 新算法按自己的成交回报核对父计划，不把手工成交当执行进度。
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentProgress {
    filled_lots: u64,
    submitted_children: u64,
    cancelled_children: u64,
    reprice_requests: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompositionStrategy {
    config: CompositionConfig,
    fast: Option<RollingMean>,
    slow: Option<RollingMean>,
    exposure_bps: u32,
    generation: u64,
    parent: Option<Parent>,
    child: Option<Child>,
    last_submit_ns: Option<u64>,
    decision: Option<Decision>,
    proposal: Option<OrderRequest>,
}
impl CompositionStrategy {
    pub fn new(config: CompositionConfig) -> Result<Self, &'static str> {
        config.validate()?;
        let (fast, slow) = match config.signal {
            SignalConfig::SmaCross { fast, slow, .. } => {
                (Some(RollingMean::new(fast)?), Some(RollingMean::new(slow)?))
            }
            _ => (None, None),
        };
        Ok(Self {
            config,
            fast,
            slow,
            exposure_bps: 0,
            generation: 0,
            parent: None,
            child: None,
            last_submit_ns: None,
            decision: None,
            proposal: None,
        })
    }
    pub fn warmup(&mut self, q: Quote) {
        self.signal(q);
    }
    fn signal(&mut self, q: Quote) -> Option<u32> {
        match self.config.signal {
            SignalConfig::Constant { exposure_bps } => self.exposure_bps = exposure_bps,
            SignalConfig::Threshold {
                buy_below,
                sell_above,
            } => {
                if q.ask <= buy_below {
                    self.exposure_bps = 10000;
                } else if q.bid >= sell_above {
                    self.exposure_bps = 0;
                }
            }
            SignalConfig::SmaCross { band_bps, .. } => {
                let a = self.fast.as_mut()?.push(q.bid);
                let b = self.slow.as_mut()?.push(q.bid);
                let (Some(a), Some(b)) = (a, b) else {
                    return None;
                };
                if u128::from(a) * 10000 > u128::from(b) * u128::from(10000 + band_bps) {
                    self.exposure_bps = 10000;
                } else if u128::from(a) * 10000 < u128::from(b) * u128::from(10000 - band_bps) {
                    self.exposure_bps = 0;
                }
            }
        }
        Some(self.exposure_bps)
    }
    fn authorized(&self, p: &Parent, step: u64, now: u64) -> u64 {
        match self.config.schedule {
            Schedule::Immediate | Schedule::Iceberg { .. } | Schedule::BestLimit { .. } => p.total,
            Schedule::Twap {
                interval_ns,
                slices,
            } => {
                let due = (now.saturating_sub(p.start_ns) / interval_ns)
                    .saturating_add(1)
                    .min(u64::from(slices));
                (u128::from(p.total / step) * u128::from(due) / u128::from(slices)) as u64 * step
            }
        }
    }
    fn tracks_parent_fills(&self) -> bool {
        self.config.version == 2
            || matches!(
                self.config.schedule,
                Schedule::Iceberg { .. } | Schedule::BestLimit { .. }
            )
    }
    fn execution_limit(&self, side: Side, q: Quote) -> Price {
        match self.config.schedule {
            Schedule::Iceberg { limit, .. } => limit,
            Schedule::BestLimit { .. } => {
                if side == Side::Buy {
                    q.bid
                } else {
                    q.ask
                }
            }
            _ => {
                if side == Side::Buy {
                    q.ask
                } else {
                    q.bid
                }
            }
        }
    }
    fn within_guard(&self, side: Side, limit: Price) -> bool {
        match self.config.schedule {
            Schedule::BestLimit { limit_guard, .. } => {
                if side == Side::Buy {
                    limit <= limit_guard
                } else {
                    limit >= limit_guard
                }
            }
            _ => true,
        }
    }
    fn finish(&mut self, mut d: Decision, reason: DecisionReason, action: Action) -> Action {
        d.reason = reason;
        d.action = action;
        self.decision = Some(d);
        action
    }
    pub fn decide(&mut self, v: StrategyView<'_>, c: Constraints) -> Action {
        self.decide_impl(v, c, None)
    }
    /// 可信策略扩展提供整数目标；执行仍共享同一资源/TWAP/回报状态机。
    pub fn decide_target(&mut self, v: StrategyView<'_>, c: Constraints, target: u64) -> Action {
        self.decide_impl(v, c, Some(target))
    }
    fn decide_impl(
        &mut self,
        v: StrategyView<'_>,
        c: Constraints,
        target_override: Option<u64>,
    ) -> Action {
        // 所有下单成功/拒绝必须反馈后才能再次规划；PaperRuntime 同步执行此握手。
        if self.proposal.is_some() {
            return Action::None;
        }
        let pos = v.account.position();
        let projected = pos
            .checked_add(v.account.pending_buy())
            .and_then(|p| p.checked_sub(v.account.reserved_sell()));
        let mut d = Decision {
            quote_sequence: v.quote.sequence,
            timestamp_ns: v.quote.timestamp_ns,
            exposure_bps: self.exposure_bps,
            target_lots: pos,
            position_lots: pos,
            projected_lots: projected.unwrap_or(pos),
            authorized_lots: 0,
            parent_id: self.parent.as_ref().map(|p| p.id),
            action: Action::None,
            order_id: None,
            reason: DecisionReason::Aligned,
        };
        if !c.valid()
            || v.quote.validate().is_err()
            || pos > Quantity::MAX
            || v.account.pending_buy() > Quantity::MAX.saturating_sub(pos)
            || v.account.reserved_sell() > pos
            || v.account.available_cash() < 0
            || !pos.is_multiple_of(c.quantity_step)
            || projected.is_none()
        {
            return self.finish(d, DecisionReason::InvalidContext, Action::None);
        }
        if let Some(p) = &self.parent {
            d.authorized_lots = self.authorized(p, c.quantity_step, v.quote.timestamp_ns);
        }
        let Some(exposure) = target_override
            .map(|t| if t == 0 { 0 } else { 10000 })
            .or_else(|| self.signal(v.quote))
        else {
            return self.finish(d, DecisionReason::Warmup, Action::None);
        };
        d.exposure_bps = exposure;
        let per_unit =
            i128::from(v.quote.ask.units()) + fee(i128::from(v.quote.ask.units()), c.fee_bps);
        self.exposure_bps = exposure;
        let raw = if let Some(target) = target_override {
            target.min(c.max_position)
        } else {
            let base = match self.config.sizing {
                SizingConfig::FixedLots { lots } => i128::from(lots),
                SizingConfig::CashBudget { budget_minor } => budget_minor / per_unit,
            };
            (base * i128::from(exposure) / 10000).min(i128::from(c.max_position)) as u64
        };
        let mut target = raw / c.quantity_step * c.quantity_step;
        let spread = u128::from(v.quote.ask.units() - v.quote.bid.units()) * 10000
            > u128::from(v.quote.bid.units()) * u128::from(self.config.max_spread_bps);
        // 过滤只阻止增加风险；宽价差也不能阻止撤销待买或退出已有仓位。
        if spread {
            target = target.min(pos);
        }
        d.target_lots = target;
        if v.active_orders > usize::from(self.child.is_some()) {
            return self.finish(d, DecisionReason::ForeignWorking, Action::None);
        }
        if self.parent.as_ref().is_some_and(|p| {
            p.progress.as_ref().is_some_and(|progress| {
                let expected = if p.side == Side::Buy {
                    p.start_position + progress.filled_lots
                } else {
                    p.start_position - progress.filled_lots
                };
                expected != pos
            })
        }) {
            let action = if let Some(child) = &mut self.child {
                if child.cancel_pending {
                    Action::None
                } else {
                    child.cancel_pending = true;
                    Action::Cancel(child.id)
                }
            } else {
                Action::None
            };
            return self.finish(d, DecisionReason::ForeignPosition, action);
        }
        let desired_child_limit = self
            .child
            .as_ref()
            .map(|child| self.execution_limit(child.request.side, v.quote));
        let price_guard = self.child.as_ref().is_some_and(|child| {
            !self.within_guard(
                child.request.side,
                desired_child_limit.expect("child limit"),
            )
        });
        let min_reprice_ns =
            if let Schedule::BestLimit { min_reprice_ns, .. } = self.config.schedule {
                Some(min_reprice_ns)
            } else {
                None
            };
        if let Some(child) = &mut self.child {
            d.order_id = Some(child.id);
            if child.cancel_pending {
                return self.finish(d, DecisionReason::CancelPending, Action::None);
            }
            let wrong = match child.request.side {
                Side::Buy => target < d.projected_lots,
                Side::Sell => target > d.projected_lots,
            };
            let expired = v.quote.timestamp_ns.saturating_sub(child.submitted_ns)
                >= self.config.max_working_ns;
            let reprice = min_reprice_ns.is_some_and(|interval| {
                child.request.limit != desired_child_limit.expect("child limit")
                    && v.quote.timestamp_ns.saturating_sub(child.submitted_ns) >= interval
            });
            if wrong || expired || price_guard || reprice {
                child.cancel_pending = true;
                let id = child.id;
                if reprice
                    && !wrong
                    && !expired
                    && !price_guard
                    && let Some(p) = &mut self.parent
                    && let Some(progress) = &mut p.progress
                {
                    progress.reprice_requests += 1;
                }
                return self.finish(
                    d,
                    if wrong {
                        DecisionReason::TargetChanged
                    } else if expired {
                        DecisionReason::WorkingExpired
                    } else if price_guard {
                        DecisionReason::PriceGuard
                    } else {
                        DecisionReason::PriceMoved
                    },
                    Action::Cancel(id),
                );
            }
            return self.finish(d, DecisionReason::Working, Action::None);
        }
        if pos == target {
            return self.finish(
                d,
                if spread && raw > pos {
                    DecisionReason::SpreadBlocked
                } else {
                    DecisionReason::Aligned
                },
                Action::None,
            );
        }
        let delta = pos.abs_diff(target);
        if delta < self.config.min_rebalance_lots {
            return self.finish(d, DecisionReason::Dust, Action::None);
        }
        let side = if target > pos { Side::Buy } else { Side::Sell };
        if let Schedule::Iceberg {
            replenish_interval_ns,
            ..
        } = self.config.schedule
            && self
                .last_submit_ns
                .is_some_and(|t| v.quote.timestamp_ns.saturating_sub(t) < replenish_interval_ns)
        {
            return self.finish(d, DecisionReason::ReplenishWaiting, Action::None);
        }
        if side == Side::Buy
            && self
                .last_submit_ns
                .is_some_and(|t| v.quote.timestamp_ns.saturating_sub(t) < self.config.cooldown_ns)
        {
            return self.finish(d, DecisionReason::Cooldown, Action::None);
        }
        let reset = self.parent.as_ref().is_none_or(|p| {
            p.target != target
                || p.side != side
                || pos < p.start_position.min(target)
                || pos > p.start_position.max(target)
        });
        if reset {
            if self.generation >= MAX_PARENTS {
                return self.finish(d, DecisionReason::ParentCapacity, Action::None);
            }
            self.generation += 1;
            self.parent = Some(Parent {
                id: self.generation,
                start_ns: v.quote.timestamp_ns,
                start_position: pos,
                target,
                side,
                total: delta,
                progress: self.tracks_parent_fills().then(ParentProgress::default),
            });
        }
        let p = self.parent.as_ref().expect("parent installed");
        d.parent_id = Some(p.id);
        let authorized = self.authorized(p, c.quantity_step, v.quote.timestamp_ns);
        d.authorized_lots = authorized;
        let completed = p.progress.as_ref().map_or_else(
            || pos.abs_diff(p.start_position),
            |progress| progress.filled_lots,
        );
        let due = authorized.saturating_sub(completed);
        if due == 0 {
            return self.finish(d, DecisionReason::SliceNotDue, Action::None);
        }
        let limit = self.execution_limit(side, v.quote);
        if !self.within_guard(side, limit) {
            return self.finish(d, DecisionReason::PriceGuard, Action::None);
        }
        if self.tracks_parent_fills() {
            if !limit.units().is_multiple_of(c.price_tick) {
                return self.finish(d, DecisionReason::PriceGrid, Action::None);
            }
            let reference = if side == Side::Buy {
                v.quote.ask
            } else {
                v.quote.bid
            };
            if u128::from(limit.units().abs_diff(reference.units())) * 10000
                > u128::from(reference.units()) * u128::from(c.price_collar_bps)
            {
                return self.finish(d, DecisionReason::PriceBand, Action::None);
            }
        }
        // 目标预算仍按现价计算；实际买单冻结/资源夹紧必须按委托限价覆盖费用。
        let execution_per_unit =
            i128::from(limit.units()) + fee(i128::from(limit.units()), c.fee_bps);
        let budget = (c.max_order_notional / i128::from(limit.units()))
            .min(i128::from(Quantity::MAX)) as u64;
        let available = match side {
            Side::Buy => (v.account.available_cash() / execution_per_unit)
                .max(0)
                .min(i128::from(Quantity::MAX)) as u64,
            Side::Sell => pos - v.account.reserved_sell(),
        };
        let size = due
            .min(delta)
            .min(self.config.max_child_lots)
            .min(
                if let Schedule::Iceberg { display_lots, .. } = self.config.schedule {
                    display_lots
                } else {
                    Quantity::MAX
                },
            )
            .min(budget)
            .min(available)
            / c.quantity_step
            * c.quantity_step;
        if size == 0 {
            return self.finish(d, DecisionReason::Budget, Action::None);
        }
        let request = OrderRequest {
            side,
            limit,
            quantity: Quantity::new(size).expect("bounded positive size"),
            time_in_force: TimeInForce::GoodTilCancelled,
        };
        self.proposal = Some(request);
        self.finish(d, DecisionReason::Submitting, Action::Submit(request))
    }
    pub fn validate_state(&self, config: &CompositionConfig) -> Result<(), &'static str> {
        config.validate()?;
        if &self.config != config
            || self.exposure_bps > 10000
            || self.generation > MAX_PARENTS
            || self.proposal.is_some()
        {
            return Err("composition checkpoint identity/handshake conflict");
        }
        if !matches!(config.signal, SignalConfig::Constant { .. })
            && self.exposure_bps != 0
            && self.exposure_bps != 10000
        {
            return Err("invalid discrete signal exposure");
        }
        match config.signal {
            SignalConfig::SmaCross { fast, slow, .. } => {
                self.fast
                    .as_ref()
                    .ok_or("fast state absent")?
                    .validate_state(fast)?;
                self.slow
                    .as_ref()
                    .ok_or("slow state absent")?
                    .validate_state(slow)?;
            }
            _ => {
                if self.fast.is_some() || self.slow.is_some() {
                    return Err("unexpected signal windows");
                }
            }
        }
        if let Some(p) = &self.parent {
            if p.id == 0
                || p.id != self.generation
                || p.total == 0
                || p.total > Quantity::MAX
                || p.start_position > Quantity::MAX
                || p.target > Quantity::MAX
                || p.total != p.start_position.abs_diff(p.target)
                || p.side
                    != if p.target > p.start_position {
                        Side::Buy
                    } else {
                        Side::Sell
                    }
            {
                return Err("invalid target parent");
            }
            if p.progress.is_some() != self.tracks_parent_fills()
                || p.progress.as_ref().is_some_and(|progress| {
                    progress.filled_lots > p.total
                        || progress.submitted_children > crate::engine::MAX_LIFETIME_ORDERS
                        || progress.cancelled_children > progress.submitted_children
                        || progress.reprice_requests > progress.submitted_children
                })
            {
                return Err("invalid algorithm parent progress");
            }
        } else if self.generation != 0 {
            return Err("target generation lacks parent");
        }
        if let Some(ch) = &self.child
            && (ch.id.0 == 0
                || ch.remaining == 0
                || ch.remaining > ch.request.quantity.units()
                || ch.request.time_in_force != TimeInForce::GoodTilCancelled
                || self.last_submit_ns != Some(ch.submitted_ns)
                || self.parent.as_ref().is_none_or(|p| {
                    p.side != ch.request.side || ch.request.quantity.units() > p.total
                }))
        {
            return Err("invalid target child");
        }
        if let Some(ch) = &self.child
            && self.tracks_parent_fills()
        {
            let display = if let Schedule::Iceberg { display_lots, .. } = config.schedule {
                display_lots
            } else {
                Quantity::MAX
            };
            if ch.request.quantity.units() > config.max_child_lots.min(display)
                || !self.within_guard(ch.request.side, ch.request.limit)
                || matches!(config.schedule, Schedule::Iceberg { limit, .. } if ch.request.limit != limit)
                || self
                    .parent
                    .as_ref()
                    .and_then(|p| p.progress.as_ref())
                    .is_none_or(|p| {
                        p.submitted_children == 0
                            || self
                                .parent
                                .as_ref()
                                .is_some_and(|parent| p.filled_lots + ch.remaining > parent.total)
                    })
            {
                return Err("algorithm child outside display/guard/progress");
            }
        }
        if let Some(d) = &self.decision
            && (d.quote_sequence == 0
                || d.exposure_bps > 10000
                || d.target_lots > Quantity::MAX
                || d.position_lots > Quantity::MAX
                || d.projected_lots > Quantity::MAX
                || d.authorized_lots > Quantity::MAX
                || d.parent_id
                    .is_some_and(|id| id == 0 || id > self.generation))
        {
            return Err("invalid target decision");
        }
        Ok(())
    }
    pub fn validate_execution(&self, e: &Engine) -> Result<(), &'static str> {
        self.validate_state(&self.config)?;
        if let Some(ch) = &self.child {
            let o = e.order(ch.id).ok_or("target child absent in engine")?;
            if !o.status.is_active()
                || o.remaining != ch.remaining
                || o.request != ch.request
                || e.last_quote()
                    .is_none_or(|q| ch.submitted_ns > q.timestamp_ns)
            {
                return Err("target child/engine mismatch");
            }
        }
        if self.decision.as_ref().is_some_and(|d| {
            e.last_quote()
                .is_none_or(|q| d.timestamp_ns > q.timestamp_ns || d.quote_sequence > q.sequence)
        }) {
            return Err("target decision from future");
        }
        if self
            .last_submit_ns
            .is_some_and(|t| e.last_quote().is_none_or(|q| t > q.timestamp_ns))
            || self
                .parent
                .as_ref()
                .is_some_and(|p| e.last_quote().is_none_or(|q| p.start_ns > q.timestamp_ns))
        {
            return Err("target plan from future");
        }
        Ok(())
    }
    pub fn decision(&self) -> Option<&Decision> {
        self.decision.as_ref()
    }
    pub fn risk_rejected(&mut self, request: OrderRequest, reason: crate::paper::RiskReject) {
        if self.proposal.take() == Some(request)
            && let Some(d) = &mut self.decision
        {
            d.reason = DecisionReason::RiskRejected(reason);
        }
    }
}
impl Strategy for CompositionStrategy {
    fn on_quote(&mut self, v: StrategyView<'_>) -> Action {
        self.warmup(v.quote);
        Action::None
    }
    fn on_quote_with_constraints(
        &mut self,
        v: StrategyView<'_>,
        c: Constraints,
        a: &mut ActionBuffer,
    ) {
        let _ = a.push(self.decide(v, c));
    }
    fn on_submit_result(&mut self, r: OrderRequest, result: Result<OrderId, RejectReason>) {
        if self.proposal.take() != Some(r) {
            return;
        }
        let Some(d) = &mut self.decision else {
            return;
        };
        match result {
            Ok(id) => {
                if let Some(p) = &mut self.parent
                    && let Some(progress) = &mut p.progress
                {
                    progress.submitted_children += 1;
                }
                self.child = Some(Child {
                    id,
                    request: r,
                    remaining: r.quantity.units(),
                    submitted_ns: d.timestamp_ns,
                    cancel_pending: false,
                });
                self.last_submit_ns = Some(d.timestamp_ns);
                d.order_id = Some(id);
                d.reason = DecisionReason::Accepted;
            }
            Err(reason) => d.reason = DecisionReason::EngineRejected(reason),
        }
    }
    fn on_event(&mut self, event: Event) {
        match event {
            Event::Fill(f) => {
                if let Some(ch) = &mut self.child
                    && ch.id == f.order_id
                {
                    if f.side != ch.request.side || f.quantity > ch.remaining {
                        return;
                    }
                    ch.remaining -= f.quantity;
                    if let Some(p) = &mut self.parent
                        && let Some(progress) = &mut p.progress
                    {
                        progress.filled_lots += f.quantity;
                    }
                    if ch.remaining == 0 {
                        self.child = None;
                    }
                }
            }
            Event::Cancelled { order_id, .. }
                if self.child.as_ref().is_some_and(|c| c.id == order_id) =>
            {
                if let Some(p) = &mut self.parent
                    && let Some(progress) = &mut p.progress
                {
                    progress.cancelled_children += 1;
                }
                self.child = None;
            }
            _ => {}
        }
    }
    fn on_risk_rejected(&mut self, r: OrderRequest, reason: crate::paper::RiskReject) {
        self.risk_rejected(r, reason);
    }
    fn validate_execution(&self, e: &Engine) -> Result<(), &'static str> {
        CompositionStrategy::validate_execution(self, e)
    }
    fn decision(&self) -> Option<&Decision> {
        CompositionStrategy::decision(self)
    }
    fn diagnostics(&self) -> Option<serde_json::Value> {
        self.tracks_parent_fills().then(|| serde_json::json!({"algorithm":self.config.schedule,"parent":self.parent,"child":self.child,"scope":"single-market paper parent/child; cancel acknowledgement before replacement; no native iceberg or external queue model"}))
    }
}
