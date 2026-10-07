//! 与冻结旧版分别链接；逐步回执/完整检查点摘要比较，而非两个扫描策略共用新容器。
use kaze_quant::{
    engine::{Engine, EngineConfig, LiquidityModel},
    types::*,
};
use sha2::{Digest, Sha256};
fn main() {
    for model in [LiquidityModel::QuoteRefresh, LiquidityModel::DeltaBudget] {
        let cfg = EngineConfig {
            initial_cash: 100000,
            max_position: 100,
            max_orders: 128,
            max_active_orders: 16,
            fee_bps: 17,
            latency_ns: 2,
            slippage_bps: 10,
            liquidity_model: model,
        };
        let mut e = Engine::new(cfg.clone()).unwrap();
        let mut seed = 713_u64;
        let mut seq = 0;
        let mut h = Sha256::new();
        let mut events_count = 0;
        for step in 0..20000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let mut events = vec![];
            let result = match seed % 10 {
                0..=3 => {
                    seq += 1;
                    let p = 95 + (seed >> 16) % 12;
                    serde_json::to_value(e.on_quote(
                        Quote {
                            sequence: seq,
                            timestamp_ns: seq * 5,
                            bid: Price::new(p).unwrap(),
                            ask: Price::new(p + 1).unwrap(),
                            bid_quantity: (seed >> 24) % 8,
                            ask_quantity: (seed >> 32) % 8,
                        },
                        &mut events,
                    ))
                    .unwrap()
                }
                4..=7 => serde_json::to_value(e.submit(
                    OrderRequest {
                        side: if seed & 16 == 0 {
                            Side::Buy
                        } else {
                            Side::Sell
                        },
                        limit: Price::new(95 + (seed >> 16) % 14).unwrap(),
                        quantity: Quantity::new(1 + (seed >> 24) % 5).unwrap(),
                        time_in_force: if seed & 32 == 0 {
                            TimeInForce::ImmediateOrCancel
                        } else {
                            TimeInForce::GoodTilCancelled
                        },
                    },
                    &mut events,
                ))
                .unwrap(),
                8 => serde_json::to_value(
                    e.cancel(OrderId(1 + (seed >> 16) % (step as u64 + 1)), &mut events),
                )
                .unwrap(),
                _ => serde_json::to_value(e.compact_terminal_orders((seed >> 24) as usize % 8))
                    .unwrap(),
            };
            events_count += events.len();
            e.check_invariants().unwrap();
            h.update(serde_json::to_vec(&(step, result, events, e.snapshot())).unwrap());
            if step % 17 == 0 {
                e = Engine::restore(
                    cfg.clone(),
                    serde_json::from_slice(&serde_json::to_vec(&e.snapshot()).unwrap()).unwrap(),
                )
                .unwrap();
            }
        }
        let mut events = vec![];
        e.finish(&mut events);
        h.update(serde_json::to_vec(&(events, e.snapshot())).unwrap());
        println!(
            "{}",
            serde_json::json!({"model":model,"steps":20000,"events":events_count,"digest":format!("{:x}",h.finalize()),"state":e.snapshot()})
        );
    }
}
