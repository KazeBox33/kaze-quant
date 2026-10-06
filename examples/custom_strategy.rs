//! 批量信号与回报回调示例；两张 IOC 自己订单共享下一条报价的 3 单位流动性。
use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::paper::{Command, Envelope, PaperRuntime};
use kaze_quant::strategy::{ActionBuffer, Strategy, StrategyView};
use kaze_quant::types::*;
struct SplitBuyer {
    fired: bool,
}
impl Strategy for SplitBuyer {
    fn on_quote_batch(&mut self, view: StrategyView<'_>, actions: &mut ActionBuffer) {
        if self.fired {
            return;
        }
        self.fired = true;
        for _ in 0..2 {
            actions
                .push(Action::Submit(OrderRequest {
                    side: Side::Buy,
                    limit: view.quote.ask,
                    quantity: Quantity::new(2).unwrap(),
                    time_in_force: TimeInForce::ImmediateOrCancel,
                }))
                .unwrap();
        }
    }
    fn on_event(&mut self, event: Event) {
        println!("strategy callback: {event:?}");
    }
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut config: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json"))?;
    config.markets.truncate(1);
    config.markets[0].strategy = StrategyConfig::Passive {};
    let mut runtime =
        PaperRuntime::with_strategies(config, vec![Box::new(SplitBuyer { fired: false })])?;
    for sequence in 1..=2 {
        let quote = Quote {
            sequence,
            timestamp_ns: sequence,
            bid: Price::new(99)?,
            ask: Price::new(100)?,
            bid_quantity: 3,
            ask_quantity: 3,
        };
        runtime.process(&Envelope {
            seq: sequence,
            command: Command::Quote { market: 0, quote },
        })?;
    }
    runtime.check_invariants()?;
    let engine = runtime.engine(0).unwrap();
    assert_eq!(engine.account().position(), 3);
    assert_eq!(engine.account().cash(), 999698); // 两次手续费各1分，成交总额300。
    println!(
        "position={}, cash_minor={}",
        engine.account().position(),
        engine.account().cash()
    );
    Ok(())
}
