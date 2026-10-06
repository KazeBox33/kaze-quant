use crate::account::Account;
use crate::types::*;

pub struct StrategyView<'a> {
    pub quote: Quote,
    pub account: &'a Account,
    pub active_orders: usize,
}

/// 泛型调用会静态分发；策略只借用快照，不能直接改账户。
pub trait Strategy {
    fn on_quote(&mut self, _view: StrategyView<'_>) -> Action {
        Action::None
    }
    /// 多动作策略可以覆盖此方法；缓冲区的硬上限避免信号意外产生无限委托。
    fn on_quote_batch(&mut self, view: StrategyView<'_>, actions: &mut ActionBuffer) {
        let _ = actions.push(self.on_quote(view));
    }
    /// 回报由引擎生成，策略只能消费，不能改写账户或订单。
    fn on_event(&mut self, _event: Event) {}
    fn on_submit_result(&mut self, _request: OrderRequest, _result: Result<OrderId, RejectReason>) {
    }
}

pub struct ThresholdStrategy {
    pub buy_below: Price,
    pub sell_above: Price,
    pub quantity: Quantity,
}

impl Strategy for ThresholdStrategy {
    fn on_quote(&mut self, view: StrategyView<'_>) -> Action {
        if view.active_orders != 0 {
            return Action::None;
        }
        let q = view.quote;
        let (side, limit, quantity) = if view.account.position() == 0 && q.ask < self.buy_below {
            (Side::Buy, self.buy_below, self.quantity)
        } else if view.account.position() > 0 && q.bid > self.sell_above {
            (
                Side::Sell,
                self.sell_above,
                Quantity::new(view.account.position()).expect("bounded position"),
            )
        } else {
            return Action::None;
        };
        Action::Submit(OrderRequest {
            side,
            limit,
            quantity,
            time_in_force: TimeInForce::GoodTilCancelled,
        })
    }
}

/// 预分配环形窗口 + 增量和：push O(1)，不逐次重算整个窗口。
pub struct RollingMean {
    values: Vec<u64>,
    next: usize,
    count: usize,
    sum: u128,
}

impl RollingMean {
    pub fn new(window: usize) -> Result<Self, &'static str> {
        if window == 0 || window > 1_000_000 {
            return Err("window must be in 1..=1_000_000");
        }
        Ok(Self {
            values: vec![0; window],
            next: 0,
            count: 0,
            sum: 0,
        })
    }
    /// 未满窗口时返回 None；均值向下取整到整数价格单位。
    pub fn push(&mut self, value: Price) -> Option<u64> {
        self.sum -= u128::from(self.values[self.next]);
        self.values[self.next] = value.units();
        self.sum += u128::from(value.units());
        self.next += 1;
        if self.next == self.values.len() {
            self.next = 0;
        }
        self.count = (self.count + 1).min(self.values.len());
        (self.count == self.values.len()).then(|| (self.sum / self.count as u128) as u64)
    }
}

/// 只用于学习信号/执行分离的均值趋势示例，不代表已验证的交易优势。
pub struct MomentumStrategy {
    mean: RollingMean,
    quantity: Quantity,
}
impl MomentumStrategy {
    pub fn new(window: usize, quantity: Quantity) -> Result<Self, &'static str> {
        Ok(Self {
            mean: RollingMean::new(window)?,
            quantity,
        })
    }
}
impl Strategy for MomentumStrategy {
    fn on_quote(&mut self, view: StrategyView<'_>) -> Action {
        let Some(mean) = self.mean.push(view.quote.bid) else {
            return Action::None;
        };
        if view.active_orders != 0 {
            return Action::None;
        }
        let (side, limit, quantity) =
            if view.account.position() == 0 && view.quote.bid.units() > mean {
                (Side::Buy, view.quote.ask, self.quantity)
            } else if view.account.position() > 0 && view.quote.bid.units() < mean {
                (
                    Side::Sell,
                    view.quote.bid,
                    Quantity::new(view.account.position()).expect("bounded position"),
                )
            } else {
                return Action::None;
            };
        Action::Submit(OrderRequest {
            side,
            limit,
            quantity,
            time_in_force: TimeInForce::ImmediateOrCancel,
        })
    }
}

/// 每次回调复用同一有界缓冲；即便策略忽略 push 错误，调用方也能检测溢出。
pub struct ActionBuffer {
    actions: Vec<Action>,
    capacity: usize,
    overflowed: bool,
}
impl ActionBuffer {
    pub fn new(capacity: usize) -> Result<Self, &'static str> {
        if capacity == 0 || capacity > 1024 {
            return Err("action capacity must be in 1..=1024");
        }
        Ok(Self {
            actions: Vec::with_capacity(capacity),
            capacity,
            overflowed: false,
        })
    }
    pub fn clear(&mut self) {
        self.actions.clear();
        self.overflowed = false;
    }
    pub fn push(&mut self, action: Action) -> Result<(), &'static str> {
        if action == Action::None {
            return Ok(());
        }
        if self.actions.len() == self.capacity {
            self.overflowed = true;
            return Err("strategy action capacity exceeded");
        }
        self.actions.push(action);
        Ok(())
    }
    pub fn actions(&self) -> Result<&[Action], &'static str> {
        if self.overflowed {
            return Err("strategy action capacity exceeded");
        }
        Ok(&self.actions)
    }
}
