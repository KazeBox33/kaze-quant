//! 可重放的命令边界：直接整数索引路由，多资产采用独立预算；没有隐藏跨账户融资。
use crate::config::{BuiltinStrategy, PaperConfig};
use crate::engine::{Engine, EngineSnapshot, MAX_LIFETIME_ORDERS, Metrics};
use crate::strategy::{ActionBuffer, Strategy, StrategyView};
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::fmt;

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
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Notice {
    Engine { market: usize, event: Event },
    RiskRejected { market: usize, reason: RiskReject },
    Halted { market: usize, reason: HaltReason },
    CancelMissing { market: usize, order_id: OrderId },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    pub seq: u64,
    pub duplicate: bool,
    pub notices: Vec<Notice>,
}
struct Market {
    engine: Engine,
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
}
impl PaperRuntime {
    pub fn new(config: PaperConfig) -> Result<Self, PaperError> {
        config.validate()?;
        let strategies = config
            .markets
            .iter()
            .map(|m| m.strategy.build().map(|s| Box::new(s) as Box<dyn Strategy>))
            .collect::<Result<Vec<_>, _>>()?;
        Self::with_strategies(config, strategies)
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
            markets.push(Market {
                engine: Engine::new(m.engine.clone())?,
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
        })
    }
    pub fn config(&self) -> &PaperConfig {
        &self.config
    }
    pub fn engine(&self, market: usize) -> Option<&Engine> {
        self.markets.get(market).map(|m| &m.engine)
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
            | Command::Cancel { market, .. } => Some(*market),
            _ => None,
        };
        if market.is_some_and(|i| i >= self.markets.len()) {
            return Err("unknown market index".into());
        }
        match command {
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
                    let m = &mut self.markets[market];
                    self.actions.clear();
                    m.strategy.on_quote_batch(
                        StrategyView {
                            quote,
                            account: m.engine.account(),
                            active_orders: m.engine.active_count(),
                        },
                        &mut self.actions,
                    );
                    if self.actions.actions().is_err() {
                        self.halt_market(market, HaltReason::StrategyCapacity, &mut notices);
                    } else {
                        for slot in 0..self.actions.actions().expect("validated batch").len() {
                            let action = self.actions.actions().expect("validated batch")[slot];
                            self.action(market, action, &mut notices);
                        }
                    }
                }
            }
            Command::Submit { market, request } => {
                self.action(market, Action::Submit(request), &mut notices)
            }
            Command::Cancel { market, order_id } => {
                self.action(market, Action::Cancel(order_id), &mut notices)
            }
            Command::Advance { timestamp_ns } => {
                self.clock_ns = timestamp_ns;
                self.expire_feeds(&mut notices);
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
    fn action(&mut self, i: usize, action: Action, notices: &mut Vec<Notice>) {
        let mut events = Vec::new();
        match action {
            Action::Submit(request) => {
                if let Some(retain) = self.retention
                    && self.markets[i].engine.orders().len()
                        >= self.config.markets[i].engine.max_orders
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
                    self.markets[i].risk_rejections += 1;
                    notices.push(Notice::RiskRejected { market: i, reason });
                } else {
                    let outcome = self.markets[i].engine.submit(request, &mut events);
                    self.markets[i].strategy.on_submit_result(request, outcome);
                }
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
            .map(|m| {
                Ok(MarketSnapshot {
                    engine: m.engine.snapshot(),
                    strategy: m
                        .strategy
                        .checkpoint()
                        .ok_or("strategy does not provide a deterministic checkpoint")?,
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
            m.strategy.validate_state(&c.strategy)?;
            let engine = Engine::restore(c.engine.clone(), m.engine)?;
            if engine
                .last_quote()
                .is_some_and(|q| q.timestamp_ns > state.clock_ns)
                || engine.metrics().quotes > state.processed
                || m.risk_rejections > state.processed.saturating_mul(64)
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
                strategy: Box::new(m.strategy),
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
        };
        runtime.check_invariants()?;
        Ok(runtime)
    }
    pub fn check_invariants(&self) -> Result<(), PaperError> {
        for m in &self.markets {
            m.engine.check_invariants()?;
            if m.halted.is_some() && m.engine.active_count() != 0 {
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
    strategy: BuiltinStrategy,
    halted: Option<HaltReason>,
    risk_rejections: u64,
}
