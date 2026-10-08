use kaze_quant::{
    config::*,
    managed::{Control, MemberConfig, Parameters},
    paper::*,
    store::{SqliteSession, StoreOptions},
    types::*,
};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};
fn p(n: u64) -> Price {
    Price::new(n).unwrap()
}
fn config(children: Vec<StrategyConfig>) -> PaperConfig {
    let mut c: PaperConfig =
        serde_json::from_str(include_str!("../configs/iceberg-demo.json")).unwrap();
    c.markets[0].engine.initial_cash = 10000;
    c.markets[0].engine.max_position = 100;
    c.markets[0].risk.price_collar_bps = 10000;
    let n = children.len();
    let params = Parameters {
        members: children
            .into_iter()
            .enumerate()
            .map(|(i, strategy)| MemberConfig {
                name: format!("member-{i}"),
                initial_cash: 10000 / n as i128,
                max_position: 50,
                strategy,
            })
            .collect(),
        max_working: 64,
    };
    c.markets[0].strategy = StrategyConfig::Registered {
        name: "managed".into(),
        version: 1,
        parameters: serde_json::to_value(params).unwrap(),
    };
    c
}
fn composition() -> StrategyConfig {
    let c: PaperConfig =
        serde_json::from_str(include_str!("../configs/target-twap-v2-demo.json")).unwrap();
    let StrategyConfig::Composition { mut plan } = c.markets[0].strategy.clone() else {
        unreachable!()
    };
    plan.sizing = kaze_quant::target::SizingConfig::FixedLots { lots: 5 };
    plan.max_child_lots = 5;
    plan.schedule = kaze_quant::target::Schedule::Immediate;
    StrategyConfig::Composition { plan }
}
fn quote(n: u64, liq: u64) -> Command {
    Command::Quote {
        market: 0,
        quote: Quote {
            sequence: n,
            timestamp_ns: n,
            bid: p(100),
            ask: p(101),
            bid_quantity: liq,
            ask_quantity: liq,
        },
    }
}
fn control(owner: usize, operation: Control) -> Command {
    Command::StrategyControl {
        market: 0,
        owner,
        operation,
    }
}
fn run(r: &mut PaperRuntime, c: Command) -> Receipt {
    let seq = r.processed() + 1;
    let x = r.process(&Envelope { seq, command: c }).unwrap();
    r.check_invariants().unwrap();
    x
}
fn start(r: &mut PaperRuntime, owner: usize) {
    run(r, control(owner, Control::Init));
    run(r, control(owner, Control::Start));
}
fn members(r: &PaperRuntime) -> Value {
    r.report().markets[0].strategy_diagnostics.as_ref().unwrap()["members"].clone()
}
fn request(side: Side, qty: u64) -> OrderRequest {
    OrderRequest {
        side,
        quantity: Quantity::new(qty).unwrap(),
        limit: p(101),
        time_in_force: TimeInForce::GoodTilCancelled,
    }
}
fn owned(owner: usize, action: Action) -> Command {
    Command::StrategyAction {
        market: 0,
        owner,
        action,
    }
}
fn rejected(receipt: &Receipt, reason: RiskReject) {
    assert!(
        receipt
            .notices
            .iter()
            .any(|n| matches!(n,Notice::RiskRejected{reason:r,..}if *r==reason))
    );
}
#[test]
fn shared_liquidity_partial_fee_subledgers_and_owner_stop_hand_ledger() {
    let mut r = PaperRuntime::new(config(vec![composition(), composition()])).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    run(&mut r, quote(1, 0));
    assert_eq!(r.engine(0).unwrap().active_count(), 2);
    run(&mut r, quote(2, 3));
    let m = members(&r);
    assert_eq!(m[0]["position_units"], 3);
    assert_eq!(m[1]["position_units"], 0);
    assert_eq!(r.engine(0).unwrap().account().position(), 3);
    run(&mut r, quote(3, 3));
    let m = members(&r);
    assert_eq!(m[0]["position_units"], 5);
    assert_eq!(m[1]["position_units"], 1);
    assert_eq!(m[0]["cash_minor"], "4493");
    assert_eq!(m[1]["cash_minor"], "4898");
    rejected(
        &run(&mut r, owned(0, Action::Cancel(OrderId(2)))),
        RiskReject::Ownership,
    );
    assert!(
        r.engine(0)
            .unwrap()
            .order(OrderId(2))
            .unwrap()
            .status
            .is_active()
    );
    rejected(
        &run(
            &mut r,
            Command::Cancel {
                market: 0,
                order_id: OrderId(2),
            },
        ),
        RiskReject::Ownership,
    );
    run(&mut r, control(0, Control::Stop));
    assert_eq!(r.engine(0).unwrap().active_count(), 1);
    run(&mut r, quote(4, 4));
    let m = members(&r);
    assert_eq!(m[0]["position_units"], 5);
    assert_eq!(m[1]["position_units"], 5);
    assert_eq!(r.engine(0).unwrap().account().cash(), 8986);
    assert_eq!(r.engine(0).unwrap().account().fees_paid(), 4);
    run(&mut r, control(1, Control::Stop));
    run(
        &mut r,
        Command::StrategyTransfer {
            market: 0,
            from: 0,
            to: 1,
            amount: 300,
        },
    );
    let m = members(&r);
    assert_eq!(m[0]["cash_minor"], "4193");
    assert_eq!(m[1]["cash_minor"], "4793");
    assert_eq!(r.engine(0).unwrap().account().cash(), 8986);
}
#[test]
fn explicit_lifecycle_invalid_transitions_do_not_mutate_and_resume_retains_parent() {
    let c = config(vec![composition(), composition()]);
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    let before = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    assert!(
        r.process(&Envelope {
            seq: 1,
            command: control(0, Control::Start)
        })
        .is_err()
    );
    assert_eq!(before, serde_json::to_value(r.snapshot().unwrap()).unwrap());
    run(&mut r, quote(1, 5));
    assert_eq!(r.engine(0).unwrap().active_count(), 0);
    start(&mut r, 0);
    run(&mut r, quote(2, 0));
    run(&mut r, quote(3, 2));
    run(&mut r, control(0, Control::Stop));
    assert_eq!(r.engine(0).unwrap().account().position(), 2);
    assert_eq!(r.engine(0).unwrap().active_count(), 0);
    r = PaperRuntime::restore(c, r.snapshot().unwrap()).unwrap();
    run(&mut r, control(0, Control::Start));
    run(&mut r, quote(4, 0));
    assert_eq!(
        r.engine(0)
            .unwrap()
            .orders()
            .iter()
            .find(|o| o.status.is_active())
            .unwrap()
            .request
            .quantity
            .units(),
        3
    );
    run(&mut r, quote(5, 3));
    assert_eq!(members(&r)[0]["position_units"], 5);
}
#[test]
fn no_borrowing_cash_or_stock_from_another_member() {
    let mut c = config(vec![StrategyConfig::Passive {}, StrategyConfig::Passive {}]);
    let v = &mut match &mut c.markets[0].strategy {
        StrategyConfig::Registered { parameters, .. } => parameters,
        _ => unreachable!(),
    };
    v["members"][0]["initial_cash"] = json!("100");
    v["members"][1]["initial_cash"] = json!("9900");
    let mut r = PaperRuntime::new(c).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    run(&mut r, quote(1, 0));
    rejected(
        &run(&mut r, owned(0, Action::Submit(request(Side::Buy, 1)))),
        RiskReject::StrategyBudget,
    );
    run(&mut r, owned(1, Action::Submit(request(Side::Buy, 2))));
    run(&mut r, quote(2, 2));
    rejected(
        &run(&mut r, owned(0, Action::Submit(request(Side::Sell, 1)))),
        RiskReject::StrategyBudget,
    );
    assert_eq!(members(&r)[1]["position_units"], 2);
    rejected(
        &run(
            &mut r,
            Command::Submit {
                market: 0,
                request: request(Side::Buy, 1),
            },
        ),
        RiskReject::Ownership,
    );
}
fn bracket() -> StrategyConfig {
    let c: PaperConfig =
        serde_json::from_str(include_str!("../configs/breakout-bracket-demo.json")).unwrap();
    c.markets[0].strategy.clone()
}
#[test]
fn identical_brackets_have_separate_conditions_oco_and_fill_owners() {
    let c = config(vec![bracket(), bracket()]);
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    let source: Vec<Envelope> = include_str!("../data/breakout-bracket-demo.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    for mut e in source {
        if let Command::Quote { quote, .. } = &mut e.command {
            quote.bid_quantity = 6;
            quote.ask_quantity = 6;
        }
        run(&mut r, e.command);
        let bytes = serde_json::to_vec(&r.snapshot().unwrap()).unwrap();
        r = PaperRuntime::restore(c.clone(), serde_json::from_slice(&bytes).unwrap()).unwrap();
    }
    assert_eq!(r.engine(0).unwrap().account().position(), 0);
    let m = members(&r);
    assert_eq!(m[0]["position_units"], 0);
    assert_eq!(m[1]["position_units"], 0);
    assert!(r.engine(0).unwrap().metrics().fills >= 4);
    assert!(m[0]["fees_minor"].as_str().unwrap().parse::<u64>().unwrap() > 0);
    assert!(m[1]["fees_minor"].as_str().unwrap().parse::<u64>().unwrap() > 0);
}
#[test]
fn stop_one_bracket_cancels_only_its_waiting_conditions() {
    let mut r = PaperRuntime::new(config(vec![bracket(), bracket()])).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    run(&mut r, quote(1, 0));
    assert_eq!(r.conditionals(0).unwrap().len(), 2);
    run(&mut r, control(0, Control::Stop));
    assert_eq!(r.conditionals(0).unwrap().len(), 1);
    assert_eq!(members(&r)[1]["waiting_conditions"], 1);
}
#[test]
fn stopped_transfer_is_free_capital_only_and_preserves_total() {
    let mut r = PaperRuntime::new(config(vec![
        StrategyConfig::Passive {},
        StrategyConfig::Passive {},
    ]))
    .unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    let before = r.report();
    assert!(
        r.process(&Envelope {
            seq: r.processed() + 1,
            command: Command::StrategyTransfer {
                market: 0,
                from: 0,
                to: 1,
                amount: 1
            }
        })
        .is_err()
    );
    assert_eq!(before, r.report());
    run(&mut r, control(0, Control::Stop));
    run(&mut r, control(1, Control::Stop));
    run(
        &mut r,
        Command::StrategyTransfer {
            market: 0,
            from: 0,
            to: 1,
            amount: 5000,
        },
    );
    assert_eq!(members(&r)[0]["cash_minor"], "0");
    assert_eq!(members(&r)[1]["cash_minor"], "10000");
    for (from, to, amount) in [(0, 0, 1), (0, 1, 1), (1, 0, -1), (9, 0, 1)] {
        assert!(
            r.process(&Envelope {
                seq: r.processed() + 1,
                command: Command::StrategyTransfer {
                    market: 0,
                    from,
                    to,
                    amount
                }
            })
            .is_err()
        );
    }
    r.check_invariants().unwrap();
}
#[test]
fn malformed_accounts_ownership_child_references_and_capital_are_rejected_without_panic() {
    let c = config(vec![composition(), composition()]);
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    run(&mut r, quote(1, 0));
    let state = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    fn path(s: &mut Value) -> &mut Value {
        s.pointer_mut("/markets/0/strategy/registered/state")
            .unwrap()
    }
    for variant in 0..7 {
        let mut s = state.clone();
        let m = path(&mut s);
        match variant {
            0 => m["orders"]["1"]["owner"] = json!(1),
            1 => m["members"][0]["account"]["cash"] = json!("9999"),
            2 => m["members"][0]["account"]["position"] = json!(u64::MAX),
            3 => m["members"][0]["capital"] = json!("0"),
            4 => m["orders"]["1"]["reserve_per_unit"] = json!(i128::MAX.to_string()),
            5 => m["members"][0]["account"]["net_buy_notional"] = json!(i128::MIN.to_string()),
            _ => m["members"][0]["child"]["state"]["Composition"]["child"]["id"] = json!(2),
        };
        let saved = serde_json::from_value(s).unwrap();
        assert!(
            PaperRuntime::restore(c.clone(), saved).is_err(),
            "variant {variant}"
        );
    }
}
#[test]
fn aggregate_capital_duplicate_nested_or_legacy_child_configuration_rejected() {
    for variant in 0..4 {
        let mut c = config(vec![composition(), composition()]);
        let StrategyConfig::Registered { parameters, .. } = &mut c.markets[0].strategy else {
            unreachable!()
        };
        match variant {
            0 => parameters["members"][0]["initial_cash"] = json!("5001"),
            1 => parameters["members"][1]["name"] = json!("member-0"),
            2 => {
                parameters["members"][0]["strategy"] =
                    json!({"type":"registered","name":"managed","version":1,"parameters":{}})
            }
            _ => parameters["members"][0]["strategy"]["plan"]["version"] = json!(1),
        };
        assert!(PaperRuntime::new(c).is_err(), "variant {variant}");
    }
}
#[test]
fn global_position_limit_still_limits_sum_of_members() {
    let mut c = config(vec![composition(), composition()]);
    c.markets[0].engine.max_position = 5;
    let StrategyConfig::Registered { parameters, .. } = &mut c.markets[0].strategy else {
        unreachable!()
    };
    for m in parameters["members"].as_array_mut().unwrap() {
        m["max_position"] = json!(5);
    }
    let mut r = PaperRuntime::new(c).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    run(&mut r, quote(1, 0));
    assert_eq!(r.engine(0).unwrap().active_count(), 1);
    assert_eq!(r.engine(0).unwrap().account().pending_buy(), 5);
    assert_eq!(members(&r)[1]["active_orders"], 0);
}
#[test]
fn finish_halts_every_member_and_releases_work_but_preserves_actual_positions() {
    let c = config(vec![composition(), composition()]);
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    run(&mut r, quote(1, 0));
    run(&mut r, quote(2, 2));
    run(&mut r, Command::Finish {});
    assert_eq!(r.engine(0).unwrap().account().position(), 2);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 0);
    assert_eq!(members(&r)[1]["lifecycle"], "stopped");
    PaperRuntime::restore(c, r.snapshot().unwrap()).unwrap();
}
static SERIAL: AtomicU64 = AtomicU64::new(0);
#[test]
fn every_command_sqlite_restart_dedupe_and_failed_batch_preserve_original_ownership_and_state() {
    let dir = std::env::temp_dir().join(format!(
        "kaze-managed-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    let db = dir.join("session.db");
    let c = config(vec![composition(), composition()]);
    let opts = StoreOptions::default();
    let mut memory = PaperRuntime::new(c.clone()).unwrap();
    memory.configure_retention(64).unwrap();
    let inputs = [
        control(0, Control::Init),
        control(1, Control::Init),
        control(0, Control::Start),
        control(1, Control::Start),
        quote(1, 0),
        quote(2, 3),
        owned(0, Action::Cancel(OrderId(2))),
        control(0, Control::Stop),
        quote(3, 5),
        control(1, Control::Stop),
        Command::StrategyTransfer {
            market: 0,
            from: 0,
            to: 1,
            amount: 100,
        },
        Command::Finish {},
    ];
    for command in inputs {
        let e = Envelope {
            seq: memory.processed() + 1,
            command,
        };
        let expected = memory.process(&e).unwrap();
        memory.compact_checkpoint();
        let mut sql = SqliteSession::open(&db, c.clone(), opts.clone()).unwrap();
        assert_eq!(
            sql.execute_batch(std::slice::from_ref(&e)).unwrap()[0],
            expected
        );
        assert!(sql.execute_batch(std::slice::from_ref(&e)).unwrap()[0].duplicate);
        assert_eq!(sql.runtime().report(), memory.report());
        sql.verify_full().unwrap();
        drop(sql);
        let state = memory.snapshot().unwrap();
        memory = PaperRuntime::restore(
            c.clone(),
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap(),
        )
        .unwrap();
    }
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn invalid_lifecycle_later_in_batch_rolls_back_fill_fee_and_stop() {
    let dir = std::env::temp_dir().join(format!(
        "kaze-managed-rollback-{}-{}",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&dir).unwrap();
    let db = dir.join("s.db");
    let c = config(vec![composition(), composition()]);
    let mut sql = SqliteSession::open(&db, c, StoreOptions::default()).unwrap();
    for command in [
        control(0, Control::Init),
        control(0, Control::Start),
        quote(1, 0),
    ] {
        let seq = sql.runtime().processed() + 1;
        sql.execute_batch(&[Envelope { seq, command }]).unwrap();
    }
    let before = sql.runtime().report();
    assert!(
        sql.execute_batch(&[
            Envelope {
                seq: 4,
                command: quote(2, 2)
            },
            Envelope {
                seq: 5,
                command: control(0, Control::Stop)
            },
            Envelope {
                seq: 6,
                command: control(0, Control::Stop)
            }
        ])
        .is_err()
    );
    assert_eq!(before, sql.runtime().report());
    sql.verify_full().unwrap();
    drop(sql);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn ready_warmup_updates_indicators_but_cannot_place_orders() {
    let mut s = composition();
    let StrategyConfig::Composition { plan } = &mut s else {
        unreachable!()
    };
    plan.signal = kaze_quant::target::SignalConfig::SmaCross {
        fast: 1,
        slow: 2,
        band_bps: 0,
    };
    let c = config(vec![s, StrategyConfig::Passive {}]);
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    run(&mut r, control(0, Control::Init));
    run(&mut r, quote(1, 10));
    run(&mut r, quote(2, 10));
    assert_eq!(r.engine(0).unwrap().active_count(), 0);
    let b = serde_json::to_vec(&r.snapshot().unwrap()).unwrap();
    r = PaperRuntime::restore(c, serde_json::from_slice(&b).unwrap()).unwrap();
    run(&mut r, control(0, Control::Start));
    let mut q = quote(3, 0);
    if let Command::Quote { quote, .. } = &mut q {
        quote.bid = p(102);
        quote.ask = p(103);
    }
    run(&mut r, q);
    assert_eq!(r.engine(0).unwrap().active_count(), 1);
}
#[test]
fn owned_condition_activation_rechecks_budget_releases_identity_and_respects_work_capacity() {
    use kaze_quant::conditional::*;
    let mut c = config(vec![StrategyConfig::Passive {}, StrategyConfig::Passive {}]);
    let StrategyConfig::Registered { parameters, .. } = &mut c.markets[0].strategy else {
        unreachable!()
    };
    parameters["max_working"] = json!(1);
    parameters["members"][0]["initial_cash"] = json!("0");
    parameters["members"][1]["initial_cash"] = json!("10000");
    let mut r = PaperRuntime::new(c).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    run(&mut r, quote(1, 0));
    let cond = ConditionalRequest {
        reference: Reference::Ask,
        direction: Direction::AboveOrEqual,
        trigger: p(101),
        order: request(Side::Buy, 2),
        expires_at_ns: None,
        oco_group: Some(1),
    };
    run(&mut r, owned(0, Action::SubmitConditional(cond)));
    assert_eq!(r.conditionals(0).unwrap().len(), 1);
    rejected(
        &run(&mut r, owned(1, Action::Submit(request(Side::Buy, 2)))),
        RiskReject::StrategyCapacity,
    );
    let receipt = run(&mut r, quote(2, 0));
    rejected(&receipt, RiskReject::StrategyBudget);
    assert_eq!(r.conditionals(0).unwrap().len(), 0);
    assert_eq!(members(&r)[0]["waiting_conditions"], 0);
    run(&mut r, owned(1, Action::SubmitConditional(cond)));
    run(&mut r, quote(3, 0));
    assert_eq!(r.conditionals(0).unwrap().len(), 0);
    assert_eq!(r.engine(0).unwrap().active_count(), 1);
    assert_eq!(members(&r)[1]["active_orders"], 1);
    assert_eq!(members(&r)[1]["pending_buy_units"], 2);
}

#[test]
fn per_member_decision_receipts_are_causal_and_stopped_member_has_no_stale_trace() {
    let mut r = PaperRuntime::new(config(vec![composition(), composition()])).unwrap();
    start(&mut r, 0);
    start(&mut r, 1);
    let first = run(&mut r, quote(1, 0));
    let rows: Vec<_> = first
        .notices
        .iter()
        .filter_map(|n| {
            if let Notice::OwnedDecision {
                owner, decision, ..
            } = n
            {
                Some((*owner, decision.quote_sequence))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(rows, vec![(0, 1), (1, 1)]);
    run(&mut r, control(0, Control::Stop));
    let next = run(&mut r, quote(2, 3));
    let rows: Vec<_> = next
        .notices
        .iter()
        .filter_map(|n| {
            if let Notice::OwnedDecision {
                owner, decision, ..
            } = n
            {
                Some((*owner, decision.quote_sequence))
            } else {
                None
            }
        })
        .collect();
    assert_eq!(rows, vec![(1, 2)]);
}

#[test]
fn oversized_or_overflowed_member_windows_are_rejected_before_child_allocation() {
    for child in [
        StrategyConfig::Momentum {
            window: 1_000_000,
            quantity: Quantity::new(1).unwrap(),
        },
        StrategyConfig::SmaCross {
            fast: usize::MAX,
            slow: usize::MAX,
            band_bps: 0,
            quantity: Quantity::new(1).unwrap(),
        },
    ] {
        assert!(PaperRuntime::new(config(vec![child, StrategyConfig::Passive {}])).is_err());
    }
}
