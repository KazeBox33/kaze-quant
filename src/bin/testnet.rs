use kaze_quant::binance::BinanceTestnet;
use kaze_quant::decimal;
use kaze_quant::execution::{ExecutionJournal, OrderIntent};
use kaze_quant::paper::PaperError;
use std::path::Path;
fn run() -> Result<(), PaperError> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--help") || args.is_empty() {
        println!(
            "Binance Spot TESTNET only; never real-money endpoint.\nUsage: kaze-testnet JOURNAL submit INTENT.json MAX_USDT\n       kaze-testnet JOURNAL reconcile\n       kaze-testnet JOURNAL cancel CLIENT_ID\n       kaze-testnet JOURNAL audit\nCredentials: KAZE_BINANCE_TESTNET_KEY / KAZE_BINANCE_TESTNET_SECRET (local env only).\nUncertain submissions are never resent, even if query returns not-found."
        );
        return Ok(());
    }
    if args.len() < 2 {
        return Err("journal and action required".into());
    }
    let mut journal = ExecutionJournal::open(Path::new(&args[0]))?;
    if args[1] == "audit" && args.len() == 2 {
        println!("{}", serde_json::to_string_pretty(&journal.audit()?)?);
        return Ok(());
    }
    let mut venue = BinanceTestnet::from_env()?;
    let result = match args[1].as_str() {
        "submit" if args.len() == 4 => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])?
                .take(8193)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8192 {
                return Err("intent exceeds 8 KiB".into());
            }
            let intent: OrderIntent = serde_json::from_slice(&bytes)?;
            let cap = decimal::parse(&args[3])?;
            if cap > 100 * decimal::SCALE {
                return Err("testnet pilot cap must be <=100 USDT per order".into());
            }
            journal.submit_once(&mut venue, &intent, cap)
        }
        "reconcile" if args.len() == 2 => journal.reconcile(&mut venue),
        "cancel" if args.len() == 3 => journal.cancel_once(&mut venue, &args[2]),
        _ => Err("invalid action/arguments".into()),
    };
    println!("{}", serde_json::to_string_pretty(&journal.audit()?)?);
    result
}
fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
