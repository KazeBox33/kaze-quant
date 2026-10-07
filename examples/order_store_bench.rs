//! 固定合成负载：新旧二进制执行同一程序；准备/审计不计时，结果不能外推实盘。
use kaze_quant::{
    engine::{Engine, EngineConfig},
    types::*,
};
use std::{hint::black_box, time::Instant};
fn quote(n: u64) -> Quote {
    Quote {
        sequence: n,
        timestamp_ns: n,
        bid: Price::new(100).unwrap(),
        ask: Price::new(101).unwrap(),
        bid_quantity: 0,
        ask_quantity: 0,
    }
}
fn request() -> OrderRequest {
    OrderRequest {
        side: Side::Buy,
        limit: Price::new(99).unwrap(),
        quantity: Quantity::new(1).unwrap(),
        time_in_force: TimeInForce::GoodTilCancelled,
    }
}
fn main() {
    let a: Vec<_> = std::env::args().collect();
    let mode = &a[1];
    let n: usize = a[2].parse().unwrap();
    let mut e = Engine::new(EngineConfig {
        initial_cash: 1_000_000_000,
        max_position: 1_000_000,
        max_active_orders: n + 1,
        max_orders: n + 2,
        fee_bps: 0,
        ..EngineConfig::default()
    })
    .unwrap();
    e.on_quote(quote(1), &mut ()).unwrap();
    let ids: Vec<_> = (0..n)
        .map(|_| e.submit(request(), &mut ()).unwrap())
        .collect();
    let operations = match mode.as_str() {
        "cancel" => n,
        "churn" => 10000,
        "lookup" => 1_000_000,
        "quote" => 1000,
        "snapshot" => 100,
        _ => panic!("mode"),
    };
    let start = Instant::now();
    let mut checksum = 0u64;
    match mode.as_str() {
        "cancel" => {
            for &id in ids.iter().rev() {
                checksum += u64::from(e.cancel(id, &mut ()));
            }
        }
        "churn" => {
            for _ in 0..operations {
                let id = e.submit(request(), &mut ()).unwrap();
                assert!(e.cancel(id, &mut ()));
                checksum += e.compact_terminal_orders(0) as u64;
            }
        }
        "lookup" => {
            for i in 0..operations {
                checksum += black_box(e.order(black_box(ids[(i * 7919) % n])))
                    .unwrap()
                    .id
                    .0;
            }
        }
        "quote" => {
            for i in 0..operations {
                e.on_quote(quote(i as u64 + 2), &mut ()).unwrap();
            }
            checksum = e.metrics().orders_examined;
        }
        "snapshot" => {
            for _ in 0..operations {
                checksum += black_box(serde_json::to_vec(&e.snapshot()).unwrap()).len() as u64;
            }
        }
        _ => unreachable!(),
    }
    let elapsed = start.elapsed().as_nanos();
    e.check_invariants().unwrap();
    println!(
        "{}",
        serde_json::json!({"mode":mode,"orders":n,"operations":operations,"elapsed_ns":elapsed,"checksum":checksum,"state":e.snapshot()})
    );
}
