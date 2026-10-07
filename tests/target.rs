use kaze_quant::{
    config::*,
    engine::Engine,
    paper::*,
    store::{SqliteSession, StoreOptions},
    strategy::{Strategy, StrategyView},
    target::*,
    types::*,
};
use std::sync::atomic::{AtomicU64, Ordering};
fn plan() -> CompositionConfig {
    CompositionConfig {
        version: 1,
        signal: SignalConfig::Constant {
            exposure_bps: 10000,
        },
        sizing: SizingConfig::FixedLots { lots: 10 },
        max_spread_bps: 10000,
        cooldown_ns: 0,
        min_rebalance_lots: 1,
        max_child_lots: 10,
        max_working_ns: 1000,
        schedule: Schedule::Immediate,
    }
}
fn config(p: CompositionConfig) -> PaperConfig {
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
    c.markets.truncate(1);
    let m = &mut c.markets[0];
    m.engine.initial_cash = 10000;
    m.engine.latency_ns = 0;
    m.engine.fee_bps = 10;
    m.price_tick = 1;
    m.quantity_step = 1;
    m.strategy = StrategyConfig::Composition { plan: Box::new(p) };
    c
}
fn q(seq: u64, time: u64, bid: u64, ask: u64, liquidity: u64) -> Envelope {
    Envelope {
        seq,
        command: Command::Quote {
            market: 0,
            quote: Quote {
                sequence: seq,
                timestamp_ns: time,
                bid: Price::new(bid).unwrap(),
                ask: Price::new(ask).unwrap(),
                bid_quantity: liquidity,
                ask_quantity: liquidity,
            },
        },
    }
}
fn quote(e: &Envelope) -> Quote {
    if let Command::Quote { quote, .. } = e.command {
        quote
    } else {
        unreachable!()
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
#[test]
fn repeated_target_and_partial_fills_do_not_duplicate_pending_exposure() {
    let c = config(plan());
    let mut r = PaperRuntime::new(c).unwrap();
    let a = r.process(&q(1, 1, 100, 101, 3)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::Accepted);
    assert_eq!(r.engine(0).unwrap().account().pending_buy(), 10);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 1020);
    assert_eq!(r.report().markets[0].position_units, 0);
    let b = r.process(&q(2, 2, 100, 101, 3)).unwrap();
    assert_eq!(decision(&b).projected_lots, 10);
    assert_eq!(decision(&b).reason, DecisionReason::Working);
    assert_eq!(r.report().markets[0].metrics.accepted, 1);
    assert_eq!(r.engine(0).unwrap().account().cash(), 9696);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 714);
    r.process(&q(3, 3, 100, 101, 7)).unwrap();
    r.process(&q(4, 4, 100, 101, 7)).unwrap();
    assert_eq!(r.report().markets[0].position_units, 10);
    assert_eq!(r.report().markets[0].cash_minor, 8988);
    assert_eq!(r.report().markets[0].fees_minor, 2);
    assert_eq!(r.report().markets[0].metrics.accepted, 1);
    r.check_invariants().unwrap();
}
#[test]
fn target_reversal_cancels_remaining_before_selling_actual_partial_position() {
    let mut p = plan();
    p.signal = SignalConfig::Threshold {
        buy_below: Price::new(105).unwrap(),
        sell_above: Price::new(110).unwrap(),
    };
    let mut r = PaperRuntime::new(config(p)).unwrap();
    r.process(&q(1, 1, 99, 100, 0)).unwrap();
    r.process(&q(2, 2, 99, 100, 4)).unwrap();
    let a = r.process(&q(3, 3, 111, 112, 0)).unwrap();
    assert_eq!(decision(&a).action, Action::Cancel(OrderId(1)));
    assert_eq!(r.report().markets[0].metrics.accepted, 1);
    assert_eq!(r.report().markets[0].position_units, 4);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 0);
    let b = r.process(&q(4, 4, 111, 112, 0)).unwrap();
    assert!(matches!(
        decision(&b).action,
        Action::Submit(OrderRequest {
            side: Side::Sell,
            ..
        })
    ));
    assert_eq!(r.engine(0).unwrap().account().reserved_sell(), 4);
    r.process(&q(5, 5, 111, 112, 4)).unwrap();
    assert_eq!(r.report().markets[0].position_units, 0);
    assert_eq!(r.report().markets[0].cash_minor, 10042);
    assert_eq!(r.report().markets[0].fees_minor, 2);
    assert_eq!(r.report().markets[0].metrics.accepted, 2);
}
#[test]
fn spread_gate_cancels_pending_buys_and_does_not_block_risk_reduction() {
    let mut p = plan();
    p.max_spread_bps = 100;
    let mut r = PaperRuntime::new(config(p)).unwrap();
    let a = r.process(&q(1, 1, 100, 150, 0)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::SpreadBlocked);
    assert_eq!(r.report().markets[0].metrics.accepted, 0);
    r.process(&q(2, 2, 100, 101, 0)).unwrap();
    r.process(&q(3, 3, 100, 101, 3)).unwrap();
    let a = r.process(&q(4, 4, 100, 150, 0)).unwrap();
    assert!(matches!(decision(&a).action, Action::Cancel(_)));
    assert_eq!(r.report().markets[0].position_units, 3);
    let mut p = plan();
    p.max_spread_bps = 100;
    p.signal = SignalConfig::Threshold {
        buy_below: Price::new(105).unwrap(),
        sell_above: Price::new(110).unwrap(),
    };
    let mut r = PaperRuntime::new(config(p)).unwrap();
    r.process(&q(1, 1, 100, 101, 10)).unwrap();
    r.process(&q(2, 2, 100, 101, 10)).unwrap();
    let a = r.process(&q(3, 3, 150, 200, 0)).unwrap();
    assert!(matches!(
        decision(&a).action,
        Action::Submit(OrderRequest {
            side: Side::Sell,
            ..
        })
    ));
}
#[test]
fn cooldown_does_not_cancel_own_working_order_or_block_exit() {
    let mut p = plan();
    p.cooldown_ns = 100;
    p.max_child_lots = 5;
    let mut r = PaperRuntime::new(config(p)).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    let a = r.process(&q(2, 2, 100, 101, 1)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::Working);
    let a = r.process(&q(3, 3, 100, 101, 4)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::Cooldown);
    assert_eq!(r.report().markets[0].position_units, 5);
    r.process(&q(4, 101, 100, 101, 0)).unwrap();
    assert_eq!(r.report().markets[0].metrics.accepted, 2);
}
#[test]
fn grid_and_twap_integer_remainder_are_exact_and_never_fill_same_quote() {
    let mut p = plan();
    p.schedule = Schedule::Twap {
        interval_ns: 10,
        slices: 3,
    };
    p.max_child_lots = 6;
    p.min_rebalance_lots = 2;
    let mut c = config(p);
    c.markets[0].quantity_step = 2;
    let mut r = PaperRuntime::new(c).unwrap();
    let a = r.process(&q(1, 1, 100, 101, 10)).unwrap();
    assert_eq!(decision(&a).authorized_lots, 2);
    assert_eq!(r.report().markets[0].position_units, 0);
    r.process(&q(2, 2, 100, 101, 2)).unwrap();
    let a = r.process(&q(3, 11, 100, 101, 10)).unwrap();
    assert_eq!(decision(&a).authorized_lots, 6);
    r.process(&q(4, 12, 100, 101, 4)).unwrap();
    let a = r.process(&q(5, 21, 100, 101, 10)).unwrap();
    assert_eq!(decision(&a).authorized_lots, 10);
    r.process(&q(6, 22, 100, 101, 4)).unwrap();
    let quantities: Vec<_> = r
        .engine(0)
        .unwrap()
        .orders()
        .iter()
        .map(|o| o.request.quantity.units())
        .collect();
    assert_eq!(quantities, [2, 4, 4]);
    assert_eq!(r.report().markets[0].position_units, 10);
    assert_eq!(r.report().markets[0].metrics.accepted, 3);
}
#[test]
fn missed_slice_times_and_working_partial_order_never_create_burst_children() {
    let mut p = plan();
    p.schedule = Schedule::Twap {
        interval_ns: 10,
        slices: 3,
    };
    p.max_child_lots = 3;
    let mut r = PaperRuntime::new(config(p)).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    r.process(&q(2, 100, 100, 101, 0)).unwrap();
    assert_eq!(r.report().markets[0].metrics.accepted, 1);
    r.process(&q(3, 101, 100, 101, 3)).unwrap();
    assert_eq!(r.report().markets[0].metrics.accepted, 2);
    assert_eq!(r.engine(0).unwrap().account().pending_buy(), 3);
}
#[test]
fn expired_child_is_cancelled_and_replaced_only_on_later_quote() {
    let mut p = plan();
    p.max_working_ns = 10;
    let mut r = PaperRuntime::new(config(p)).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    let a = r.process(&q(2, 11, 100, 105, 0)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::WorkingExpired);
    assert_eq!(r.report().markets[0].metrics.accepted, 1);
    r.process(&q(3, 12, 100, 105, 0)).unwrap();
    assert_eq!(r.report().markets[0].metrics.accepted, 2);
}
#[test]
fn pending_cancel_survives_strategy_restore_without_replacement_permission() {
    let mut p = plan();
    p.signal = SignalConfig::Threshold {
        buy_below: Price::new(105).unwrap(),
        sell_above: Price::new(110).unwrap(),
    };
    let mut s = CompositionStrategy::new(p.clone()).unwrap();
    let c = config(p.clone());
    let mut e = Engine::new(c.markets[0].engine.clone()).unwrap();
    let constraints = Constraints::from_market(&c.markets[0]);
    let qt = quote(&q(1, 1, 99, 100, 0));
    e.on_quote(qt, &mut ()).unwrap();
    let a = s.decide(
        StrategyView {
            quote: qt,
            account: e.account(),
            active_orders: 0,
        },
        constraints,
    );
    let Action::Submit(req) = a else {
        panic!("expected buy")
    };
    let id = e.submit(req, &mut ()).unwrap();
    s.on_submit_result(req, Ok(id));
    let qt = quote(&q(2, 2, 111, 112, 0));
    e.on_quote(qt, &mut ()).unwrap();
    assert_eq!(
        s.decide(
            StrategyView {
                quote: qt,
                account: e.account(),
                active_orders: 1
            },
            constraints
        ),
        Action::Cancel(id)
    );
    let mut s: CompositionStrategy =
        serde_json::from_value(serde_json::to_value(s).unwrap()).unwrap();
    s.validate_state(&p).unwrap();
    s.validate_execution(&e).unwrap();
    let qt = quote(&q(3, 3, 111, 112, 0));
    e.on_quote(qt, &mut ()).unwrap();
    assert_eq!(
        s.decide(
            StrategyView {
                quote: qt,
                account: e.account(),
                active_orders: 1
            },
            constraints
        ),
        Action::None
    );
    assert_eq!(s.decision().unwrap().reason, DecisionReason::CancelPending);
    assert_eq!(e.active_count(), 1);
    let mut events = vec![];
    e.cancel(id, &mut events);
    for event in events {
        s.on_event(event);
    }
    assert_eq!(
        s.decide(
            StrategyView {
                quote: qt,
                account: e.account(),
                active_orders: 0
            },
            constraints
        ),
        Action::None
    );
}
#[test]
fn cash_sizing_includes_per_unit_fee_and_platform_capacity_rejection_clears_proposal() {
    let mut p = plan();
    p.sizing = SizingConfig::CashBudget { budget_minor: 306 };
    let mut c = config(p);
    c.markets[0].engine.initial_cash = 306;
    let mut r = PaperRuntime::new(c).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    assert_eq!(r.engine(0).unwrap().account().pending_buy(), 3);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 306);
    let mut p = plan();
    p.max_child_lots = 5;
    let mut c = config(p);
    c.markets[0].engine.max_orders = 1;
    c.markets[0].engine.max_active_orders = 1;
    let mut r = PaperRuntime::new(c).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    let a = r.process(&q(2, 2, 100, 101, 5)).unwrap();
    assert_eq!(
        decision(&a).reason,
        DecisionReason::RiskRejected(RiskReject::HistoryCapacity)
    );
    r.snapshot().unwrap();
    let a = r.process(&q(3, 3, 100, 101, 0)).unwrap();
    assert_eq!(
        decision(&a).reason,
        DecisionReason::RiskRejected(RiskReject::HistoryCapacity)
    );
}
#[test]
fn manual_order_is_not_silently_adopted_or_cancelled_by_composition() {
    let mut r = PaperRuntime::new(config(plan())).unwrap();
    r.process(&q(1, 1, 100, 101, 10)).unwrap();
    r.process(&q(2, 2, 100, 101, 10)).unwrap();
    r.process(&Envelope {
        seq: 3,
        command: Command::Submit {
            market: 0,
            request: OrderRequest {
                side: Side::Sell,
                limit: Price::new(110).unwrap(),
                quantity: Quantity::new(2).unwrap(),
                time_in_force: TimeInForce::GoodTilCancelled,
            },
        },
    })
    .unwrap();
    let a = r.process(&q(4, 4, 100, 101, 0)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::ForeignWorking);
    assert_eq!(r.report().markets[0].active_orders, 1);
}
#[test]
fn malformed_parameters_and_engine_child_corruption_fail_closed() {
    let mut p = plan();
    p.signal = SignalConfig::SmaCross {
        fast: 1,
        slow: usize::MAX,
        band_bps: 0,
    };
    assert!(CompositionStrategy::new(p).is_err());
    let mut p = plan();
    p.version = 3;
    assert!(config(p).validate().is_err());
    let c = config(plan());
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    r.process(&q(1, 1, 100, 101, 0)).unwrap();
    let value = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    for (field, bad) in [
        ("remaining", serde_json::json!(1)),
        ("id", serde_json::json!(999)),
    ] {
        let mut v = value.clone();
        v["markets"][0]["strategy"]["Composition"]["child"][field] = bad;
        assert!(PaperRuntime::restore(c.clone(), serde_json::from_value(v).unwrap()).is_err());
    }
    let mut v = value;
    v["markets"][0]["strategy"]["Composition"]["parent"]["total"] = serde_json::json!(11);
    assert!(PaperRuntime::restore(c, serde_json::from_value(v).unwrap()).is_err());
}
static NEXT: AtomicU64 = AtomicU64::new(0);
#[test]
fn failed_transaction_rolls_back_partial_fill_and_child_progress_together() {
    let dir = std::env::temp_dir().join(format!(
        "kaze-target-rollback-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    let db = dir.join("run.db");
    let c = config(plan());
    let mut s = SqliteSession::open(&db, c.clone(), StoreOptions::default()).unwrap();
    s.execute_batch(&[q(1, 1, 100, 101, 3), q(2, 2, 100, 101, 3)])
        .unwrap();
    let old = serde_json::to_value(s.runtime().snapshot().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_target BEFORE INSERT ON commands BEGIN SELECT RAISE(ABORT,'target injected failure'); END;").unwrap();
    assert!(s.execute_batch(&[q(3, 3, 100, 101, 3)]).is_err());
    assert_eq!(
        serde_json::to_value(s.runtime().snapshot().unwrap()).unwrap(),
        old
    );
    drop(s);
    conn.execute_batch("DROP TRIGGER fail_target;").unwrap();
    drop(conn);
    let mut s = SqliteSession::open(&db, c, StoreOptions::default()).unwrap();
    assert_eq!(
        serde_json::to_value(s.runtime().snapshot().unwrap()).unwrap(),
        old
    );
    assert_eq!(s.verify_full().unwrap(), 2);
    s.execute_batch(&[q(3, 3, 100, 101, 3)]).unwrap();
    assert_eq!(s.runtime().report().markets[0].position_units, 6);
    assert_eq!(s.runtime().engine(0).unwrap().account().pending_buy(), 4);
    assert_eq!(s.verify_full().unwrap(), 3);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn sqlite_batch_retries_restarts_and_failed_write_keep_identical_parent_child_receipts() {
    let dir = std::env::temp_dir().join(format!(
        "kaze-target-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    let db = dir.join("run.db");
    let mut p = plan();
    p.schedule = Schedule::Twap {
        interval_ns: 10,
        slices: 3,
    };
    let c = config(p);
    let commands: Vec<_> = (1..=30).map(|seq| q(seq, seq, 100, 101, 2)).collect();
    let mut reference = PaperRuntime::new(c.clone()).unwrap();
    let expected: Vec<_> = commands
        .iter()
        .map(|e| reference.process(e).unwrap())
        .collect();
    for batch in commands.chunks(3) {
        let mut s = SqliteSession::open(&db, c.clone(), StoreOptions::default()).unwrap();
        let got = s.execute_batch(batch).unwrap();
        for r in got {
            assert_eq!(r, expected[r.seq as usize - 1]);
        }
        assert_eq!(s.execute_batch(batch).unwrap().len(), batch.len());
        assert!(s.execute_batch(batch).unwrap().iter().all(|r| r.duplicate));
        s.verify_full().unwrap();
    }
    let mut s = SqliteSession::open(&db, c.clone(), StoreOptions::default()).unwrap();
    assert_eq!(s.runtime().report(), reference.report());
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch("CREATE TRIGGER fail_target BEFORE INSERT ON commands BEGIN SELECT RAISE(ABORT,'target injected failure'); END;").unwrap();
    let old = s.runtime().report();
    assert!(s.execute_batch(&[q(31, 31, 100, 101, 2)]).is_err());
    assert_eq!(s.runtime().report(), old);
    drop(s);
    conn.execute_batch("DROP TRIGGER fail_target;").unwrap();
    drop(conn);
    let s = SqliteSession::open(&db, c, StoreOptions::default()).unwrap();
    assert_eq!(s.runtime().report(), old);
    assert_eq!(s.verify_full().unwrap(), 30);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn signal_prefixes_and_every_checkpoint_produce_identical_causal_decisions() {
    let mut p = plan();
    p.signal = SignalConfig::SmaCross {
        fast: 2,
        slow: 3,
        band_bps: 0,
    };
    let c = config(p);
    let prices = [100, 98, 105, 108, 104, 101, 96, 106, 109, 103, 99, 95];
    let commands: Vec<_> = prices
        .iter()
        .enumerate()
        .map(|(i, &bid)| q(i as u64 + 1, i as u64 + 1, bid, bid + 1, 2))
        .collect();
    let mut full = PaperRuntime::new(c.clone()).unwrap();
    let mut resumed = PaperRuntime::new(c.clone()).unwrap();
    let mut receipts = vec![];
    for cmd in &commands {
        let r = full.process(cmd).unwrap();
        assert_eq!(r, resumed.process(cmd).unwrap());
        receipts.push(r);
        resumed = PaperRuntime::restore(c.clone(), resumed.snapshot().unwrap()).unwrap();
    }
    for end in 1..=commands.len() {
        let mut prefix = PaperRuntime::new(c.clone()).unwrap();
        let got: Vec<_> = commands[..end]
            .iter()
            .map(|e| prefix.process(e).unwrap())
            .collect();
        assert_eq!(got, receipts[..end]);
    }
    assert_eq!(full.report(), resumed.report());
}
#[test]
fn research_warmup_seeds_indicators_but_never_transfers_training_positions() {
    use kaze_quant::research::*;
    let mut p = plan();
    p.signal = SignalConfig::SmaCross {
        fast: 2,
        slow: 3,
        band_bps: 0,
    };
    let c = config(p);
    let s = c.markets[0].strategy.clone();
    let quotes: Vec<_> = [100, 99, 105, 110, 110]
        .iter()
        .enumerate()
        .map(|(i, &bid)| Ok(quote(&q(i as u64 + 1, i as u64 + 1, bid, bid + 1, 20))))
        .collect();
    let fold = Fold {
        train_start_ns: 1,
        train_end_ns: 3,
        test_start_ns: 4,
        test_end_ns: 5,
    };
    let cost = CostScenario {
        name: "fixed".into(),
        fee_bps: 10,
        latency_ns: 0,
        slippage_bps: 0,
        liquidity_model: kaze_quant::engine::LiquidityModel::QuoteRefresh,
    };
    let result = evaluate(&c, &s, &fold, true, &cost, quotes).unwrap();
    assert_eq!(result.quote_count, 2);
    assert_eq!(result.state.metrics.accepted, 1);
    assert_eq!(result.state.position_units, 10);
    // 10 × 111 + ceil(1110 × 10 / 10000) = 1112；训练不消费现金。
    assert_eq!(result.state.cash_minor, 8888);
    assert_eq!(result.state.fees_minor, 2);
}
#[test]
fn invalid_context_zero_budget_position_cap_dust_and_single_slice_are_bounded() {
    let c = config(plan());
    let e = Engine::new(c.markets[0].engine.clone()).unwrap();
    let mut s = CompositionStrategy::new(plan()).unwrap();
    let mut constraints = Constraints::from_market(&c.markets[0]);
    constraints.quantity_step = 0;
    let qt = quote(&q(1, u64::MAX, 100, 101, 0));
    assert_eq!(
        s.decide(
            StrategyView {
                quote: qt,
                account: e.account(),
                active_orders: 0
            },
            constraints
        ),
        Action::None
    );
    assert_eq!(s.decision().unwrap().reason, DecisionReason::InvalidContext);
    let mut p = plan();
    p.schedule = Schedule::Twap {
        interval_ns: 1,
        slices: 1,
    };
    let mut c = config(p);
    c.markets[0].risk.max_order_notional = 50;
    let mut r = PaperRuntime::new(c).unwrap();
    let a = r.process(&q(1, u64::MAX, 100, 101, 0)).unwrap();
    assert_eq!(decision(&a).reason, DecisionReason::Budget);
    assert_eq!(decision(&a).authorized_lots, 10);
    r.snapshot().unwrap();
    let mut p = plan();
    p.sizing = SizingConfig::CashBudget {
        budget_minor: 100000,
    };
    p.min_rebalance_lots = 5;
    let mut c = config(p);
    c.markets[0].engine.max_position = 3;
    let mut r = PaperRuntime::new(c).unwrap();
    let a = r.process(&q(1, 1, 100, 101, 0)).unwrap();
    assert_eq!(decision(&a).target_lots, 3);
    assert_eq!(decision(&a).reason, DecisionReason::Dust);
}
