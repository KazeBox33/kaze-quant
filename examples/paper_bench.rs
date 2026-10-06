//! 每命令同步持久化的成本实验；单次命令计时，不把批次时间描述为延迟分位数。
use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::journal::DurableSession;
use kaze_quant::paper::{Command, Envelope, PaperRuntime};
use kaze_quant::types::*;
use std::time::Instant;
fn config(commands: usize) -> PaperConfig {
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
    c.markets.truncate(1);
    c.max_commands = commands + 1;
    c.markets[0].strategy = StrategyConfig::Passive {};
    c.markets[0].engine.max_orders = commands + 1;
    c.markets[0].engine.fee_bps = 0;
    c.markets[0].risk.max_drawdown = 1_000_000;
    c
}
fn input(count: usize) -> Vec<Envelope> {
    let mut inputs = Vec::with_capacity(count);
    let mut quote_seq = 0;
    for i in 0..count {
        let command = match i % 4 {
            0 | 2 => {
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
            }
            1 | 3 => Command::Submit {
                market: 0,
                request: OrderRequest {
                    side: if i % 4 == 1 { Side::Buy } else { Side::Sell },
                    limit: Price::new(100).unwrap(),
                    quantity: Quantity::new(1).unwrap(),
                    time_in_force: TimeInForce::ImmediateOrCancel,
                },
            },
            _ => unreachable!(),
        };
        inputs.push(Envelope {
            seq: i as u64 + 1,
            command,
        });
    }
    inputs
}
fn emit(label: &str, run: usize, mut samples: Vec<u128>) {
    samples.sort_unstable();
    let n = samples.len();
    let percentile = |p: usize| samples[(n * p).div_ceil(100).saturating_sub(1)];
    let sum: u128 = samples.iter().sum();
    println!(
        "{label},{run},{n},{sum},{},{},{},{}",
        percentile(50),
        percentile(95),
        percentile(99),
        samples[n - 1]
    );
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if cfg!(debug_assertions) {
        return Err("run with --release".into());
    }
    let count = std::env::args()
        .nth(1)
        .map_or(Ok(2000), |s| s.parse::<usize>())?;
    if !(8..=100_000).contains(&count) {
        return Err("count must be in 8..=100000".into());
    }
    let inputs = input(count);
    println!("mode,run,commands,total_ns,p50_ns,p95_ns,p99_ns,max_ns");
    let dir = std::env::temp_dir().join(format!(
        "kaze-paper-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    ));
    std::fs::create_dir(&dir)?;
    for run in 0..3 {
        let mut reference = PaperRuntime::new(config(count))?;
        let mut samples = Vec::with_capacity(count);
        for c in &inputs {
            let start = Instant::now();
            let receipt = reference.process(c)?;
            std::hint::black_box(receipt);
            samples.push(start.elapsed().as_nanos());
        }
        reference.check_invariants()?;
        emit("memory", run, samples);
        let path = dir.join(format!("run-{run}.wal"));
        let mut session = DurableSession::open(&path, config(count))?;
        let mut samples = Vec::with_capacity(count);
        for c in &inputs {
            let start = Instant::now();
            let receipt = session.execute(c.clone())?;
            std::hint::black_box(receipt);
            samples.push(start.elapsed().as_nanos());
        }
        session.runtime().check_invariants()?;
        assert_eq!(session.runtime().report(), reference.report());
        emit("wal_sync", run, samples);
        drop(session);
        let start = Instant::now();
        let recovered = DurableSession::open(&path, config(count))?;
        let elapsed = start.elapsed().as_nanos();
        assert_eq!(recovered.runtime().report(), reference.report());
        eprintln!(
            "recovery run={run} commands={count} elapsed_ns={elapsed} (includes binary hash + full replay)"
        );
    }
    std::fs::remove_dir_all(dir)?;
    Ok(())
}
