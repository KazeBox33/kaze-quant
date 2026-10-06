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
            "kaze-paper-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn command(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_kaze-paper"));
        c.current_dir(env!("CARGO_MANIFEST_DIR"))
            .arg("--journal")
            .arg(self.0.join("session.wal"));
        c
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn jsonl_demo_and_resume_are_identical_and_report_uses_string_money() {
    let t = Scratch::new();
    let o = t
        .command()
        .args(["--input", "data/paper.jsonl", "--finish"])
        .arg("--report")
        .arg(t.0.join("first.json"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8(o.stdout).unwrap().lines().count(), 41);
    let o = t
        .command()
        .args(["--input", "data/paper.jsonl", "--finish"])
        .arg("--report")
        .arg(t.0.join("resume.json"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    for l in String::from_utf8(o.stdout).unwrap().lines() {
        assert!(
            serde_json::from_str::<serde_json::Value>(l).unwrap()["duplicate"]
                .as_bool()
                .unwrap()
        );
    }
    let a: serde_json::Value =
        serde_json::from_slice(&fs::read(t.0.join("first.json")).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_slice(&fs::read(t.0.join("resume.json")).unwrap()).unwrap();
    assert_eq!(a["state"], b["state"]);
    assert_eq!(a["journal_chain_sha256"], b["journal_chain_sha256"]);
    assert_eq!(a["state"]["markets"][0]["cash_minor"], "1014065");
    assert_eq!(a["state"]["closed"], true);
}
#[test]
fn killed_process_recovers_durable_ack_and_retries_safely() {
    let t = Scratch::new();
    let mut child = t
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = include_str!("../data/paper.jsonl");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    for l in input.lines().take(8) {
        writeln!(stdin, "{l}").unwrap();
        stdin.flush().unwrap();
        let mut ack = String::new();
        stdout.read_line(&mut ack).unwrap();
        assert!(!ack.is_empty());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&ack).unwrap()["duplicate"],
            false
        );
    }
    child.kill().unwrap();
    child.wait().unwrap();
    drop(stdin);
    drop(stdout);
    let output = t
        .command()
        .args(["--input", "data/paper.jsonl", "--finish"])
        .arg("--report")
        .arg(t.0.join("recovered.json"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("recovered=8"));
    for (i, line) in String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .enumerate()
    {
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(line).unwrap()["duplicate"],
            i < 8
        );
    }
    let reference = Scratch::new();
    let o = reference
        .command()
        .args(["--input", "data/paper.jsonl", "--finish"])
        .arg("--report")
        .arg(reference.0.join("reference.json"))
        .output()
        .unwrap();
    assert!(o.status.success());
    let a: serde_json::Value =
        serde_json::from_slice(&fs::read(t.0.join("recovered.json")).unwrap()).unwrap();
    let b: serde_json::Value =
        serde_json::from_slice(&fs::read(reference.0.join("reference.json")).unwrap()).unwrap();
    assert_eq!(a["state"], b["state"]);
    assert_eq!(a["journal_chain_sha256"], b["journal_chain_sha256"]);
}
#[test]
fn oversized_or_bad_command_is_not_acked_and_never_publishes_report() {
    for input in [
        "x".repeat(8193),
        "{\"seq\":1,\"command\":{\"type\":\"halt\",\"typo\":true}}\n".into(),
        "{\"seq\":2,\"command\":{\"type\":\"halt\"}}\n".into(),
    ] {
        let t = Scratch::new();
        fs::write(t.0.join("bad.jsonl"), input).unwrap();
        let o = t
            .command()
            .arg("--input")
            .arg(t.0.join("bad.jsonl"))
            .arg("--report")
            .arg(t.0.join("report.json"))
            .output()
            .unwrap();
        assert!(!o.status.success());
        assert!(o.stdout.is_empty());
        assert!(!t.0.join("report.json").exists());
        let o = t.command().arg("--recover-only").output().unwrap();
        assert!(o.status.success());
        assert!(String::from_utf8_lossy(&o.stderr).contains("high_watermark=0"));
    }
}
#[test]
fn reporting_never_overwrites_and_eof_is_not_a_finish() {
    let t = Scratch::new();
    let report = t.0.join("report.json");
    let o = t
        .command()
        .args(["--input", "data/paper.jsonl"])
        .arg("--report")
        .arg(&report)
        .output()
        .unwrap();
    assert!(o.status.success());
    let before = fs::read(&report).unwrap();
    let v: serde_json::Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(v["state"]["closed"], false);
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
