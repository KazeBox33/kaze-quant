use crate::types::{Fill, Price, Side};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub(crate) cash: i128,
    pub(crate) position: u64,
    pub(crate) reserved_cash: i128,
    pub(crate) reserved_sell: u64,
    pub(crate) pending_buy: u64,
    pub(crate) fees_paid: i128,
    pub(crate) net_buy_notional: i128,
}

impl Account {
    pub(crate) fn new(cash: i128) -> Self {
        Self {
            cash,
            position: 0,
            reserved_cash: 0,
            reserved_sell: 0,
            pending_buy: 0,
            fees_paid: 0,
            net_buy_notional: 0,
        }
    }
    pub fn cash(&self) -> i128 {
        self.cash
    }
    pub fn position(&self) -> u64 {
        self.position
    }
    pub fn available_cash(&self) -> i128 {
        self.cash - self.reserved_cash
    }
    pub fn reserved_cash(&self) -> i128 {
        self.reserved_cash
    }
    pub fn reserved_sell(&self) -> u64 {
        self.reserved_sell
    }
    pub fn pending_buy(&self) -> u64 {
        self.pending_buy
    }
    pub fn fees_paid(&self) -> i128 {
        self.fees_paid
    }
    /// 以 bid 估值表示立即卖出的报价价值；不包含未来平仓手续费。
    pub fn equity(&self, mark: Price) -> i128 {
        self.cash + i128::from(self.position) * i128::from(mark.units())
    }
    pub(crate) fn apply(&mut self, fill: Fill, reserve_per_unit: i128) {
        let notional = i128::from(fill.price.units()) * i128::from(fill.quantity);
        match fill.side {
            Side::Buy => {
                self.reserved_cash -= reserve_per_unit * i128::from(fill.quantity);
                self.pending_buy -= fill.quantity;
                self.cash -= notional + fill.fee;
                self.position += fill.quantity;
                self.net_buy_notional += notional;
            }
            Side::Sell => {
                self.reserved_sell -= fill.quantity;
                self.position -= fill.quantity;
                self.cash += notional - fill.fee;
                self.net_buy_notional -= notional;
            }
        }
        self.fees_paid += fill.fee;
    }
    pub(crate) fn release(&mut self, side: Side, remaining: u64, per_unit: i128) {
        match side {
            Side::Buy => {
                self.reserved_cash -= per_unit * i128::from(remaining);
                self.pending_buy -= remaining;
            }
            Side::Sell => self.reserved_sell -= remaining,
        }
    }
}

/// 每次成交向上取整到分。bps 最大 10000，由 EngineConfig 校验。
pub fn fee(notional: i128, bps: u32) -> i128 {
    // 内核调用输入为有界非负成交额。公开函数对非法输入给出明确断言。
    assert!(notional >= 0 && notional <= i128::from(Price::MAX) * 1_000_000_000);
    assert!(bps <= 10_000);
    (notional * i128::from(bps) + 9_999) / 10_000
}
