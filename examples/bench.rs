//! cargo run --release --example bench -- 7 > reports/benchmark.csv
//! 比较算法工作量，计时范围不含引擎/窗口创建、I/O、结果审计。
use kaze_quant::engine::{Engine, EngineConfig, ScanPolicy};
use kaze_quant::strategy::RollingMean;
use kaze_quant::types::*;
use std::hint::black_box;
use std::time::Instant;

fn request(side: Side) -> OrderRequest {
    OrderRequest {
        side,
        limit: Price::new(10_000).unwrap(),
        quantity: Quantity::new(1).unwrap(),
        time_in_force: TimeInForce::ImmediateOrCancel,
    }
}
fn quote(seq: u64) -> Quote {
    Quote {
        sequence: seq,
        timestamp_ns: seq * 100,
        bid: Price::new(10_000).unwrap(),
        ask: Price::new(10_000).unwrap(),
        bid_quantity: 1,
        ask_quantity: 1,
    }
}
fn churn(n: usize, policy: ScanPolicy) -> (u128, u64, i128) {
    let config = EngineConfig {
        fee_bps: 0,
        max_position: 1,
        max_active_orders: 1,
        max_orders: n,
        ..EngineConfig::default()
    };
    let mut engine = Engine::with_scan_policy(config, policy).unwrap();
    let start = Instant::now();
    for index in 1..=n {
        engine
            .on_quote(black_box(quote(index as u64)), &mut ())
            .unwrap();
        let side = if engine.account().position() == 0 {
            Side::Buy
        } else {
            Side::Sell
        };
        engine.submit(black_box(request(side)), &mut ()).unwrap();
    }
    engine.finish(&mut ());
    let elapsed = start.elapsed().as_nanos();
    engine.check_invariants().unwrap();
    (
        elapsed,
        engine.metrics().orders_examined,
        black_box(engine.equity()),
    )
}

fn all_active(n: usize, policy: ScanPolicy) -> (u128, u64, i128) {
    let config = EngineConfig {
        fee_bps: 0,
        max_position: n as u64,
        max_active_orders: n,
        max_orders: n,
        ..EngineConfig::default()
    };
    let mut engine = Engine::with_scan_policy(config, policy).unwrap();
    engine.on_quote(quote(1), &mut ()).unwrap();
    let mut r = request(Side::Buy);
    r.limit = Price::new(1).unwrap();
    r.time_in_force = TimeInForce::GoodTilCancelled;
    for _ in 0..n {
        engine.submit(r, &mut ()).unwrap();
    }
    let start = Instant::now();
    for seq in 2..=10_001 {
        engine.on_quote(black_box(quote(seq)), &mut ()).unwrap();
    }
    let elapsed = start.elapsed().as_nanos();
    engine.check_invariants().unwrap();
    (
        elapsed,
        engine.metrics().orders_examined,
        black_box(engine.equity()),
    )
}

fn means(values: &[Price], window: usize, incremental: bool) -> (u128, u128) {
    let mut ring = RollingMean::new(window).unwrap();
    let mut checksum = 0u128;
    let start = Instant::now();
    if incremental {
        for &value in values {
            if let Some(mean) = ring.push(black_box(value)) {
                checksum += u128::from(mean);
            }
        }
    } else {
        for index in window..=values.len() {
            let sum: u128 = black_box(&values[index - window..index])
                .iter()
                .map(|p| u128::from(p.units()))
                .sum();
            checksum += sum / window as u128;
        }
    }
    (start.elapsed().as_nanos(), black_box(checksum))
}

fn row(workload: &str, n: usize, variant: &str, samples: &mut [u128], work: u64, checksum: i128) {
    samples.sort_unstable();
    println!(
        "{workload},{n},{variant},{},{},{},{},{work},{checksum}",
        samples.len(),
        samples[0],
        samples[samples.len() / 2],
        samples[samples.len() - 1]
    );
}

fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run benchmarks with --release; debug includes full-history invariant audits");
        std::process::exit(1);
    }
    let repeats: usize = std::env::args()
        .nth(1)
        .map_or(Ok(7), |s| s.parse())
        .expect("integer repeats");
    assert!((3..=31).contains(&repeats), "repeats must be 3..=31");
    println!(
        "workload,size,variant,samples,min_batch_ns,median_batch_ns,max_batch_ns,orders_examined,checksum"
    );
    for n in [1_000, 5_000, 20_000] {
        let mut active_samples = Vec::new();
        let mut history_samples = Vec::new();
        let warm_active = churn(n, ScanPolicy::Active);
        let warm_history = churn(n, ScanPolicy::History);
        assert_eq!(warm_active.2, warm_history.2);
        for repeat in 0..repeats {
            // 交替顺序减少固定顺序造成的温度/频率偏差。
            let (active, history) = if repeat % 2 == 0 {
                (churn(n, ScanPolicy::Active), churn(n, ScanPolicy::History))
            } else {
                let h = churn(n, ScanPolicy::History);
                (churn(n, ScanPolicy::Active), h)
            };
            assert_eq!(active.2, history.2);
            active_samples.push(active.0);
            history_samples.push(history.0);
        }
        row(
            "order_churn",
            n,
            "active",
            &mut active_samples,
            warm_active.1,
            warm_active.2,
        );
        row(
            "order_churn",
            n,
            "history",
            &mut history_samples,
            warm_history.1,
            warm_history.2,
        );
    }
    let mut active_samples = Vec::new();
    let mut history_samples = Vec::new();
    let a = all_active(1_000, ScanPolicy::Active);
    let h = all_active(1_000, ScanPolicy::History);
    assert_eq!(a.2, h.2);
    for repeat in 0..repeats {
        let (a, h) = if repeat % 2 == 0 {
            (
                all_active(1_000, ScanPolicy::Active),
                all_active(1_000, ScanPolicy::History),
            )
        } else {
            let h = all_active(1_000, ScanPolicy::History);
            (all_active(1_000, ScanPolicy::Active), h)
        };
        active_samples.push(a.0);
        history_samples.push(h.0);
    }
    row(
        "all_active_10000_quotes",
        1_000,
        "active",
        &mut active_samples,
        a.1,
        a.2,
    );
    row(
        "all_active_10000_quotes",
        1_000,
        "history",
        &mut history_samples,
        h.1,
        h.2,
    );
    let values: Vec<Price> = (0..200_000)
        .map(|i| Price::new(10_000 + (i * 17 % 200) as u64).unwrap())
        .collect();
    for window in [16, 256] {
        let a = means(&values, window, true);
        let h = means(&values, window, false);
        assert_eq!(a.1, h.1);
        let mut fast = Vec::new();
        let mut slow = Vec::new();
        for repeat in 0..repeats {
            let (a, h) = if repeat % 2 == 0 {
                (means(&values, window, true), means(&values, window, false))
            } else {
                let h = means(&values, window, false);
                (means(&values, window, true), h)
            };
            assert_eq!(a.1, h.1);
            fast.push(a.0);
            slow.push(h.0);
        }
        row(
            "rolling_mean_200000_updates",
            window,
            "incremental",
            &mut fast,
            0,
            a.1 as i128,
        );
        row(
            "rolling_mean_200000_updates",
            window,
            "resum",
            &mut slow,
            0,
            h.1 as i128,
        );
    }
}
