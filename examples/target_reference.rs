//! 不经过 SQLite 的同内核对照：完整逐事件回执摘要，不只比较最终盈亏。
//! 计时包含 CSV 解码、策略、撮合、回执序列化/哈希和热记录回收，不包含全量审计。
use kaze_quant::{
    config::PaperConfig,
    journal::{file_hash, hex},
    paper::{Command, Envelope, PaperRuntime},
    replay::QuoteReader,
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::BufReader, path::Path, time::Instant};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("target_reference CONFIG QUOTES".into());
    }
    let c: PaperConfig = serde_json::from_reader(File::open(&args[0])?)?;
    let mut r = PaperRuntime::new(c)?;
    r.configure_retention(64)?;
    let input_sha256 = file_hash(Path::new(&args[1]))?;
    let config_sha256 = file_hash(Path::new(&args[0]))?;
    let mut h = Sha256::new();
    let start = Instant::now();
    for q in QuoteReader::new(BufReader::new(File::open(&args[1])?))? {
        let q = q?;
        let receipt = r.process(&Envelope {
            seq: q.sequence,
            command: Command::Quote {
                market: 0,
                quote: q,
            },
        })?;
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
            "receipt_sha256":hex(&h.finalize()), "digest_rule":"u64 LE receipt length + serde JSON bytes in sequence order",
            "timing":"CSV decode + shared paper core + receipt serialization/hash + retention; no SQLite; final audit excluded"
        }))?
    );
    Ok(())
}
