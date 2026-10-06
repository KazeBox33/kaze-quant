//! cargo run --release --example durable_strategy -- reports/pulse.db
//! 同一二进制重开可去重；策略计数器与账本在同一事务提交。
#[path = "support/pulse.rs"]
mod pulse;
use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::paper::{Command, Envelope};
use kaze_quant::store::{SqliteSession, StoreOptions};
use kaze_quant::types::*;
use std::path::PathBuf;
use std::sync::Arc;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(std::env::args().nth(1).unwrap_or("reports/pulse.db".into()));
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json"))?;
    c.markets.truncate(1);
    c.markets[0].strategy = StrategyConfig::Registered {
        name: "pulse".into(),
        version: 1,
        parameters: serde_json::json!({"period":4,"quantity":1}),
    };
    let registry = Arc::new(pulse::registry());
    let mut s = SqliteSession::open_with_registry(
        &path,
        c.clone(),
        StoreOptions::default(),
        registry.clone(),
    )?;
    for seq in 1..=20 {
        s.execute_batch(&[Envelope {
            seq,
            command: Command::Quote {
                market: 0,
                quote: Quote {
                    sequence: seq,
                    timestamp_ns: seq * 1000000,
                    bid: Price::new(99)?,
                    ask: Price::new(100)?,
                    bid_quantity: 10,
                    ask_quantity: 10,
                },
            },
        }])?;
    }
    let report = s.runtime().report();
    drop(s);
    let s = SqliteSession::open_with_registry(&path, c, StoreOptions::default(), registry)?;
    assert_eq!(s.runtime().report(), report);
    assert_eq!(s.verify_full()?, 20);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
