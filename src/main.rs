use kaze_quant::engine::{Engine, EngineConfig};
use kaze_quant::replay::{QuoteReader, TraceWriter, replay};
use kaze_quant::strategy::{MomentumStrategy, Strategy, StrategyView, ThresholdStrategy};
use kaze_quant::types::*;
use kaze_quant::{journal, storage};
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const HELP: &str = "Rust 量化引擎学习实验室（本地模拟）
用法: kaze-quant [--input PATH] [--output DIR]
       [--strategy threshold|momentum] [--latency-ns N] [--fee-bps N]
默认: data/demo.csv, threshold, latency=0, fee=10 bps
价格/金额单位为分，数量为整数资产单位，timestamp_ns 为模拟时间。
输出目录需没有已完成的 summary.json / events.csv；失败文件以 .partial 标记。";

struct Options {
    input: PathBuf,
    output: PathBuf,
    strategy: String,
    config: EngineConfig,
}
enum SelectedStrategy {
    Threshold(ThresholdStrategy),
    Momentum(MomentumStrategy),
}
impl Strategy for SelectedStrategy {
    fn on_quote(&mut self, view: StrategyView<'_>) -> Action {
        match self {
            Self::Threshold(s) => s.on_quote(view),
            Self::Momentum(s) => s.on_quote(view),
        }
    }
}

fn options() -> Result<Option<Options>, String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let mut o = Options {
        input: "data/demo.csv".into(),
        output: format!("reports/run-{stamp}").into(),
        strategy: "threshold".into(),
        config: EngineConfig::default(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!("{HELP}");
            return Ok(None);
        }
        if !matches!(
            arg.as_str(),
            "--input" | "--output" | "--strategy" | "--latency-ns" | "--fee-bps"
        ) {
            return Err(format!("unknown argument: {arg}\n{HELP}"));
        }
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {arg}"))?;
        match arg.as_str() {
            "--input" => o.input = value.into(),
            "--output" => o.output = value.into(),
            "--strategy" => o.strategy = value,
            "--latency-ns" => {
                o.config.latency_ns = value.parse().map_err(|_| "invalid latency-ns")?
            }
            "--fee-bps" => o.config.fee_bps = value.parse().map_err(|_| "invalid fee-bps")?,
            _ => unreachable!(),
        }
    }
    if !matches!(o.strategy.as_str(), "threshold" | "momentum") {
        return Err("strategy must be threshold or momentum".into());
    }
    o.config.validate().map_err(str::to_owned)?;
    Ok(Some(o))
}

fn run(o: Options) -> Result<(), Box<dyn std::error::Error>> {
    let mut input_hasher = sha2::Sha256::default();
    let reader = QuoteReader::new(BufReader::new(HashReader {
        reader: File::open(&o.input)?,
        hasher: &mut input_hasher,
    }))?;
    let mut engine = Engine::new(o.config.clone())?;
    let mut strategy = match o.strategy.as_str() {
        "threshold" => SelectedStrategy::Threshold(ThresholdStrategy {
            buy_below: Price::new(10_000)?,
            sell_above: Price::new(10_500)?,
            quantity: Quantity::new(10)?,
        }),
        _ => SelectedStrategy::Momentum(MomentumStrategy::new(5, Quantity::new(10)?)?),
    };
    fs::create_dir_all(&o.output)?;
    let _writer_lock = storage::lock_file(&o.output.join(".writer.lock"))?;
    if o.output.join("summary.json").exists() || o.output.join("events.csv").exists() {
        return Err(
            "output already contains completed results; choose a fresh --output directory".into(),
        );
    }
    let partial_trace = o.output.join("events.csv.partial");
    let trace_file = File::create_new(&partial_trace)?;
    let mut trace = TraceWriter::new(BufWriter::new(&trace_file))?;
    let outcome = replay(reader, &mut engine, &mut strategy, &mut trace)?;
    trace.finish()?;
    trace_file.sync_all()?;
    engine.check_invariants()?;
    let metrics = engine.metrics();
    let account = engine.account();
    let pnl = engine.equity() - o.config.initial_cash;
    let summary = format!(
        concat!(
            "{{\n  \"schema_version\": 1,\n  \"strategy\": \"{}\",\n",
            "  \"initial_cash_minor\": {},\n  \"fee_bps\": {},\n  \"latency_ns\": {},\n",
            "  \"quotes\": {},\n  \"orders_accepted\": {},\n  \"orders_rejected\": {},\n",
            "  \"fills\": {},\n  \"orders_cancelled\": {},\n  \"cash_minor\": {},\n",
            "  \"position_units\": {},\n  \"equity_minor\": {},\n  \"pnl_minor\": {},\n",
            "  \"fees_minor\": {},\n  \"max_drawdown_minor\": {},\n  \"orders_examined\": {},\n",
            "  \"strategy_rejections\": {},\n  \"failed_cancels\": {}\n}}\n"
        ),
        o.strategy,
        o.config.initial_cash,
        o.config.fee_bps,
        o.config.latency_ns,
        metrics.quotes,
        metrics.accepted,
        metrics.rejected,
        metrics.fills,
        metrics.cancelled,
        account.cash(),
        account.position(),
        engine.equity(),
        pnl,
        account.fees_paid(),
        metrics.max_drawdown,
        metrics.orders_examined,
        outcome.strategy_rejections,
        outcome.failed_cancels
    );
    let manifest = serde_json::json!({
        "schema_version": 1, "package_version": env!("CARGO_PKG_VERSION"),
        "binary_sha256": journal::file_hash(&std::env::current_exe()?)?,
        "input_sha256": journal::hex(&sha2::Digest::finalize(input_hasher)),
        "engine_config": o.config,
        "strategy": o.strategy,
        "strategy_parameters": match o.strategy.as_str() {
            "threshold" => serde_json::json!({"buy_below": 10000, "sell_above": 10500, "quantity": 10}),
            _ => serde_json::json!({"window": 5, "quantity": 10})
        }
    });
    storage::publish_new(
        &o.output.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    let partial_summary = o.output.join("summary.json.partial");
    {
        let mut file = File::create_new(&partial_summary)?;
        file.write_all(summary.as_bytes())?;
        file.sync_all()?;
    }
    fs::hard_link(&partial_trace, o.output.join("events.csv"))?;
    storage::sync_parent(&partial_trace)?;
    // summary.json 最后发布且不可覆盖；中断时没有完成标记。
    fs::hard_link(&partial_summary, o.output.join("summary.json"))?;
    storage::sync_parent(&partial_summary)?;
    fs::remove_file(partial_trace)?;
    fs::remove_file(partial_summary)?;
    storage::sync_parent(&o.output.join("summary.json"))?;
    println!(
        "回放完成: {} 条报价，{} 张接受订单，{} 次成交",
        metrics.quotes, metrics.accepted, metrics.fills
    );
    println!(
        "现金={} 分，持仓={}，权益={} 分，盈亏={} 分，手续费={} 分",
        account.cash(),
        account.position(),
        engine.equity(),
        pnl,
        account.fees_paid()
    );
    println!("结果: {}", o.output.display());
    Ok(())
}

fn main() {
    let result = match options() {
        Ok(Some(o)) => run(o),
        Ok(None) => Ok(()),
        Err(e) => Err(e.into()),
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

struct HashReader<'a, R> {
    reader: R,
    hasher: &'a mut sha2::Sha256,
}
impl<R: Read> Read for HashReader<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(buf)?;
        sha2::Digest::update(self.hasher, &buf[..n]);
        Ok(n)
    }
}
