use crate::account::{Account, fee};
use crate::types::*;

/// 引擎只发出值事件，不拥有日志文件或历史事件集合。
pub trait EventSink {
    fn emit(&mut self, event: Event);
}
impl EventSink for Vec<Event> {
    fn emit(&mut self, event: Event) {
        self.push(event);
    }
}
impl EventSink for () {
    fn emit(&mut self, _: Event) {}
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EngineConfig {
    #[serde(with = "crate::config::money")]
    pub initial_cash: i128,
    pub max_position: u64,
    pub max_active_orders: usize,
    /// 包括被拒绝和已结束的订单；达到上限时返回 HistoryLimit。
    pub max_orders: usize,
    pub fee_bps: u32,
    pub latency_ns: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            initial_cash: 1_000_000,
            max_position: 100,
            max_active_orders: 128,
            max_orders: 10_000,
            fee_bps: 10,
            latency_ns: 0,
        }
    }
}

impl EngineConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.initial_cash < 0 || self.initial_cash > 1_000_000_000_000_000_000_000_000_000_000 {
            return Err("initial cash must be in 0..=10^30 minor units");
        }
        if self.max_position == 0 || self.max_position > Quantity::MAX {
            return Err("max_position out of bounds");
        }
        if self.max_orders == 0 || self.max_orders > 1_000_000 {
            return Err("max_orders must be in 1..=1_000_000");
        }
        if self.max_active_orders == 0 || self.max_active_orders > self.max_orders {
            return Err("max_active_orders must be in 1..=max_orders");
        }
        if self.fee_bps > 10_000 {
            return Err("fee_bps must not exceed 10000");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Metrics {
    pub quotes: u64,
    pub accepted: u64,
    pub rejected: u64,
    pub fills: u64,
    pub cancelled: u64,
    /// 算法工作量计数，便于将时间差与扫描次数对应。
    pub orders_examined: u64,
    pub peak_equity: i128,
    pub max_drawdown: i128,
}

/// History 是特意保留的朴素扫描实现，用于结果对照与性能实验。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanPolicy {
    Active,
    History,
}

pub struct Engine {
    config: EngineConfig,
    account: Account,
    orders: Vec<Order>,
    /// 连续索引保持提交顺序；终结订单稳定压缩，禁止 swap_remove 改变优先级。
    active: Vec<usize>,
    quote: Option<Quote>,
    metrics: Metrics,
    policy: ScanPolicy,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Result<Self, &'static str> {
        Self::with_scan_policy(config, ScanPolicy::Active)
    }
    pub fn with_scan_policy(
        config: EngineConfig,
        policy: ScanPolicy,
    ) -> Result<Self, &'static str> {
        config.validate()?;
        Ok(Self {
            account: Account::new(config.initial_cash),
            orders: Vec::with_capacity(config.max_orders),
            active: Vec::with_capacity(config.max_active_orders),
            quote: None,
            metrics: Metrics {
                peak_equity: config.initial_cash,
                ..Metrics::default()
            },
            config,
            policy,
        })
    }
    pub fn config(&self) -> &EngineConfig {
        &self.config
    }
    pub fn account(&self) -> &Account {
        &self.account
    }
    pub fn metrics(&self) -> Metrics {
        self.metrics
    }
    pub fn last_quote(&self) -> Option<Quote> {
        self.quote
    }
    pub fn orders(&self) -> &[Order] {
        &self.orders
    }
    pub fn active_count(&self) -> usize {
        self.active.len()
    }
    pub fn order(&self, id: OrderId) -> Option<&Order> {
        let idx = usize::try_from(id.0.checked_sub(1)?).ok()?;
        self.orders.get(idx)
    }
    pub fn equity(&self) -> i128 {
        self.quote
            .map_or(self.account.cash(), |q| self.account.equity(q.bid))
    }

    /// 外部输入必须先完全校验，失败时不修改账户、订单、时钟与统计。
    pub fn on_quote(&mut self, q: Quote, sink: &mut impl EventSink) -> Result<(), &'static str> {
        q.validate()?;
        if let Some(prev) = self.quote {
            if q.sequence <= prev.sequence {
                return Err("quote sequences must strictly increase");
            }
            if q.timestamp_ns < prev.timestamp_ns {
                return Err("quote timestamps must not decrease");
            }
        }
        self.quote = Some(q);
        self.metrics.quotes += 1;
        let mut bid_available = q.bid_quantity;
        let mut ask_available = q.ask_quantity;
        // 没有终结/拒绝历史时，所有记录均活跃，可直接连续扫描。
        // 一旦存在非活跃历史，切换到索引，避免间接访问成为密集负载的成本。
        if self.policy == ScanPolicy::History || self.active.len() == self.orders.len() {
            for index in 0..self.orders.len() {
                self.metrics.orders_examined = self.metrics.orders_examined.saturating_add(1);
                self.try_execute(index, q, &mut bid_available, &mut ask_available, sink);
            }
        } else {
            for slot in 0..self.active.len() {
                let index = self.active[slot];
                self.metrics.orders_examined = self.metrics.orders_examined.saturating_add(1);
                self.try_execute(index, q, &mut bid_available, &mut ask_available, sink);
            }
        }
        // retain 不分配，不打乱相对顺序；终结记录仍保留在 orders 中。
        self.active.retain(|&i| self.orders[i].status.is_active());
        let equity = self.equity();
        self.metrics.peak_equity = self.metrics.peak_equity.max(equity);
        self.metrics.max_drawdown = self
            .metrics
            .max_drawdown
            .max(self.metrics.peak_equity - equity);
        debug_assert!(self.check_invariants().is_ok());
        Ok(())
    }

    fn try_execute(
        &mut self,
        index: usize,
        q: Quote,
        bid_available: &mut u64,
        ask_available: &mut u64,
        sink: &mut impl EventSink,
    ) {
        let order = &self.orders[index];
        if !order.status.is_active()
            || order.submitted_sequence >= q.sequence
            || order.eligible_at_ns > q.timestamp_ns
        {
            return;
        }
        let (price, available, crossing) = match order.request.side {
            Side::Buy => (q.ask, ask_available, order.request.limit >= q.ask),
            Side::Sell => (q.bid, bid_available, order.request.limit <= q.bid),
        };
        if crossing && *available > 0 {
            let quantity = order.remaining.min(*available);
            let fill = Fill {
                order_id: order.id,
                side: order.request.side,
                quantity,
                price,
                fee: fee(
                    i128::from(price.units()) * i128::from(quantity),
                    self.config.fee_bps,
                ),
                sequence: q.sequence,
                timestamp_ns: q.timestamp_ns,
            };
            self.account.apply(fill, order.reserved_per_unit);
            let order = &mut self.orders[index];
            order.remaining -= quantity;
            order.status = if order.remaining == 0 {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
            *available -= quantity;
            self.metrics.fills += 1;
            sink.emit(Event::Fill(fill));
        }
        if self.orders[index].status.is_active()
            && self.orders[index].request.time_in_force == TimeInForce::ImmediateOrCancel
        {
            self.cancel_index(index, sink);
        }
    }

    pub fn submit(
        &mut self,
        request: OrderRequest,
        sink: &mut impl EventSink,
    ) -> Result<OrderId, RejectReason> {
        // 达到历史容量时无新 ID 和事件：调用方必须检查 Result。
        if self.orders.len() >= self.config.max_orders {
            self.metrics.rejected += 1;
            return Err(RejectReason::HistoryLimit);
        }
        let id = OrderId(self.orders.len() as u64 + 1);
        let sequence = self.quote.map_or(0, |q| q.sequence);
        let timestamp = self.quote.map_or(0, |q| q.timestamp_ns);
        let eligible = timestamp.checked_add(self.config.latency_ns);
        let quantity = request.quantity.units();
        let per_unit = i128::from(request.limit.units())
            + fee(i128::from(request.limit.units()), self.config.fee_bps);
        let reserve = per_unit * i128::from(quantity);
        let rejection = if self.quote.is_none() {
            Some(RejectReason::NoMarket)
        } else if eligible.is_none() {
            Some(RejectReason::TimestampOverflow)
        } else if self.active.len() >= self.config.max_active_orders {
            Some(RejectReason::ActiveOrderLimit)
        } else {
            match request.side {
                Side::Buy
                    if self.account.position + self.account.pending_buy + quantity
                        > self.config.max_position =>
                {
                    Some(RejectReason::PositionLimit)
                }
                Side::Buy if reserve > self.account.available_cash() => {
                    Some(RejectReason::InsufficientCash)
                }
                Side::Sell if quantity > self.account.position - self.account.reserved_sell => {
                    Some(RejectReason::InsufficientPosition)
                }
                _ => None,
            }
        };
        let index = self.orders.len();
        self.orders.push(Order {
            id,
            request,
            remaining: quantity,
            status: rejection.map_or(OrderStatus::Accepted, OrderStatus::Rejected),
            submitted_sequence: sequence,
            eligible_at_ns: eligible.unwrap_or(timestamp),
            reserved_per_unit: if request.side == Side::Buy {
                per_unit
            } else {
                0
            },
        });
        if let Some(reason) = rejection {
            self.metrics.rejected += 1;
            sink.emit(Event::Rejected {
                order_id: id,
                reason,
            });
            return Err(reason);
        }
        match request.side {
            Side::Buy => {
                self.account.reserved_cash += reserve;
                self.account.pending_buy += quantity;
            }
            Side::Sell => self.account.reserved_sell += quantity,
        }
        self.active.push(index);
        self.metrics.accepted += 1;
        sink.emit(Event::Accepted {
            order_id: id,
            sequence,
        });
        debug_assert!(self.check_invariants().is_ok());
        Ok(id)
    }

    fn cancel_index(&mut self, index: usize, sink: &mut impl EventSink) {
        let order = &mut self.orders[index];
        self.account
            .release(order.request.side, order.remaining, order.reserved_per_unit);
        order.status = OrderStatus::Cancelled;
        self.metrics.cancelled += 1;
        sink.emit(Event::Cancelled {
            order_id: order.id,
            sequence: self.quote.map_or(0, |q| q.sequence),
        });
    }
    pub fn cancel(&mut self, id: OrderId, sink: &mut impl EventSink) -> bool {
        let Some(order) = self.order(id) else {
            return false;
        };
        if !order.status.is_active() {
            return false;
        }
        let index = (id.0 - 1) as usize;
        self.cancel_index(index, sink);
        self.active.retain(|&i| i != index);
        debug_assert!(self.check_invariants().is_ok());
        true
    }
    /// 回放结束撤销所有未成交订单，释放冻结资源；持仓仍按最终 bid 估值。
    pub fn finish(&mut self, sink: &mut impl EventSink) {
        for slot in 0..self.active.len() {
            self.cancel_index(self.active[slot], sink);
        }
        self.active.clear();
        debug_assert!(self.check_invariants().is_ok());
    }

    /// 昂贵的独立审计，只在 debug/test 或调用方主动要求时扫描完整历史。
    pub fn check_invariants(&self) -> Result<(), &'static str> {
        if self.account.cash < 0
            || self.account.reserved_cash < 0
            || self.account.available_cash() < 0
        {
            return Err("cash or cash reservation invalid");
        }
        if self.account.reserved_sell > self.account.position {
            return Err("sell reservation invalid");
        }
        if self.account.position + self.account.pending_buy > self.config.max_position {
            return Err("position exposure invalid");
        }
        if self.account.cash
            != self.config.initial_cash - self.account.net_buy_notional - self.account.fees_paid
        {
            return Err("cash conservation failed");
        }
        let (mut cash, mut buy, mut sell, mut count) = (0, 0, 0, 0);
        let mut last_index = None;
        for &i in &self.active {
            if last_index.is_some_and(|last| last >= i) {
                return Err("active order sequence invalid");
            }
            last_index = Some(i);
            if !self.orders[i].status.is_active() {
                return Err("terminal order in active index");
            }
        }
        for order in &self.orders {
            if order.remaining > order.request.quantity.units() {
                return Err("order remaining invalid");
            }
            if order.status == OrderStatus::Filled && order.remaining != 0 {
                return Err("filled order has remaining quantity");
            }
            if order.status.is_active() {
                if order.remaining == 0 {
                    return Err("active order has no remaining quantity");
                }
                count += 1;
                match order.request.side {
                    Side::Buy => {
                        buy += order.remaining;
                        cash += order.reserved_per_unit * i128::from(order.remaining);
                    }
                    Side::Sell => sell += order.remaining,
                }
            }
        }
        if count != self.active.len() || count > self.config.max_active_orders {
            return Err("active index count invalid");
        }
        if cash != self.account.reserved_cash
            || buy != self.account.pending_buy
            || sell != self.account.reserved_sell
        {
            return Err("reservation reconciliation failed");
        }
        Ok(())
    }
}
