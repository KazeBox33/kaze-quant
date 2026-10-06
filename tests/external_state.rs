use kaze_quant::{
    decimal::{self, SCALE},
    execution::*,
    external_state::*,
    types::Side,
    user_stream::*,
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
            "kaze-assets-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    fn open(&self) -> ExecutionJournal {
        ExecutionJournal::open(&self.0.join("external.db")).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn identity() -> ExecutionIdentity {
    ExecutionIdentity {
        venue: "binance-spot-testnet".into(),
        account_hash: "a".repeat(64),
    }
}
fn account(btc: &str, usdt: &str, bnb: &str) -> AccountObservation {
    AccountObservation {
        can_trade: true,
        update_time: 100,
        balances: vec![("BTC", btc), ("USDT", usdt), ("BNB", bnb)]
            .into_iter()
            .map(|(a, n)| Balance {
                asset: a.into(),
                free: n.into(),
                locked: "0".into(),
            })
            .collect(),
    }
}
fn binding() -> InstrumentBinding {
    InstrumentBinding {
        symbol: "BTCUSDT".into(),
        base_asset: "BTC".into(),
        quote_asset: "USDT".into(),
    }
}
fn intent() -> OrderIntent {
    OrderIntent {
        client_order_id: "kaze-ledger".into(),
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        price: "100".into(),
        quantity: "0.02".into(),
    }
}
fn order(q: &str, status: VenueStatus, time: u64) -> OrderObservation {
    OrderObservation {
        symbol: "BTCUSDT".into(),
        client_order_id: "kaze-ledger".into(),
        order_id: 7,
        reported_client_order_id: None,
        price: "100".into(),
        orig_qty: "0.02".into(),
        executed_qty: q.into(),
        cummulative_quote_qty: decimal::format(decimal::parse(q).unwrap() * 100).unwrap(),
        status,
        side: "BUY".into(),
        update_time: time,
    }
}
fn trade(id: u64, fee: &str, asset: &str) -> TradeObservation {
    TradeObservation {
        symbol: "BTCUSDT".into(),
        id,
        order_id: 7,
        price: "100".into(),
        qty: "0.01".into(),
        quote_qty: "1".into(),
        commission: fee.into(),
        commission_asset: asset.into(),
        is_buyer: true,
    }
}
struct Venue {
    account: AccountObservation,
    id: ExecutionIdentity,
    order: OrderObservation,
    trades: Vec<TradeObservation>,
    foreign: bool,
    sends: u64,
    queries: u64,
}
impl Venue {
    fn new() -> Self {
        Self {
            account: account("1", "1000", "2"),
            id: identity(),
            order: order("0", VenueStatus::New, 10),
            trades: vec![],
            foreign: false,
            sends: 0,
            queries: 0,
        }
    }
}
impl ExecutionVenue for Venue {
    fn account_identity(&mut self) -> Result<(ExecutionIdentity, AccountObservation), VenueError> {
        Ok((self.id.clone(), self.account.clone()))
    }
    fn account_open_orders(&mut self) -> Result<Vec<OrderObservation>, VenueError> {
        Ok(if self.foreign {
            let mut o = self.order.clone();
            o.order_id = 99;
            o.client_order_id = "external".into();
            vec![o]
        } else {
            vec![]
        })
    }
    fn validate_intent(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
        Ok(())
    }
    fn submit(&mut self, _: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.sends += 1;
        Ok(self.order.clone())
    }
    fn query(&mut self, _: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.queries += 1;
        Ok(self.order.clone())
    }
    fn cancel(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
        Ok(())
    }
    fn account(&mut self) -> Result<AccountObservation, VenueError> {
        Ok(self.account.clone())
    }
    fn open_orders(&mut self, _: &str) -> Result<Vec<OrderObservation>, VenueError> {
        Ok(vec![])
    }
    fn trades(&mut self, _: &OrderObservation) -> Result<Vec<TradeObservation>, VenueError> {
        Ok(self.trades.clone())
    }
}
fn bound(s: &Scratch) -> (ExecutionJournal, Venue) {
    let mut j = s.open();
    let mut v = Venue::new();
    j.bind_account(&identity(), &v.account, &[binding()])
        .unwrap();
    j.reconcile(&mut v).unwrap();
    (j, v)
}
fn prepare(j: &mut ExecutionJournal) {
    j.prepare(&intent(), 10 * SCALE).unwrap();
    j.observe(&order("0", VenueStatus::New, 10)).unwrap();
}
#[test]
fn original_currency_fees_and_replay_match_hand_calculated_assets_after_restart() {
    let s = Scratch::new();
    let (mut j, mut v) = bound(&s);
    prepare(&mut j);
    j.observe(&order("0.02", VenueStatus::Filled, 30)).unwrap();
    j.record_trades(&[trade(1, "0.0001", "BTC"), trade(2, "0.001", "BNB")])
        .unwrap();
    let want = j.expected_balances().unwrap();
    assert_eq!(want["BTC"], "1.01990000");
    assert_eq!(want["USDT"], "998.00000000");
    assert_eq!(want["BNB"], "1.99900000");
    j.record_trades(&[trade(1, "0.0001000000", "BTC")]).unwrap();
    assert_eq!(want, j.expected_balances().unwrap());
    v.order = order("0.02", VenueStatus::Filled, 30);
    v.trades = vec![trade(1, "0.0001", "BTC"), trade(2, "0.001", "BNB")];
    v.account = account("1.0199", "998", "1.999");
    j.reconcile(&mut v).unwrap();
    assert!(j.require_reconciled().is_ok());
    drop(j);
    let mut j = s.open();
    assert!(j.require_reconciled().is_err());
    assert_eq!(want, j.expected_balances().unwrap());
    assert_eq!(j.external_audit().unwrap()["movement_replay_equal"], true);
    j.reconcile(&mut v).unwrap();
}
#[test]
fn sale_and_quote_currency_fee_are_recorded_without_floating_point() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    let mut i = intent();
    i.side = Side::Sell;
    j.prepare(&i, 10 * SCALE).unwrap();
    let mut o = order("0.02", VenueStatus::Filled, 30);
    o.side = "SELL".into();
    j.observe(&o).unwrap();
    let mut a = trade(1, "0.001", "USDT");
    a.qty = "0.02".into();
    a.quote_qty = "2".into();
    a.is_buyer = false;
    j.record_trades(&[a]).unwrap();
    let m = j.expected_balances().unwrap();
    assert_eq!(m["BTC"], "0.98000000");
    assert_eq!(m["USDT"], "1001.99900000");
}
#[test]
fn invalid_trade_rolls_back_entire_page_and_balances() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    prepare(&mut j);
    j.observe(&order("0.02", VenueStatus::Filled, 30)).unwrap();
    let before = j.expected_balances().unwrap();
    let mut bad = trade(2, "0", "USDT");
    bad.order_id = 88;
    assert!(j.record_trades(&[trade(1, "0.001", "USDT"), bad]).is_err());
    assert_eq!(before, j.expected_balances().unwrap());
    assert_eq!(j.audit().unwrap()["trades"].as_array().unwrap().len(), 0);
}
#[test]
fn missing_or_unexplained_assets_block_new_intents_but_correct_rest_recovers_gap() {
    let s = Scratch::new();
    let (mut j, mut v) = bound(&s);
    v.account = account("1", "999", "2");
    assert!(j.reconcile(&mut v).is_err());
    assert!(j.prepare(&intent(), 10 * SCALE).is_err());
    v.account = account("1", "1000", "2");
    v.account.balances[1].free = "900".into();
    v.account.balances[1].locked = "100".into();
    j.reconcile(&mut v).unwrap();
    assert!(j.prepare(&intent(), 10 * SCALE).unwrap());
}
#[test]
fn account_identity_conflict_is_detected_before_query_and_is_sticky() {
    let s = Scratch::new();
    let (mut j, mut v) = bound(&s);
    prepare(&mut j);
    v.id.account_hash = "b".repeat(64);
    assert!(j.reconcile(&mut v).is_err());
    assert_eq!(v.queries, 0);
    v.id = identity();
    assert!(j.reconcile(&mut v).is_err());
    assert!(j.require_reconciled().is_err());
}
#[test]
fn foreign_open_order_or_transfer_cannot_be_cleared_by_matching_totals() {
    let s = Scratch::new();
    let (mut j, mut v) = bound(&s);
    v.foreign = true;
    assert!(j.reconcile(&mut v).is_err());
    v.foreign = false;
    assert!(j.reconcile(&mut v).is_err());
    let s2 = Scratch::new();
    let (mut j, mut v) = bound(&s2);
    j.ingest_user_event(&UserEvent::ExternalBalanceChanged)
        .unwrap();
    assert!(j.reconcile(&mut v).is_err());
}
#[test]
fn private_partial_fill_duplicate_and_late_event_never_double_post() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    prepare(&mut j);
    let e = UserEvent::Execution(ExecutionEvent {
        execution_id: 10,
        order: order("0.01", VenueStatus::PartiallyFilled, 20),
        trade: Some(trade(1, "0.001", "USDT")),
    });
    assert!(j.ingest_user_event(&e).unwrap());
    let assets = j.expected_balances().unwrap();
    assert!(!j.ingest_user_event(&e).unwrap());
    assert_eq!(assets, j.expected_balances().unwrap());
    j.observe(&order("0.02", VenueStatus::Filled, 30)).unwrap();
    j.record_trades(&[trade(2, "0.001", "USDT")]).unwrap();
    let before = j.expected_balances().unwrap();
    let late = UserEvent::Execution(ExecutionEvent {
        execution_id: 11,
        order: order("0.01", VenueStatus::PartiallyFilled, 20),
        trade: Some(trade(1, "0.001", "USDT")),
    });
    assert!(j.ingest_user_event(&late).unwrap());
    assert_eq!(before, j.expected_balances().unwrap());
    assert_eq!(
        j.orders().unwrap()[0].observation.as_ref().unwrap().status,
        VenueStatus::Filled
    );
}
#[test]
fn private_trade_failure_rolls_back_order_event_and_asset_projection() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    prepare(&mut j);
    let before = j.expected_balances().unwrap();
    let mut t = trade(1, "0", "USDT");
    t.is_buyer = false;
    let e = UserEvent::Execution(ExecutionEvent {
        execution_id: 10,
        order: order("0.01", VenueStatus::PartiallyFilled, 20),
        trade: Some(t),
    });
    assert!(j.ingest_user_event(&e).is_err());
    assert_eq!(
        j.orders().unwrap()[0]
            .observation
            .as_ref()
            .unwrap()
            .executed_qty,
        "0"
    );
    assert_eq!(before, j.expected_balances().unwrap());
    assert!(j.require_reconciled().is_err());
}
#[test]
fn private_identity_reuse_or_conflicting_trade_is_sticky_failure() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    prepare(&mut j);
    let mut e = ExecutionEvent {
        execution_id: 10,
        order: order("0.01", VenueStatus::PartiallyFilled, 20),
        trade: Some(trade(1, "0.001", "USDT")),
    };
    j.ingest_user_event(&UserEvent::Execution(e.clone()))
        .unwrap();
    e.trade.as_mut().unwrap().commission = "0.002".into();
    assert!(j.ingest_user_event(&UserEvent::Execution(e)).is_err());
    assert_eq!(j.expected_balances().unwrap()["USDT"], "998.99900000");
}
#[test]
fn baseline_refuses_legacy_history_duplicate_assets_and_unknown_instrument() {
    let s = Scratch::new();
    let mut j = s.open();
    j.prepare(&intent(), 10 * SCALE).unwrap();
    assert!(
        j.bind_account(&identity(), &account("1", "1000", "2"), &[binding()])
            .is_err()
    );
    let s2 = Scratch::new();
    let mut j = s2.open();
    let mut a = account("1", "1000", "2");
    a.balances.push(a.balances[0].clone());
    assert!(j.bind_account(&identity(), &a, &[binding()]).is_err());
    assert!(j.execution_identity().unwrap().is_none());
    drop(j);
    let (mut j, _) = bound(&s2);
    let mut i = intent();
    i.symbol = "ETHUSDT".into();
    assert!(j.prepare(&i, 10 * SCALE).is_err());
}
#[test]
fn durable_corruption_is_detected_by_trade_replay_and_hostile_integer_never_panics() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    prepare(&mut j);
    j.observe(&order("0.02", VenueStatus::Filled, 30)).unwrap();
    j.record_trades(&[trade(1, "0.001", "USDT"), trade(2, "0.001", "USDT")])
        .unwrap();
    drop(j);
    let c = rusqlite::Connection::open(s.0.join("external.db")).unwrap();
    c.execute("UPDATE asset_movements SET units='1' WHERE asset='BTC'", [])
        .unwrap();
    drop(c);
    let j = s.open();
    assert!(j.external_audit().is_err());
    drop(j);
    let c = rusqlite::Connection::open(s.0.join("external.db")).unwrap();
    c.execute(
        "UPDATE asset_movements SET units=?1 WHERE asset='BTC'",
        [i128::MIN.to_string()],
    )
    .unwrap();
    drop(c);
    let j = s.open();
    assert!(j.expected_balances().is_err());
}
#[test]
fn grouped_audit_reports_orphans_and_missing_trades() {
    let orders = vec![TrackedOrder {
        intent: intent(),
        phase: "terminal".into(),
        observation: Some(order("0.02", VenueStatus::Filled, 30)),
    }];
    assert!(
        order_trade_problems(&orders, &[trade(1, "0", ""), trade(2, "0", "")])
            .unwrap()
            .is_empty()
    );
    let mut orphan = trade(3, "0", "");
    orphan.order_id = 88;
    assert_eq!(order_trade_problems(&orders, &[orphan]).unwrap().len(), 2);
}

#[test]
fn account_asset_identifiers_remain_case_sensitive_and_zero_fee_has_no_currency() {
    let s = Scratch::new();
    let mut j = s.open();
    let mut v = Venue::new();
    v.account.balances.push(Balance {
        asset: "TestAsset".into(),
        free: "1".into(),
        locked: "0".into(),
    });
    v.account.balances.push(Balance {
        asset: "TESTASSET".into(),
        free: "2".into(),
        locked: "0".into(),
    });
    v.account.balances.push(Balance {
        asset: "测试币".into(),
        free: "3".into(),
        locked: "0".into(),
    });
    j.bind_account(&identity(), &v.account, &[binding()])
        .unwrap();
    j.reconcile(&mut v).unwrap();
    assert_eq!(j.expected_balances().unwrap()["测试币"], "3.00000000");
    assert_eq!(j.expected_balances().unwrap()["TestAsset"], "1.00000000");
    assert_eq!(j.expected_balances().unwrap()["TESTASSET"], "2.00000000");
    prepare(&mut j);
    j.observe(&order("0.02", VenueStatus::Filled, 30)).unwrap();
    j.record_trades(&[trade(1, "0", "USDT"), trade(2, "0", "")])
        .unwrap();
    j.record_trades(&[trade(1, "0.00000000", "")]).unwrap();
    assert_eq!(j.expected_balances().unwrap()["USDT"], "998.00000000");
}

#[test]
fn trade_overfill_quote_conflict_and_private_subscription_failure_never_change_assets() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    prepare(&mut j);
    j.observe(&order("0.01", VenueStatus::PartiallyFilled, 20))
        .unwrap();
    let before = j.expected_balances().unwrap();
    assert!(
        j.record_trades(&[trade(1, "0", ""), trade(2, "0", "")])
            .is_err()
    );
    assert_eq!(before, j.expected_balances().unwrap());
    let mut t = trade(1, "0", "");
    t.quote_qty = "0.9".into();
    assert!(j.record_trades(&[t]).is_err());
    assert_eq!(before, j.expected_balances().unwrap());
    assert!(
        j.ingest_user_frame(
            br#"{"subscriptionId":7,"event":{"e":"outboundAccountPosition"}}"#,
            8
        )
        .is_err()
    );
    assert!(j.require_reconciled().is_err());
}

#[test]
fn private_terminal_cumulative_cannot_regress_even_when_event_is_older() {
    let s = Scratch::new();
    let (mut j, _) = bound(&s);
    prepare(&mut j);
    j.observe(&order("0.01", VenueStatus::Canceled, 30))
        .unwrap();
    let e = UserEvent::Execution(ExecutionEvent {
        execution_id: 12,
        order: order("0", VenueStatus::Canceled, 20),
        trade: None,
    });
    assert!(j.ingest_user_event(&e).is_err());
    assert_eq!(
        j.orders().unwrap()[0]
            .observation
            .as_ref()
            .unwrap()
            .executed_qty,
        "0.01"
    );
}
#[test]
fn private_protocol_checks_subscription_and_uses_transaction_time() {
    let v = serde_json::json!({"subscriptionId":3,"event":{"e":"executionReport","E":999,"T":20,"I":10,"s":"BTCUSDT","c":"kaze-ledger","C":"","i":7,"o":"LIMIT","f":"GTC","S":"BUY","p":"100","q":"0.02","z":"0.01","Z":"1","X":"PARTIALLY_FILLED","x":"TRADE","l":"0.01","L":"100","Y":"1","t":1,"n":"0.001","N":"USDT"}});
    let bytes = serde_json::to_vec(&v).unwrap();
    assert!(parse_event(&bytes, 4).is_err());
    let UserEvent::Execution(e) = parse_event(&bytes, 3).unwrap() else {
        panic!()
    };
    assert_eq!(e.order.update_time, 20);
    assert_eq!(e.trade.unwrap().id, 1);
    assert!(parse_event(&vec![b' '; 65537], 3).is_err());
    assert!(parse_event(br#"{"subscriptionId":3,"event":{"e":"unknown"}}"#, 3).is_err());
}
#[test]
fn partial_account_and_termination_need_rest_without_rebasing_assets() {
    let s = Scratch::new();
    let (mut j, mut v) = bound(&s);
    let before = j.expected_balances().unwrap();
    j.ingest_user_event(&UserEvent::AccountChanged).unwrap();
    assert!(j.require_reconciled().is_err());
    assert_eq!(before, j.expected_balances().unwrap());
    j.reconcile(&mut v).unwrap();
    j.ingest_user_event(&UserEvent::Terminated).unwrap();
    assert!(j.require_reconciled().is_err());
}

#[test]
fn bound_account_restart_same_intent_queries_only_and_rest_recovers_missing_fills() {
    let s = Scratch::new();
    let (mut j, mut v) = bound(&s);
    j.submit_once(&mut v, &intent(), 10 * SCALE).unwrap();
    assert_eq!(v.sends, 1);
    drop(j);
    v.order = order("0.02", VenueStatus::Filled, 30);
    v.trades = vec![trade(1, "0.001", "USDT"), trade(2, "0.001", "USDT")];
    v.account = account("1.02", "997.998", "2");
    let mut j = s.open();
    j.submit_once(&mut v, &intent(), 10 * SCALE).unwrap();
    assert_eq!(v.sends, 1);
    assert!(
        !j.audit().unwrap()["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    j.reconcile(&mut v).unwrap();
    assert_eq!(j.expected_balances().unwrap()["USDT"], "997.99800000");
    assert!(j.require_reconciled().is_ok());
    assert_eq!(v.sends, 1);
}
