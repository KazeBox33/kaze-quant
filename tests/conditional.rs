use kaze_quant::{
    conditional::*,
    config::{PaperConfig, StrategyConfig},
    paper::*,
    types::*,
};
fn p(v: u64) -> Price {
    Price::new(v).unwrap()
}
fn q(s: u64, b: u64) -> Quote {
    Quote {
        sequence: s,
        timestamp_ns: s * 10,
        bid: p(b),
        ask: p(b + 1),
        bid_quantity: 3,
        ask_quantity: 3,
    }
}
fn r(reference: Reference, direction: Direction, t: u64) -> ConditionalRequest {
    ConditionalRequest {
        reference,
        direction,
        trigger: p(t),
        order: OrderRequest {
            side: Side::Buy,
            limit: p(120),
            quantity: Quantity::new(1).unwrap(),
            time_in_force: TimeInForce::GoodTilCancelled,
        },
        expires_at_ns: None,
        oco_group: None,
    }
}
fn c() -> PaperConfig {
    let mut c: PaperConfig =
        serde_json::from_str(include_str!("../configs/bar-atr-demo.json")).unwrap();
    c.markets[0].strategy = StrategyConfig::Passive {};
    c.markets[0].engine.initial_cash = 10000;
    c.markets[0].risk.price_collar_bps = 10000;
    c
}
fn run(rt: &mut PaperRuntime, command: Command) -> Receipt {
    rt.process(&Envelope {
        seq: rt.processed() + 1,
        command,
    })
    .unwrap()
}
#[test]
fn indexed_selection_matches_exhaustive_four_trigger_domains_and_submission_order() {
    let mut b = ConditionalBook::new(128).unwrap();
    for reference in [Reference::Bid, Reference::Ask] {
        for direction in [Direction::AboveOrEqual, Direction::BelowOrEqual] {
            for t in 90..111 {
                b.submit(r(reference, direction, t), q(1, 100), 10).unwrap();
            }
        }
    }
    for v in 80..121 {
        let q = q(2, v);
        assert_eq!(
            b.select(q, TriggerPolicy::Indexed).ids,
            b.select(q, TriggerPolicy::Scan).ids
        );
    }
    for bid in 80..121 {
        let q = q(2, bid);
        assert_eq!(
            b.select(q, TriggerPolicy::Adaptive).ids,
            b.select(q, TriggerPolicy::Scan).ids
        );
    }
    assert!(b.select(q(1, 100), TriggerPolicy::Indexed).ids.is_empty());
    b.check_invariants().unwrap();
}
#[test]
fn expiry_wins_equal_price_trigger_and_never_activates() {
    let mut b = ConditionalBook::new(2).unwrap();
    let mut a = r(Reference::Bid, Direction::AboveOrEqual, 100);
    a.expires_at_ns = Some(20);
    let id = b.submit(a, q(1, 99), 10).unwrap();
    let e = b
        .on_quote(q(2, 101), TriggerPolicy::Indexed, |_| {
            panic!("expired callback")
        })
        .unwrap();
    assert_eq!(
        e,
        [ConditionalEvent::Cancelled {
            conditional_id: id,
            reason: CancelReason::Expired
        }]
    );
    assert_eq!(b.metrics().expired, 1);
    b.check_invariants().unwrap();
}
#[test]
fn oco_cancels_peers_after_acceptance_and_both_crossing_choose_oldest_id() {
    let mut b = ConditionalBook::new(3).unwrap();
    let mut a = r(Reference::Bid, Direction::AboveOrEqual, 100);
    a.oco_group = Some(1);
    let first = b.submit(a, q(1, 99), 10).unwrap();
    let peer = b.submit(a, q(1, 99), 10).unwrap();
    let mut n = 0;
    let e = b
        .on_quote(q(2, 100), TriggerPolicy::Indexed, |_| {
            n += 1;
            Ok(OrderId(50))
        })
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        e,
        [
            ConditionalEvent::Triggered {
                conditional_id: first,
                order_id: OrderId(50),
                quote_sequence: 2
            },
            ConditionalEvent::Cancelled {
                conditional_id: peer,
                reason: CancelReason::OcoPeerAccepted
            }
        ]
    );
    assert!(b.is_empty());
}
#[test]
fn rejected_activation_consumes_intent_but_keeps_oco_peer_and_never_retries() {
    let mut b = ConditionalBook::new(3).unwrap();
    let mut a = r(Reference::Bid, Direction::AboveOrEqual, 100);
    a.oco_group = Some(1);
    let id = b.submit(a, q(1, 99), 10).unwrap();
    a.trigger = p(105);
    let peer = b.submit(a, q(1, 99), 10).unwrap();
    let e = b
        .on_quote(q(2, 100), TriggerPolicy::Indexed, |_| {
            Err(ActivationReject::Engine(RejectReason::InsufficientCash))
        })
        .unwrap();
    assert!(
        matches!(e[0],ConditionalEvent::ActivationRejected {conditional_id,..} if conditional_id==id)
    );
    assert!(b.get(peer).is_some());
    assert!(
        b.on_quote(q(3, 101), TriggerPolicy::Indexed, |_| panic!("no retry"))
            .unwrap()
            .is_empty()
    );
    assert_eq!(b.metrics().activation_rejected, 1);
}
#[test]
fn capacity_cancel_and_recovery_never_reuse_id_or_corrupt_indices() {
    let mut b = ConditionalBook::new(1).unwrap();
    let a = r(Reference::Ask, Direction::AboveOrEqual, 110);
    let first = b.submit(a, q(1, 99), 10).unwrap();
    assert_eq!(b.submit(a, q(1, 99), 10), Err(ConditionalReject::Capacity));
    b.cancel(first, CancelReason::Operator);
    let next = b.submit(a, q(1, 99), 10).unwrap();
    assert_ne!(first, next);
    assert!(matches!(
        b.cancel(first, CancelReason::Operator),
        ConditionalEvent::CancelMissing { .. }
    ));
    let bytes = serde_json::to_vec(&b.snapshot().unwrap()).unwrap();
    let mut restored = ConditionalBook::restore(
        1,
        serde_json::from_slice(&bytes).unwrap(),
        Some(q(1, 99)),
        10,
    )
    .unwrap();
    assert_eq!(
        restored
            .on_quote(q(2, 109), TriggerPolicy::Indexed, |_| Ok(OrderId(1)))
            .unwrap(),
        b.on_quote(q(2, 109), TriggerPolicy::Indexed, |_| Ok(OrderId(1)))
            .unwrap()
    );
    assert_eq!(restored.snapshot(), b.snapshot());
}
#[test]
fn malformed_counter_id_future_time_and_expired_checkpoint_fail_restore() {
    let mut b = ConditionalBook::new(2).unwrap();
    b.submit(
        r(Reference::Bid, Direction::AboveOrEqual, 110),
        q(1, 99),
        10,
    )
    .unwrap();
    let state = serde_json::to_value(b.snapshot().unwrap()).unwrap();
    for field in ["id", "time", "expiry", "count", "group"] {
        let mut v = state.clone();
        match field {
            "id" => v["pending"][0]["id"] = serde_json::json!(0),
            "time" => v["pending"][0]["submitted_at_ns"] = serde_json::json!(11),
            "expiry" => v["pending"][0]["request"]["expires_at_ns"] = serde_json::json!(10),
            "count" => v["metrics"]["accepted"] = serde_json::json!(2),
            "group" => v["pending"][0]["request"]["oco_group"] = serde_json::json!(0),
            _ => unreachable!(),
        };
        assert!(
            ConditionalBook::restore(2, serde_json::from_value(v).unwrap(), Some(q(1, 99)), 10)
                .is_err(),
            "{field}"
        );
    }
}
#[test]
fn invalid_quote_and_regressed_expiry_are_atomic() {
    let mut b = ConditionalBook::new(2).unwrap();
    b.submit(
        r(Reference::Bid, Direction::AboveOrEqual, 110),
        q(1, 99),
        10,
    )
    .unwrap();
    b.expire(15).unwrap();
    let before = b.snapshot();
    let mut invalid = q(2, 100);
    invalid.ask = p(99);
    assert!(
        b.on_quote(invalid, TriggerPolicy::Indexed, |_| panic!())
            .is_err()
    );
    assert_eq!(b.snapshot(), before);
    assert!(b.expire(14).is_err());
    assert_eq!(b.snapshot(), before);
}
#[test]
fn activation_is_next_quote_only_and_latent_intent_does_not_freeze_money() {
    let mut rt = PaperRuntime::new(c()).unwrap();
    run(
        &mut rt,
        Command::Quote {
            market: 0,
            quote: q(1, 99),
        },
    );
    let mut a = r(Reference::Ask, Direction::AboveOrEqual, 101);
    a.order.limit = p(105);
    run(
        &mut rt,
        Command::SubmitConditional {
            market: 0,
            request: a,
        },
    );
    assert_eq!(rt.engine(0).unwrap().account().reserved_cash(), 0);
    let triggered = run(
        &mut rt,
        Command::Quote {
            market: 0,
            quote: q(2, 100),
        },
    );
    assert!(triggered.notices.iter().any(|n| matches!(
        n,
        Notice::Conditional {
            event: ConditionalEvent::Triggered { .. },
            ..
        }
    )));
    assert_eq!(rt.engine(0).unwrap().account().position(), 0);
    assert_eq!(rt.engine(0).unwrap().account().reserved_cash(), 106);
    run(
        &mut rt,
        Command::Quote {
            market: 0,
            quote: q(3, 100),
        },
    );
    assert_eq!(rt.engine(0).unwrap().account().position(), 1);
    assert_eq!(rt.engine(0).unwrap().account().cash(), 9898);
    rt.check_invariants().unwrap();
}
#[test]
fn trigger_rechecks_resources_and_does_not_retry_after_cash_rejection() {
    let mut cfg = c();
    cfg.markets[0].engine.initial_cash = 50;
    let mut rt = PaperRuntime::new(cfg).unwrap();
    run(
        &mut rt,
        Command::Quote {
            market: 0,
            quote: q(1, 99),
        },
    );
    let a = r(Reference::Bid, Direction::AboveOrEqual, 100);
    run(
        &mut rt,
        Command::SubmitConditional {
            market: 0,
            request: a,
        },
    );
    let receipt = run(
        &mut rt,
        Command::Quote {
            market: 0,
            quote: q(2, 100),
        },
    );
    assert!(receipt.notices.iter().any(|n| matches!(
        n,
        Notice::Conditional {
            event: ConditionalEvent::ActivationRejected {
                reason: ActivationReject::Engine(RejectReason::InsufficientCash),
                ..
            },
            ..
        }
    )));
    run(
        &mut rt,
        Command::Quote {
            market: 0,
            quote: q(3, 100),
        },
    );
    assert_eq!(rt.engine(0).unwrap().metrics().rejected, 1);
    assert!(rt.conditionals(0).unwrap().is_empty());
}
#[test]
fn watchdog_halt_and_finish_cancel_latent_orders_before_any_activation() {
    for command in [
        Command::Advance {
            timestamp_ns: 1000000020,
        },
        Command::Halt {},
        Command::Finish {},
    ] {
        let mut rt = PaperRuntime::new(c()).unwrap();
        run(
            &mut rt,
            Command::Quote {
                market: 0,
                quote: q(1, 99),
            },
        );
        run(
            &mut rt,
            Command::SubmitConditional {
                market: 0,
                request: r(Reference::Bid, Direction::AboveOrEqual, 100),
            },
        );
        run(&mut rt, command);
        assert!(rt.conditionals(0).unwrap().is_empty());
        assert_eq!(rt.engine(0).unwrap().metrics().accepted, 0);
        rt.check_invariants().unwrap();
    }
}
#[test]
fn simultaneous_4096_triggers_commit_above_old_receipt_limit_and_restart_without_refiring() {
    use kaze_quant::store::{SqliteSession, StoreOptions};
    let path = std::env::temp_dir().join(format!("kaze-conditional-bulk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir(&path).unwrap();
    let mut cfg = c();
    cfg.markets[0].engine.max_orders = 8192;
    cfg.markets[0].engine.max_active_orders = 4096;
    cfg.markets[0].engine.max_position = 10000;
    cfg.markets[0].engine.initial_cash = 1_000_000_000;
    let db = path.join("run.db");
    let mut s = SqliteSession::open(&db, cfg.clone(), StoreOptions::default()).unwrap();
    s.execute_batch(&[Envelope {
        seq: 1,
        command: Command::Quote {
            market: 0,
            quote: q(1, 99),
        },
    }])
    .unwrap();
    for first in (0..4096).step_by(256) {
        let batch: Vec<_> = (first..first + 256)
            .map(|i| Envelope {
                seq: i as u64 + 2,
                command: Command::SubmitConditional {
                    market: 0,
                    request: r(Reference::Bid, Direction::AboveOrEqual, 100),
                },
            })
            .collect();
        s.execute_batch(&batch).unwrap();
    }
    let trigger = Envelope {
        seq: 4098,
        command: Command::Quote {
            market: 0,
            quote: q(2, 100),
        },
    };
    let receipt = s
        .execute_batch(std::slice::from_ref(&trigger))
        .unwrap()
        .remove(0);
    assert_eq!(s.runtime().engine(0).unwrap().metrics().accepted, 4096);
    assert_eq!(
        s.runtime().conditionals(0).unwrap().metrics().triggered,
        4096
    );
    assert!(serde_json::to_vec(&receipt).unwrap().len() > 65536);
    let before = s.runtime().report();
    drop(s);
    let mut s = SqliteSession::open(&db, cfg, StoreOptions::default()).unwrap();
    assert!(s.execute_batch(&[trigger]).unwrap()[0].duplicate);
    assert_eq!(s.runtime().report(), before);
    assert_eq!(s.verify_full().unwrap(), 4098);
    drop(s);
    std::fs::remove_dir_all(path).unwrap();
}
