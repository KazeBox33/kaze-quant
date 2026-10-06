use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::journal::digest;
use kaze_quant::paper::*;
use kaze_quant::store::{SqliteSession, StoreOptions};
use kaze_quant::types::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "kaze-sql-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn db(&self) -> PathBuf {
        self.0.join("run.db")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn config() -> PaperConfig {
    serde_json::from_str(include_str!("../configs/paper.json")).unwrap()
}
fn commands() -> Vec<Envelope> {
    include_str!("../data/paper.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect()
}
#[test]
fn batches_restart_and_full_audit_match_every_reference_receipt() {
    let t = Scratch::new();
    let inputs = commands();
    let mut reference = PaperRuntime::new(config()).unwrap();
    let expected: Vec<_> = inputs
        .iter()
        .map(|c| reference.process(c).unwrap())
        .collect();
    for chunk in inputs.chunks(7) {
        let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
        let receipts = s.execute_batch(chunk).unwrap();
        for (c, r) in chunk.iter().zip(receipts) {
            assert_eq!(r, expected[c.seq as usize - 1]);
        }
        s.verify_full().unwrap();
    }
    let s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    assert_eq!(s.runtime().report(), reference.report());
    assert_eq!(s.verify_full().unwrap(), inputs.len() as u64);
}
#[test]
fn invalid_late_command_rolls_back_whole_candidate_and_sequence() {
    let t = Scratch::new();
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    let before = s.runtime().report();
    let batch = [
        commands()[0].clone(),
        Envelope {
            seq: 2,
            command: Command::Quote {
                market: 999,
                quote: Quote {
                    sequence: 1,
                    timestamp_ns: 0,
                    bid: Price::new(1).unwrap(),
                    ask: Price::new(1).unwrap(),
                    bid_quantity: 1,
                    ask_quantity: 1,
                },
            },
        },
    ];
    assert!(s.execute_batch(&batch).is_err());
    assert_eq!(s.runtime().report(), before);
    assert_eq!(s.verify_full().unwrap(), 0);
    s.execute_batch(&commands()[..2]).unwrap();
}
#[test]
fn duplicates_inside_and_after_restart_never_reapply() {
    let t = Scratch::new();
    let c = commands()[0].clone();
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    let r = s.execute_batch(&[c.clone(), c.clone()]).unwrap();
    assert!(!r[0].duplicate);
    assert!(r[1].duplicate);
    let before = s.runtime().report();
    drop(s);
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    assert!(s.execute_batch(&[c]).unwrap()[0].duplicate);
    assert_eq!(s.runtime().report(), before);
    assert!(
        s.execute_batch(&[Envelope {
            seq: 1,
            command: Command::Halt {}
        }])
        .is_err()
    );
    s.verify_full().unwrap();
}
#[test]
fn checkpoint_corruption_and_rehashed_invalid_state_are_rejected() {
    let t = Scratch::new();
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    s.execute_batch(&commands()[..2]).unwrap();
    drop(s);
    let conn = rusqlite::Connection::open(t.db()).unwrap();
    let saved: Vec<u8> = conn
        .query_row("SELECT state FROM meta", [], |r| r.get(0))
        .unwrap();
    let mut v: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    v["markets"][0]["engine"]["account"]["pending_buy"] = serde_json::json!(u64::MAX);
    let bad = serde_json::to_vec(&v).unwrap();
    conn.execute("UPDATE meta SET state=?1", [&bad]).unwrap();
    drop(conn);
    assert!(SqliteSession::open(&t.db(), config(), StoreOptions::default()).is_err());
    let conn = rusqlite::Connection::open(t.db()).unwrap();
    conn.execute("UPDATE meta SET state_hash=?1", [digest(&bad).as_slice()])
        .unwrap();
    drop(conn);
    assert!(SqliteSession::open(&t.db(), config(), StoreOptions::default()).is_err());
}
#[test]
fn mid_history_damage_requires_full_audit_and_is_never_silently_repaired() {
    let t = Scratch::new();
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    s.execute_batch(&commands()).unwrap();
    drop(s);
    let conn = rusqlite::Connection::open(t.db()).unwrap();
    conn.execute("UPDATE commands SET receipt=x'7b7d' WHERE seq=1", [])
        .unwrap();
    drop(conn);
    let s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    assert!(s.verify_full().is_err());
}
#[test]
fn exclusive_writer_and_manifest_changes_are_rejected() {
    let t = Scratch::new();
    let s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    assert!(SqliteSession::open(&t.db(), config(), StoreOptions::default()).is_err());
    drop(s);
    let mut c = config();
    c.markets[0].engine.fee_bps += 1;
    assert!(SqliteSession::open(&t.db(), c, StoreOptions::default()).is_err());
    let opts = StoreOptions {
        terminal_retention: 1,
        ..Default::default()
    };
    assert!(SqliteSession::open(&t.db(), config(), opts).is_err());
}
#[test]
fn disk_write_failure_keeps_memory_unchanged_and_poisoned_until_recovery() {
    let t = Scratch::new();
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    let conn = rusqlite::Connection::open(t.db()).unwrap();
    conn.execute_batch("CREATE TRIGGER fault BEFORE INSERT ON commands BEGIN SELECT RAISE(ABORT,'injected write failure'); END;").unwrap();
    let before = s.runtime().report();
    assert!(
        s.execute_batch(&commands()[..2])
            .unwrap_err()
            .to_string()
            .contains("injected")
    );
    assert_eq!(s.runtime().report(), before);
    assert!(
        s.execute_batch(&commands()[..1])
            .unwrap_err()
            .to_string()
            .contains("poisoned")
    );
    conn.execute_batch("DROP TRIGGER fault;").unwrap();
    drop(conn);
    drop(s);
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    assert_eq!(s.verify_full().unwrap(), 0);
    s.execute_batch(&commands()[..2]).unwrap();
}
#[test]
fn hot_history_remains_bounded_and_ids_never_reuse_beyond_legacy_capacity() {
    let t = Scratch::new();
    let mut c = config();
    c.markets.truncate(1);
    c.max_commands = 1;
    c.markets[0].strategy = StrategyConfig::Passive {};
    c.markets[0].engine.max_active_orders = 2;
    c.markets[0].engine.max_orders = 4;
    let opts = StoreOptions {
        terminal_retention: 1,
        ..Default::default()
    };
    let mut s = SqliteSession::open(&t.db(), c, opts).unwrap();
    let mut inputs = Vec::new();
    for i in 0..100 {
        let quote = Command::Quote {
            market: 0,
            quote: Quote {
                sequence: i + 1,
                timestamp_ns: i,
                bid: Price::new(100).unwrap(),
                ask: Price::new(100).unwrap(),
                bid_quantity: 1,
                ask_quantity: 1,
            },
        };
        let submit = Command::Submit {
            market: 0,
            request: OrderRequest {
                side: Side::Buy,
                limit: Price::new(100).unwrap(),
                quantity: Quantity::new(1).unwrap(),
                time_in_force: TimeInForce::GoodTilCancelled,
            },
        };
        let cancel = Command::Cancel {
            market: 0,
            order_id: OrderId(i + 1),
        };
        for command in [quote, submit, cancel] {
            inputs.push(Envelope {
                seq: inputs.len() as u64 + 1,
                command,
            });
        }
    }
    for chunk in inputs.chunks(128) {
        s.execute_batch(chunk).unwrap();
        assert!(
            s.runtime().engine(0).unwrap().orders().len()
                <= 1 + s.runtime().engine(0).unwrap().active_count()
        );
    }
    assert_eq!(s.runtime().engine(0).unwrap().orders()[0].id, OrderId(100));
    assert!(s.runtime().engine(0).unwrap().order(OrderId(1)).is_none());
    assert_eq!(s.verify_full().unwrap(), 300);
}
#[test]
fn online_backup_restores_identical_state_and_never_overwrites() {
    let t = Scratch::new();
    let mut s = SqliteSession::open(&t.db(), config(), StoreOptions::default()).unwrap();
    s.execute_batch(&commands()[..7]).unwrap();
    let backup = t.0.join("backup.db");
    s.backup_new(&backup).unwrap();
    assert!(s.backup_new(&backup).is_err());
    let b = SqliteSession::open(&backup, config(), StoreOptions::default()).unwrap();
    assert_eq!(s.runtime().report(), b.runtime().report());
    assert_eq!(s.chain_sha256(), b.chain_sha256());
    assert_eq!(b.verify_full().unwrap(), 7);
}
#[test]
fn real_sqlite_full_rolls_back_and_recovers_last_confirmed_batch() {
    let t = Scratch::new();
    let options = StoreOptions {
        max_database_bytes: 1024 * 1024,
        ..Default::default()
    };
    let mut s = SqliteSession::open(&t.db(), config(), options.clone()).unwrap();
    let mut failed = false;
    for _ in 0..100 {
        let before = s.runtime().report();
        let inputs: Vec<_> = (1..=256)
            .map(|i| Envelope {
                seq: before.processed + i,
                command: Command::Advance {
                    timestamp_ns: before.clock_ns + i,
                },
            })
            .collect();
        match s.execute_batch(&inputs) {
            Ok(_) => (),
            Err(e) => {
                assert!(e.to_string().contains("full"), "{e}");
                assert_eq!(s.runtime().report(), before);
                failed = true;
                break;
            }
        }
    }
    assert!(failed, "workload must exhaust 1 MiB quota");
    let before = s.runtime().report();
    drop(s);
    let s = SqliteSession::open(&t.db(), config(), options).unwrap();
    assert_eq!(s.runtime().report(), before);
    assert_eq!(s.verify_full().unwrap(), before.processed);
}
#[test]
fn reader_pinned_wal_applies_backpressure_then_resumes() {
    let t = Scratch::new();
    let options = StoreOptions {
        max_wal_bytes: 1024 * 1024,
        ..Default::default()
    };
    let mut s = SqliteSession::open(&t.db(), config(), options).unwrap();
    let reader = rusqlite::Connection::open(t.db()).unwrap();
    reader.execute_batch("BEGIN;").unwrap();
    let _: i64 = reader
        .query_row("SELECT seq FROM meta", [], |r| r.get(0))
        .unwrap();
    let mut blocked = None;
    for _ in 0..100 {
        let before = s.runtime().report();
        let inputs: Vec<_> = (1..=256)
            .map(|i| Envelope {
                seq: before.processed + i,
                command: Command::Advance {
                    timestamp_ns: before.clock_ns + i,
                },
            })
            .collect();
        match s.execute_batch(&inputs) {
            Ok(_) => (),
            Err(e) => {
                assert!(e.to_string().contains("reader"), "{e}");
                assert_eq!(s.runtime().report(), before);
                blocked = Some(inputs);
                break;
            }
        }
    }
    let inputs = blocked.expect("reader must pin WAL beyond quota");
    reader.execute_batch("ROLLBACK;").unwrap();
    s.execute_batch(&inputs).unwrap();
    s.verify_full().unwrap();
}
