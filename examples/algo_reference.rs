//! 固定无成交报价上的BestLimit原始下单/已确认撤单意图摘要；包含本平台完整纸面路径。
use kaze_quant::{
    config::PaperConfig,
    journal::hex,
    paper::{Command, Envelope, Notice, PaperRuntime},
    replay::QuoteReader,
    target::DecisionReason,
    types::*,
};
use sha2::{Digest, Sha256};
use std::{fs::File, io::BufReader, time::Instant};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("algo_reference CONFIG QUOTES".into());
    }
    let c: PaperConfig = serde_json::from_reader(File::open(&args[0])?)?;
    let mut r = PaperRuntime::new(c)?;
    r.configure_retention(64)?;
    let mut hash = Sha256::new();
    let mut actions = 0u64;
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
        for notice in &receipt.notices {
            if let Notice::StrategyDecision { decision, .. } = notice {
                let tuple = match decision.action {
                    Action::Submit(req) if decision.reason == DecisionReason::Accepted => Some((
                        1u8,
                        decision.order_id.ok_or("accepted ID missing")?.0,
                        if req.side == Side::Buy { 1u8 } else { 2u8 },
                        req.limit.units(),
                        req.quantity.units(),
                    )),
                    Action::Cancel(id) => {
                        if !receipt.notices.iter().any(|n|matches!(n,Notice::Engine{event:Event::Cancelled{order_id,..},..} if *order_id==id)){return Err("cancel not confirmed".into());}
                        Some((2u8, id.0, 0u8, 0u64, 0u64))
                    }
                    _ => None,
                };
                if let Some((kind, id, side, price, qty)) = tuple {
                    hash.update([kind]);
                    hash.update(q.sequence.to_le_bytes());
                    hash.update(id.to_le_bytes());
                    hash.update([side]);
                    hash.update(price.to_le_bytes());
                    hash.update(qty.to_le_bytes());
                    actions += 1;
                }
            }
        }
        r.compact_checkpoint();
    }
    let elapsed_ns = start.elapsed().as_nanos();
    r.check_invariants()?;
    println!(
        "{}",
        serde_json::json!({"elapsed_ns":elapsed_ns,"actions":actions,"trace_sha256":hex(&hash.finalize()),"state":r.report(),"scope":"CSV+full paper planning/risk/core/retention and accepted-submit/confirmed-cancel digest; no SQLite or event receipt serialization; zero-liquidity no-fill comparison trace"})
    );
    Ok(())
}
