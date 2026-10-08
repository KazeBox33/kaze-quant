//! 可重放的命令边界：直接整数索引路由，多资产采用独立预算；没有隐藏跨账户融资。
use crate::conditional::{
    ActivationReject, CancelReason, ConditionalBook, ConditionalEvent, ConditionalId,
    ConditionalReject, ConditionalRequest, ConditionalSnapshot, TriggerPolicy,
};
use crate::config::StrategyConfig;
use crate::config::{BuiltinStrategy, PaperConfig};
use crate::engine::{Engine, EngineSnapshot, MAX_LIFETIME_ORDERS, Metrics};
use crate::registry::{CustomCheckpoint, StrategyRegistry};
use crate::strategy::{ActionBuffer, ActionSource, Strategy, StrategyView};
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;

#[derive(Debug)]
pub struct PaperError(pub String);
impl fmt::Display for PaperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}
impl std::error::Error for PaperError {}
impl From<&str> for PaperError {
    fn from(s: &str) -> Self {
        Self(s.into())
    }
}
impl From<std::io::Error> for PaperError {
    fn from(e: std::io::Error) -> Self {
        Self(e.to_string())
    }
}
impl From<serde_json::Error> for PaperError {
    fn from(e: serde_json::Error) -> Self {
        Self(e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub seq: u64,
    pub command: Command,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Quote {
        market: usize,
        quote: Quote,
    },
    Submit {
        market: usize,
        request: OrderRequest,
    },
    Cancel {
        market: usize,
        order_id: OrderId,
    },
    SubmitConditional {
        market: usize,
        request: ConditionalRequest,
    },
    CancelConditional {
        market: usize,
        conditional_id: ConditionalId,
    },
    StrategyControl {
        market: usize,
        owner: usize,
        operation: crate::managed::Control,
    },
    StrategyAction {
        market: usize,
        owner: usize,
        action: Action,
    },
    StrategyTransfer {
        market: usize,
        from: usize,
        to: usize,
        #[serde(with = "crate::config::money")]
        amount: i128,
    },
    /// 适配器的 watchdog 用同一时间轴推进时钟，触发行情超时保护。
    Advance {
        timestamp_ns: u64,
    },
    Halt {},
    Finish {},
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HaltReason {
    Operator,
    StaleFeed,
    Drawdown,
    StrategyCapacity,
    Finished,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskReject {
    Halted,
    NoMarket,
    StaleFeed,
    Notional,
    PriceCollar,
    Grid,
    HistoryCapacity,
    Ownership,
    StrategyState,
    StrategyBudget,
    StrategyCapacity,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notice {
    OwnedDecision {
        market: usize,
        owner: usize,
        decision: crate::target::Decision,
    },
    OwnedAction {
        market: usize,
        owner: usize,
        action: Action,
        gate_rejection: Option<RiskReject>,
    },
    StrategyControl {
        market: usize,
        owner: usize,
        operation: crate::managed::Control,
    },
    StrategyTransfer {
        market: usize,
        from: usize,
        to: usize,
        #[serde(with = "crate::config::money")]
        amount: i128,
    },
    Engine {
        market: usize,
        event: Event,
    },
    RiskRejected {
        market: usize,
        reason: RiskReject,
    },
    Halted {
        market: usize,
        reason: HaltReason,
    },
    CancelMissing {
        market: usize,
        order_id: OrderId,
    },
    Conditional {
        market: usize,
        event: ConditionalEvent,
    },
    StrategyDecision {
        market: usize,
        decision: crate::target::Decision,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub seq: u64,
    pub duplicate: bool,
    pub notices: Vec<Notice>,
}
struct Market {
    engine: Engine,
    conditionals: ConditionalBook,
    strategy: Box<dyn Strategy>,
    halted: Option<HaltReason>,
    risk_rejections: u64,
}

pub struct PaperRuntime {
    config: PaperConfig,
    markets: Vec<Market>,
    clock_ns: u64,
    closed: bool,
    processed: u64,
    actions: ActionBuffer,
    retention: Option<usize>,
    registry: Arc<StrategyRegistry>,
}
impl PaperRuntime {
    pub fn new(config: PaperConfig) -> Result<Self, PaperError> {
        Self::new_with_registry(config, Arc::new(StrategyRegistry::standard()))
    }
    pub fn new_with_registry(
        config: PaperConfig,
        registry: Arc<StrategyRegistry>,
    ) -> Result<Self, PaperError> {
        config.validate()?;
        let strategies = config
            .markets
            .iter()
            .map(|m| -> Result<Box<dyn Strategy>, PaperError> {
                match &m.strategy {
                    StrategyConfig::Registered {
                        name,
                        version,
                        parameters,
                    } => registry.build(name, *version, parameters, None),
                    config => Ok(Box::new(config.build()?)),
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut runtime = Self::with_strategies(config, strategies)?;
        runtime.registry = registry;
        Ok(runtime)
    }
    pub fn registry(&self) -> Arc<StrategyRegistry> {
        self.registry.clone()
    }
    /// 自定义策略的内存运行入口。DurableSession 只启用版本化内置策略；
    /// 若要持久恢复自定义策略，将其显式注册到 StrategyConfig 并升级执行版本。
    pub fn with_strategies(
        config: PaperConfig,
        strategies: Vec<Box<dyn Strategy>>,
    ) -> Result<Self, PaperError> {
        config.validate()?;
        if strategies.len() != config.markets.len() {
            return Err("strategy count must match markets".into());
        }
        let mut markets = Vec::with_capacity(config.markets.len());
        for (m, strategy) in config.markets.iter().zip(strategies) {
            strategy.validate_market_config(m)?;
            markets.push(Market {
                engine: Engine::new(m.engine.clone())?,
                conditionals: ConditionalBook::new(m.engine.max_active_orders.min(4096))?,
                strategy,
                halted: None,
                risk_rejections: 0,
            });
        }
        Ok(Self {
            config,
            markets,
            clock_ns: 0,
            closed: false,
            processed: 0,
            actions: ActionBuffer::new(64)?,
            retention: None,
            registry: Arc::new(StrategyRegistry::standard()),
        })
    }
    pub fn config(&self) -> &PaperConfig {
        &self.config
    }
    pub fn engine(&self, market: usize) -> Option<&Engine> {
        self.markets.get(market).map(|m| &m.engine)
    }
    pub fn conditionals(&self, market: usize) -> Option<&ConditionalBook> {
        self.markets.get(market).map(|m| &m.conditionals)
    }
    pub fn halted(&self, market: usize) -> Option<HaltReason> {
        self.markets.get(market).and_then(|m| m.halted)
    }
    pub fn clock_ns(&self) -> u64 {
        self.clock_ns
    }
    pub fn closed(&self) -> bool {
        self.closed
    }
    pub fn processed(&self) -> u64 {
        self.processed
    }

    /// 与 apply 分离，以便持久化层先校验、写日志，再修改内存。
    pub fn validate(&self, command: &Command) -> Result<(), PaperError> {
        if self.closed {
            return Err("session already finished".into());
        }
        let market = match command {
            Command::Quote { market, .. }
            | Command::Submit { market, .. }
            | Command::Cancel { market, .. }
            | Command::SubmitConditional { market, .. }
            | Command::CancelConditional { market, .. }
            | Command::StrategyControl { market, .. }
            | Command::StrategyAction { market, .. }
            | Command::StrategyTransfer { market, .. } => Some(*market),
            _ => None,
        };
        if market.is_some_and(|i| i >= self.markets.len()) {
            return Err("unknown market index".into());
        }
        match command {
            Command::StrategyControl {
                market,
                owner,
                operation,
            } => {
                if self.markets[*market].halted.is_some()
                    && *operation == crate::managed::Control::Start
                {
                    return Err("cannot start a halted market".into());
                }
                self.markets[*market]
                    .strategy
                    .validate_control(*owner, *operation)?;
            }
            Command::StrategyAction {
                market,
                owner,
                action,
            } => self.markets[*market]
                .strategy
                .validate_owned_action(*owner, *action)?,
            Command::StrategyTransfer {
                market,
                from,
                to,
                amount,
            } => self.markets[*market]
                .strategy
                .validate_transfer(*from, *to, *amount)?,
            Command::Quote { market, quote: q } => {
                q.validate()?;
                if q.timestamp_ns < self.clock_ns {
                    return Err(
                        "quote is older than session clock; merge feeds in timestamp order".into(),
                    );
                }
                if let Some(prev) = self.markets[*market].engine.last_quote()
                    && q.sequence <= prev.sequence
                {
                    return Err("quote sequences must strictly increase".into());
                }
                let c = &self.config.markets[*market];
                if !q.bid.units().is_multiple_of(c.price_tick)
                    || !q.ask.units().is_multiple_of(c.price_tick)
                    || !q.bid_quantity.is_multiple_of(c.quantity_step)
                    || !q.ask_quantity.is_multiple_of(c.quantity_step)
                {
                    return Err("quote outside instrument grid".into());
                }
            }
            Command::Advance { timestamp_ns } if *timestamp_ns < self.clock_ns => {
                return Err("clock must not decrease".into());
            }
            _ => (),
        }
        Ok(())
    }
    pub fn process(&mut self, envelope: &Envelope) -> Result<Receipt, PaperError> {
        if envelope.seq != self.processed + 1 {
            return Err("command sequence must be contiguous".into());
        }
        if self.processed as usize >= self.config.max_commands {
            return Err("command capacity reached".into());
        }
        self.validate(&envelope.command)?;
        let receipt = self.apply(envelope);
        Ok(receipt)
    }
    pub(crate) fn apply(&mut self, envelope: &Envelope) -> Receipt {
        let mut notices = Vec::new();
        match envelope.command {
            Command::Quote { market, quote } => {
                self.clock_ns = quote.timestamp_ns;
                // 所有资产都执行 watchdog；先撤销超时订单，禁止新鲜快照填掉过期委托。
                self.expire_feeds(&mut notices);
                self.expire_conditionals(&mut notices);
                let mut events = Vec::new();
                self.markets[market]
                    .engine
                    .on_quote(quote, &mut events)
                    .expect("prevalidated quote");
                self.dispatch_events(market, events, &mut notices);
                if self.markets[market].engine.metrics().max_drawdown
                    >= self.config.markets[market].risk.max_drawdown
                {
                    self.halt_market(market, HaltReason::Drawdown, &mut notices);
                }
                // 已暂停仍接收报价用于估值；不再生成订单，直到另开经过审查的新会话。
                if self.markets[market].halted.is_none() {
                    self.trigger_conditionals(market, quote, &mut notices);
                    let m = &mut self.markets[market];
                    self.actions.clear();
                    m.strategy.on_quote_with_constraints(
                        StrategyView {
                            quote,
                            account: m.engine.account(),
                            active_orders: m.engine.active_count(),
                        },
                        crate::target::Constraints::from_market(&self.config.markets[market]),
                        &mut self.actions,
                    );
                    if self.actions.actions().is_err() {
                        self.halt_market(market, HaltReason::StrategyCapacity, &mut notices);
                    } else {
                        for slot in 0..self.actions.actions().expect("validated batch").len() {
                            let action = self.actions.actions().expect("validated batch")[slot];
                            self.action(market, action, ActionSource::Strategy, &mut notices);
                        }
                    }
                    self.markets[market]
                        .strategy
                        .visit_owned_decisions(&mut |owner, decision| {
                            notices.push(Notice::OwnedDecision {
                                market,
                                owner,
                                decision: decision.clone(),
                            })
                        });
                    if let Some(d) = self.markets[market].strategy.decision() {
                        notices.push(Notice::StrategyDecision {
                            market,
                            decision: d.clone(),
                        });
                    }
                }
            }
            Command::Submit { market, request } => self.action(
                market,
                Action::Submit(request),
                ActionSource::Operator,
                &mut notices,
            ),
            Command::Cancel { market, order_id } => self.action(
                market,
                Action::Cancel(order_id),
                ActionSource::Operator,
                &mut notices,
            ),
            Command::SubmitConditional { market, request } => self.action(
                market,
                Action::SubmitConditional(request),
                ActionSource::Operator,
                &mut notices,
            ),
            Command::CancelConditional {
                market,
                conditional_id,
            } => self.action(
                market,
                Action::CancelConditional(conditional_id),
                ActionSource::Operator,
                &mut notices,
            ),
            Command::StrategyControl {
                market,
                owner,
                operation,
            } => {
                self.actions.clear();
                self.markets[market]
                    .strategy
                    .control(owner, operation, &mut self.actions);
                self.run_managed_actions(market, &mut notices);
                notices.push(Notice::StrategyControl {
                    market,
                    owner,
                    operation,
                });
            }
            Command::StrategyAction {
                market,
                owner,
                action,
            } => {
                self.actions.clear();
                self.markets[market]
                    .strategy
                    .stage_owned_action(owner, action, &mut self.actions);
                self.run_managed_actions(market, &mut notices);
            }
            Command::StrategyTransfer {
                market,
                from,
                to,
                amount,
            } => {
                self.markets[market].strategy.transfer(from, to, amount);
                notices.push(Notice::StrategyTransfer {
                    market,
                    from,
                    to,
                    amount,
                });
            }
            Command::Advance { timestamp_ns } => {
                self.clock_ns = timestamp_ns;
                self.expire_feeds(&mut notices);
                self.expire_conditionals(&mut notices);
            }
            Command::Halt {} => {
                for i in 0..self.markets.len() {
                    self.halt_market(i, HaltReason::Operator, &mut notices);
                }
            }
            Command::Finish {} => {
                for i in 0..self.markets.len() {
                    self.halt_market(i, HaltReason::Finished, &mut notices);
                }
                self.closed = true;
            }
        }
        self.processed = envelope.seq;
        Receipt {
            seq: envelope.seq,
            duplicate: false,
            notices,
        }
    }
    fn expire_feeds(&mut self, notices: &mut Vec<Notice>) {
        for i in 0..self.markets.len() {
            if self.markets[i].engine.last_quote().is_some_and(|q| {
                self.clock_ns - q.timestamp_ns > self.config.markets[i].risk.max_quote_age_ns
            }) {
                self.halt_market(i, HaltReason::StaleFeed, notices);
            }
        }
    }
    fn halt_market(&mut self, i: usize, reason: HaltReason, notices: &mut Vec<Notice>) {
        if self.markets[i].halted.is_some() {
            return;
        }
        self.markets[i].halted = Some(reason);
        let mut events = Vec::new();
        self.markets[i].engine.finish(&mut events);
        self.dispatch_events(i, events, notices);
        let events = self.markets[i]
            .conditionals
            .cancel_all(CancelReason::Halted);
        self.dispatch_conditionals(i, events, notices);
        self.markets[i].strategy.on_market_halt();
        notices.push(Notice::Halted { market: i, reason });
    }
    fn dispatch_events(&mut self, i: usize, events: Vec<Event>, notices: &mut Vec<Notice>) {
        for event in events {
            self.markets[i].strategy.on_event(event);
            notices.push(Notice::Engine { market: i, event });
        }
    }
    fn risk(&self, i: usize, r: OrderRequest) -> Option<RiskReject> {
        let m = &self.markets[i];
        let c = &self.config.markets[i];
        if m.halted.is_some() {
            return Some(RiskReject::Halted);
        }
        let Some(q) = m.engine.last_quote() else {
            return Some(RiskReject::NoMarket);
        };
        if self.clock_ns - q.timestamp_ns > c.risk.max_quote_age_ns {
            return Some(RiskReject::StaleFeed);
        }
        if !r.limit.units().is_multiple_of(c.price_tick)
            || !r.quantity.units().is_multiple_of(c.quantity_step)
        {
            return Some(RiskReject::Grid);
        }
        if i128::from(r.limit.units()) * i128::from(r.quantity.units()) > c.risk.max_order_notional
        {
            return Some(RiskReject::Notional);
        }
        let reference = match r.side {
            Side::Buy => q.ask.units(),
            Side::Sell => q.bid.units(),
        };
        if u128::from(r.limit.units().abs_diff(reference)) * 10_000
            > u128::from(reference) * u128::from(c.risk.price_collar_bps)
        {
            return Some(RiskReject::PriceCollar);
        }
        if m.engine.orders().len() >= c.engine.max_orders {
            return Some(RiskReject::HistoryCapacity);
        }
        None
    }
    fn submit_order(
        &mut self,
        i: usize,
        request: OrderRequest,
        notices: &mut Vec<Notice>,
    ) -> Result<OrderId, ActivationReject> {
        let mut events = Vec::new();
        let result = {
            if let Some(retain) = self.retention
                && self.markets[i].engine.orders().len() >= self.config.markets[i].engine.max_orders
            {
                let budget = retain.min(
                    self.config.markets[i]
                        .engine
                        .max_orders
                        .saturating_sub(self.markets[i].engine.active_count() + 1),
                );
                self.markets[i].engine.compact_terminal_orders(budget);
            }
            if let Some(reason) = self.risk(i, request) {
                self.markets[i].strategy.on_risk_rejected(request, reason);
                self.markets[i].risk_rejections += 1;
                notices.push(Notice::RiskRejected { market: i, reason });
                Err(ActivationReject::Risk(reason))
            } else {
                let outcome = self.markets[i].engine.submit(request, &mut events);
                self.markets[i].strategy.on_submit_result(request, outcome);
                outcome.map_err(ActivationReject::Engine)
            }
        };
        self.dispatch_events(i, events, notices);
        result
    }
    fn dispatch_conditionals(
        &mut self,
        i: usize,
        events: Vec<ConditionalEvent>,
        notices: &mut Vec<Notice>,
    ) {
        for event in events {
            self.markets[i].strategy.on_conditional_event(event);
            notices.push(Notice::Conditional { market: i, event });
        }
    }
    fn expire_conditionals(&mut self, notices: &mut Vec<Notice>) {
        for i in 0..self.markets.len() {
            if !self.markets[i].conditionals.is_empty() {
                let events = self.markets[i]
                    .conditionals
                    .expire(self.clock_ns)
                    .expect("prevalidated session clock");
                self.dispatch_conditionals(i, events, notices);
            }
        }
    }
    fn trigger_conditionals(&mut self, i: usize, q: Quote, notices: &mut Vec<Notice>) {
        if self.markets[i].conditionals.is_empty() {
            return;
        }
        // 暂时移出拥有的投影，激活闭包才可借用原平台风控/账户路径；无克隆/额外账户。
        let cap = self.config.markets[i].engine.max_active_orders.min(4096);
        let mut book = std::mem::replace(
            &mut self.markets[i].conditionals,
            ConditionalBook::new(cap).expect("validated capacity"),
        );
        let result = book.on_quote_owned(q, TriggerPolicy::Adaptive, |id, request| {
            let action = Action::Submit(request);
            let result = if let Some(reason) =
                self.gate_action(i, action, ActionSource::Conditional(id), notices)
            {
                Err(ActivationReject::Risk(reason))
            } else {
                self.submit_order(i, request, notices)
            };
            self.markets[i].strategy.end_action();
            result
        });
        self.markets[i].conditionals = book;
        self.dispatch_conditionals(i, result.expect("prevalidated conditional quote"), notices);
    }
    fn submit_conditional(&mut self, i: usize, r: ConditionalRequest, notices: &mut Vec<Notice>) {
        let m = &self.markets[i];
        let c = &self.config.markets[i];
        // 潜伏意图允许未来价格/尚未持有的退出量；资金、持仓、价格带、订单容量在激活时重查。
        let risk = if m.halted.is_some() {
            Some(RiskReject::Halted)
        } else if m.engine.last_quote().is_none() {
            Some(RiskReject::NoMarket)
        } else if self.clock_ns - m.engine.last_quote().expect("checked quote").timestamp_ns
            > c.risk.max_quote_age_ns
        {
            Some(RiskReject::StaleFeed)
        } else if !r.trigger.units().is_multiple_of(c.price_tick)
            || !r.order.limit.units().is_multiple_of(c.price_tick)
            || !r.order.quantity.units().is_multiple_of(c.quantity_step)
        {
            Some(RiskReject::Grid)
        } else if i128::from(r.order.limit.units()) * i128::from(r.order.quantity.units())
            > c.risk.max_order_notional
        {
            Some(RiskReject::Notional)
        } else {
            None
        };
        let result = if let Some(reason) = risk {
            self.markets[i].risk_rejections += 1;
            self.markets[i].conditionals.record_rejection();
            Err(ConditionalReject::Risk(reason))
        } else {
            let q = self.markets[i].engine.last_quote().expect("checked quote");
            self.markets[i].conditionals.submit(r, q, self.clock_ns)
        };
        let event = match result {
            Ok(id) => ConditionalEvent::Accepted {
                conditional_id: id,
                request: r,
            },
            Err(reason) => ConditionalEvent::Rejected { request: r, reason },
        };
        self.dispatch_conditionals(i, vec![event], notices);
    }
    fn run_managed_actions(&mut self, i: usize, notices: &mut Vec<Notice>) {
        if self.actions.actions().is_err() {
            self.halt_market(i, HaltReason::StrategyCapacity, notices);
            return;
        }
        for index in 0..self.actions.actions().expect("bounded actions").len() {
            let a = self.actions.actions().expect("bounded actions")[index];
            self.action(i, a, ActionSource::Strategy, notices);
        }
    }
    fn gate_action(
        &mut self,
        i: usize,
        action: Action,
        source: ActionSource,
        notices: &mut Vec<Notice>,
    ) -> Option<RiskReject> {
        let m = &mut self.markets[i];
        let rejection = m.strategy.begin_action(action, source, &m.engine).err();
        if let Some(owner) = m.strategy.action_owner() {
            notices.push(Notice::OwnedAction {
                market: i,
                owner,
                action,
                gate_rejection: rejection,
            });
        }
        if let Some(reason) = rejection {
            m.risk_rejections += 1;
            notices.push(Notice::RiskRejected { market: i, reason });
            match action {
                Action::Submit(r) => m.strategy.on_risk_rejected(r, reason),
                Action::SubmitConditional(r) => {
                    m.strategy.on_conditional_event(ConditionalEvent::Rejected {
                        request: r,
                        reason: ConditionalReject::Risk(reason),
                    })
                }
                _ => (),
            }
        }
        rejection
    }
    fn action(
        &mut self,
        i: usize,
        action: Action,
        source: ActionSource,
        notices: &mut Vec<Notice>,
    ) {
        if self.gate_action(i, action, source, notices).is_some() {
            self.markets[i].strategy.end_action();
            return;
        }
        let mut events = Vec::new();
        match action {
            Action::Submit(request) => {
                let _ = self.submit_order(i, request, notices);
            }
            Action::SubmitConditional(request) => self.submit_conditional(i, request, notices),
            Action::CancelConditional(id) => {
                let event = self.markets[i]
                    .conditionals
                    .cancel(id, CancelReason::Operator);
                self.dispatch_conditionals(i, vec![event], notices);
            }
            Action::Cancel(order_id) => {
                if !self.markets[i].engine.cancel(order_id, &mut events) {
                    notices.push(Notice::CancelMissing {
                        market: i,
                        order_id,
                    });
                }
            }
            Action::None => (),
        }
        self.dispatch_events(i, events, notices);
        self.markets[i].strategy.end_action();
    }
    pub fn configure_retention(&mut self, terminal_budget: usize) -> Result<(), PaperError> {
        if terminal_budget > 4096 {
            return Err("terminal retention exceeds 4096 per market".into());
        }
        self.retention = Some(terminal_budget);
        Ok(())
    }
    pub fn compact_checkpoint(&mut self) {
        if let Some(retain) = self.retention {
            for m in &mut self.markets {
                m.engine.compact_terminal_orders(retain);
            }
        }
    }
    pub fn snapshot(&self) -> Result<PaperSnapshot, PaperError> {
        let markets = self
            .markets
            .iter()
            .enumerate()
            .map(|(i, m)| {
                m.strategy.validate_execution(&m.engine)?;
                m.strategy.validate_conditionals(&m.conditionals)?;
                let strategy = match &self.config.markets[i].strategy {
                    StrategyConfig::Registered {
                        name,
                        version,
                        parameters,
                    } => {
                        let state = m
                            .strategy
                            .custom_checkpoint()
                            .ok_or("registered strategy lacks deterministic checkpoint")?;
                        if state.name != *name
                            || state.version != *version
                            || serde_json::to_vec(&state.state)?.len() > 65536
                        {
                            return Err(
                                "registered strategy checkpoint identity/bounds mismatch".into()
                            );
                        }
                        // 写盘前用工厂验证新状态，并要求恢复后检查点完全相同。
                        let restored =
                            self.registry
                                .build(name, *version, parameters, Some(&state.state))?;
                        if restored.custom_checkpoint().as_ref() != Some(&state) {
                            return Err(
                                "custom strategy checkpoint fails canonical restore validation"
                                    .into(),
                            );
                        }
                        StrategyCheckpoint::Registered { registered: state }
                    }
                    _ => StrategyCheckpoint::Builtin(
                        m.strategy
                            .checkpoint()
                            .ok_or("strategy does not provide a deterministic checkpoint")?,
                    ),
                };
                Ok(MarketSnapshot {
                    engine: m.engine.snapshot(),
                    conditionals: m.conditionals.snapshot(),
                    strategy,
                    halted: m.halted,
                    risk_rejections: m.risk_rejections,
                })
            })
            .collect::<Result<Vec<_>, PaperError>>()?;
        Ok(PaperSnapshot {
            schema_version: 1,
            markets,
            clock_ns: self.clock_ns,
            closed: self.closed,
            processed: self.processed,
            retention: self.retention,
        })
    }
    pub fn restore(config: PaperConfig, state: PaperSnapshot) -> Result<Self, PaperError> {
        Self::restore_with_registry(config, state, Arc::new(StrategyRegistry::standard()))
    }
    pub fn restore_with_registry(
        config: PaperConfig,
        state: PaperSnapshot,
        registry: Arc<StrategyRegistry>,
    ) -> Result<Self, PaperError> {
        config.validate()?;
        if state.schema_version != 1
            || state.processed > MAX_LIFETIME_ORDERS
            || state.markets.len() != config.markets.len()
            || state.retention.is_some_and(|n| n > 4096)
        {
            return Err("invalid runtime snapshot bounds".into());
        }
        let mut markets = Vec::with_capacity(config.markets.len());
        for (c, m) in config.markets.iter().zip(state.markets) {
            let strategy: Box<dyn Strategy> = match (m.strategy, &c.strategy) {
                (StrategyCheckpoint::Builtin(s), config) => {
                    s.validate_state(config)?;
                    Box::new(s)
                }
                (
                    StrategyCheckpoint::Registered { registered: s },
                    StrategyConfig::Registered {
                        name,
                        version,
                        parameters,
                    },
                ) if s.name == *name && s.version == *version => {
                    registry.build(name, *version, parameters, Some(&s.state))?
                }
                _ => {
                    return Err(
                        "strategy checkpoint differs from configured registry identity".into(),
                    );
                }
            };
            let engine = Engine::restore(c.engine.clone(), m.engine)?;
            let conditionals = match m.conditionals {
                Some(s) => ConditionalBook::restore(
                    c.engine.max_active_orders.min(4096),
                    s,
                    engine.last_quote(),
                    state.clock_ns,
                )?,
                None => ConditionalBook::new(c.engine.max_active_orders.min(4096))?,
            };
            strategy.validate_execution(&engine)?;
            if engine
                .last_quote()
                .is_some_and(|q| q.timestamp_ns > state.clock_ns)
                || engine.metrics().quotes > state.processed
                || m.risk_rejections > state.processed.saturating_mul(4160)
            {
                return Err("snapshot clock or counter mismatch".into());
            }
            if state.closed && m.halted.is_none()
                || !state.closed && m.halted == Some(HaltReason::Finished)
            {
                return Err("snapshot lifecycle mismatch".into());
            }
            markets.push(Market {
                engine,
                conditionals,
                strategy,
                halted: m.halted,
                risk_rejections: m.risk_rejections,
            });
        }
        let runtime = Self {
            config,
            markets,
            clock_ns: state.clock_ns,
            closed: state.closed,
            processed: state.processed,
            actions: ActionBuffer::new(64)?,
            retention: state.retention,
            registry,
        };
        runtime.check_invariants()?;
        Ok(runtime)
    }
    pub fn check_invariants(&self) -> Result<(), PaperError> {
        for (i, m) in self.markets.iter().enumerate() {
            m.engine.check_invariants()?;
            m.conditionals.check_invariants()?;
            let c = &self.config.markets[i];
            m.strategy.validate_market_config(c)?;
            for w in m.conditionals.pending() {
                if !w.request.trigger.units().is_multiple_of(c.price_tick)
                    || !w.request.order.limit.units().is_multiple_of(c.price_tick)
                    || !w
                        .request
                        .order
                        .quantity
                        .units()
                        .is_multiple_of(c.quantity_step)
                    || i128::from(w.request.order.limit.units())
                        * i128::from(w.request.order.quantity.units())
                        > c.risk.max_order_notional
                {
                    return Err("conditional snapshot outside market constraints".into());
                }
            }
            m.strategy.validate_execution(&m.engine)?;
            m.strategy.validate_conditionals(&m.conditionals)?;
            if m.halted.is_some() && (m.engine.active_count() != 0 || !m.conditionals.is_empty()) {
                return Err("halted market has active orders".into());
            }
        }
        Ok(())
    }
    pub fn report(&self) -> PaperReport {
        PaperReport {
            schema_version: 1,
            processed: self.processed,
            clock_ns: self.clock_ns,
            closed: self.closed,
            markets: self
                .markets
                .iter()
                .enumerate()
                .map(|(i, m)| MarketReport {
                    market: i,
                    symbol: self.config.markets[i].symbol.clone(),
                    units: self.config.markets[i].units.clone(),
                    halted: m.halted,
                    cash_minor: m.engine.account().cash(),
                    reserved_cash_minor: m.engine.account().reserved_cash(),
                    position_units: m.engine.account().position(),
                    equity_minor: m.engine.equity(),
                    pnl_minor: m.engine.equity() - self.config.markets[i].engine.initial_cash,
                    fees_minor: m.engine.account().fees_paid(),
                    risk_rejections: m.risk_rejections,
                    active_orders: m.engine.active_count(),
                    metrics: m.engine.metrics().into(),
                    strategy_decision: m.strategy.decision().cloned(),
                    strategy_diagnostics: m.strategy.diagnostics(),
                    conditionals: m.conditionals.snapshot(),
                })
                .collect(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PaperReport {
    pub schema_version: u32,
    pub processed: u64,
    pub clock_ns: u64,
    pub closed: bool,
    pub markets: Vec<MarketReport>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MarketReport {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conditionals: Option<ConditionalSnapshot>,
    pub units: crate::config::InstrumentUnits,
    pub market: usize,
    pub symbol: String,
    pub halted: Option<HaltReason>,
    #[serde(with = "crate::config::money")]
    pub cash_minor: i128,
    #[serde(with = "crate::config::money")]
    pub reserved_cash_minor: i128,
    pub position_units: u64,
    #[serde(with = "crate::config::money")]
    pub equity_minor: i128,
    #[serde(with = "crate::config::money")]
    pub pnl_minor: i128,
    #[serde(with = "crate::config::money")]
    pub fees_minor: i128,
    pub risk_rejections: u64,
    pub active_orders: usize,
    pub metrics: MetricReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_decision: Option<crate::target::Decision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy_diagnostics: Option<serde_json::Value>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MetricReport {
    pub quotes: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub fills: u64,
    pub cancelled: u64,
    pub orders_examined: u64,
    #[serde(with = "crate::config::money")]
    pub peak_equity_minor: i128,
    #[serde(with = "crate::config::money")]
    pub max_drawdown_minor: i128,
}
impl From<Metrics> for MetricReport {
    fn from(m: Metrics) -> Self {
        Self {
            quotes: m.quotes,
            accepted: m.accepted,
            rejected: m.rejected,
            fills: m.fills,
            cancelled: m.cancelled,
            orders_examined: m.orders_examined,
            peak_equity_minor: m.peak_equity,
            max_drawdown_minor: m.max_drawdown,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperSnapshot {
    schema_version: u32,
    markets: Vec<MarketSnapshot>,
    clock_ns: u64,
    closed: bool,
    processed: u64,
    pub(crate) retention: Option<usize>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MarketSnapshot {
    engine: EngineSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    conditionals: Option<ConditionalSnapshot>,
    strategy: StrategyCheckpoint,
    halted: Option<HaltReason>,
    risk_rejections: u64,
}

#[derive(Clone, Serialize)]
#[serde(untagged)]
enum StrategyCheckpoint {
    Builtin(BuiltinStrategy),
    Registered { registered: CustomCheckpoint },
}

impl<'de> Deserialize<'de> for StrategyCheckpoint {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        // serde untagged 的中间 Content 不支持 u128；直接走 JSON Value 保留有界整数窗口和。
        let value = serde_json::Value::deserialize(d)?;
        if let Some(object) = value.as_object()
            && let Some(registered) = object.get("registered")
        {
            if object.len() != 1 {
                return Err(D::Error::custom("unexpected custom checkpoint fields"));
            }
            return serde_json::from_value(registered.clone())
                .map(|registered| Self::Registered { registered })
                .map_err(D::Error::custom);
        }
        serde_json::from_value(value)
            .map(Self::Builtin)
            .map_err(D::Error::custom)
    }
}
