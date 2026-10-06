use serde::{Deserialize, Serialize};
use std::fmt;

/// 单位是“分”，不是浮点元。上限使后续 i128 核算具有明确的数值边界。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(try_from = "u64", into = "u64")]
pub struct Price(u64);

impl Price {
    pub const MAX: u64 = 1_000_000_000_000;
    pub fn new(minor_units: u64) -> Result<Self, &'static str> {
        if minor_units == 0 || minor_units > Self::MAX {
            return Err("price must be in 1..=1_000_000_000_000 minor units");
        }
        Ok(Self(minor_units))
    }
    pub fn units(self) -> u64 {
        self.0
    }
}

impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}", self.0 / 100, self.0 % 100)
    }
}

/// 整数资产单位；暂不支持小数数量。零数量只用于剩余量和报价流动性。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(try_from = "u64", into = "u64")]
pub struct Quantity(u64);

impl Quantity {
    pub const MAX: u64 = 1_000_000_000;
    pub fn new(units: u64) -> Result<Self, &'static str> {
        if units == 0 || units > Self::MAX {
            return Err("order quantity must be in 1..=1_000_000_000");
        }
        Ok(Self(units))
    }
    pub fn units(self) -> u64 {
        self.0
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeInForce {
    GoodTilCancelled,
    /// 在第一条“延迟已到且晚于提交事件”的报价上尝试，剩余量撤销。
    ImmediateOrCancel,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OrderRequest {
    pub side: Side,
    pub limit: Price,
    pub quantity: Quantity,
    pub time_in_force: TimeInForce,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OrderId(pub u64);

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Quote {
    pub sequence: u64,
    pub timestamp_ns: u64,
    pub bid: Price,
    pub ask: Price,
    pub bid_quantity: u64,
    pub ask_quantity: u64,
}

impl Quote {
    pub fn validate(self) -> Result<(), &'static str> {
        if self.sequence == 0 {
            return Err("quote sequence must be positive");
        }
        if self.bid > self.ask {
            return Err("crossed quote: bid exceeds ask");
        }
        if self.bid_quantity > Quantity::MAX || self.ask_quantity > Quantity::MAX {
            return Err("quote liquidity exceeds quantity bound");
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    NoMarket,
    InsufficientCash,
    InsufficientPosition,
    PositionLimit,
    ActiveOrderLimit,
    HistoryLimit,
    TimestampOverflow,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum OrderStatus {
    Accepted,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected(RejectReason),
}

impl OrderStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Accepted | Self::PartiallyFilled)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub id: OrderId,
    pub request: OrderRequest,
    pub remaining: u64,
    pub status: OrderStatus,
    pub submitted_sequence: u64,
    pub eligible_at_ns: u64,
    /// 买单逐单位冻结，覆盖任意拆分成交的手续费向上取整。
    #[serde(with = "crate::config::money")]
    pub reserved_per_unit: i128,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fill {
    pub order_id: OrderId,
    pub side: Side,
    pub quantity: u64,
    pub price: Price,
    #[serde(with = "crate::config::money")]
    pub fee: i128,
    pub sequence: u64,
    pub timestamp_ns: u64,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Accepted {
        order_id: OrderId,
        sequence: u64,
    },
    Rejected {
        order_id: OrderId,
        reason: RejectReason,
    },
    Fill(Fill),
    Cancelled {
        order_id: OrderId,
        sequence: u64,
    },
}

/// 策略只能通过一个动作表达意图，账户与订单仍由引擎拥有。
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Submit(OrderRequest),
    Cancel(OrderId),
    None,
}

impl TryFrom<u64> for Price {
    type Error = &'static str;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<Price> for u64 {
    fn from(value: Price) -> Self {
        value.units()
    }
}
impl TryFrom<u64> for Quantity {
    type Error = &'static str;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<Quantity> for u64 {
    fn from(value: Quantity) -> Self {
        value.units()
    }
}
