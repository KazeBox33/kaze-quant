use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "kaze-run-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kaze-run"));
        c.current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("--db")
            .arg(self.0.join("session.db"));
        c
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn demo_restart_full_audit_and_report_match_independent_cli() {
    let t = Scratch::new();
    let report = t.0.join("first.json");
    let o = t
        .command()
        .args(["--input", "data/paper.jsonl", "--finish", "--verify-full"])
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let first: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
    let o = t
        .command()
        .args(["--input", "data/paper.jsonl", "--finish", "--verify-full"])
        .arg("--report")
        .arg(t.0.join("retry.json"))
        .output()
        .unwrap();
    assert!(o.status.success());
    for line in String::from_utf8(o.stdout).unwrap().lines() {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(line).unwrap()["duplicate"],
            true
        );
    }
    let retry: serde_json::Value =
        serde_json::from_slice(&fs::read(t.0.join("retry.json")).unwrap()).unwrap();
    assert_eq!(first["state"], retry["state"]);
    assert_eq!(first["audit_chain_sha256"], retry["audit_chain_sha256"]);
}
#[test]
fn idle_stdin_flushes_before_eof_and_kill_recovery_preserves_ack() {
    let t = Scratch::new();
    let mut child = t
        .command()
        .args(["--batch-size", "256", "--flush-ms", "2"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let line = include_str!("../data/paper.jsonl").lines().next().unwrap();
    writeln!(input, "{line}").unwrap();
    input.flush().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut line = String::new();
        let n = output.read_line(&mut line).unwrap();
        tx.send((n, line)).unwrap();
    });
    let ack = rx.recv_timeout(std::time::Duration::from_secs(10));
    if ack.is_err() {
        child.kill().unwrap();
        child.wait().unwrap();
        panic!("partial batch did not flush before EOF");
    }
    let (_, ack) = ack.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&ack).unwrap()["seq"],
        1
    );
    reader.join().unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    drop(input);
    let o = t
        .command()
        .args(["--recover-only", "--verify-full"])
        .arg("--report")
        .arg(t.0.join("recovered.json"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(t.0.join("recovered.json")).unwrap()).unwrap();
    assert_eq!(v["state"]["processed"], 1);
    assert_eq!(v["verified_commands"], 1);
}
#[test]
fn bad_batch_emits_no_ack_and_does_not_publish_report() {
    let t = Scratch::new();
    let mut lines: Vec<_> = include_str!("../data/paper.jsonl")
        .lines()
        .map(str::to_owned)
        .collect();
    lines[1]="{\"seq\":2,\"command\":{\"type\":\"quote\",\"market\":99,\"quote\":{\"sequence\":1,\"timestamp_ns\":0,\"bid\":1,\"ask\":1,\"bid_quantity\":1,\"ask_quantity\":1}}}".into();
    let input = t.0.join("bad.jsonl");
    fs::write(&input, lines[..2].join("\n")).unwrap();
    let o = t
        .command()
        .args(["--flush-ms", "1000"])
        .arg("--input")
        .arg(input)
        .arg("--report")
        .arg(t.0.join("bad.json"))
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert!(o.stdout.is_empty());
    assert!(!t.0.join("bad.json").exists());
    let o = t
        .command()
        .arg("--recover-only")
        .arg("--report")
        .arg(t.0.join("state.json"))
        .output()
        .unwrap();
    assert!(o.status.success());
    let v: serde_json::Value =
        serde_json::from_slice(&fs::read(t.0.join("state.json")).unwrap()).unwrap();
    assert_eq!(v["state"]["processed"], 0);
}
#[test]
fn csv_quote_input_and_report_no_clobber() {
    let t = Scratch::new();
    let report = t.0.join("report.json");
    let o = t
        .command()
        .args(["--quotes", "data/demo.csv", "--quiet", "--verify-full"])
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(o.stdout.is_empty());
    let before = fs::read(&report).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&before).unwrap();
    let bytes = fs::read("data/demo.csv").unwrap();
    assert_eq!(
        v["input_sha256"],
        kaze_quant::journal::hex(&Sha256::digest(bytes))
    );
    let o = t
        .command()
        .arg("--recover-only")
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(!o.status.success());
    assert_eq!(fs::read(report).unwrap(), before);
}
