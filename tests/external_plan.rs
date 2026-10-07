#[path = "support/plan_fixture.rs"]
mod support;
use kaze_quant::{
    decimal::{self, SCALE},
    external_plan::*,
};
use support::fixture::*;
use support::*;
#[test]
fn delayed_preflight_expires_ready_evidence_before_any_child_is_prepared() {
    let t = Scratch::new();
    let mut j = bound(&t);
    let mut c = config();
    c.max_reconciliation_ms = 1000;
    j.init_plan(c).unwrap();
    let mut v = Sim::new();
    v.preflight_delay = true;
    let e = j.tick_plan(&mut v, 1000).unwrap_err();
    assert!(e.0.contains("after preflight"));
    assert_eq!(v.posts, 0);
    assert!(j.orders().unwrap().is_empty());
}
#[test]
fn fractional_cancel_remainder_is_dust_and_never_rounded_into_extra_exposure() {
    let (_, mut j, mut v) = initialized();
    v.fill = false;
    j.tick_plan(&mut v, 1000).unwrap();
    v.fill_order(1, 500000);
    j.tick_plan(&mut v, 1010).unwrap();
    v.fill = true;
    for t in [1011, 1100, 1200, 1201] {
        j.tick_plan(&mut v, t).unwrap();
    }
    let s = j.plan_status().unwrap();
    assert!(s.paused);
    assert_eq!(s.executed_gross_quantity, "0.09500000");
    assert_eq!(s.reason, "unexecutable_dust");
    assert_eq!(v.posts, 4);
}
#[test]
fn phase_corruption_and_revision_conflict_are_rejected_before_new_post() {
    let (t, mut j, mut v) = initialized();
    v.fill = false;
    j.tick_plan(&mut v, 1000).unwrap();
    let c = rusqlite::Connection::open(t.0.join("external.db")).unwrap();
    c.execute_batch("UPDATE intents SET phase='terminal';")
        .unwrap();
    assert!(j.tick_plan(&mut v, 1100).is_err());
    assert_eq!(v.posts, 1);
    drop(j);
    c.execute_batch("UPDATE execution_meta SET revision='binance-testnet-execution-v1';")
        .unwrap();
    assert!(kaze_quant::execution::ExecutionJournal::open(&t.0.join("external.db")).is_err());
}
#[test]
fn integer_twap_restart_fees_and_duplicate_ticks_are_exact() {
    let (t, mut j, mut v) = initialized();
    v.base_fee = true;
    for time in [1000, 1100, 1200] {
        let r = j.tick_plan(&mut v, time).unwrap();
        assert_eq!(r.post_calls, 1);
        drop(j);
        j = t.open();
        assert_eq!(j.tick_plan(&mut v, time).unwrap().post_calls, 0);
    }
    let s = j.plan_status().unwrap();
    assert_eq!(s.phase, Phase::Completed);
    assert_eq!(s.executed_gross_quantity, "0.10000000");
    assert_eq!(v.posts, 3);
    assert_eq!(
        v.f.orders
            .iter()
            .map(|o| decimal::parse(&o.orig_qty).unwrap())
            .collect::<Vec<_>>(),
        vec![3000000, 3000000, 4000000]
    );
    assert_eq!(j.expected_balances().unwrap()["BTC"], "0.09990000");
    assert_eq!(j.expected_balances().unwrap()["USDT"], "199990.00000000");
    assert!(
        j.audit().unwrap()["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn suppressed_ack_is_rest_recovered_without_repeating_post() {
    let (t, mut j, mut v) = initialized();
    v.drop_ack = true;
    assert_eq!(
        j.tick_plan(&mut v, 1000).unwrap().status.phase,
        Phase::SubmitUnknown
    );
    drop(j);
    j = t.open();
    assert_eq!(j.tick_plan(&mut v, 1000).unwrap().post_calls, 0);
    assert_eq!(v.posts, 1);
    assert_eq!(
        j.plan_status().unwrap().executed_gross_quantity,
        "0.03000000"
    );
}
#[test]
fn crash_before_post_has_no_reusable_send_permission() {
    let (t, mut j, mut v) = initialized();
    v.panic_submit = true;
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| j.tick_plan(&mut v, 1000)))
            .is_err()
    );
    assert_eq!(v.posts, 0);
    drop(j);
    j = t.open();
    v.panic_submit = false;
    assert!(j.tick_plan(&mut v, 1100).is_err());
    assert_eq!(v.posts, 0);
    assert_eq!(j.plan_status().unwrap().phase, Phase::SubmitUnknown);
}
#[test]
fn partial_fill_cancel_and_next_tick_replacement_use_remaining_gross() {
    let (_, mut j, mut v) = initialized();
    v.fill = false;
    j.tick_plan(&mut v, 1000).unwrap();
    v.fill_order(1, 1000000);
    let r = j.tick_plan(&mut v, 1010).unwrap();
    assert!(r.cancel_attempted);
    assert_eq!(r.post_calls, 0);
    assert_eq!(v.cancels, 1);
    j.tick_plan(&mut v, 1011).unwrap();
    assert_eq!(v.posts, 2);
    assert_eq!(v.f.orders[1].orig_qty, "0.02000000");
}
#[test]
fn uncertain_cancel_is_query_only_even_after_observation_and_restart() {
    let (t, mut j, mut v) = initialized();
    v.fill = false;
    v.lost_cancel = true;
    j.tick_plan(&mut v, 1000).unwrap();
    assert_eq!(
        j.tick_plan(&mut v, 1010).unwrap().status.phase,
        Phase::CancelPending
    );
    drop(j);
    j = t.open();
    for time in [1011, 1100] {
        assert_eq!(j.tick_plan(&mut v, time).unwrap().operation, "query_only");
    }
    assert_eq!(v.posts, 1);
    assert_eq!(v.cancels, 1);
    assert!(j.resume_plan(&mut v).is_err());
    v.fill_order(1, 3000000);
    j.tick_plan(&mut v, 1101).unwrap();
    assert_eq!(v.posts, 2);
    assert_eq!(v.cancels, 1);
}
#[test]
fn durable_pause_cancels_only_owned_child_then_requires_explicit_resume() {
    let (t, mut j, mut v) = initialized();
    v.fill = false;
    j.tick_plan(&mut v, 1000).unwrap();
    j.pause_plan("operator").unwrap();
    drop(j);
    j = t.open();
    j.tick_plan(&mut v, 1001).unwrap();
    assert_eq!(v.cancels, 1);
    assert_eq!(
        j.tick_plan(&mut v, 1200).unwrap().status.phase,
        Phase::Paused
    );
    assert_eq!(v.posts, 1);
    j.resume_plan(&mut v).unwrap();
    j.tick_plan(&mut v, 1201).unwrap();
    assert_eq!(v.posts, 2);
    assert_eq!(v.f.orders[1].orig_qty, "0.04000000");
}
#[test]
fn changed_assets_foreign_orders_missing_trades_and_clock_regression_block_new_children() {
    for failure in 0..3 {
        let (_, mut j, mut v) = initialized();
        j.tick_plan(&mut v, 1000).unwrap();
        match failure {
            0 => v.f.account.balances[1].free = "1".into(),
            1 => v.f.foreign_open = true,
            _ => v.f.fail_trade = true,
        };
        assert!(j.tick_plan(&mut v, 1100).is_err());
        assert_eq!(v.posts, 1);
    }
    let (_, mut j, mut v) = initialized();
    j.tick_plan(&mut v, 1000).unwrap();
    assert!(j.tick_plan(&mut v, 999).is_err());
    assert!(j.plan_status().unwrap().paused);
    assert_eq!(v.posts, 1);
}
#[test]
fn preflight_and_stale_reconciliation_never_prepare_or_post() {
    let (_, mut j, mut v) = initialized();
    v.preflight_fail = true;
    assert!(j.tick_plan(&mut v, 1000).is_err());
    assert_eq!(j.orders().unwrap().len(), 0);
    assert!(j.plan_status().unwrap().paused);
    let t = Scratch::new();
    let mut j = bound(&t);
    let mut c = config();
    c.max_reconciliation_ms = 1;
    j.init_plan(c).unwrap();
    v = Sim::new();
    v.delay = true;
    assert!(j.tick_plan(&mut v, 1000).is_err());
    assert_eq!(v.posts, 0);
    assert!(j.orders().unwrap().is_empty());
}
#[test]
fn failed_child_transaction_rolls_back_plan_links_and_intent_before_network() {
    let (t, mut j, mut v) = initialized();
    let old = serde_json::to_value(j.plan_status().unwrap()).unwrap();
    let c = rusqlite::Connection::open(t.0.join("external.db")).unwrap();
    c.execute_batch("CREATE TRIGGER injected BEFORE INSERT ON plan_children BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert!(j.tick_plan(&mut v, 1000).is_err());
    assert_eq!(v.posts, 0);
    assert!(j.orders().unwrap().is_empty());
    assert_eq!(serde_json::to_value(j.plan_status().unwrap()).unwrap(), old);
    c.execute_batch("DROP TRIGGER injected;").unwrap();
    j.tick_plan(&mut v, 1000).unwrap();
    assert_eq!(v.posts, 1);
}
#[test]
fn immutable_config_manual_intent_and_corrupt_checkpoint_fail_closed() {
    let (t, mut j, mut v) = initialized();
    j.init_plan(config()).unwrap();
    let mut c = config();
    c.interval_ms = 101;
    assert!(j.init_plan(c).is_err());
    assert!(j.prepare(&intent(99), SCALE).is_err());
    let db = rusqlite::Connection::open(t.0.join("external.db")).unwrap();
    db.execute_batch("UPDATE execution_plan SET state=replace(state,'100','101');")
        .unwrap();
    assert!(j.tick_plan(&mut v, 1000).is_err());
    assert_eq!(v.posts, 0);
}
#[test]
fn child_capacity_stops_without_burst_or_extra_post() {
    let t = Scratch::new();
    let mut j = bound(&t);
    let mut c = config();
    c.slices = 1;
    c.max_children = 1;
    j.init_plan(c).unwrap();
    let mut v = Sim::new();
    j.tick_plan(&mut v, 1000).unwrap();
    assert_eq!(
        j.tick_plan(&mut v, 1001).unwrap().operation,
        "child_capacity"
    );
    assert!(j.plan_status().unwrap().paused);
    assert_eq!(v.posts, 1);
}
#[test]
fn invalid_bounds_reject_before_journal_mutation() {
    let mut c = config();
    for bad in [0, 1025] {
        c.slices = bad;
        assert!(c.validate().is_err());
    }
    c = config();
    c.total_quantity = "100".into();
    assert!(c.validate().is_err());
    c = config();
    c.quantity_step = "0".into();
    assert!(c.validate().is_err());
    c = config();
    c.interval_ms = u64::MAX;
    assert!(c.validate().is_err());
    c = config();
    c.min_child_quantity = "0.04".into();
    assert!(c.validate().is_err());
}
