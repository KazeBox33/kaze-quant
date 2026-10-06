//! 教学策略：每 period 个报价尝试切换持仓，用于验证注册/回报/恢复，不代表 alpha。
use kaze_quant::paper::PaperError;
use kaze_quant::registry::{CustomCheckpoint, StrategyRegistry};
use kaze_quant::strategy::{Strategy, StrategyView};
use kaze_quant::types::*;
use serde::{Deserialize, Serialize};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Parameters {
    period: u64,
    quantity: Quantity,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pulse {
    seen: u64,
    period: u64,
    quantity: Quantity,
}
impl Strategy for Pulse {
    fn on_quote(&mut self, view: StrategyView<'_>) -> Action {
        self.seen += 1;
        if !self.seen.is_multiple_of(self.period) || view.active_orders != 0 {
            return Action::None;
        }
        let (side, limit, quantity) = if view.account.position() == 0 {
            (Side::Buy, view.quote.ask, self.quantity)
        } else {
            (
                Side::Sell,
                view.quote.bid,
                Quantity::new(view.account.position()).expect("bounded position"),
            )
        };
        Action::Submit(OrderRequest {
            side,
            limit,
            quantity,
            time_in_force: TimeInForce::ImmediateOrCancel,
        })
    }
    fn custom_checkpoint(&self) -> Option<CustomCheckpoint> {
        Some(CustomCheckpoint {
            name: "pulse".into(),
            version: 1,
            state: serde_json::to_value(self).expect("integer strategy state"),
        })
    }
}
fn factory(
    parameters: &serde_json::Value,
    state: Option<&serde_json::Value>,
) -> Result<Box<dyn Strategy>, PaperError> {
    let p: Parameters = serde_json::from_value(parameters.clone())?;
    if !(2..=1000000).contains(&p.period) || p.quantity.units() > 100000 {
        return Err("invalid pulse parameters".into());
    }
    let s = if let Some(state) = state {
        let s: Pulse = serde_json::from_value(state.clone())?;
        if s.period != p.period || s.quantity != p.quantity || s.seen > 1_000_000_000_000 {
            return Err("pulse checkpoint does not match parameters/bounds".into());
        }
        s
    } else {
        Pulse {
            seen: 0,
            period: p.period,
            quantity: p.quantity,
        }
    };
    Ok(Box::new(s))
}
pub fn registry() -> StrategyRegistry {
    let mut r = StrategyRegistry::default();
    r.register("pulse", 1, factory)
        .expect("unique registration");
    r
}
