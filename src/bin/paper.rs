use kaze_quant::config::PaperConfig;
use kaze_quant::journal::{DurableSession, hex};
use kaze_quant::paper::{Command, Envelope, PaperError};
use kaze_quant::storage;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

const HELP: &str = "KazeQuant durable paper runtime (no live exchange orders)
Usage: kaze-paper [--config configs/paper.json] [--journal reports/paper.wal]
                  [--input PATH|-] [--report NEW_PATH] [--recover-only] [--finish]
Input: bounded JSONL Envelope {seq,command}; seq starts at 1 and is contiguous.
Each receipt is printed after the command is journaled and fsynced.
EOF keeps the session open; --finish cancels resting orders and seals it.
Recovery requires the same binary and canonical config. Retry identical seq/payload
for deduplication; conflicting duplicates, gaps, and corruption fail closed.";
struct Options {
    config: PathBuf,
    journal: PathBuf,
    input: String,
    report: Option<PathBuf>,
    recover_only: bool,
    finish: bool,
}
fn options() -> Result<Option<Options>, PaperError> {
    let mut o = Options {
        config: "configs/paper.json".into(),
        journal: "reports/paper.wal".into(),
        input: "-".into(),
        report: None,
        recover_only: false,
        finish: false,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(None);
            }
            "--recover-only" => o.recover_only = true,
            "--finish" => o.finish = true,
            "--config" | "--journal" | "--input" | "--report" => {
                let value = args.next().ok_or("missing argument value")?;
                match arg.as_str() {
                    "--config" => o.config = value.into(),
                    "--journal" => o.journal = value.into(),
                    "--input" => o.input = value,
                    _ => o.report = Some(value.into()),
                }
            }
            _ => return Err(PaperError(format!("unknown argument {arg}"))),
        }
    }
    Ok(Some(o))
}
fn run(o: Options) -> Result<(), PaperError> {
    let mut config_bytes = Vec::new();
    File::open(&o.config)?
        .take(262_145)
        .read_to_end(&mut config_bytes)?;
    if config_bytes.len() > 262_144 {
        return Err("config exceeds 256 KiB".into());
    }
    let config: PaperConfig = serde_json::from_slice(&config_bytes)?;
    config.validate()?;
    if let Some(parent) = o.journal.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut session = DurableSession::open(&o.journal, config)?;
    eprintln!(
        "recovered={} high_watermark={} repaired_tail_bytes={}",
        session.recovered_commands,
        session.high_watermark(),
        session.repaired_tail_bytes
    );
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut input_hash = Sha256::new();
    if !o.recover_only {
        let stdin = io::stdin();
        let mut reader: Box<dyn BufRead + '_> = if o.input == "-" {
            Box::new(stdin.lock())
        } else {
            Box::new(BufReader::new(File::open(&o.input)?))
        };
        let mut line = Vec::with_capacity(1024);
        let mut line_number = 0;
        loop {
            line.clear();
            let n = (&mut reader).take(8193).read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }
            line_number += 1;
            if n > 8192 {
                return Err(PaperError(format!(
                    "input line {line_number} exceeds 8192 bytes"
                )));
            }
            input_hash.update(&line);
            let envelope: Envelope = serde_json::from_slice(&line)
                .map_err(|e| PaperError(format!("input line {line_number}: {e}")))?;
            let receipt = session
                .execute(envelope)
                .map_err(|e| PaperError(format!("input line {line_number}: {e}")))?;
            serde_json::to_writer(&mut out, &receipt)?;
            out.write_all(b"\n")?;
            out.flush()?;
        }
    }
    if o.finish && !session.runtime().closed() {
        let receipt = session.execute(Envelope {
            seq: session.high_watermark() + 1,
            command: Command::Finish {},
        })?;
        serde_json::to_writer(&mut out, &receipt)?;
        out.write_all(b"\n")?;
        out.flush()?;
    }
    session.runtime().check_invariants()?;
    if let Some(path) = o.report {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let report = serde_json::json!({
            "schema_version": 1,
            "binary_sha256": session.binary_sha256,
            "config_sha256": session.config_sha256,
            "journal_chain_sha256": session.chain_sha256(),
            "input_sha256": hex(&input_hash.finalize()),
            "recovered_commands": session.recovered_commands,
            "repaired_tail_bytes": session.repaired_tail_bytes,
            "state": session.runtime().report()
        });
        storage::publish_new(&path, &serde_json::to_vec_pretty(&report)?)?;
        eprintln!("report={}", path.display());
    }
    Ok(())
}
fn main() {
    let result = match options() {
        Ok(Some(o)) => run(o),
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
