use kaze_quant::config::PaperConfig;
use kaze_quant::journal::hex;
use kaze_quant::paper::{Command, Envelope, PaperError};
use kaze_quant::replay::QuoteReader;
use kaze_quant::storage::publish_new;
use kaze_quant::store::{SqliteSession, StoreOptions};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
const HELP: &str = "KazeQuant transactional replay/paper runtime (no live orders)
Usage: kaze-run --db PATH [--config PATH] [--store-options PATH]
                [--input JSONL|- | --quotes CSV] [--batch-size 1..256]
                [--flush-ms 1..1000] [--report NEW_PATH] [--backup NEW_PATH] [--quiet]
                [--recover-only] [--verify-full] [--finish]
Default batch=256, deadline=5ms from first queued command; EOF flushes.
A whole batch commits before receipts are printed. Invalid batch emits no receipts.
EOF keeps session open. Identical seq/payload retries are safe across restart.
--verify-full scans every stored command/receipt; startup checks hot checkpoint.
Same binary, config and store-options are required for recovery.";
struct Options {
    config: PathBuf,
    db: PathBuf,
    store: Option<PathBuf>,
    input: String,
    quotes: bool,
    batch: usize,
    flush_ms: u64,
    report: Option<PathBuf>,
    backup: Option<PathBuf>,
    recover: bool,
    verify: bool,
    finish: bool,
    quiet: bool,
}
fn options() -> Result<Option<Options>, PaperError> {
    let mut o = Options {
        config: "configs/paper.json".into(),
        db: "reports/run.db".into(),
        store: None,
        input: "-".into(),
        quotes: false,
        batch: 256,
        flush_ms: 5,
        report: None,
        backup: None,
        recover: false,
        verify: false,
        finish: false,
        quiet: false,
    };
    let mut args = std::env::args().skip(1);
    let mut input_set = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                println!("{HELP}");
                return Ok(None);
            }
            "--recover-only" => o.recover = true,
            "--verify-full" => o.verify = true,
            "--finish" => o.finish = true,
            "--quiet" => o.quiet = true,
            "--config" | "--db" | "--input" | "--quotes" | "--store-options" | "--batch-size"
            | "--flush-ms" | "--report" | "--backup" => {
                let v = args.next().ok_or("missing argument value")?;
                match arg.as_str() {
                    "--config" => o.config = v.into(),
                    "--db" => o.db = v.into(),
                    "--store-options" => o.store = Some(v.into()),
                    "--input" | "--quotes" => {
                        if input_set {
                            return Err("input and quotes are mutually exclusive".into());
                        }
                        input_set = true;
                        o.quotes = arg == "--quotes";
                        o.input = v;
                    }
                    "--batch-size" => {
                        o.batch = v
                            .parse()
                            .map_err(|_| PaperError("invalid batch size".into()))?
                    }
                    "--flush-ms" => {
                        o.flush_ms = v
                            .parse()
                            .map_err(|_| PaperError("invalid flush deadline".into()))?
                    }
                    "--backup" => o.backup = Some(v.into()),
                    _ => o.report = Some(v.into()),
                }
            }
            _ => return Err(PaperError(format!("unknown argument {arg}"))),
        }
    }
    if !(1..=1024).contains(&o.batch)
        || !(1..=1000).contains(&o.flush_ms)
        || o.quotes && o.input == "-"
    {
        return Err("invalid batch/deadline or CSV input path".into());
    }
    Ok(Some(o))
}
fn read_small(path: &PathBuf) -> Result<Vec<u8>, PaperError> {
    let mut bytes = Vec::new();
    File::open(path)?.take(262145).read_to_end(&mut bytes)?;
    if bytes.len() > 262144 {
        return Err("config exceeds 256 KiB".into());
    }
    Ok(bytes)
}
/// 哈希与实际读取共用同一字节流，避免预先扫描再打开文件的身份竞态。
struct HashingRead<'a, R> {
    reader: R,
    hash: &'a mut Sha256,
}
impl<R: Read> Read for HashingRead<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let n = self.reader.read(buffer)?;
        self.hash.update(&buffer[..n]);
        Ok(n)
    }
}
enum Input {
    Command(Envelope),
    End(String),
    Error(PaperError),
}
fn input_worker(path: String, quotes: bool) -> Receiver<Input> {
    let (tx, rx) = mpsc::sync_channel(256);
    std::thread::spawn(move || {
        let result = (|| -> Result<String, PaperError> {
            if quotes {
                let mut hash = Sha256::new();
                let source = HashingRead {
                    reader: File::open(&path)?,
                    hash: &mut hash,
                };
                let reader = QuoteReader::new(BufReader::new(source))
                    .map_err(|e| PaperError(e.to_string()))?;
                for q in reader {
                    let q = q.map_err(|e| PaperError(e.to_string()))?;
                    if tx
                        .send(Input::Command(Envelope {
                            seq: q.sequence,
                            command: Command::Quote {
                                market: 0,
                                quote: q,
                            },
                        }))
                        .is_err()
                    {
                        return Err("consumer closed".into());
                    }
                }
                return Ok(hex(&hash.finalize()));
            }
            let mut reader: Box<dyn BufRead> = if path == "-" {
                Box::new(BufReader::new(io::stdin()))
            } else {
                Box::new(BufReader::new(File::open(path)?))
            };
            let mut line = Vec::with_capacity(1024);
            let mut hash = Sha256::new();
            let mut number = 0;
            loop {
                line.clear();
                let n = (&mut reader).take(8193).read_until(b'\n', &mut line)?;
                if n == 0 {
                    break;
                }
                number += 1;
                if n > 8192 {
                    return Err(PaperError(format!(
                        "input line {number} exceeds 8192 bytes"
                    )));
                }
                hash.update(&line);
                let c = serde_json::from_slice(&line)
                    .map_err(|e| PaperError(format!("input line {number}: {e}")))?;
                if tx.send(Input::Command(c)).is_err() {
                    return Err("consumer closed".into());
                }
            }
            Ok(hex(&hash.finalize()))
        })();
        let message = match result {
            Ok(h) => Input::End(h),
            Err(e) => Input::Error(e),
        };
        let _ = tx.send(message);
    });
    rx
}
struct Stats {
    batches: u64,
    commands: u64,
    commit_ns: u128,
    max_commit_ns: u128,
    histogram: [u64; 64],
}
impl Default for Stats {
    fn default() -> Self {
        Self {
            batches: 0,
            commands: 0,
            commit_ns: 0,
            max_commit_ns: 0,
            histogram: [0; 64],
        }
    }
}
impl Stats {
    fn record(&mut self, count: usize, ns: u128) {
        self.batches += 1;
        self.commands += count as u64;
        self.commit_ns += ns;
        self.max_commit_ns = self.max_commit_ns.max(ns);
        let n = ns.min(u64::MAX as u128) as u64;
        let bucket = (64 - n.max(1).leading_zeros() as usize).min(63);
        self.histogram[bucket] += 1;
    }
}
fn flush(
    session: &mut SqliteSession,
    batch: &mut Vec<Envelope>,
    out: &mut impl Write,
    quiet: bool,
    stats: &mut Stats,
) -> Result<(), PaperError> {
    if batch.is_empty() {
        return Ok(());
    }
    let start = Instant::now();
    let receipts = session.execute_batch(batch)?;
    stats.record(batch.len(), start.elapsed().as_nanos());
    if !quiet {
        for r in receipts {
            serde_json::to_writer(&mut *out, &r)?;
            out.write_all(b"\n")?;
        }
        out.flush()?;
    }
    batch.clear();
    Ok(())
}
fn run(o: Options) -> Result<(), PaperError> {
    let config: PaperConfig = serde_json::from_slice(&read_small(&o.config)?)?;
    let store = if let Some(p) = o.store {
        serde_json::from_slice(&read_small(&p)?)?
    } else {
        StoreOptions::default()
    };
    if o.batch > store.max_batch {
        return Err("batch-size exceeds store capacity".into());
    }
    if let Some(p) = o.db.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(p)?
    }
    if o.report.as_ref().is_some_and(|p| p.exists()) {
        return Err("report already exists".into());
    }
    let start = Instant::now();
    let mut session = SqliteSession::open(&o.db, config, store)?;
    let recovery_ns = start.elapsed().as_nanos();
    let recovered = session.runtime().processed();
    eprintln!("recovered={recovered} recovery_ns={recovery_ns}");
    let mut stats = Stats::default();
    let mut out = io::stdout().lock();
    let mut input_hash = None;
    let start = Instant::now();
    if !o.recover {
        let rx = input_worker(o.input, o.quotes);
        let mut batch = Vec::with_capacity(o.batch);
        let mut deadline: Option<Instant> = None;
        loop {
            let message = if let Some(d) = deadline {
                match rx.recv_timeout(d.saturating_duration_since(Instant::now())) {
                    Ok(m) => m,
                    Err(RecvTimeoutError::Timeout) => {
                        flush(&mut session, &mut batch, &mut out, o.quiet, &mut stats)?;
                        deadline = None;
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err("input worker disconnected".into());
                    }
                }
            } else {
                rx.recv()
                    .map_err(|_| PaperError("input worker disconnected".into()))?
            };
            match message {
                Input::Command(c) => {
                    if batch.is_empty() {
                        deadline = Some(Instant::now() + Duration::from_millis(o.flush_ms));
                    }
                    batch.push(c);
                    if batch.len() == o.batch {
                        flush(&mut session, &mut batch, &mut out, o.quiet, &mut stats)?;
                        deadline = None;
                    }
                }
                Input::End(hash) => {
                    flush(&mut session, &mut batch, &mut out, o.quiet, &mut stats)?;
                    input_hash = Some(hash);
                    break;
                }
                Input::Error(e) => return Err(e),
            }
        }
    }
    if o.finish && !session.runtime().closed() {
        let mut batch = vec![Envelope {
            seq: session.runtime().processed() + 1,
            command: Command::Finish {},
        }];
        flush(&mut session, &mut batch, &mut out, o.quiet, &mut stats)?;
    }
    let elapsed_ns = start.elapsed().as_nanos();
    let verified = if o.verify {
        Some(session.verify_full()?)
    } else {
        None
    };
    session.runtime().check_invariants()?;
    if let Some(path) = o.backup {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        session.backup_new(&path)?;
    }
    let report = serde_json::json!({"schema_version":1,"storage":"sqlite-wal-full","storage_stats":session.storage_stats()?,"binary_sha256":session.binary_sha256,"config_sha256":session.config_sha256,"audit_chain_sha256":session.chain_sha256(),"input_sha256":input_hash,"recovered_commands":recovered,"recovery_ns":recovery_ns,"verified_commands":verified,"run":{"received_commands":stats.commands,"committed_batches":stats.batches,"elapsed_ns":elapsed_ns,"commit_total_ns":stats.commit_ns,"commit_max_ns":stats.max_commit_ns,"commit_histogram_counts":stats.histogram.to_vec(),"histogram_rule":"bucket i upper bound 2^i ns; batch commit duration, excludes queue wait and stdout","batch_size":o.batch,"flush_ms":o.flush_ms},"state":session.runtime().report()});
    if let Some(path) = o.report {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        publish_new(&path, &serde_json::to_vec_pretty(&report)?)?;
        eprintln!("report={}", path.display());
    }
    eprintln!(
        "commands={} batches={} elapsed_ns={elapsed_ns}",
        stats.commands, stats.batches
    );
    Ok(())
}
fn main() {
    let result = match options() {
        Ok(Some(o)) => run(o),
        Ok(None) => Ok(()),
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1)
    }
}
