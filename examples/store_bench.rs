//! 磁盘确认按实际事务计时；batch 延迟不能解释成单事件延迟。
use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::journal::DurableSession;
use kaze_quant::paper::{Command, Envelope, PaperRuntime};
use kaze_quant::store::{SqliteSession, StoreOptions};
use kaze_quant::types::*;
use std::time::Instant;
fn config(count: usize) -> PaperConfig {
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
    c.markets.truncate(1);
    c.max_commands = count + 1;
    c.markets[0].strategy = StrategyConfig::Passive {};
    c.markets[0].engine.max_orders = (count + 1).min(1_000_000);
    c.markets[0].engine.fee_bps = 0;
    c.markets[0].risk.max_drawdown = 1_000_000;
    c
}
fn inputs(count: usize) -> Vec<Envelope> {
    let mut quote_seq = 0;
    (0..count)
        .map(|i| {
            let command = if i.is_multiple_of(2) {
                quote_seq += 1;
                Command::Quote {
                    market: 0,
                    quote: Quote {
                        sequence: quote_seq,
                        timestamp_ns: i as u64,
                        bid: Price::new(100).unwrap(),
                        ask: Price::new(100).unwrap(),
                        bid_quantity: 1,
                        ask_quantity: 1,
                    },
                }
            } else {
                Command::Submit {
                    market: 0,
                    request: OrderRequest {
                        side: if i % 4 == 1 { Side::Buy } else { Side::Sell },
                        limit: Price::new(100).unwrap(),
                        quantity: Quantity::new(1).unwrap(),
                        time_in_force: TimeInForce::ImmediateOrCancel,
                    },
                }
            };
            Envelope {
                seq: i as u64 + 1,
                command,
            }
        })
        .collect()
}
fn emit(
    mode: &str,
    run: usize,
    count: usize,
    batch: usize,
    elapsed: u128,
    samples: &mut [u128],
    recovery_audit: (u128, u128),
) {
    let (recovery, audit) = recovery_audit;
    samples.sort_unstable();
    let n = samples.len();
    let p = |p: usize| samples[(n * p).div_ceil(100).saturating_sub(1)];
    println!(
        "{mode},{run},{count},{batch},{n},{elapsed},{},{},{},{},{recovery},{audit}",
        p(50),
        p(95),
        p(99),
        samples[n - 1]
    );
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        return Err("use --release".into());
    }
    let count = std::env::args()
        .nth(1)
        .map_or(Ok(2000), |s| s.parse::<usize>())?;
    if !(8..=500_000).contains(&count) {
        return Err("count 8..500000".into());
    }
    let data = inputs(count);
    let cfg = config(count);
    let dir = std::env::temp_dir().join(format!("kaze-store-bench-{}", std::process::id()));
    std::fs::create_dir(&dir)?;
    println!(
        "mode,run,commands,batch_size,samples,total_ns,commit_p50_ns,commit_p95_ns,commit_p99_ns,commit_max_ns,recovery_ns,audit_ns"
    );
    for run in 0..3 {
        let mut reference = PaperRuntime::new(cfg.clone())?;
        let t = Instant::now();
        for c in &data {
            std::hint::black_box(reference.process(c)?);
        }
        let total = t.elapsed().as_nanos();
        reference.check_invariants()?;
        println!("memory,{run},{count},0,0,{total},0,0,0,0,0,0");
        if count <= 2000 {
            let path = dir.join(format!("{run}.wal"));
            let mut s = DurableSession::open(&path, cfg.clone())?;
            let mut samples = Vec::new();
            let t = Instant::now();
            for c in &data {
                let a = Instant::now();
                std::hint::black_box(s.execute(c.clone())?);
                samples.push(a.elapsed().as_nanos())
            }
            let elapsed = t.elapsed().as_nanos();
            assert_eq!(s.runtime().report(), reference.report());
            drop(s);
            let a = Instant::now();
            let s = DurableSession::open(&path, cfg.clone())?;
            let recovery = a.elapsed().as_nanos();
            assert_eq!(s.runtime().report(), reference.report());
            emit(
                "file_wal_sync",
                run,
                count,
                1,
                elapsed,
                &mut samples,
                (recovery, 0),
            );
            drop(s);
            std::fs::remove_file(path)?;
        }
        for batch in [1, 64, 256] {
            if count > 2000 && batch == 1 {
                continue;
            }
            let path = dir.join(format!("{run}-{batch}.db"));
            let mut s = SqliteSession::open(&path, cfg.clone(), StoreOptions::default())?;
            let mut samples = Vec::new();
            let t = Instant::now();
            for chunk in data.chunks(batch) {
                let a = Instant::now();
                std::hint::black_box(s.execute_batch(chunk)?);
                samples.push(a.elapsed().as_nanos())
            }
            let elapsed = t.elapsed().as_nanos();
            assert_eq!(s.runtime().report(), reference.report());
            drop(s);
            let a = Instant::now();
            let s = SqliteSession::open(&path, cfg.clone(), StoreOptions::default())?;
            let recovery = a.elapsed().as_nanos();
            assert_eq!(s.runtime().report(), reference.report());
            let a = Instant::now();
            assert_eq!(s.verify_full()?, count as u64);
            let audit = a.elapsed().as_nanos();
            emit(
                "sqlite_full",
                run,
                count,
                batch,
                elapsed,
                &mut samples,
                (recovery, audit),
            );
            drop(s);
            std::fs::remove_file(path)?;
        }
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}
