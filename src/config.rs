//! 严格、带版本的运行配置。金额采用十进制字符串，避免 JSON 消费方丢失 i128 精度。
use crate::engine::EngineConfig;
use crate::strategy::{MomentumStrategy, RollingMean, Strategy, StrategyView, ThresholdStrategy};
use crate::types::*;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub mod money {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};
    pub fn serialize<S: Serializer>(value: &i128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&value.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i128, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(D::Error::custom)
    }
}

pub const SCHEMA_VERSION: u32 = 1;
/// 更新会改变重放语义的代码时必须递增；旧日志不可悄悄采用新规则。
pub const EXECUTION_REVISION: &str = "kaze-paper-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaperConfig {
    pub schema_version: u32,
    pub max_commands: usize,
    pub markets: Vec<MarketConfig>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketConfig {
    pub symbol: String,
    #[serde(default)]
    pub units: InstrumentUnits,
    /// 每个整数 lot 的货币 minor 数，tick/quantity_step 约束整数网格。
    pub price_tick: u64,
    pub quantity_step: u64,
    pub engine: EngineConfig,
    pub risk: RiskConfig,
    pub strategy: StrategyConfig,
}
/// Price * Quantity = 货币 minor；原始报价 = Price * quantity_scale / money_scale。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentUnits {
    pub currency: String,
    pub money_scale: u64,
    pub quantity_scale: u64,
}
impl Default for InstrumentUnits {
    fn default() -> Self {
        Self {
            currency: "SIM".into(),
            money_scale: 100,
            quantity_scale: 1,
        }
    }
}
impl InstrumentUnits {
    pub fn validate(&self) -> Result<(), &'static str> {
        fn power10(mut n: u64) -> bool {
            if n == 0 {
                return false;
            }
            while n.is_multiple_of(10) {
                n /= 10;
            }
            n == 1
        }
        if self.currency.is_empty()
            || self.currency.len() > 16
            || !self.currency.bytes().all(|b| b.is_ascii_uppercase())
            || self.money_scale > 1_000_000_000_000
            || self.quantity_scale > Quantity::MAX
            || !power10(self.money_scale)
            || !power10(self.quantity_scale)
        {
            return Err("invalid instrument unit scales or currency");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiskConfig {
    #[serde(with = "money")]
    pub max_order_notional: i128,
    #[serde(with = "money")]
    pub max_drawdown: i128,
    pub max_quote_age_ns: u64,
    pub price_collar_bps: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum StrategyConfig {
    Passive {},
    Threshold {
        buy_below: Price,
        sell_above: Price,
        quantity: Quantity,
    },
    Momentum {
        window: usize,
        quantity: Quantity,
    },
    MeanReversion {
        window: usize,
        entry_bps: u32,
        exit_bps: u32,
        quantity: Quantity,
    },
}

impl PaperConfig {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != SCHEMA_VERSION {
            return Err("unsupported config schema_version");
        }
        if self.max_commands == 0 || self.max_commands > 1_000_000 {
            return Err("max_commands must be in 1..=1000000");
        }
        if self.markets.is_empty() || self.markets.len() > 64 {
            return Err("markets must contain 1..=64 entries");
        }
        let mut symbols = HashSet::new();
        let mut total_orders = 0usize;
        let mut total_windows = 0usize;
        for m in &self.markets {
            if m.symbol.is_empty()
                || m.symbol.len() > 64
                || !m
                    .symbol
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_/".contains(&c))
            {
                return Err("invalid symbol");
            }
            if !symbols.insert(&m.symbol) {
                return Err("duplicate symbol");
            }
            m.units.validate()?;
            m.engine.validate()?;
            total_orders += m.engine.max_orders;
            if total_orders > 1_000_000 {
                return Err("aggregate max_orders exceeds 1000000");
            }
            if m.price_tick == 0
                || m.price_tick > Price::MAX
                || m.quantity_step == 0
                || m.quantity_step > Quantity::MAX
            {
                return Err("invalid instrument grid");
            }
            if m.risk.max_order_notional <= 0
                || m.risk.max_order_notional > i128::from(Price::MAX) * i128::from(Quantity::MAX)
                || m.risk.max_drawdown <= 0
                || m.risk.max_drawdown > 1_000_000_000_000_000_000_000_000_000_000
                || m.risk.max_quote_age_ns == 0
                || m.risk.price_collar_bps > 10_000
            {
                return Err("invalid risk limits");
            }
            let quantity = match m.strategy {
                StrategyConfig::Passive {} => None,
                StrategyConfig::Threshold {
                    buy_below,
                    sell_above,
                    quantity,
                } => {
                    if buy_below >= sell_above
                        || !buy_below.units().is_multiple_of(m.price_tick)
                        || !sell_above.units().is_multiple_of(m.price_tick)
                    {
                        return Err("invalid threshold price grid or ordering");
                    }
                    Some(quantity)
                }
                StrategyConfig::Momentum { quantity, .. }
                | StrategyConfig::MeanReversion { quantity, .. } => Some(quantity),
            };
            if quantity.is_some_and(|q| {
                !q.units().is_multiple_of(m.quantity_step) || q.units() > m.engine.max_position
            }) {
                return Err("strategy quantity outside instrument grid or position limit");
            }
            match m.strategy {
                StrategyConfig::Momentum { window, .. }
                | StrategyConfig::MeanReversion { window, .. } => {
                    if window == 0 || window > 1_000_000 {
                        return Err("window must be in 1..=1000000");
                    }
                    total_windows += window;
                }
                _ => (),
            }
            if total_windows > 1_000_000 {
                return Err("aggregate strategy windows exceed 1000000");
            }
            if let StrategyConfig::MeanReversion {
                entry_bps,
                exit_bps,
                ..
            } = m.strategy
                && (entry_bps == 0 || entry_bps > 10_000 || exit_bps > 10_000)
            {
                return Err("invalid mean reversion bands");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum BuiltinStrategy {
    Passive,
    Threshold(ThresholdStrategy),
    Momentum(MomentumStrategy),
    MeanReversion {
        mean: RollingMean,
        entry_bps: u32,
        exit_bps: u32,
        quantity: Quantity,
    },
}
impl StrategyConfig {
    pub fn build(&self) -> Result<BuiltinStrategy, &'static str> {
        Ok(match *self {
            Self::Passive {} => BuiltinStrategy::Passive,
            Self::Threshold {
                buy_below,
                sell_above,
                quantity,
            } => BuiltinStrategy::Threshold(ThresholdStrategy {
                buy_below,
                sell_above,
                quantity,
            }),
            Self::Momentum { window, quantity } => {
                BuiltinStrategy::Momentum(MomentumStrategy::new(window, quantity)?)
            }
            Self::MeanReversion {
                window,
                entry_bps,
                exit_bps,
                quantity,
            } => {
                if entry_bps == 0 || entry_bps > 10_000 || exit_bps > 10_000 {
                    return Err("invalid mean reversion bands");
                }
                BuiltinStrategy::MeanReversion {
                    mean: RollingMean::new(window)?,
                    entry_bps,
                    exit_bps,
                    quantity,
                }
            }
        })
    }
}
impl Strategy for BuiltinStrategy {
    fn checkpoint(&self) -> Option<BuiltinStrategy> {
        Some(self.clone())
    }
    fn on_quote(&mut self, view: StrategyView<'_>) -> Action {
        match self {
            Self::Passive => Action::None,
            Self::Threshold(s) => s.on_quote(view),
            Self::Momentum(s) => s.on_quote(view),
            Self::MeanReversion {
                mean,
                entry_bps,
                exit_bps,
                quantity,
            } => {
                let Some(avg) = mean.push(view.quote.bid) else {
                    return Action::None;
                };
                if view.active_orders != 0 {
                    return Action::None;
                }
                let (side, limit, size) = if view.account.position() == 0
                    && u128::from(view.quote.ask.units()) * 10_000
                        < u128::from(avg) * u128::from(10_000 - *entry_bps)
                {
                    (Side::Buy, view.quote.ask, *quantity)
                } else if view.account.position() > 0
                    && u128::from(view.quote.bid.units()) * 10_000
                        >= u128::from(avg) * u128::from(10_000 + *exit_bps)
                {
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
                    quantity: size,
                    time_in_force: TimeInForce::ImmediateOrCancel,
                })
            }
        }
    }
}

impl BuiltinStrategy {
    pub fn validate_state(&self, config: &StrategyConfig) -> Result<(), &'static str> {
        match (self, config) {
            (Self::Passive, StrategyConfig::Passive {}) => Ok(()),
            (
                Self::Threshold(s),
                StrategyConfig::Threshold {
                    buy_below,
                    sell_above,
                    quantity,
                },
            ) if s.buy_below == *buy_below
                && s.sell_above == *sell_above
                && s.quantity == *quantity =>
            {
                Ok(())
            }
            (Self::Momentum(s), StrategyConfig::Momentum { window, quantity }) => {
                s.validate_state(*window, *quantity)
            }
            (
                Self::MeanReversion {
                    mean,
                    entry_bps,
                    exit_bps,
                    quantity,
                },
                StrategyConfig::MeanReversion {
                    window,
                    entry_bps: e,
                    exit_bps: x,
                    quantity: q,
                },
            ) if entry_bps == e && exit_bps == x && quantity == q => mean.validate_state(*window),
            _ => Err("strategy checkpoint differs from configuration"),
        }
    }
}
