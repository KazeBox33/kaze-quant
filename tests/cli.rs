use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "quant-lab-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_kaze-quant"));
        command
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("--output")
            .arg(self.0.join("result"));
        command
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn assert_success(output: Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cli_emits_verified_summary_and_trace() {
    let temp = Scratch::new();
    assert_success(temp.command().output().unwrap());
    let summary = fs::read_to_string(temp.0.join("result/summary.json")).unwrap();
    assert!(summary.contains("\"pnl_minor\": 14065"));
    assert!(summary.contains("\"fees_minor\": 415"));
    let trace = fs::read_to_string(temp.0.join("result/events.csv")).unwrap();
    assert_eq!(trace.lines().count(), 13);
    assert!(!temp.0.join("result/events.csv.partial").exists());
}

#[test]
fn cli_refuses_to_overwrite_completed_results() {
    let temp = Scratch::new();
    assert_success(temp.command().output().unwrap());
    let before = fs::read(temp.0.join("result/events.csv")).unwrap();
    assert!(!temp.command().output().unwrap().status.success());
    assert_eq!(fs::read(temp.0.join("result/events.csv")).unwrap(), before);
}

#[test]
fn cli_bad_input_never_publishes_completion_marker() {
    let temp = Scratch::new();
    let input = temp.0.join("bad.csv");
    fs::write(&input, "sequence,timestamp_ns,bid,ask,bid_quantity,ask_quantity\n1,1,9900,9910,1,1\n1,2,9900,9910,1,1\n").unwrap();
    let output = temp.command().arg("--input").arg(input).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("line 3"));
    assert!(!temp.0.join("result/summary.json").exists());
    assert!(!temp.0.join("result/events.csv").exists());
    assert!(temp.0.join("result/events.csv.partial").exists());
}

#[test]
fn cli_rejects_unknown_flags_and_invalid_fees() {
    for args in [
        ["--unknown", "1"],
        ["--fee-bps", "10001"],
        ["--strategy", "typo"],
        ["--latency-ns", "-1"],
    ] {
        let temp = Scratch::new();
        assert!(!temp.command().args(args).output().unwrap().status.success());
        assert!(!temp.0.join("result").exists());
    }
}

#[test]
fn cli_supports_momentum_and_latency() {
    let temp = Scratch::new();
    assert_success(
        temp.command()
            .args(["--strategy", "momentum", "--latency-ns", "2000000"])
            .output()
            .unwrap(),
    );
    let summary = fs::read_to_string(temp.0.join("result/summary.json")).unwrap();
    assert!(summary.contains("\"strategy\": \"momentum\""));
    assert!(summary.contains("\"latency_ns\": 2000000"));
}
