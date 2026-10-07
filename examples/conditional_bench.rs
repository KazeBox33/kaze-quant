//! 稀疏不触发与密集候选选择；Scan与Indexed共用容器，隔离算法差异，不含交易。
use kaze_quant::{conditional::*, types::*};
use std::{hint::black_box, time::Instant};
fn main() {
    let a: Vec<_> = std::env::args().collect();
    let policy = match a[1].as_str() {
        "indexed" => TriggerPolicy::Indexed,
        "adaptive" => TriggerPolicy::Adaptive,
        "scan" => TriggerPolicy::Scan,
        _ => panic!("expected indexed, adaptive or scan"),
    };
    let n: usize = a[2].parse().unwrap();
    let dense = a[3] == "dense";
    let loops: usize = a[4].parse().unwrap();
    let q = Quote {
        sequence: 2,
        timestamp_ns: 20,
        bid: Price::new(100).unwrap(),
        ask: Price::new(101).unwrap(),
        bid_quantity: 1,
        ask_quantity: 1,
    };
    let mut b = ConditionalBook::new(n).unwrap();
    for i in 0..n {
        let above = i % 2 == 0;
        let t = if above {
            if dense { 90 } else { 110 }
        } else if dense {
            110
        } else {
            90
        };
        b.submit(
            ConditionalRequest {
                reference: Reference::Bid,
                direction: if above {
                    Direction::AboveOrEqual
                } else {
                    Direction::BelowOrEqual
                },
                trigger: Price::new(t + i as u64 % 10).unwrap(),
                order: OrderRequest {
                    side: Side::Buy,
                    limit: Price::new(120).unwrap(),
                    quantity: Quantity::new(1).unwrap(),
                    time_in_force: TimeInForce::GoodTilCancelled,
                },
                expires_at_ns: None,
                oco_group: None,
            },
            Quote {
                sequence: 1,
                timestamp_ns: 10,
                ..q
            },
            10,
        )
        .unwrap();
    }
    let start = Instant::now();
    let mut checksum = 0u64;
    let mut selected = 0usize;
    let mut examined = 0usize;
    for _ in 0..loops {
        let s = black_box(&b).select(black_box(q), policy);
        selected += s.ids.len();
        examined += s.examined;
        checksum = checksum.wrapping_add(s.ids.iter().map(|id| id.0).sum::<u64>());
        black_box(&s);
    }
    let elapsed = start.elapsed().as_nanos();
    b.check_invariants().unwrap();
    println!(
        "{}",
        serde_json::json!({"policy":a[1],"mode":a[3],"orders":n,"loops":loops,"elapsed_ns":elapsed,"selected":selected,"examined":examined,"checksum":checksum})
    );
}
