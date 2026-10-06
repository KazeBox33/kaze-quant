#[path = "support/recovery_fixture.rs"]
mod fixture;
use fixture::*;
use kaze_quant::{decimal::SCALE, execution::*, recovery::*, user_stream::*};
fn opts() -> RecoveryOptions {
    RecoveryOptions {
        page_limit: 2,
        max_pages: 20,
        max_records: 100,
    }
}
fn prepared(j: &mut ExecutionJournal, id: u64) {
    j.prepare(&intent(id), 10 * SCALE).unwrap();
}
#[test]
fn lost_ack_and_missing_private_fill_recover_atomically_without_new_post() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let before = j.history_cursors().unwrap()[0].confirmed.clone();
    let mut v = Fixture::new(1);
    let stats = j.recover_history(&mut v, opts()).unwrap();
    assert_eq!(stats.new_trades, 1);
    assert_eq!(v.posts, 0);
    assert_eq!(j.expected_balances().unwrap()["USDT"], "199998.99900000");
    assert_ne!(before, j.history_cursors().unwrap()[0].confirmed);
    j.submit_once(&mut v, &intent(1), 10 * SCALE).unwrap();
    assert_eq!(v.posts, 0);
}
#[test]
fn short_missing_page_cannot_advance_cursor_or_economics() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let before = j.expected_balances().unwrap();
    let cursors = j.history_audit().unwrap();
    let mut v = Fixture::new(1);
    v.missing_trade = true;
    assert!(j.recover_history(&mut v, opts()).is_err());
    assert_eq!(before, j.expected_balances().unwrap());
    assert_eq!(cursors, j.history_audit().unwrap());
    assert!(j.orders().unwrap()[0].observation.is_none());
    v.missing_trade = false;
    j.recover_history(&mut v, opts()).unwrap();
}
#[test]
fn wrong_account_balance_rolls_back_order_fill_account_and_cursor() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let audit = j.audit().unwrap();
    let cursors = j.history_audit().unwrap();
    let mut v = Fixture::new(1);
    v.account = account(0);
    assert!(j.recover_history(&mut v, opts()).is_err());
    let after = j.audit().unwrap();
    assert_eq!(audit["orders"], after["orders"]);
    assert_eq!(audit["trades"], after["trades"]);
    assert_eq!(audit["account"], after["account"]);
    assert_eq!(cursors, j.history_audit().unwrap());
    assert!(j.require_reconciled().is_err());
    v.account = account(1);
    j.recover_history(&mut v, opts()).unwrap();
}
#[test]
fn exact_full_pages_need_next_page_and_budgets_fail_before_commit() {
    let s = Scratch::new();
    let mut j = bound(&s);
    preload(&s, &j, 2);
    let mut v = Fixture::new(2);
    let before = j.history_audit().unwrap();
    assert!(
        j.recover_history(
            &mut v,
            RecoveryOptions {
                page_limit: 2,
                max_pages: 1,
                max_records: 100
            }
        )
        .is_err()
    );
    assert_eq!(before, j.history_audit().unwrap());
    let stats = j.recover_history(&mut v, opts()).unwrap();
    assert_eq!(stats.order_pages, 2);
    assert_eq!(stats.trade_pages, 2);
    assert_eq!(stats.new_trades, 0);
}
#[test]
fn noncontiguous_ids_are_valid_but_unsorted_or_regressing_pages_are_not() {
    let s = Scratch::new();
    let mut j = bound(&s);
    preload(&s, &j, 3);
    let mut v = Fixture::new(3);
    v.orders.remove(1);
    v.trades.remove(1);
    j.recover_history(&mut v, opts()).unwrap();
    assert_eq!(j.history_cursors().unwrap()[0].confirmed.trade_id, Some(3));
    let s2 = Scratch::new();
    let mut j = bound(&s2);
    preload(&s2, &j, 2);
    let mut v = Fixture::new(2);
    v.reversed = true;
    assert!(j.recover_history(&mut v, opts()).is_err());
}
#[test]
fn foreign_closed_order_is_detected_even_with_no_open_orders_or_balance_change() {
    let s = Scratch::new();
    let mut j = bound(&s);
    let mut v = Fixture::new(1);
    v.account = account(0);
    assert!(j.recover_history(&mut v, opts()).is_err());
    assert_eq!(
        j.audit().unwrap()["external_ledger"]["external_activity_blocked"],
        true
    );
    assert_eq!(v.posts, 0);
}
#[test]
fn seeded_older_history_is_never_double_booked_and_seed_binding_is_complete() {
    let s = Scratch::new();
    let mut j = s.open();
    j.bind_account_with_history(
        &identity(),
        &account(1),
        &[binding()],
        &[HistoryHead {
            symbol: "BTCUSDT".into(),
            order_id: Some(1),
            trade_id: Some(1),
        }],
    )
    .unwrap();
    let mut v = Fixture::new(1);
    j.recover_history(&mut v, opts()).unwrap();
    assert!(j.audit().unwrap()["trades"].as_array().unwrap().is_empty());
    assert_eq!(j.expected_balances().unwrap()["USDT"], "199998.99900000");
    let s2 = Scratch::new();
    let mut j = s2.open();
    assert!(
        j.bind_account_with_history(&identity(), &account(0), &[binding()], &[])
            .is_err()
    );
    assert!(j.execution_identity().unwrap().is_none());
}
#[test]
fn restart_requires_recovery_and_repeated_overlap_never_charges_twice() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let mut v = Fixture::new(1);
    j.recover_history(&mut v, opts()).unwrap();
    let assets = j.expected_balances().unwrap();
    drop(j);
    let mut j = s.open();
    assert!(j.require_reconciled().is_err());
    let stats = j.recover_history(&mut v, opts()).unwrap();
    assert_eq!(stats.new_trades, 0);
    assert_eq!(stats.trade_records, 1);
    assert_eq!(stats.pending_order_queries, 0);
    assert_eq!(assets, j.expected_balances().unwrap());
}
#[test]
fn exchange_history_reset_and_identity_conflict_fail_closed_before_queries() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let mut v = Fixture::new(1);
    j.recover_history(&mut v, opts()).unwrap();
    v.head_override = Some(HistoryHead {
        symbol: "BTCUSDT".into(),
        order_id: None,
        trade_id: None,
    });
    assert!(j.recover_history(&mut v, opts()).is_err());
    assert_eq!(
        j.audit().unwrap()["external_ledger"]["external_activity_blocked"],
        true
    );
    let s2 = Scratch::new();
    let mut j = bound(&s2);
    let mut v = Fixture::new(0);
    v.identity.account_hash = "b".repeat(64);
    assert!(j.recover_history(&mut v, opts()).is_err());
    assert_eq!(v.calls, 1);
}
#[test]
fn cursor_and_baseline_corruption_are_detected_before_network() {
    let s = Scratch::new();
    let mut j = bound(&s);
    let c = rusqlite::Connection::open(s.0.join("external.db")).unwrap();
    c.execute("UPDATE recovery_cursors SET last_trade='5'", [])
        .unwrap();
    let mut v = Fixture::new(0);
    assert!(j.recover_history(&mut v, opts()).is_err());
    assert_eq!(v.calls, 0);
    drop(c);
    drop(j);
    let s2 = Scratch::new();
    let mut j = bound(&s2);
    let c = rusqlite::Connection::open(s2.0.join("external.db")).unwrap();
    c.execute("UPDATE ledger_baseline SET units='1' WHERE asset='BTC'", [])
        .unwrap();
    assert!(j.recover_history(&mut v, opts()).is_err());
}
#[test]
fn unknown_unsent_intent_not_found_is_never_resubmission_permission() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let mut v = Fixture::new(0);
    v.missing_query = true;
    assert!(j.recover_history(&mut v, opts()).is_err());
    assert_eq!(v.posts, 0);
    assert!(j.orders().unwrap()[0].observation.is_none());
    assert!(j.prepare(&intent(2), 10 * SCALE).is_err());
}
#[test]
fn late_private_fill_after_rest_cursor_is_deduplicated_by_trade_identity() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let mut v = Fixture::new(1);
    j.recover_history(&mut v, opts()).unwrap();
    let before = j.expected_balances().unwrap();
    j.ingest_user_event(&UserEvent::Execution(ExecutionEvent {
        execution_id: 9,
        order: observation(1),
        trade: Some(trade(1)),
    }))
    .unwrap();
    assert_eq!(before, j.expected_balances().unwrap());
    assert_eq!(j.recover_history(&mut v, opts()).unwrap().new_trades, 0);
}
#[test]
fn legacy_bound_journal_cannot_gain_a_cursor_without_baseline_provenance() {
    let s = Scratch::new();
    let mut j = s.open();
    j.bind_account(&identity(), &account(0), &[binding()])
        .unwrap();
    assert!(j.recover_history(&mut Fixture::new(0), opts()).is_err());
    assert!(!j.has_history_recovery().unwrap());
}
#[test]
fn trade_transport_failure_and_record_capacity_preserve_confirmed_round() {
    let s = Scratch::new();
    let mut j = bound(&s);
    prepared(&mut j, 1);
    let before = j.history_audit().unwrap();
    let mut v = Fixture::new(1);
    v.fail_trade = true;
    assert!(j.recover_history(&mut v, opts()).is_err());
    v.fail_trade = false;
    assert!(
        j.recover_history(
            &mut v,
            RecoveryOptions {
                page_limit: 2,
                max_pages: 20,
                max_records: 1
            }
        )
        .is_err()
    );
    assert_eq!(before, j.history_audit().unwrap());
}
