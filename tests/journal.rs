use kaze_quant::config::PaperConfig;
use kaze_quant::journal::DurableSession;
use kaze_quant::paper::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "kaze-journal-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn wal(&self) -> PathBuf {
        self.0.join("session.wal")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn config() -> PaperConfig {
    serde_json::from_str(include_str!("../configs/paper.json")).unwrap()
}
fn commands() -> Vec<Envelope> {
    include_str!("../data/paper.jsonl")
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
#[test]
fn restart_reconstructs_strategies_orders_accounts_and_every_receipt() {
    let t = Scratch::new();
    let inputs = commands();
    let mut reference = PaperRuntime::new(config()).unwrap();
    let mut expected = Vec::new();
    for c in &inputs {
        expected.push(reference.process(c).unwrap());
    }
    for chunk in inputs.chunks(7) {
        let mut s = DurableSession::open(&t.wal(), config()).unwrap();
        for c in chunk {
            assert_eq!(s.execute(c.clone()).unwrap(), expected[c.seq as usize - 1]);
        }
    }
    let s = DurableSession::open(&t.wal(), config()).unwrap();
    assert_eq!(s.runtime().report(), reference.report());
    s.runtime().check_invariants().unwrap();
}
#[test]
fn retries_across_restart_never_repeat_fills_or_callbacks() {
    let t = Scratch::new();
    let inputs = commands();
    let report;
    {
        let mut s = DurableSession::open(&t.wal(), config()).unwrap();
        for c in &inputs {
            s.execute(c.clone()).unwrap();
        }
        report = s.runtime().report();
    }
    let mut s = DurableSession::open(&t.wal(), config()).unwrap();
    let bytes = fs::metadata(t.wal()).unwrap().len();
    for c in inputs {
        let ack = s.execute(c).unwrap();
        assert!(ack.duplicate);
        assert!(ack.notices.is_empty());
    }
    assert_eq!(s.runtime().report(), report);
    assert_eq!(fs::metadata(t.wal()).unwrap().len(), bytes);
    let conflicting = Envelope {
        seq: 1,
        command: Command::Halt {},
    };
    assert!(s.execute(conflicting).is_err());
    assert_eq!(s.runtime().report(), report);
}
#[test]
fn incomplete_last_frame_is_preserved_and_only_its_suffix_is_repaired() {
    let t = Scratch::new();
    let inputs = commands();
    let prefix;
    {
        let mut s = DurableSession::open(&t.wal(), config()).unwrap();
        s.execute(inputs[0].clone()).unwrap();
        prefix = fs::read(t.wal()).unwrap();
        s.execute(inputs[1].clone()).unwrap();
    }
    let full = fs::read(t.wal()).unwrap();
    let added = full.len() - prefix.len();
    for cut in [1, 2, 3, 4, added / 2, added - 32, added - 1] {
        let path = t.0.join(format!("cut-{cut}.wal"));
        fs::write(&path, &full[..prefix.len() + cut]).unwrap();
        let mut s = DurableSession::open(&path, config()).unwrap();
        assert_eq!(s.high_watermark(), 1);
        assert_eq!(s.repaired_tail_bytes, cut as u64);
        assert_eq!(fs::read(&path).unwrap(), prefix);
        s.execute(inputs[1].clone()).unwrap();
        assert_eq!(fs::read(path).unwrap(), full);
    }
}
#[test]
fn complete_checksum_corruption_is_not_truncated_or_ignored() {
    let t = Scratch::new();
    {
        let mut s = DurableSession::open(&t.wal(), config()).unwrap();
        s.execute(commands()[0].clone()).unwrap();
    }
    let mut bytes = fs::read(t.wal()).unwrap();
    let n = bytes.len();
    bytes[n - 1] ^= 1;
    fs::write(t.wal(), &bytes).unwrap();
    let e = DurableSession::open(&t.wal(), config()).err().unwrap();
    assert!(e.to_string().contains("checksum"));
    assert_eq!(fs::read(t.wal()).unwrap(), bytes);
}
#[test]
fn manifest_config_change_or_partial_header_blocks_recovery() {
    let t = Scratch::new();
    drop(DurableSession::open(&t.wal(), config()).unwrap());
    let before = fs::read(t.wal()).unwrap();
    let mut c = config();
    c.markets[0].engine.fee_bps = 11;
    assert!(DurableSession::open(&t.wal(), c).is_err());
    assert_eq!(fs::read(t.wal()).unwrap(), before);
    for bytes in [
        b"KAZ".as_slice(),
        b"KAZEWAL1",
        b"KAZEWAL1\x01\x00\x00\x00\x7b",
    ] {
        let p = t.0.join(format!("partial-{}", bytes.len()));
        fs::write(&p, bytes).unwrap();
        assert!(DurableSession::open(&p, config()).is_err());
        assert_eq!(fs::read(p).unwrap(), bytes);
    }
}
#[test]
fn only_one_writer_can_open_a_session() {
    let t = Scratch::new();
    let s = DurableSession::open(&t.wal(), config()).unwrap();
    assert!(DurableSession::open(&t.wal(), config()).is_err());
    drop(s);
    assert!(DurableSession::open(&t.wal(), config()).is_ok());
}
#[test]
fn invalid_command_does_not_write_and_valid_same_id_can_be_retried() {
    let t = Scratch::new();
    let mut s = DurableSession::open(&t.wal(), config()).unwrap();
    let before = fs::read(t.wal()).unwrap();
    assert!(
        s.execute(Envelope {
            seq: 2,
            command: Command::Halt {}
        })
        .is_err()
    );
    assert!(
        s.execute(Envelope {
            seq: 1,
            command: Command::Cancel {
                market: 100,
                order_id: kaze_quant::types::OrderId(1)
            }
        })
        .is_err()
    );
    assert_eq!(fs::read(t.wal()).unwrap(), before);
    s.execute(commands()[0].clone()).unwrap();
    assert_eq!(s.high_watermark(), 1);
}
#[test]
fn command_capacity_and_closed_session_remain_enforced_after_restart() {
    let t = Scratch::new();
    let mut c = config();
    c.max_commands = 1;
    {
        let mut s = DurableSession::open(&t.wal(), c.clone()).unwrap();
        s.execute(Envelope {
            seq: 1,
            command: Command::Finish {},
        })
        .unwrap();
    }
    let mut s = DurableSession::open(&t.wal(), c).unwrap();
    assert!(s.runtime().closed());
    assert!(
        s.execute(Envelope {
            seq: 2,
            command: Command::Halt {}
        })
        .is_err()
    );
    assert!(
        s.execute(Envelope {
            seq: 1,
            command: Command::Finish {}
        })
        .unwrap()
        .duplicate
    );
}
#[test]
fn invalid_frame_length_is_quarantined_without_large_allocation() {
    let t = Scratch::new();
    drop(DurableSession::open(&t.wal(), config()).unwrap());
    let mut bytes = fs::read(t.wal()).unwrap();
    bytes.extend(u32::MAX.to_le_bytes());
    fs::write(t.wal(), &bytes).unwrap();
    assert!(DurableSession::open(&t.wal(), config()).is_err());
    assert_eq!(fs::read(t.wal()).unwrap(), bytes);
}
