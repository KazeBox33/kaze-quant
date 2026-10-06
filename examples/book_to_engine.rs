//! 深度更新 → 订单簿 → Quote → 交易引擎。
use kaze_quant::book::{DepthUpdate, OrderBook};
use kaze_quant::engine::{Engine, EngineConfig};
use kaze_quant::types::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut book = OrderBook::new(64)?;
    let mut engine = Engine::new(EngineConfig {
        initial_cash: 10_000,
        fee_bps: 0,
        ..EngineConfig::default()
    })?;
    let mut events = Vec::new();
    let updates = [
        (Side::Buy, 999, 10),
        (Side::Sell, 1000, 10),
        (Side::Sell, 1000, 2),
        (Side::Sell, 1000, 1),
    ];
    for (index, (side, price, quantity)) in updates.into_iter().enumerate() {
        let u = DepthUpdate {
            sequence: index as u64 + 1,
            timestamp_ns: (index as u64 + 1) * 100,
            side,
            price: Price::new(price)?,
            quantity,
        };
        if let Some(quote) = book.apply(u).map_err(|e| format!("book error: {e:?}"))? {
            engine.on_quote(quote, &mut events)?;
            if quote.sequence == 2 {
                engine
                    .submit(
                        OrderRequest {
                            side: Side::Buy,
                            limit: Price::new(1000)?,
                            quantity: Quantity::new(3)?,
                            time_in_force: TimeInForce::GoodTilCancelled,
                        },
                        &mut events,
                    )
                    .map_err(|e| format!("order rejected: {e:?}"))?;
            }
        }
    }
    engine.finish(&mut events);
    assert_eq!(engine.account().position(), 3);
    assert_eq!(engine.account().cash(), 7000);
    engine.check_invariants()?;
    for event in events {
        println!("{event:?}");
    }
    Ok(())
}
