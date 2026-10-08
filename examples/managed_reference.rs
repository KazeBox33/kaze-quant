//! 不经过 SQLite 的同内核对照：完整逐事件回执摘要，不只比较最终盈亏。
//! 计时包含 CSV 解码、策略、撮合、回执序列化/哈希和热记录回收，不包含全量审计。
use kaze_quant::{
    config::PaperConfig,
    journal::{file_hash, hex},
    paper::{Command, Envelope, Notice, PaperRuntime},
    replay::QuoteReader,
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::BufReader, path::Path, time::Instant};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("managed_reference CONFIG QUOTES".into());
    }
    let c: PaperConfig = serde_json::from_reader(File::open(&args[0])?)?;
    let mut r = PaperRuntime::new(c)?;
    r.configure_retention(64)?;
    let c = r.config().clone();
    let members = if let kaze_quant::config::StrategyConfig::Registered {
        name, parameters, ..
    } = &c.markets[0].strategy
    {
        if name != "managed" {
            return Err("managed config required".into());
        }
        parameters["members"]
            .as_array()
            .ok_or("members absent")?
            .len()
    } else {
        return Err("managed config required".into());
    };
    for owner in 0..members {
        for operation in [
            kaze_quant::managed::Control::Init,
            kaze_quant::managed::Control::Start,
        ] {
            r.process(&Envelope {
                seq: r.processed() + 1,
                command: Command::StrategyControl {
                    market: 0,
                    owner,
                    operation,
                },
            })?;
        }
    }
    let offset = r.processed();
    let input_sha256 = file_hash(Path::new(&args[1]))?;
    let config_sha256 = file_hash(Path::new(&args[0]))?;
    let mut h = Sha256::new();
    let start = Instant::now();
    for q in QuoteReader::new(BufReader::new(File::open(&args[1])?))? {
        let q = q?;
        let mut receipt = r.process(&Envelope {
            seq: q.sequence + offset,
            command: Command::Quote {
                market: 0,
                quote: q,
            },
        })?;
        // Only this benchmark normalizes the explicit initialization prefix and ownership notices.
        // Stored CLI receipts remain exact and are independently audited.
        receipt.seq -= offset;
        receipt
            .notices
            .retain(|n| !matches!(n, Notice::OwnedAction { .. }));
        let bytes = serde_json::to_vec(&receipt)?;
        h.update((bytes.len() as u64).to_le_bytes());
        h.update(bytes);
        r.compact_checkpoint();
    }
    let elapsed_ns = start.elapsed().as_nanos();
    r.check_invariants()?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "state":r.report(), "elapsed_ns":elapsed_ns,
            "input_sha256":input_sha256, "config_file_sha256":config_sha256,
            "receipt_sha256":hex(&h.finalize()), "digest_rule":"init prefix excluded, seq normalized to input quote sequence, OwnedAction omitted; u64 LE receipt length + serde JSON bytes; stored receipts remain unmodified",
            "timing":"CSV decode + shared paper core + receipt serialization/hash + retention; no SQLite; final audit excluded"
        }))?
    );
    Ok(())
}
