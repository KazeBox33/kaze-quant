use kaze_quant::{
    conditional::*,
    config::PaperConfig,
    paper::*,
    store::{SqliteSession, StoreOptions},
    types::*,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "kaze-bracket-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
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
fn cfg() -> PaperConfig {
    serde_json::from_str(include_str!("../configs/breakout-bracket-demo.json")).unwrap()
}
fn q(s: u64, bid: u64, qty: u64) -> Envelope {
    Envelope {
        seq: s,
        command: Command::Quote {
            market: 0,
            quote: Quote {
                sequence: s,
                timestamp_ns: s * 10,
                bid: Price::new(bid).unwrap(),
                ask: Price::new(bid + 1).unwrap(),
                bid_quantity: qty,
                ask_quantity: qty,
            },
        },
    }
}
fn inputs() -> Vec<Envelope> {
    vec![
        q(1, 99, 3),
        q(2, 100, 3),
        q(3, 100, 1),
        q(4, 100, 2),
        q(5, 111, 3),
        q(6, 111, 1),
        q(7, 111, 2),
    ]
}
fn phase(r: &PaperRuntime) -> String {
    r.report().markets[0].strategy_diagnostics.as_ref().unwrap()["phase"]["type"]
        .as_str()
        .unwrap()
        .into()
}
#[test]
fn partial_entry_waits_for_actual_fills_then_oco_exit_partial_fills_hand_ledger() {
    let mut r = PaperRuntime::new(cfg()).unwrap();
    for e in inputs() {
        let receipt = r.process(&e).unwrap();
        if e.seq == 2 {
            assert_eq!(r.engine(0).unwrap().account().position(), 0);
            assert_eq!(phase(&r), "entry_working");
        }
        if e.seq == 3 {
            assert_eq!(r.conditionals(0).unwrap().len(), 0);
            assert_eq!(r.engine(0).unwrap().account().position(), 1);
        }
        if e.seq == 4 {
            assert_eq!(r.conditionals(0).unwrap().len(), 2);
            assert_eq!(phase(&r), "exits_waiting");
            assert_eq!(r.engine(0).unwrap().account().reserved_sell(), 0);
        }
        if e.seq == 5 {
            assert!(receipt.notices.iter().any(|n| matches!(
                n,
                Notice::Conditional {
                    event: ConditionalEvent::Cancelled {
                        reason: CancelReason::OcoPeerAccepted,
                        ..
                    },
                    ..
                }
            )));
            assert_eq!(r.engine(0).unwrap().account().reserved_sell(), 3);
            assert!(r.conditionals(0).unwrap().is_empty());
        }
        r.check_invariants().unwrap();
    }
    assert_eq!(phase(&r), "complete");
    assert_eq!(r.engine(0).unwrap().account().position(), 0);
    assert_eq!(r.engine(0).unwrap().account().cash(), 10026);
    assert_eq!(r.engine(0).unwrap().account().fees_paid(), 4);
    assert_eq!(r.engine(0).unwrap().metrics().fills, 4);
}
#[test]
fn every_snapshot_and_new_sqlite_process_match_all_receipts_and_dedupe() {
    let t = Scratch::new();
    let mut reference = PaperRuntime::new(cfg()).unwrap();
    let mut restored = PaperRuntime::new(cfg()).unwrap();
    for e in inputs() {
        let expected = reference.process(&e).unwrap();
        let state = serde_json::to_vec(&restored.snapshot().unwrap()).unwrap();
        restored = PaperRuntime::restore(cfg(), serde_json::from_slice(&state).unwrap()).unwrap();
        assert_eq!(restored.process(&e).unwrap(), expected);
        let mut session = SqliteSession::open(&t.db(), cfg(), StoreOptions::default()).unwrap();
        assert_eq!(
            session.execute_batch(std::slice::from_ref(&e)).unwrap()[0],
            expected
        );
        let before = session.runtime().report();
        assert!(session.execute_batch(&[e]).unwrap()[0].duplicate);
        assert_eq!(session.runtime().report(), before);
        session.verify_full().unwrap();
        assert_eq!(before, reference.report());
    }
}
#[test]
fn trigger_then_invalid_command_rolls_back_intent_activation_and_retry_is_single_child() {
    let t = Scratch::new();
    let mut s = SqliteSession::open(&t.db(), cfg(), StoreOptions::default()).unwrap();
    s.execute_batch(&[q(1, 99, 3)]).unwrap();
    let before = s.runtime().report();
    let mut bad = q(3, 100, 3);
    if let Command::Quote { market, .. } = &mut bad.command {
        *market = 99;
    }
    assert!(s.execute_batch(&[q(2, 100, 3), bad]).is_err());
    assert_eq!(s.runtime().report(), before);
    s.verify_full().unwrap();
    drop(s);
    let mut s = SqliteSession::open(&t.db(), cfg(), StoreOptions::default()).unwrap();
    s.execute_batch(&[q(2, 100, 3)]).unwrap();
    assert_eq!(s.runtime().engine(0).unwrap().metrics().accepted, 1);
    assert_eq!(s.runtime().conditionals(0).unwrap().metrics().triggered, 1);
}
#[test]
fn stop_exit_and_gap_below_stop_limit_are_distinct_from_guaranteed_execution() {
    for gap in [false, true] {
        let mut r = PaperRuntime::new(cfg()).unwrap();
        for e in [
            q(1, 99, 3),
            q(2, 100, 3),
            q(3, 100, 3),
            q(4, if gap { 80 } else { 89 }, 3),
            q(5, if gap { 80 } else { 89 }, 3),
        ] {
            r.process(&e).unwrap();
        }
        if gap {
            assert_eq!(phase(&r), "exit_working");
            assert_eq!(r.engine(0).unwrap().account().position(), 3);
            assert_eq!(r.engine(0).unwrap().metrics().fills, 1);
        } else {
            assert_eq!(phase(&r), "complete");
            assert_eq!(r.engine(0).unwrap().account().cash(), 9962);
            assert_eq!(r.engine(0).unwrap().account().position(), 0);
        }
    }
}
#[test]
fn capacity_one_preserves_known_leg_and_halt_cancels_it_without_hidden_orders() {
    let mut c = cfg();
    c.markets[0].engine.max_active_orders = 1;
    let mut r = PaperRuntime::new(c).unwrap();
    for e in [q(1, 99, 3), q(2, 100, 3), q(3, 100, 3)] {
        r.process(&e).unwrap();
    }
    assert_eq!(r.conditionals(0).unwrap().len(), 1);
    r.check_invariants().unwrap();
    r.process(&Envelope {
        seq: 4,
        command: Command::Halt {},
    })
    .unwrap();
    assert!(r.conditionals(0).unwrap().is_empty());
    r.check_invariants().unwrap();
}
#[test]
fn malformed_strategy_child_and_conditional_identity_fail_canonical_restore() {
    let mut r = PaperRuntime::new(cfg()).unwrap();
    r.process(&q(1, 99, 3)).unwrap();
    let mut v = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    v["markets"][0]["strategy"]["registered"]["state"]["phase"]["id"] = serde_json::json!(99);
    assert!(PaperRuntime::restore(cfg(), serde_json::from_value(v).unwrap()).is_err());
    r.process(&q(2, 100, 3)).unwrap();
    let mut v = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    v["markets"][0]["strategy"]["registered"]["state"]["phase"]["remaining"] = serde_json::json!(1);
    assert!(PaperRuntime::restore(cfg(), serde_json::from_value(v).unwrap()).is_err());
}
#[test]
fn expired_entry_and_cash_rejection_pause_without_rearming() {
    for expire in [true, false] {
        let mut c = cfg();
        if expire {
            c.markets[0].strategy = serde_json::from_value({
                let mut v = serde_json::to_value(&c.markets[0].strategy).unwrap();
                v["parameters"]["entry_lifetime_ns"] = serde_json::json!(10);
                v
            })
            .unwrap();
        } else {
            c.markets[0].engine.initial_cash = 50;
        }
        let mut r = PaperRuntime::new(c).unwrap();
        for e in [q(1, 99, 3), q(2, 100, 3), q(3, 100, 3)] {
            r.process(&e).unwrap();
        }
        assert_eq!(phase(&r), "paused");
        assert!(r.conditionals(0).unwrap().is_empty());
        assert_eq!(r.engine(0).unwrap().account().position(), 0);
        assert_eq!(r.conditionals(0).unwrap().metrics().accepted, 1);
    }
}
