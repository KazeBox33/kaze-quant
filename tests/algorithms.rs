use kaze_quant::{
    config::*,
    paper::*,
    store::{SqliteSession, StoreOptions},
    strategy::{Strategy, StrategyView},
    target::*,
    types::*,
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
fn price(v: u64) -> Price {
    Price::new(v).unwrap()
}
fn plan(schedule: Schedule, lots: u64) -> CompositionConfig {
    CompositionConfig {
        version: 1,
        signal: SignalConfig::Constant {
            exposure_bps: 10000,
        },
        sizing: SizingConfig::FixedLots { lots },
        max_spread_bps: 10000,
        cooldown_ns: 0,
        min_rebalance_lots: 1,
        max_child_lots: 10,
        max_working_ns: 1000,
        schedule,
    }
}
fn config(p: CompositionConfig) -> PaperConfig {
    let mut c: PaperConfig =
        serde_json::from_str(include_str!("../configs/target-twap-demo.json")).unwrap();
    c.markets[0].risk.price_collar_bps = 10000;
    c.markets[0].strategy = StrategyConfig::Composition { plan: Box::new(p) };
    c
}
fn iceberg() -> Schedule {
    Schedule::Iceberg {
        limit: price(102),
        display_lots: 3,
        replenish_interval_ns: 5,
    }
}
fn best() -> Schedule {
    Schedule::BestLimit {
        limit_guard: price(120),
        min_reprice_ns: 0,
    }
}
fn q(seq: u64, time: u64, bid: u64, ask: u64, liquidity: u64) -> Envelope {
    Envelope {
        seq,
        command: Command::Quote {
            market: 0,
            quote: Quote {
                sequence: seq,
                timestamp_ns: time,
                bid: price(bid),
                ask: price(ask),
                bid_quantity: liquidity,
                ask_quantity: liquidity,
            },
        },
    }
}
fn decision(r: &Receipt) -> &Decision {
    r.notices
        .iter()
        .find_map(|n| {
            if let Notice::StrategyDecision { decision, .. } = n {
                Some(decision)
            } else {
                None
            }
        })
        .unwrap()
}
fn iceberg_inputs() -> Vec<Envelope> {
    vec![
        q(1, 1, 100, 101, 0),
        q(2, 2, 100, 101, 1),
        q(3, 3, 100, 101, 1),
        q(4, 4, 100, 101, 1),
        q(5, 6, 100, 101, 0),
        q(6, 7, 100, 101, 3),
        q(7, 100, 100, 101, 0),
        q(8, 101, 100, 101, 3),
        q(9, 105, 100, 101, 0),
        q(10, 106, 100, 101, 1),
    ]
}
fn best_inputs() -> Vec<Envelope> {
    vec![
        q(1, 1, 100, 101, 0),
        q(2, 2, 99, 100, 2),
        q(3, 3, 99, 100, 0),
        q(4, 4, 98, 99, 1),
        q(5, 5, 98, 99, 0),
        q(6, 6, 97, 98, 2),
    ]
}
fn progress(r: &PaperRuntime) -> serde_json::Value {
    r.report().markets[0].strategy_diagnostics.as_ref().unwrap()["parent"]["progress"].clone()
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "kaze-algo-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn db(&self) -> PathBuf {
        self.0.join("session.db")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn iceberg_display_replenishment_no_burst_and_partial_fee_hand_ledger() {
    let mut r = PaperRuntime::new(config(plan(iceberg(), 10))).unwrap();
    for e in iceberg_inputs() {
        let receipt = r.process(&e).unwrap();
        assert!(r.engine(0).unwrap().account().pending_buy() <= 3);
        if e.seq == 1 {
            assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 309);
            assert_eq!(r.report().markets[0].position_units, 0);
        }
        if [4, 6, 8].contains(&e.seq) {
            assert_eq!(decision(&receipt).reason, DecisionReason::ReplenishWaiting);
        }
        if e.seq == 7 {
            assert_eq!(r.report().markets[0].metrics.accepted, 3);
        }
        r.check_invariants().unwrap();
    }
    let m = &r.report().markets[0];
    assert_eq!(
        (
            m.position_units,
            m.cash_minor,
            m.fees_minor,
            m.metrics.fills
        ),
        (10, 8984, 6, 6)
    );
    let quantities: Vec<_> = r
        .engine(0)
        .unwrap()
        .orders()
        .iter()
        .map(|o| o.request.quantity.units())
        .collect();
    assert_eq!(quantities, [3, 3, 3, 1]);
    assert_eq!(
        progress(&r),
        serde_json::json!({"filled_lots":10,"submitted_children":4,"cancelled_children":0,"reprice_requests":0})
    );
}
#[test]
fn fixed_limit_resources_use_limit_including_fee_instead_of_current_ask() {
    let mut c = config(plan(
        Schedule::Iceberg {
            limit: price(120),
            display_lots: 3,
            replenish_interval_ns: 0,
        },
        10,
    ));
    c.markets[0].engine.initial_cash = 210;
    let mut r = PaperRuntime::new(c).unwrap();
    r.process(&q(1, 1, 99, 100, 0)).unwrap();
    assert_eq!(r.engine(0).unwrap().account().pending_buy(), 1);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 121);
    assert_eq!(r.report().markets[0].metrics.rejected, 0);
}
#[test]
fn best_limit_partial_fill_before_reprice_ack_and_never_overfills_parent() {
    let mut r = PaperRuntime::new(config(plan(best(), 5))).unwrap();
    for e in best_inputs() {
        let a = r.process(&e).unwrap();
        if [2, 4].contains(&e.seq) {
            assert_eq!(decision(&a).reason, DecisionReason::PriceMoved);
            assert!(matches!(decision(&a).action, Action::Cancel(_)));
            assert_eq!(r.report().markets[0].active_orders, 0);
        }
        r.check_invariants().unwrap();
    }
    let m = &r.report().markets[0];
    assert_eq!((m.position_units, m.cash_minor, m.fees_minor), (5, 9502, 3));
    let orders = r.engine(0).unwrap().orders().to_vec();
    assert_eq!(
        orders
            .iter()
            .map(|o| (o.request.limit.units(), o.request.quantity.units()))
            .collect::<Vec<_>>(),
        [(100, 5), (99, 3), (98, 2)]
    );
    assert_eq!(
        progress(&r),
        serde_json::json!({"filled_lots":5,"submitted_children":3,"cancelled_children":2,"reprice_requests":2})
    );
}
#[test]
fn best_limit_reprice_debounce_and_price_guard_both_cancel_safely() {
    let mut r = PaperRuntime::new(config(plan(
        Schedule::BestLimit {
            limit_guard: price(101),
            min_reprice_ns: 50,
        },
        5,
    )))
    .unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    assert_eq!(
        decision(&r.process(&q(2, 2, 101, 102, 0)).unwrap()).reason,
        DecisionReason::Working
    );
    assert_eq!(
        decision(&r.process(&q(3, 51, 101, 102, 0)).unwrap()).reason,
        DecisionReason::PriceMoved
    );
    r.process(&q(4, 52, 101, 102, 0)).unwrap();
    assert_eq!(
        decision(&r.process(&q(5, 53, 102, 103, 0)).unwrap()).reason,
        DecisionReason::PriceGuard
    );
    assert_eq!(
        decision(&r.process(&q(6, 54, 102, 103, 0)).unwrap()).reason,
        DecisionReason::PriceGuard
    );
    assert_eq!(r.report().markets[0].metrics.accepted, 2);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 0);
    r.process(&q(7, 55, 100, 101, 0)).unwrap();
    assert_eq!(r.report().markets[0].metrics.accepted, 3);
}
#[test]
fn pending_reprice_without_cancel_ack_survives_restore_and_never_replaces() {
    let p = plan(best(), 5);
    let c = config(p.clone());
    let m = &c.markets[0];
    let mut e = kaze_quant::engine::Engine::new(m.engine.clone()).unwrap();
    let mut s = CompositionStrategy::new(p.clone()).unwrap();
    let constraints = Constraints::from_market(m);
    let mut events = Vec::new();
    fn view(quote: Quote, engine: &kaze_quant::engine::Engine) -> StrategyView<'_> {
        StrategyView {
            quote,
            account: engine.account(),
            active_orders: engine.active_count(),
        }
    }
    let a = if let Command::Quote { quote, .. } = q(1, 1, 100, 101, 0).command {
        quote
    } else {
        unreachable!()
    };
    e.on_quote(a, &mut events).unwrap();
    let Action::Submit(req) = s.decide(view(a, &e), constraints) else {
        panic!()
    };
    let id = e.submit(req, &mut events);
    s.on_submit_result(req, id);
    let b = Quote {
        sequence: 2,
        timestamp_ns: 2,
        bid: price(101),
        ask: price(102),
        ..a
    };
    e.on_quote(b, &mut events).unwrap();
    assert_eq!(
        s.decide(view(b, &e), constraints),
        Action::Cancel(id.unwrap())
    );
    s = serde_json::from_value(serde_json::to_value(s).unwrap()).unwrap();
    s.validate_state(&p).unwrap();
    s.validate_execution(&e).unwrap();
    let d = Quote {
        sequence: 3,
        timestamp_ns: 3,
        ..b
    };
    e.on_quote(d, &mut events).unwrap();
    assert_eq!(s.decide(view(d, &e), constraints), Action::None);
    assert_eq!(s.decision().unwrap().reason, DecisionReason::CancelPending);
    assert_eq!(e.active_count(), 1);
}
#[test]
fn both_algorithms_every_checkpoint_and_sqlite_restart_equal_original_receipts() {
    for (p, inputs) in [
        (plan(iceberg(), 10), iceberg_inputs()),
        (plan(best(), 5), best_inputs()),
    ] {
        let c = config(p);
        let t = Scratch::new();
        let mut reference = PaperRuntime::new(c.clone()).unwrap();
        let mut restored = PaperRuntime::new(c.clone()).unwrap();
        for input in inputs {
            let expected = reference.process(&input).unwrap();
            restored = PaperRuntime::restore(c.clone(), restored.snapshot().unwrap()).unwrap();
            assert_eq!(restored.process(&input).unwrap(), expected);
            let mut durable =
                SqliteSession::open(&t.db(), c.clone(), StoreOptions::default()).unwrap();
            assert_eq!(
                durable.execute_batch(std::slice::from_ref(&input)).unwrap()[0],
                expected
            );
            let state = durable.runtime().report();
            assert!(durable.execute_batch(std::slice::from_ref(&input)).unwrap()[0].duplicate);
            assert_eq!(state, durable.runtime().report());
            assert_eq!(durable.verify_full().unwrap(), input.seq);
            assert_eq!(state, reference.report());
        }
    }
}
#[test]
fn failed_transaction_rolls_back_partial_fill_cancel_and_parent_counters() {
    let t = Scratch::new();
    let c = config(plan(best(), 5));
    let mut s = SqliteSession::open(&t.db(), c.clone(), StoreOptions::default()).unwrap();
    let inputs = best_inputs();
    s.execute_batch(&inputs[..1]).unwrap();
    let before = s.runtime().report();
    let bad = Envelope {
        seq: 3,
        command: Command::Cancel {
            market: 99,
            order_id: OrderId(1),
        },
    };
    assert!(s.execute_batch(&[inputs[1].clone(), bad]).is_err());
    assert_eq!(s.runtime().report(), before);
    drop(s);
    let mut s = SqliteSession::open(&t.db(), c, StoreOptions::default()).unwrap();
    s.execute_batch(&inputs[1..]).unwrap();
    assert_eq!(s.runtime().report().markets[0].cash_minor, 9502);
    assert_eq!(s.verify_full().unwrap(), 6);
}
#[test]
fn malformed_display_guard_clock_and_parent_progress_are_rejected() {
    assert!(
        plan(
            Schedule::Iceberg {
                limit: price(100),
                display_lots: 0,
                replenish_interval_ns: 0
            },
            10
        )
        .validate()
        .is_err()
    );
    assert!(
        plan(
            Schedule::BestLimit {
                limit_guard: price(100),
                min_reprice_ns: 86_400_000_000_001
            },
            10
        )
        .validate()
        .is_err()
    );
    let mut c = config(plan(iceberg(), 10));
    c.markets[0].quantity_step = 2;
    c.markets[0].strategy = StrategyConfig::Composition {
        plan: Box::new(CompositionConfig {
            min_rebalance_lots: 2,
            ..plan(iceberg(), 10)
        }),
    };
    assert!(c.validate().is_err());
    let c = config(plan(best(), 5));
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    r.process(&best_inputs()[0]).unwrap();
    let snapshot = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    assert!(snapshot["markets"][0]["strategy"]["Composition"]["parent"]["progress"].is_object());
    for (field, value) in [
        ("filled_lots", 6),
        ("reprice_requests", 2),
        ("submitted_children", 0),
        ("cancelled_children", 2),
    ] {
        let mut s = snapshot.clone();
        s["markets"][0]["strategy"]["Composition"]["parent"]["progress"][field] =
            serde_json::json!(value);
        let parsed = serde_json::from_value(s);
        assert!(
            parsed.is_err() || PaperRuntime::restore(c.clone(), parsed.unwrap()).is_err(),
            "{field}"
        );
    }
}
#[test]
fn foreign_completed_manual_fill_is_not_adopted_as_parent_progress() {
    let mut r = PaperRuntime::new(config(plan(best(), 3))).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    r.process(&q(2, 2, 99, 100, 3)).unwrap();
    r.process(&Envelope {
        seq: 3,
        command: Command::Submit {
            market: 0,
            request: OrderRequest {
                side: Side::Buy,
                limit: price(110),
                quantity: Quantity::new(1).unwrap(),
                time_in_force: TimeInForce::GoodTilCancelled,
            },
        },
    })
    .unwrap();
    let a = r.process(&q(4, 4, 99, 100, 1)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::ForeignPosition);
    assert_eq!(r.report().markets[0].position_units, 4);
    assert_eq!(progress(&r)["filled_lots"], 3);
    assert_eq!(r.report().markets[0].metrics.accepted, 2);
    let c = r.config().clone();
    let restored = PaperRuntime::restore(c, r.snapshot().unwrap()).unwrap();
    assert_eq!(restored.report(), r.report());
}
#[test]
fn best_limit_reversal_uses_actual_fills_and_sells_at_ask_then_waits() {
    let mut p = plan(
        Schedule::BestLimit {
            limit_guard: price(105),
            min_reprice_ns: 0,
        },
        5,
    );
    p.signal = SignalConfig::Threshold {
        buy_below: price(105),
        sell_above: price(110),
    };
    let mut r = PaperRuntime::new(config(p)).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    r.process(&q(2, 2, 99, 100, 2)).unwrap(); // 买2，其余3随本方价变化撤销。
    let a = r.process(&q(3, 3, 111, 112, 0)).unwrap();
    assert!(
        matches!(decision(&a).action,Action::Submit(OrderRequest{side:Side::Sell,limit,..}) if limit==price(112))
    );
    assert_eq!(r.engine(0).unwrap().account().reserved_sell(), 2);
    r.process(&q(4, 4, 111, 112, 10)).unwrap();
    assert_eq!(r.report().markets[0].position_units, 2); // bid未达到卖出限价。
    r.process(&q(5, 5, 112, 113, 2)).unwrap();
    let m = &r.report().markets[0];
    assert_eq!(
        (m.position_units, m.cash_minor, m.fees_minor),
        (0, 10022, 2)
    );
    assert_eq!(progress(&r)["filled_lots"], 2);
    assert_eq!(
        r.report().markets[0].strategy_diagnostics.as_ref().unwrap()["parent"]["side"],
        "Sell"
    );
}
#[test]
fn halt_releases_visible_order_without_liquidating_filled_position() {
    let mut r = PaperRuntime::new(config(plan(iceberg(), 10))).unwrap();
    r.process(&iceberg_inputs()[0]).unwrap();
    r.process(&iceberg_inputs()[1]).unwrap();
    r.process(&Envelope {
        seq: 3,
        command: Command::Halt {},
    })
    .unwrap();
    assert_eq!(r.report().markets[0].position_units, 1);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 0);
    assert_eq!(progress(&r)["filled_lots"], 1);
    assert_eq!(progress(&r)["cancelled_children"], 1);
    r.check_invariants().unwrap();
}
#[test]
fn known_price_band_is_filtered_before_proposal_without_platform_rejection() {
    let mut c = config(plan(
        Schedule::Iceberg {
            limit: price(120),
            display_lots: 3,
            replenish_interval_ns: 0,
        },
        10,
    ));
    c.markets[0].risk.price_collar_bps = 100;
    let mut r = PaperRuntime::new(c).unwrap();
    for (i, ask) in [101, 102, 119].into_iter().enumerate() {
        let a = r
            .process(&q(i as u64 + 1, i as u64 + 1, ask - 1, ask, 0))
            .unwrap();
        if i < 2 {
            assert_eq!(decision(&a).reason, DecisionReason::PriceBand);
            assert_eq!(progress(&r)["submitted_children"], 0);
        } else {
            assert_eq!(decision(&a).reason, DecisionReason::Accepted);
        }
    }
    assert_eq!(r.report().markets[0].risk_rejections, 0);
    assert_eq!(r.report().markets[0].metrics.accepted, 1);
}
#[test]
fn v2_twap_preserves_v1_original_receipts_and_tracks_actual_parent_fills() {
    let mut v1: PaperConfig =
        serde_json::from_str(include_str!("../configs/target-twap-demo.json")).unwrap();
    let v2: PaperConfig =
        serde_json::from_str(include_str!("../configs/target-twap-v2-demo.json")).unwrap();
    let mut old = PaperRuntime::new(v1.clone()).unwrap();
    let mut new = PaperRuntime::new(v2.clone()).unwrap();
    for line in include_str!("../data/target-twap.jsonl").lines() {
        let e: Envelope = serde_json::from_str(line).unwrap();
        assert_eq!(old.process(&e).unwrap(), new.process(&e).unwrap());
        new = PaperRuntime::restore(v2.clone(), new.snapshot().unwrap()).unwrap();
    }
    assert_eq!(
        progress(&new),
        serde_json::json!({"filled_lots":10,"submitted_children":3,"cancelled_children":0,"reprice_requests":0})
    );
    assert_eq!(
        (
            new.report().markets[0].position_units,
            new.report().markets[0].cash_minor
        ),
        (10, 8980)
    );
    assert!(old.report().markets[0].strategy_diagnostics.is_none());
    // 旧版本继续保留原配置/检查点格式，不凭空给旧会话改变手工仓位处理语义。
    if let StrategyConfig::Composition { plan } = &mut v1.markets[0].strategy {
        plan.version = 3;
    }
    assert!(v1.validate().is_err());
}
#[test]
fn v2_immediate_has_shared_parent_report_and_resource_contract() {
    let mut p = plan(Schedule::Immediate, 5);
    p.version = 2;
    let mut r = PaperRuntime::new(config(p)).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    r.process(&q(2, 2, 100, 101, 2)).unwrap();
    r.process(&q(3, 3, 100, 101, 3)).unwrap();
    assert_eq!(progress(&r)["filled_lots"], 5);
    assert_eq!(progress(&r)["submitted_children"], 1);
    r.check_invariants().unwrap();
}
