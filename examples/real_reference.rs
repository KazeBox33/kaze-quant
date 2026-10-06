//! 独立于存储层的逐报价内存对照；共用执行内核，不能替代手算交易语义测试。
use kaze_quant::config::PaperConfig;
use kaze_quant::paper::{Command, Envelope, PaperRuntime};
use kaze_quant::replay::QuoteReader;
use std::fs::File;
use std::io::BufReader;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("real_reference CONFIG QUOTES".into());
    }
    let mut c: PaperConfig = serde_json::from_reader(File::open(&args[0])?)?;
    c.max_commands = 1_000_000;
    let mut r = PaperRuntime::new(c)?;
    r.configure_retention(64)?;
    let reader = QuoteReader::new(BufReader::new(File::open(&args[1])?))?;
    for q in reader {
        let q = q?;
        r.process(&Envelope {
            seq: q.sequence,
            command: Command::Quote {
                market: 0,
                quote: q,
            },
        })?;
        r.compact_checkpoint();
    }
    r.check_invariants()?;
    println!("{}", serde_json::to_string_pretty(&r.report())?);
    Ok(())
}
