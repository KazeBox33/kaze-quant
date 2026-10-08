//! 同一现货市场的多策略所有权、虚拟子账和显式生命周期。共享一个Engine撮合，不重复消费流动性。
use crate::{
    account::{Account, fee},
    conditional::*,
    config::{BuiltinStrategy, MarketConfig, PaperConfig, StrategyConfig},
    engine::Engine,
    paper::{PaperError, RiskReject},
    registry::CustomCheckpoint,
    strategy::{ActionBuffer, ActionSource, Strategy, StrategyView},
    target::Constraints,
    types::*,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
const NAME: &str = "managed";
const MAX_MEMBERS: usize = 8;
const MAX_CAPITAL: i128 = 1_000_000_000_000_000_000_000_000_000_000;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Created,
    Ready,
    Running,
    Stopped,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Control {
    Init,
    Start,
    Stop,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberConfig {
    pub name: String,
    #[serde(with = "crate::config::money")]
    pub initial_cash: i128,
    pub max_position: u64,
    pub strategy: StrategyConfig,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub members: Vec<MemberConfig>,
    pub max_working: usize,
}
impl Parameters {
    fn validate(&self) -> Result<(), PaperError> {
        if !(1..=MAX_MEMBERS).contains(&self.members.len()) || !(1..=64).contains(&self.max_working)
        {
            return Err("managed member/working capacity out of bounds".into());
        }
        let mut names = BTreeSet::new();
        let mut sum = 0i128;
        let mut windows = 0usize;
        for m in &self.members {
            crate::registry::validate_identity(&m.name, 1)?;
            if !names.insert(&m.name)
                || !(0..=MAX_CAPITAL).contains(&m.initial_cash)
                || !(1..=Quantity::MAX).contains(&m.max_position)
            {
                return Err("managed duplicate name/capital/position invalid".into());
            }
            sum += m.initial_cash;
            let slots = match &m.strategy {
                StrategyConfig::Momentum { window, .. }
                | StrategyConfig::MeanReversion { window, .. } => *window,
                StrategyConfig::SmaCross { fast, slow, .. } => fast
                    .checked_add(*slow)
                    .ok_or("managed window size overflow")?,
                StrategyConfig::Composition { plan } => {
                    plan.validate()?;
                    plan.window_slots()
                }
                StrategyConfig::Registered {
                    name, parameters, ..
                } if name == "bar-atr" => {
                    let p: crate::bar_strategy::Parameters =
                        serde_json::from_value(parameters.clone())?;
                    p.atr_period
                        .checked_add(p.trend_window)
                        .ok_or("managed bar window size overflow")?
                }
                _ => 0,
            };
            windows = windows
                .checked_add(slots)
                .ok_or("managed aggregate window size overflow")?;
            if windows > 1024 {
                return Err("managed aggregate indicator windows exceed 1024 slots".into());
            }
            match &m.strategy {
                StrategyConfig::Registered{name,version,..} if *version!=1 || !matches!(name.as_str(),"bar-atr"|"breakout-bracket") => return Err("managed children require builtin or standard bar-atr/bracket v1; nesting forbidden".into()),
                StrategyConfig::Composition{plan} if plan.version!=2 => return Err("managed composition requires own-fill version 2".into()),
                _=>(),
            }
        }
        if sum > MAX_CAPITAL {
            return Err("managed total capital exceeded".into());
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ChildCheckpoint {
    Builtin { state: serde_json::Value },
    Registered { checkpoint: CustomCheckpoint },
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberState {
    lifecycle: Lifecycle,
    account: Account,
    #[serde(with = "crate::config::money")]
    capital: i128,
    active: usize,
    waiting: usize,
    child: ChildCheckpoint,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedOrder {
    owner: usize,
    request: OrderRequest,
    remaining: u64,
    #[serde(with = "crate::config::money")]
    reserve_per_unit: i128,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedConditional {
    owner: usize,
    request: ConditionalRequest,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    members: Vec<MemberState>,
    orders: BTreeMap<u64, OwnedOrder>,
    conditionals: BTreeMap<u64, OwnedConditional>,
}
struct Member {
    lifecycle: Lifecycle,
    account: Account,
    capital: i128,
    active: usize,
    waiting: usize,
    child: Box<dyn Strategy>,
}
#[derive(Clone, Copy)]
struct Pending {
    owner: usize,
    action: Action,
    reserve: i128,
    activation: Option<ConditionalId>,
}
pub struct ManagedStrategy {
    parameters: Parameters,
    members: Vec<Member>,
    orders: BTreeMap<u64, OwnedOrder>,
    conditionals: BTreeMap<u64, OwnedConditional>,
    pending: VecDeque<Pending>,
    current: Option<Pending>,
    retired: BTreeMap<u64, usize>,
    scratch: ActionBuffer,
}
fn checkpoint(s: &dyn Strategy) -> Option<ChildCheckpoint> {
    s.custom_checkpoint()
        .map(|checkpoint| ChildCheckpoint::Registered { checkpoint })
        .or_else(|| {
            s.checkpoint().and_then(|state| {
                serde_json::to_value(state)
                    .ok()
                    .map(|state| ChildCheckpoint::Builtin { state })
            })
        })
}
fn child(
    c: &StrategyConfig,
    saved: Option<ChildCheckpoint>,
) -> Result<Box<dyn Strategy>, PaperError> {
    match (c, saved) {
        (
            StrategyConfig::Registered {
                name,
                version,
                parameters,
            },
            saved,
        ) => {
            let state = match saved {
                None => None,
                Some(ChildCheckpoint::Registered { checkpoint })
                    if checkpoint.name == *name && checkpoint.version == *version =>
                {
                    Some(checkpoint.state)
                }
                _ => return Err("managed child checkpoint identity changed".into()),
            };
            match (name.as_str(), *version) {
                ("bar-atr", 1) => crate::bar_strategy::factory(parameters, state.as_ref()),
                ("breakout-bracket", 1) => {
                    crate::bracket_strategy::factory(parameters, state.as_ref())
                }
                _ => Err("unsupported managed child factory".into()),
            }
        }
        (c, None) => Ok(Box::new(c.build()?)),
        (c, Some(ChildCheckpoint::Builtin { state })) => {
            let state: BuiltinStrategy = serde_json::from_value(state)?;
            state.validate_state(c)?;
            Ok(Box::new(state))
        }
        _ => Err("managed child checkpoint type changed".into()),
    }
}
fn wire(mut r: ConditionalRequest, owner: usize) -> Option<ConditionalRequest> {
    if let Some(g) = r.oco_group {
        if g == 0 || g > (crate::engine::MAX_LIFETIME_ORDERS - 7) / 8 {
            return None;
        }
        r.oco_group = Some(g * 8 + owner as u64);
    }
    Some(r)
}
fn unwire(mut r: ConditionalRequest) -> ConditionalRequest {
    r.oco_group = r.oco_group.map(|g| g / 8);
    r
}
impl ManagedStrategy {
    fn state(&self) -> Option<State> {
        if !self.pending.is_empty() || self.current.is_some() || !self.retired.is_empty() {
            return None;
        }
        Some(State {
            members: self
                .members
                .iter()
                .map(|m| {
                    Some(MemberState {
                        lifecycle: m.lifecycle,
                        account: m.account.clone(),
                        capital: m.capital,
                        active: m.active,
                        waiting: m.waiting,
                        child: checkpoint(m.child.as_ref())?,
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            orders: self.orders.clone(),
            conditionals: self.conditionals.clone(),
        })
    }
    fn enqueue(&mut self, owner: usize, action: Action, out: &mut ActionBuffer) {
        let action = match action {
            Action::SubmitConditional(r) => match wire(r, owner) {
                Some(r) => Action::SubmitConditional(r),
                None => {
                    out.invalidate();
                    return;
                }
            },
            other => other,
        };
        if action != Action::None {
            self.pending.push_back(Pending {
                owner,
                action,
                reserve: 0,
                activation: None,
            });
            let _ = out.push(action);
        }
    }
    fn validate_state(&self) -> Result<(), &'static str> {
        if self.current.is_some()
            || !self.pending.is_empty()
            || !self.retired.is_empty()
            || self.members.len() != self.parameters.members.len()
            || self.orders.len() + self.conditionals.len() > self.parameters.max_working
        {
            return Err("managed checkpoint handshake/capacity invalid");
        }
        for (i, m) in self.members.iter().enumerate() {
            let cfg = &self.parameters.members[i];
            let mut reserve = 0i128;
            let mut pending = 0u64;
            let mut sell = 0u64;
            let mut count = 0usize;
            for (&id, o) in &self.orders {
                if o.owner >= self.members.len()
                    || id == 0
                    || id > crate::engine::MAX_LIFETIME_ORDERS
                    || o.remaining == 0
                    || o.remaining > o.request.quantity.units()
                    || !(0..=2 * i128::from(Price::MAX)).contains(&o.reserve_per_unit)
                {
                    return Err("managed owned order invalid");
                }
                if o.owner == i {
                    count += 1;
                    match o.request.side {
                        Side::Buy => {
                            reserve += o.reserve_per_unit * i128::from(o.remaining);
                            pending += o.remaining;
                        }
                        Side::Sell => sell += o.remaining,
                    }
                }
            }
            let waiting = self.conditionals.values().filter(|o| o.owner == i).count();
            if m.account.position() > cfg.max_position
                || !(-10i128.pow(33)..=10i128.pow(33)).contains(&m.account.net_buy_notional)
                || !(0..=10i128.pow(33)).contains(&m.account.fees_paid())
                || m.active != count
                || m.waiting != waiting
                || m.account.reserved_cash() != reserve
                || m.account.pending_buy() != pending
                || m.account.reserved_sell() != sell
                || m.account.cash() < reserve
                || m.account.position() < sell
                || m.account.position() + pending > cfg.max_position
                || m.capital < 0
                || m.capital > MAX_CAPITAL
                || m.account.fees_paid() < 0
                || m.account.cash()
                    != m.capital - m.account.net_buy_notional - m.account.fees_paid()
            {
                return Err("managed member ledger/limits inconsistent");
            }
            if m.lifecycle != Lifecycle::Running && (count != 0 || waiting != 0) {
                return Err("inactive managed member has work");
            }
        }
        for (&id, c) in &self.conditionals {
            if id == 0
                || id > crate::engine::MAX_LIFETIME_ORDERS
                || c.owner >= self.members.len()
                || c.request.validate().is_err()
                || wire(unwire(c.request), c.owner) != Some(c.request)
            {
                return Err("managed conditional owner/namespace invalid");
            }
        }
        Ok(())
    }
}
impl Strategy for ManagedStrategy {
    fn validate_market_config(&self, market: &MarketConfig) -> Result<(), &'static str> {
        if self
            .parameters
            .members
            .iter()
            .map(|m| m.initial_cash)
            .sum::<i128>()
            != market.engine.initial_cash
        {
            return Err("managed capital must exactly partition market initial cash");
        }
        for member in &self.parameters.members {
            if member.max_position > market.engine.max_position {
                return Err("managed position exceeds market limit");
            }
            let mut m = market.clone();
            m.engine.initial_cash = member.initial_cash;
            m.engine.max_position = member.max_position;
            m.strategy = member.strategy.clone();
            PaperConfig {
                schema_version: 1,
                max_commands: 1,
                markets: vec![m],
            }
            .validate()?;
        }
        Ok(())
    }
    fn validate_control(&self, owner: usize, op: Control) -> Result<(), &'static str> {
        let m = self.members.get(owner).ok_or("unknown managed owner")?;
        match (op, m.lifecycle) {
            (Control::Init, Lifecycle::Created)
            | (Control::Start, Lifecycle::Ready | Lifecycle::Stopped)
            | (Control::Stop, Lifecycle::Running) => Ok(()),
            _ => Err("invalid strategy lifecycle transition"),
        }
    }
    fn control(&mut self, owner: usize, op: Control, out: &mut ActionBuffer) {
        self.members[owner].lifecycle = match op {
            Control::Init => Lifecycle::Ready,
            Control::Start => Lifecycle::Running,
            Control::Stop => Lifecycle::Stopped,
        };
        if op == Control::Stop {
            let actions: Vec<_> = self
                .orders
                .iter()
                .filter(|(_, o)| o.owner == owner)
                .map(|(&id, _)| Action::Cancel(OrderId(id)))
                .chain(
                    self.conditionals
                        .iter()
                        .filter(|(_, o)| o.owner == owner)
                        .map(|(&id, _)| Action::CancelConditional(ConditionalId(id))),
                )
                .collect();
            for a in actions {
                self.enqueue(owner, a, out);
            }
        }
    }
    fn validate_owned_action(&self, owner: usize, a: Action) -> Result<(), &'static str> {
        if self
            .members
            .get(owner)
            .is_none_or(|m| m.lifecycle != Lifecycle::Running)
        {
            return Err("owned action requires running member");
        }
        if let Action::SubmitConditional(r) = a
            && wire(r, owner).is_none()
        {
            return Err("local OCO group exceeds managed namespace");
        }
        Ok(())
    }
    fn stage_owned_action(&mut self, owner: usize, a: Action, out: &mut ActionBuffer) {
        self.enqueue(owner, a, out);
    }
    fn validate_transfer(&self, from: usize, to: usize, amount: i128) -> Result<(), &'static str> {
        let a = self.members.get(from).ok_or("unknown transfer sender")?;
        let b = self.members.get(to).ok_or("unknown transfer receiver")?;
        if from == to
            || amount <= 0
            || amount > MAX_CAPITAL
            || matches!(a.lifecycle, Lifecycle::Created | Lifecycle::Running)
            || matches!(b.lifecycle, Lifecycle::Created | Lifecycle::Running)
            || amount > a.account.available_cash()
            || amount > a.capital
            || b.capital > MAX_CAPITAL - amount
        {
            return Err(
                "transfer requires distinct initialized stopped/ready members and free capital",
            );
        }
        Ok(())
    }
    fn transfer(&mut self, from: usize, to: usize, amount: i128) {
        self.members[from].account.cash -= amount;
        self.members[from].capital -= amount;
        self.members[to].account.cash += amount;
        self.members[to].capital += amount;
    }
    fn on_quote_with_constraints(
        &mut self,
        v: StrategyView<'_>,
        c: Constraints,
        out: &mut ActionBuffer,
    ) {
        for i in 0..self.members.len() {
            if self.members[i].lifecycle == Lifecycle::Ready {
                let m = &mut self.members[i];
                let _ = m.child.on_quote(StrategyView {
                    quote: v.quote,
                    account: &m.account,
                    active_orders: 0,
                });
                continue;
            }
            if self.members[i].lifecycle != Lifecycle::Running {
                continue;
            }
            self.scratch.clear();
            let m = &mut self.members[i];
            let mut own = c;
            own.max_position = self.parameters.members[i].max_position;
            m.child.on_quote_with_constraints(
                StrategyView {
                    quote: v.quote,
                    account: &m.account,
                    active_orders: m.active,
                },
                own,
                &mut self.scratch,
            );
            match self.scratch.actions() {
                Ok(a) => {
                    let count = a.len();
                    for index in 0..count {
                        let x = self.scratch.actions().expect("checked actions")[index];
                        self.enqueue(i, x, out);
                    }
                }
                Err(_) => {
                    out.invalidate();
                }
            }
        }
    }
    fn begin_action(
        &mut self,
        action: Action,
        source: ActionSource,
        e: &Engine,
    ) -> Result<(), RiskReject> {
        self.current = None;
        let p = match source {
            ActionSource::Strategy => self.pending.pop_front().filter(|p| p.action == action),
            ActionSource::Conditional(id) => self
                .conditionals
                .get(&id.0)
                .filter(|c| action == Action::Submit(c.request.order))
                .map(|c| Pending {
                    owner: c.owner,
                    action,
                    reserve: 0,
                    activation: Some(id),
                }),
            ActionSource::Operator => None,
        }
        .ok_or(RiskReject::Ownership)?;
        self.current = Some(p);
        let m = &self.members[p.owner];
        let cancellation = matches!(action, Action::Cancel(_) | Action::CancelConditional(_));
        if !cancellation && m.lifecycle != Lifecycle::Running {
            return Err(RiskReject::StrategyState);
        }
        match action {
            Action::Cancel(id) if self.orders.get(&id.0).is_none_or(|o| o.owner != p.owner) => {
                return Err(RiskReject::Ownership);
            }
            Action::CancelConditional(id)
                if self
                    .conditionals
                    .get(&id.0)
                    .is_none_or(|o| o.owner != p.owner) =>
            {
                return Err(RiskReject::Ownership);
            }
            Action::Submit(r) => {
                let count = self.orders.len() + self.conditionals.len()
                    - usize::from(matches!(source, ActionSource::Conditional(_)));
                if count >= self.parameters.max_working {
                    return Err(RiskReject::StrategyCapacity);
                }
                let reserve = if r.side == Side::Buy {
                    i128::from(r.limit.units())
                        + fee(i128::from(r.limit.units()), e.config().fee_bps)
                } else {
                    0
                };
                if (r.side == Side::Buy
                    && (reserve * i128::from(r.quantity.units()) > m.account.available_cash()
                        || r.quantity.units()
                            > self.parameters.members[p.owner].max_position
                                - m.account.position()
                                - m.account.pending_buy()))
                    || (r.side == Side::Sell
                        && r.quantity.units() > m.account.position() - m.account.reserved_sell())
                {
                    return Err(RiskReject::StrategyBudget);
                }
                self.current.as_mut().expect("current owner").reserve = reserve;
            }
            Action::SubmitConditional(_)
                if self.orders.len() + self.conditionals.len() >= self.parameters.max_working =>
            {
                return Err(RiskReject::StrategyCapacity);
            }
            _ => (),
        }
        Ok(())
    }
    fn action_owner(&self) -> Option<usize> {
        self.current.map(|p| p.owner)
    }
    fn end_action(&mut self) {
        if let Some(p) = self.current.take()
            && let Some(id) = p.activation
            && let Some(o) = self.conditionals.remove(&id.0)
        {
            self.members[o.owner].waiting -= 1;
            self.retired.insert(id.0, o.owner);
        }
    }
    fn on_submit_result(&mut self, r: OrderRequest, result: Result<OrderId, RejectReason>) {
        let Some(p) = self.current else {
            return;
        };
        let m = &mut self.members[p.owner];
        if let Ok(id) = result {
            let qty = r.quantity.units();
            match r.side {
                Side::Buy => {
                    m.account.reserved_cash += p.reserve * i128::from(qty);
                    m.account.pending_buy += qty;
                }
                Side::Sell => m.account.reserved_sell += qty,
            };
            m.active += 1;
            self.orders.insert(
                id.0,
                OwnedOrder {
                    owner: p.owner,
                    request: r,
                    remaining: qty,
                    reserve_per_unit: p.reserve,
                },
            );
        }
        m.child.on_submit_result(r, result);
    }
    fn on_risk_rejected(&mut self, r: OrderRequest, reason: RiskReject) {
        if let Some(p) = self.current {
            self.members[p.owner].child.on_risk_rejected(r, reason);
        }
    }
    fn on_event(&mut self, event: Event) {
        let id = match event {
            Event::Fill(f) => f.order_id,
            Event::Accepted { order_id, .. }
            | Event::Cancelled { order_id, .. }
            | Event::Rejected { order_id, .. } => order_id,
        };
        if let Some(o) = self.orders.get_mut(&id.0) {
            let owner = o.owner;
            let m = &mut self.members[owner];
            let mut terminal = false;
            match event {
                Event::Fill(f) => {
                    m.account.apply(f, o.reserve_per_unit);
                    o.remaining -= f.quantity;
                    terminal = o.remaining == 0;
                }
                Event::Cancelled { .. } => {
                    m.account
                        .release(o.request.side, o.remaining, o.reserve_per_unit);
                    terminal = true;
                }
                _ => (),
            }
            m.child.on_event(event);
            if terminal {
                m.active -= 1;
                self.orders.remove(&id.0);
            }
        } else if let Some(p) = self.current {
            self.members[p.owner].child.on_event(event);
        }
    }
    fn on_conditional_event(&mut self, event: ConditionalEvent) {
        let (owner, local) = match event {
            ConditionalEvent::Accepted {
                conditional_id,
                request,
            } => {
                let Some(p) = self.current else {
                    return;
                };
                self.conditionals.insert(
                    conditional_id.0,
                    OwnedConditional {
                        owner: p.owner,
                        request,
                    },
                );
                self.members[p.owner].waiting += 1;
                (
                    p.owner,
                    ConditionalEvent::Accepted {
                        conditional_id,
                        request: unwire(request),
                    },
                )
            }
            ConditionalEvent::Rejected { request, reason } => {
                let Some(p) = self.current else {
                    return;
                };
                (
                    p.owner,
                    ConditionalEvent::Rejected {
                        request: unwire(request),
                        reason,
                    },
                )
            }
            ConditionalEvent::Triggered { conditional_id, .. }
            | ConditionalEvent::ActivationRejected { conditional_id, .. }
            | ConditionalEvent::Cancelled { conditional_id, .. } => {
                let owner = if let Some(o) = self.conditionals.remove(&conditional_id.0) {
                    self.members[o.owner].waiting -= 1;
                    o.owner
                } else if let Some(owner) = self.retired.remove(&conditional_id.0) {
                    owner
                } else {
                    return;
                };
                (owner, event)
            }
            ConditionalEvent::CancelMissing { .. } => {
                let Some(p) = self.current else {
                    return;
                };
                (p.owner, event)
            }
        };
        self.members[owner].child.on_conditional_event(local);
    }
    fn on_market_halt(&mut self) {
        let pending: Vec<_> = self.pending.drain(..).collect();
        for p in pending {
            match p.action {
                Action::Submit(r) => self.members[p.owner]
                    .child
                    .on_risk_rejected(r, RiskReject::StrategyState),
                Action::SubmitConditional(r) => {
                    self.members[p.owner]
                        .child
                        .on_conditional_event(ConditionalEvent::Rejected {
                            request: unwire(r),
                            reason: ConditionalReject::Risk(RiskReject::StrategyState),
                        })
                }
                _ => (),
            }
        }
        self.current = None;
        for m in &mut self.members {
            if m.lifecycle != Lifecycle::Created {
                m.lifecycle = Lifecycle::Stopped;
            }
        }
    }
    fn validate_execution(&self, e: &Engine) -> Result<(), &'static str> {
        self.validate_state()?;
        let mut sum = Account::new(0);
        let mut capital = 0i128;
        for (i, m) in self.members.iter().enumerate() {
            capital += m.capital;
            sum.cash += m.account.cash;
            sum.position += m.account.position;
            sum.reserved_cash += m.account.reserved_cash;
            sum.reserved_sell += m.account.reserved_sell;
            sum.pending_buy += m.account.pending_buy;
            sum.fees_paid += m.account.fees_paid;
            sum.net_buy_notional += m.account.net_buy_notional;
            for id in m.child.owned_order_ids() {
                if self.orders.get(&id.0).is_none_or(|o| o.owner != i) {
                    return Err("managed child references another owner order");
                }
            }
            m.child.validate_execution(e)?;
        }
        if capital != e.config().initial_cash
            || &sum != e.account()
            || self.orders.len() != e.active_count()
        {
            return Err("managed total account/ownership reconciliation failed");
        }
        for (&id, o) in &self.orders {
            let core = e.order(OrderId(id)).ok_or("owned order missing")?;
            if !core.status.is_active()
                || core.request != o.request
                || core.remaining != o.remaining
                || core.reserved_per_unit != o.reserve_per_unit
            {
                return Err("managed order/core mismatch");
            }
        }
        Ok(())
    }
    fn validate_conditionals(&self, b: &ConditionalBook) -> Result<(), &'static str> {
        if self.conditionals.len() != b.len() {
            return Err("managed conditional coverage mismatch");
        }
        for (&id, o) in &self.conditionals {
            if b.get(ConditionalId(id))
                .is_none_or(|w| w.request != o.request)
            {
                return Err("managed conditional identity mismatch");
            }
        }
        for (i, m) in self.members.iter().enumerate() {
            let view = b.filtered_validation(|mut w| {
                if self.conditionals.get(&w.id.0)?.owner != i {
                    return None;
                }
                w.request = unwire(w.request);
                Some(w)
            });
            m.child.validate_conditionals(&view)?;
        }
        Ok(())
    }
    fn visit_owned_decisions(&self, visitor: &mut dyn FnMut(usize, &crate::target::Decision)) {
        if self.members.len() > 1 {
            for (owner, m) in self.members.iter().enumerate() {
                if m.lifecycle == Lifecycle::Running
                    && let Some(d) = m.child.decision()
                {
                    visitor(owner, d);
                }
            }
        }
    }
    fn decision(&self) -> Option<&crate::target::Decision> {
        if self.members.len() == 1 {
            self.members[0].child.decision()
        } else {
            None
        }
    }
    fn custom_checkpoint(&self) -> Option<CustomCheckpoint> {
        Some(CustomCheckpoint {
            name: NAME.into(),
            version: 1,
            state: serde_json::to_value(self.state()?).ok()?,
        })
    }
    fn diagnostics(&self) -> Option<serde_json::Value> {
        Some(
            serde_json::json!({"strategy":NAME,"members":self.members.iter().enumerate().map(|(i,m)|serde_json::json!({"owner":i,"name":self.parameters.members[i].name,"lifecycle":m.lifecycle,"capital_minor":m.capital.to_string(),"cash_minor":m.account.cash().to_string(),"reserved_cash_minor":m.account.reserved_cash().to_string(),"position_units":m.account.position(),"pending_buy_units":m.account.pending_buy(),"reserved_sell_units":m.account.reserved_sell(),"fees_minor":m.account.fees_paid().to_string(),"active_orders":m.active,"waiting_conditions":m.waiting,"decision":m.child.decision(),"diagnostics":m.child.diagnostics()})).collect::<Vec<_>>(),"order_owners":self.orders.iter().map(|(id,o)|(*id,o.owner)).collect::<Vec<_>>(),"conditional_owners":self.conditionals.iter().map(|(id,o)|(*id,o.owner)).collect::<Vec<_>>(),"scope":"single-market long-only paper; one shared matching core, fixed member order, explicit stopped-member capital transfers; no cross-market/shared-currency portfolio, sandbox, external route or alpha"}),
        )
    }
}
pub fn factory(
    parameters: &serde_json::Value,
    saved: Option<&serde_json::Value>,
) -> Result<Box<dyn Strategy>, PaperError> {
    let p: Parameters = serde_json::from_value(parameters.clone())?;
    p.validate()?;
    let state: Option<State> = saved
        .map(|s| serde_json::from_value(s.clone()))
        .transpose()?;
    if state
        .as_ref()
        .is_some_and(|s| s.members.len() != p.members.len())
    {
        return Err("managed member count changed".into());
    }
    let (members, orders, conditionals) = if let Some(s) = state {
        let mut members = Vec::new();
        for (cfg, m) in p.members.iter().zip(s.members) {
            members.push(Member {
                lifecycle: m.lifecycle,
                account: m.account,
                capital: m.capital,
                active: m.active,
                waiting: m.waiting,
                child: child(&cfg.strategy, Some(m.child))?,
            });
        }
        (members, s.orders, s.conditionals)
    } else {
        let mut members = Vec::new();
        for cfg in &p.members {
            members.push(Member {
                lifecycle: Lifecycle::Created,
                account: Account::new(cfg.initial_cash),
                capital: cfg.initial_cash,
                active: 0,
                waiting: 0,
                child: child(&cfg.strategy, None)?,
            });
        }
        (members, BTreeMap::new(), BTreeMap::new())
    };
    let s = ManagedStrategy {
        parameters: p,
        members,
        orders,
        conditionals,
        pending: VecDeque::with_capacity(64),
        current: None,
        retired: BTreeMap::new(),
        scratch: ActionBuffer::new(64)?,
    };
    s.validate_state()?;
    Ok(Box::new(s))
}
