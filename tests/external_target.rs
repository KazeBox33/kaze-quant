#[path = "support/plan_fixture.rs"]
mod support;
use kaze_quant::{
    decimal::SCALE, execution::*, external_target::*, recovery::HistoryHead, types::Side,
};
use support::{fixture::*, *};
fn request() -> TargetRequest {
    TargetRequest {
        version: 1,
        plan_id: "kaze-net-target".into(),
        symbol: "BTCUSDT".into(),
        limit_price: "100".into(),
        target_quantity: "0.10".into(),
        tolerance_quantity: "0.0001".into(),
        base_fee_reserve: "0.0001".into(),
        quote_fee_reserve: "0.01".into(),
        max_gross_quantity: "0.10".into(),
        quantity_step: "0.01".into(),
        min_child_quantity: "0.01".into(),
        max_child_quantity: "0.04".into(),
        slices: 3,
        interval_ms: 100,
        max_working_ms: 10,
        max_reconciliation_ms: 60000,
        max_children: 32,
    }
}
fn baseline(a: AccountObservation) -> (Scratch, ExecutionJournal, Sim) {
    let t = Scratch::new();
    let mut j = t.open();
    let mut v = Sim::new();
    v.f.account = a;
    j.bind_account_with_history(
        &identity(),
        &v.f.account,
        &[binding()],
        &[HistoryHead {
            symbol: "BTCUSDT".into(),
            order_id: None,
            trade_id: None,
        }],
    )
    .unwrap();
    j.reconcile(&mut v).unwrap();
    (t, j, v)
}
fn input() -> AllocationInput {
    AllocationInput {
        net_base: 0,
        free_base: 0,
        free_quote: 11 * SCALE,
        target: SCALE / 10,
        tolerance: 10000,
        price: 100 * SCALE,
        step: SCALE / 100,
        min_total: 3 * SCALE / 100,
        max_gross: SCALE / 10,
        base_fee_reserve: 10000,
        quote_fee_reserve: SCALE / 100,
    }
}
#[test]
fn read_only_preview_compiles_buy_and_original_base_fee_stays_in_range() {
    let (t, mut j, mut v) = baseline(account(0));
    let r = request();
    let p = j.preview_target(&r).unwrap();
    assert_eq!(p["gross_quantity"], "0.10000000");
    assert_eq!(p["projected_net_low"], "0.09990000");
    assert_eq!(p["projected_net_high"], "0.10000000");
    assert_eq!(p["quote_required"], "10.01000000");
    assert_eq!(p["within_tolerance_if_reserves_hold"], true);
    assert_eq!(j.preview_target(&r).unwrap(), p);
    assert!(j.orders().unwrap().is_empty());
    assert_eq!(v.posts, 0);
    let plan = serde_json::from_value(p["plan"].clone()).unwrap();
    j.init_plan(plan).unwrap();
    v.base_fee = true;
    for time in [1000, 1100, 1200] {
        j.tick_plan(&mut v, time).unwrap();
        drop(j);
        j = t.open();
    }
    j.reconcile(&mut v).unwrap();
    assert_eq!(j.expected_balances().unwrap()["BTC"], "0.09990000");
    assert_eq!(v.posts, 3);
    assert!(j.preview_target(&r).is_err()); // 已有父计划不能编译第二个来绕过残量。
}
#[test]
fn sell_reserves_base_fees_and_reports_nonzero_closeout_residual() {
    let mut a = account(0);
    a.balances[0].free = "0.10".into();
    let (_, mut j, mut v) = baseline(a);
    let mut r = request();
    r.target_quantity = "0".into();
    r.tolerance_quantity = "0.01".into();
    let p = j.preview_target(&r).unwrap();
    assert_eq!(p["side"], "Sell");
    assert_eq!(p["gross_quantity"], "0.09000000");
    assert_eq!(p["base_required"], "0.09010000");
    assert_eq!(p["projected_net_low"], "0.00990000");
    assert_eq!(p["projected_net_high"], "0.01000000");
    j.init_plan(serde_json::from_value(p["plan"].clone()).unwrap())
        .unwrap();
    v.base_fee = true;
    for t in [1000, 1100, 1200] {
        j.tick_plan(&mut v, t).unwrap();
    }
    j.reconcile(&mut v).unwrap();
    assert_eq!(j.expected_balances().unwrap()["BTC"], "0.00991000");
    assert_eq!(v.posts, 3);
}
#[test]
fn locked_assets_contribute_to_net_but_never_to_available_sells() {
    let mut a = account(0);
    a.balances[0].free = "0.02".into();
    a.balances[0].locked = "0.08".into();
    let (_, j, v) = baseline(a);
    let mut r = request();
    r.target_quantity = "0".into();
    let p = j.preview_target(&r).unwrap();
    assert_eq!(p["net_quantity"], "0.10000000");
    assert_eq!(p["free_base"], "0.02000000");
    assert_eq!(p["decision"], "insufficient_resources");
    assert!(p["plan"].is_null());
    assert_eq!(v.posts, 0);
}
#[test]
fn hold_and_grid_dust_do_not_create_a_parent_or_intent() {
    let (_, j, v) = baseline(account(0));
    let mut r = request();
    r.target_quantity = "0.00005".into();
    let hold = j.preview_target(&r).unwrap();
    assert_eq!(hold["decision"], "hold");
    assert!(hold["plan"].is_null());
    r.target_quantity = "0.02".into();
    let dust = j.preview_target(&r).unwrap();
    assert_eq!(dust["decision"], "below_minimum");
    assert!(dust["plan"].is_null());
    assert!(j.orders().unwrap().is_empty());
    assert_eq!(v.posts, 0);
}
#[test]
fn fractional_quote_cost_rounds_up_and_fee_reserve_cannot_be_spent() {
    let mut i = input();
    i.price = 150000001;
    i.step = 1;
    i.min_total = 1;
    i.max_gross = 100;
    i.target = 100;
    i.tolerance = 0;
    i.base_fee_reserve = 0;
    i.quote_fee_reserve = 7;
    i.free_quote = 11;
    let a = allocate(i).unwrap();
    assert_eq!(a.gross, 2);
    assert_eq!(a.quote_required, 11);
    assert_eq!(a.projected_net_high, 2);
    i.free_quote = 6;
    assert_eq!(
        allocate(i).unwrap().decision,
        Decision::InsufficientResources
    );
}
#[test]
fn gross_cap_and_resources_produce_explicit_target_shortfall() {
    let mut i = input();
    i.target = SCALE;
    let a = allocate(i).unwrap();
    assert_eq!(a.gross, SCALE / 10);
    assert!(!a.within_tolerance_if_reserves_hold);
    assert_eq!(a.worst_residual, 90010000);
    i.free_quote = 5 * SCALE;
    let a = allocate(i).unwrap();
    assert_eq!(a.gross, 4 * SCALE / 100);
    assert_eq!(a.quote_required, 401000000);
}
#[test]
fn restart_gap_unknown_or_bound_plan_cannot_be_used_as_preview_readiness() {
    let (t, mut j, mut v) = baseline(account(0));
    drop(j);
    j = t.open();
    assert!(j.preview_target(&request()).is_err());
    j.reconcile(&mut v).unwrap();
    assert!(j.preview_target(&request()).is_ok());
    j.mark_execution_gap("fixture gap", false).unwrap();
    assert!(j.preview_target(&request()).is_err());
    j.reconcile(&mut v).unwrap();
    j.prepare(&intent(1), 100 * SCALE).unwrap();
    assert!(j.preview_target(&request()).is_err());
    assert_eq!(v.posts, 0);
}
#[test]
fn changed_or_untradeable_snapshot_cannot_borrow_previous_ready_state() {
    let (_, mut j, _) = baseline(account(0));
    let mut a = account(0);
    a.balances[1].free = "1".into();
    j.record_account(&a).unwrap();
    assert!(j.preview_target(&request()).is_err());
    a = account(0);
    a.can_trade = false;
    j.record_account(&a).unwrap();
    assert!(j.preview_target(&request()).is_err());
    a = account(0);
    a.balances.push(a.balances[0].clone());
    assert!(j.record_account(&a).is_err());
    assert!(j.preview_target(&request()).is_err());
}
#[test]
fn invalid_version_symbol_grid_capacity_and_economic_limits_are_rejected() {
    let (_, j, _) = baseline(account(0));
    for edit in 0..7 {
        let mut r = request();
        match edit {
            0 => r.version = 2,
            1 => r.symbol = "ETHUSDT".into(),
            2 => r.quantity_step = "0".into(),
            3 => r.max_gross_quantity = "2".into(),
            4 => r.max_child_quantity = "0.11".into(),
            5 => r.base_fee_reserve = "-1".into(),
            _ => r.slices = 0,
        };
        assert!(j.preview_target(&r).is_err());
    }
    let mut i = input();
    i.free_base = 1;
    assert!(allocate(i).is_err());
    i = input();
    i.price = 10i128.pow(30);
    i.target = 10i128.pow(30) + 1;
    i.max_gross = 10i128.pow(30);
    assert!(allocate(i).is_err());
}
#[test]
fn bounded_exhaustive_grid_oracle_matches_maximum_feasible_quantity() {
    // 不调用生产公式：枚举每个候选lot，逐一验证净目标、原币资源和费用预留。
    let mut cases = 0;
    for net in [0, 5, 15, 25] {
        for target in [0, 4, 10, 20, 30] {
            for base_reserve in [0, 1, 3] {
                for quote_reserve in [0, 2] {
                    for free_quote in [0, 4, 13, 70] {
                        for step in [1, 2, 3] {
                            for free_base in [0, net / 2, net] {
                                let i = AllocationInput {
                                    net_base: net,
                                    free_base,
                                    free_quote,
                                    target,
                                    tolerance: 1,
                                    price: 150000001,
                                    step,
                                    min_total: 2 * step,
                                    max_gross: 10 * step,
                                    base_fee_reserve: base_reserve,
                                    quote_fee_reserve: quote_reserve,
                                };
                                let a = allocate(i).unwrap();
                                let hold = (target - net).abs() <= 1;
                                let buy = target > net;
                                let mut best = 0;
                                if !hold {
                                    for q in (step..=i.max_gross).step_by(step as usize) {
                                        let cost = (q * 150000001 + SCALE - 1) / SCALE;
                                        let ok = free_quote >= quote_reserve
                                            && if buy {
                                                q >= base_reserve
                                                    && net + q <= target
                                                    && cost + quote_reserve <= free_quote
                                            } else {
                                                q + base_reserve <= free_base
                                                    && net - q - base_reserve >= target
                                            };
                                        if ok && q >= i.min_total {
                                            best = q;
                                        }
                                    }
                                }
                                assert_eq!(a.gross, best, "{i:?}");
                                assert_eq!(a.decision == Decision::Hold, hold);
                                if best > 0 {
                                    assert_eq!(
                                        a.side,
                                        Some(if buy { Side::Buy } else { Side::Sell })
                                    );
                                    assert!(a.quote_required <= free_quote);
                                    assert!(a.base_required <= free_base);
                                }
                                cases += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    assert_eq!(cases, 4320);
}
