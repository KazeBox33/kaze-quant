#[path = "support/plan_fixture.rs"]
mod support;
use kaze_quant::{
    decimal::{self, SCALE},
    execution::*,
    external_target::TargetRequest,
    recovery::HistoryHead,
};
use support::{fixture::*, *};
fn request() -> TargetRequest {
    serde_json::from_str(include_str!("../configs/net-target-preview.json")).unwrap()
}
fn ready() -> (Scratch, ExecutionJournal, Sim) {
    let t = Scratch::new();
    let j = bound(&t);
    (t, j, Sim::new())
}
fn init() -> (Scratch, ExecutionJournal, Sim) {
    let (t, mut j, mut v) = ready();
    j.init_net_target(&mut v, request()).unwrap();
    (t, j, v)
}
#[test]
fn dropped_ack_restores_original_target_child_without_new_submit() {
    let (t, mut j, mut v) = init();
    v.drop_ack = true;
    v.base_fee = true;
    j.tick_plan(&mut v, 1000).unwrap();
    assert_eq!(j.net_target_status().unwrap()["phase"], "submit_unknown");
    drop(j);
    j = t.open();
    v.drop_ack = false;
    assert_eq!(j.tick_plan(&mut v, 1000).unwrap().post_calls, 0);
    assert_eq!(v.posts, 1);
    assert_eq!(
        j.net_target_status().unwrap()["base_fee_remaining"],
        "0.00007000"
    );
}
#[test]
fn stale_initial_reconciliation_does_not_commit_mandate_or_plan() {
    let (t, mut j, mut v) = ready();
    let mut r = request();
    r.max_reconciliation_ms = 1;
    v.delay = true;
    assert!(j.init_net_target(&mut v, r).is_err());
    let c = rusqlite::Connection::open(t.0.join("external.db")).unwrap();
    for table in ["net_target", "execution_plan", "intents"] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(v.posts, 0);
}
#[test]
fn mandate_parent_and_base_fee_feedback_survive_every_process_open() {
    let (t, mut j, mut v) = init();
    v.base_fee = true;
    for time in [1000, 1100, 1200] {
        j.tick_plan(&mut v, time).unwrap();
        drop(j);
        j = t.open();
        assert_eq!(
            j.net_target_status().unwrap()["phase"],
            "needs_reconciliation"
        );
        assert!(
            !j.net_target_status().unwrap()["confirmed_satisfied"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(j.tick_plan(&mut v, time).unwrap().post_calls, 0);
    }
    let s = j.net_target_status().unwrap();
    assert_eq!(s["phase"], "satisfied");
    assert_eq!(s["current_net_quantity"], "0.09990000");
    assert_eq!(s["base_fee_remaining"], "0.00000000");
    assert_eq!(s["quote_fee_remaining"], "0.01000000");
    assert_eq!(v.posts, 3);
    assert!(j.submit_once(&mut v, &intent(9), 100 * SCALE).is_err());
    assert_eq!(v.posts, 3);
}
#[test]
fn completed_gross_with_net_residual_never_generates_automatic_repair() {
    let (t, mut j, mut v) = ready();
    let mut r = request();
    r.tolerance_quantity = "0".into();
    j.init_net_target(&mut v, r.clone()).unwrap();
    v.base_fee = true;
    for time in [1000, 1100, 1200, 1201] {
        j.tick_plan(&mut v, time).unwrap();
    }
    assert_eq!(
        j.net_target_status().unwrap()["phase"],
        "completed_with_residual"
    );
    assert_eq!(
        j.net_target_status().unwrap()["residual_quantity"],
        "0.00010000"
    );
    drop(j);
    j = t.open();
    j.init_net_target(&mut v, r.clone()).unwrap();
    for time in 1202..1210 {
        assert_eq!(j.tick_plan(&mut v, time).unwrap().post_calls, 0);
    }
    r.target_quantity = "0.11".into();
    assert!(j.init_net_target(&mut v, r).is_err());
    assert_eq!(v.posts, 3);
}
#[test]
fn base_and_quote_budget_excess_remain_blocked_across_restart_and_resume() {
    for base in [false, true] {
        let (t, mut j, mut v) = ready();
        let mut r = request();
        if base {
            r.base_fee_reserve = "0.00001".into();
        } else {
            r.quote_fee_reserve = "0.001".into();
        }
        j.init_net_target(&mut v, r.clone()).unwrap();
        v.base_fee = base;
        j.tick_plan(&mut v, 1000).unwrap();
        assert_eq!(j.tick_plan(&mut v, 1100).unwrap().post_calls, 0);
        let s = j.net_target_status().unwrap();
        assert_eq!(s["phase"], "budget_or_bound_violation");
        assert!(j.plan_status().unwrap().paused);
        drop(j);
        j = t.open();
        assert!(j.resume_plan(&mut v).is_err());
        j.init_net_target(&mut v, r).unwrap();
        assert_eq!(j.tick_plan(&mut v, 1200).unwrap().post_calls, 0);
        assert_eq!(v.posts, 1);
    }
}
#[test]
fn violating_partial_fill_pauses_and_cancels_original_child() {
    let (_, mut j, mut v) = ready();
    let mut r = request();
    r.base_fee_reserve = "0.000001".into();
    j.init_net_target(&mut v, r).unwrap();
    v.base_fee = true;
    v.fill = false;
    j.tick_plan(&mut v, 1000).unwrap();
    v.fill_order(1, 500000);
    let result = j.tick_plan(&mut v, 1010).unwrap();
    assert!(result.cancel_attempted);
    assert_eq!(v.posts, 1);
    assert_eq!(v.cancels, 1);
    assert_eq!(j.tick_plan(&mut v, 1100).unwrap().post_calls, 0);
}
#[test]
fn unknown_cancel_keeps_mandate_and_query_only_until_late_terminal_fill() {
    let (t, mut j, mut v) = init();
    v.fill = false;
    v.lost_cancel = true;
    j.tick_plan(&mut v, 1000).unwrap();
    v.fill_order(1, 1000000);
    j.tick_plan(&mut v, 1010).unwrap();
    assert_eq!(j.net_target_status().unwrap()["phase"], "cancel_pending");
    drop(j);
    j = t.open();
    assert_eq!(j.tick_plan(&mut v, 1100).unwrap().operation, "query_only");
    assert!(j.resume_plan(&mut v).is_err());
    assert_eq!(v.posts, 1);
    assert_eq!(v.cancels, 1);
    v.fill_order(1, 2000000);
    v.fill = true;
    j.tick_plan(&mut v, 1200).unwrap();
    j.tick_plan(&mut v, 1201).unwrap();
    j.tick_plan(&mut v, 1202).unwrap();
    assert_eq!(v.posts, 3);
    assert_eq!(j.net_target_status().unwrap()["phase"], "satisfied");
    assert_eq!(v.cancels, 1);
}
#[test]
fn fresh_free_resources_are_rechecked_without_spending_locked_quote() {
    let (_, mut j, mut v) = init();
    j.tick_plan(&mut v, 1000).unwrap();
    for b in &mut v.f.account.balances {
        if b.asset == "USDT" {
            b.locked = b.free.clone();
            b.free = "0".into();
        }
    }
    assert!(j.tick_plan(&mut v, 1100).is_err());
    assert!(j.plan_status().unwrap().paused);
    assert_eq!(v.posts, 1);
}
#[test]
fn parent_and_mandate_initialization_roll_back_together_on_sql_failure() {
    let (t, mut j, mut v) = ready();
    let c = rusqlite::Connection::open(t.0.join("external.db")).unwrap();
    c.execute_batch("CREATE TRIGGER fail_target BEFORE INSERT ON net_target BEGIN SELECT RAISE(ABORT,'fixture init failure'); END;").unwrap();
    assert!(j.init_net_target(&mut v, request()).is_err());
    for table in ["net_target", "execution_plan", "intents"] {
        assert_eq!(
            c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    assert_eq!(
        c.query_row("SELECT revision FROM execution_meta", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "binance-testnet-execution-v1"
    );
    assert_eq!(v.posts, 0);
}
#[test]
fn checksum_or_revision_conflict_is_rejected_on_open() {
    for checksum in [false, true] {
        let (t, j, v) = init();
        drop(j);
        let c = rusqlite::Connection::open(t.0.join("external.db")).unwrap();
        c.execute_batch(if checksum {
            "UPDATE net_target SET checksum='bad';"
        } else {
            "UPDATE execution_meta SET revision='binance-testnet-execution-v2-plan';"
        })
        .unwrap();
        assert!(ExecutionJournal::open(&t.0.join("external.db")).is_err());
        assert_eq!(v.posts, 0);
    }
}
#[test]
fn immutable_initialization_does_not_reset_clock_children_or_budget() {
    let (_, mut j, mut v) = init();
    j.tick_plan(&mut v, 1000).unwrap();
    let before = j.plan_status().unwrap();
    j.init_net_target(&mut v, request()).unwrap();
    let after = j.plan_status().unwrap();
    assert_eq!(before.start_ms, after.start_ms);
    assert_eq!(before.children, after.children);
    assert_eq!(before.ticks, after.ticks);
    assert_eq!(v.posts, 1);
    let mut r = request();
    r.base_fee_reserve = "0.01".into();
    assert!(j.init_net_target(&mut v, r).is_err());
    assert_eq!(v.posts, 1);
}
#[test]
fn nonexecutable_preview_never_persists_target_or_parent() {
    for value in ["0.00005", "0.02"] {
        let (t, mut j, mut v) = ready();
        let mut r = request();
        r.target_quantity = value.into();
        assert!(j.init_net_target(&mut v, r).is_err());
        let c = rusqlite::Connection::open(t.0.join("external.db")).unwrap();
        assert_eq!(
            c.query_row("SELECT count(*) FROM net_target", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(v.posts, 0);
    }
}
#[test]
fn sell_net_target_reserve_is_not_reported_as_complete_liquidation() {
    let t = Scratch::new();
    let mut j = t.open();
    let mut v = Sim::new();
    v.f.account.balances[0].free = "0.10".into();
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
    let mut r = request();
    r.target_quantity = "0".into();
    r.tolerance_quantity = "0.01".into();
    j.init_net_target(&mut v, r).unwrap();
    v.base_fee = true;
    for time in [1000, 1100, 1200, 1201] {
        j.tick_plan(&mut v, time).unwrap();
    }
    let s = j.net_target_status().unwrap();
    assert_eq!(s["phase"], "satisfied");
    assert_eq!(s["residual_quantity"], "0.00991000");
    assert_eq!(s["current_net_quantity"], "0.00991000");
    assert_eq!(v.posts, 3);
}
#[test]
fn third_currency_fee_is_accounted_but_blocks_more_target_orders() {
    let t = Scratch::new();
    let mut j = t.open();
    let mut v = Sim::new();
    v.f.account.balances.push(Balance {
        asset: "BNB".into(),
        free: "1".into(),
        locked: "0".into(),
    });
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
    j.init_net_target(&mut v, request()).unwrap();
    j.tick_plan(&mut v, 1000).unwrap();
    let fee = decimal::parse(&v.f.trades[0].commission).unwrap();
    v.f.trades[0].commission_asset = "BNB".into();
    for b in &mut v.f.account.balances {
        if b.asset == "USDT" {
            b.free = decimal::format(decimal::parse(&b.free).unwrap() + fee).unwrap();
        } else if b.asset == "BNB" {
            b.free = decimal::format(SCALE - fee).unwrap();
        }
    }
    assert_eq!(j.tick_plan(&mut v, 1100).unwrap().post_calls, 0);
    assert_eq!(v.posts, 1);
    assert_eq!(
        j.net_target_status().unwrap()["violation"],
        "net target unsupported fee asset"
    );
    assert!(
        j.audit().unwrap()["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(j.expected_balances().unwrap()["BNB"], "0.99700000");
}
