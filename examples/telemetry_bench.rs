//! release测量固定内存统计的独立成本，不将其冒充完整引擎/网络基准。
use kaze_quant::telemetry::LatencyHistogram;
use std::hint::black_box;
use std::time::Instant;
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("run this benchmark with --release");
        std::process::exit(2);
    }
    let count: usize = std::env::args()
        .nth(1)
        .map(|s| s.parse().expect("integer count"))
        .unwrap_or(1_000_000);
    assert!((100_000..=10_000_000).contains(&count));
    // 在计时之外生成输入。包含常态和长尾；不依赖网络、策略或磁盘。
    let input: Vec<u64> = (0..count)
        .map(|i| {
            if i % 250 == 0 {
                1_000_000_000
            } else {
                10_000 + (i as u64 * 7919) % 10_000_000
            }
        })
        .collect();
    let expected_sum: u128 = input.iter().map(|v| *v as u128).sum();
    let mut records = Vec::new();
    for round in 0..8 {
        let mut hist = LatencyHistogram::default();
        let started = Instant::now();
        for &value in black_box(&input) {
            hist.record(black_box(value)).unwrap();
        }
        let elapsed_ns = started.elapsed().as_nanos();
        assert_eq!(hist.samples(), count as u64);
        assert_eq!(hist.report()["sum_ns"], expected_sum.to_string());
        black_box(&hist);
        if round > 0 {
            records.push(serde_json::json!({"round":round,"observations":count,"elapsed_ns":elapsed_ns.to_string(),"ns_per_observation":elapsed_ns as f64/count as f64}));
        }
    }
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"mode":"scalar-latency-histogram-component","collector_bytes":std::mem::size_of::<LatencyHistogram>(),"rounds":records,"conditions":"release, one warmup, seven measured rounds, preloaded deterministic input, no affinity, background jobs may exist; report query outside timing; excludes pipeline timestamp reads/other collectors/strategy/network/durability"})).unwrap());
}
