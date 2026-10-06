use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::journal::{file_hash, hex};
use kaze_quant::paper::PaperError;
use kaze_quant::replay::QuoteReader;
use kaze_quant::research::*;
use kaze_quant::storage::publish_new;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dataset {
    path: PathBuf,
    sha256: String,
    source_manifest: PathBuf,
    config: PathBuf,
    folds: Vec<Fold>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    schema_version: u32,
    #[serde(default)]
    availability: serde_json::Value,
    datasets: Vec<Dataset>,
    candidates: Vec<StrategyConfig>,
    costs: Vec<CostScenario>,
}
struct HashRead<'a, R> {
    r: R,
    h: &'a mut Sha256,
}
impl<R: Read> Read for HashRead<'_, R> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        let n = self.r.read(b)?;
        self.h.update(&b[..n]);
        Ok(n)
    }
}
fn trial(
    d: &Dataset,
    c: &PaperConfig,
    s: &StrategyConfig,
    f: &Fold,
    test: bool,
    cost: &CostScenario,
) -> Result<Evaluation, PaperError> {
    let mut hash = Sha256::new();
    let read = HashRead {
        r: std::fs::File::open(&d.path)?,
        h: &mut hash,
    };
    let quotes = QuoteReader::new(BufReader::new(read))
        .map_err(|e| PaperError(e.to_string()))?
        .map(|q| q.map_err(|e| PaperError(e.to_string())));
    let result = evaluate(c, s, f, test, cost, quotes)?;
    if hex(&hash.finalize()) != d.sha256 {
        return Err("actual consumed dataset hash differs from research plan".into());
    }
    Ok(result)
}
fn run() -> Result<(), PaperError> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        println!(
            "Usage: kaze-research PLAN.json NEW_REPORT.json\nFrozen training grid; walk-forward independent test accounts; exact hashed CSV datasets.\nFirst cost scenario selects parameters; all remaining scenarios evaluate frozen OOS selection."
        );
        return if args.first().is_some_and(|s| s == "--help") {
            Ok(())
        } else {
            Err("two arguments required".into())
        };
    }
    let bytes = std::fs::read(&args[0])?;
    if bytes.len() > 262144 {
        return Err("plan exceeds 256 KiB".into());
    }
    let plan: Plan = serde_json::from_slice(&bytes)?;
    if plan.schema_version != 1
        || plan.datasets.is_empty()
        || plan.datasets.len() > 64
        || plan.candidates.is_empty()
        || plan.candidates.len() > 64
        || plan.costs.is_empty()
        || plan.costs.len() > 16
    {
        return Err("invalid research plan bounds".into());
    }
    let report = Path::new(&args[1]);
    if report.exists() {
        return Err("report already exists".into());
    }
    let mut outputs = Vec::new();
    for d in &plan.datasets {
        let c: PaperConfig = serde_json::from_slice(&std::fs::read(&d.config)?)?;
        if d.folds.is_empty() || d.folds.len() > 32 {
            return Err("invalid fold count".into());
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&d.source_manifest)?)?;
        if manifest["normalized_sha256"] != d.sha256 {
            return Err("source manifest and dataset hash disagree".into());
        }
        let mut last_end = 0;
        for f in &d.folds {
            f.validate()?;
            if f.test_start_ns <= last_end {
                return Err("OOS folds overlap or regress".into());
            }
            last_end = f.test_end_ns;
            let training = plan
                .candidates
                .iter()
                .map(|s| trial(d, &c, s, f, false, &plan.costs[0]))
                .collect::<Result<Vec<_>, _>>()?;
            let selected = choose_training(&training)?;
            let tests = plan
                .costs
                .iter()
                .map(|cost| {
                    trial(d, &c, &plan.candidates[selected], f, true, cost)
                        .map(|r| serde_json::json!({"scenario":cost,"result":r}))
                })
                .collect::<Result<Vec<_>, _>>()?;
            outputs.push(serde_json::json!({"dataset":d.path,"source_manifest":manifest,"config":c,"config_sha256":file_hash(&d.config)?,"fold":f,"training":training,"selected_index":selected,"selected_strategy":plan.candidates[selected],"out_of_sample":tests}));
        }
    }
    let passed = outputs.iter().all(|f| {
        f["out_of_sample"].as_array().unwrap().iter().all(|s| {
            s["result"]["liquidation_adjusted_pnl_minor"]
                .as_str()
                .and_then(|v| v.parse::<i128>().ok())
                .is_some_and(|p| p > 0)
                && s["result"]["state"]["risk_rejections"] == 0
                && s["result"]["state"]["metrics"]["fills"]
                    .as_u64()
                    .is_some_and(|n| n >= 20)
        })
    });
    let result = serde_json::json!({"schema_version":1,"binary_sha256":file_hash(&std::env::current_exe()?)?,"plan_sha256":hex(&Sha256::digest(&bytes)),"availability":plan.availability,"candidates":plan.candidates,"selection":"maximum training liquidation-adjusted PnL; fixed candidate order ties; no OOS optimization","screen_passed":passed,"screen_rule":"every OOS cost scenario positive, >=20 fills, zero risk rejections; necessary exploratory screen only, not profitability certification","folds":outputs,"limits":"L1 quote simulation; no external queue/market impact; estimated exit fee is not executed liquidation; no annualized Sharpe from short prefixes; futures quotes interpreted as unlevered spot-like model"});
    publish_new(report, &serde_json::to_vec_pretty(&result)?)?;
    println!("screen_passed={passed} folds={}", outputs.len());
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
