use kaze_quant::decimal::{self, SCALE};
use kaze_quant::execution::*;
use kaze_quant::types::Side;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "kaze-execution-{}-{}",
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
fn intent() -> OrderIntent {
    OrderIntent {
        client_order_id: "kaze-test-1".into(),
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        price: "100.00".into(),
        quantity: "0.01".into(),
    }
}
fn obs(q: &str, status: VenueStatus, time: u64) -> OrderObservation {
    OrderObservation {
        symbol: "BTCUSDT".into(),
        client_order_id: "kaze-test-1".into(),
        order_id: 7,
        reported_client_order_id: None,
        price: "100.00000000".into(),
        orig_qty: "0.01000000".into(),
        executed_qty: q.into(),
        cummulative_quote_qty: decimal::format(decimal::parse(q).unwrap() * 100).unwrap(),
        status,
        side: "BUY".into(),
        update_time: time,
    }
}
fn trade() -> TradeObservation {
    TradeObservation {
        symbol: "BTCUSDT".into(),
        id: 4,
        order_id: 7,
        price: "100".into(),
        qty: "0.01".into(),
        quote_qty: "1".into(),
        commission: "0.001".into(),
        commission_asset: "USDT".into(),
        is_buyer: true,
    }
}
fn account() -> AccountObservation {
    AccountObservation {
        can_trade: true,
        balances: vec![Balance {
            asset: "USDT".into(),
            free: "1000".into(),
            locked: "0".into(),
        }],
        update_time: 100,
    }
}
#[derive(Default)]
struct Mock {
    sends: u64,
    query_count: u64,
    known_queries: Vec<Option<u64>>,
    timeout: bool,
    missing: bool,
    canceled: bool,
    external: bool,
}
impl ExecutionVenue for Mock {
    fn validate_intent(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
        Ok(())
    }
    fn submit(&mut self, _: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.sends += 1;
        if self.timeout {
            Err(VenueError::Unknown)
        } else {
            Ok(obs("0", VenueStatus::New, 10))
        }
    }
    fn query(&mut self, _: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.query_count += 1;
        if self.missing {
            Err(VenueError::NotFound)
        } else if self.canceled {
            Ok(obs("0", VenueStatus::Canceled, 20))
        } else {
            Ok(obs("0", VenueStatus::New, 10))
        }
    }
    fn query_known(
        &mut self,
        i: &OrderIntent,
        id: Option<u64>,
    ) -> Result<OrderObservation, VenueError> {
        self.known_queries.push(id);
        self.query(i)
    }
    fn cancel(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
        self.canceled = true;
        Err(VenueError::Unknown)
    }
    fn account(&mut self) -> Result<AccountObservation, VenueError> {
        Ok(account())
    }
    fn open_orders(&mut self, _: &str) -> Result<Vec<OrderObservation>, VenueError> {
        if self.external {
            let mut o = obs("0", VenueStatus::New, 10);
            o.client_order_id = "someone-else".into();
            Ok(vec![o])
        } else {
            Ok(vec![])
        }
    }
    fn trades(&mut self, _: &OrderObservation) -> Result<Vec<TradeObservation>, VenueError> {
        Ok(vec![])
    }
}
#[test]
fn decimal_is_exact_and_rejects_lossy_inputs() {
    for s in ["0", "1", "0.00000001", "98765.43210000"] {
        let n = decimal::parse(s).unwrap();
        assert_eq!(decimal::parse(&decimal::format(n).unwrap()).unwrap(), n);
    }
    for s in ["-1", "1e2", " 1", "1.000000001", "1..0", ".1", "nan"] {
        assert!(decimal::parse(s).is_err(), "{s}");
    }
    assert_eq!(decimal::parse("1.000000000").unwrap(), SCALE);
    assert!(decimal::rescale(1, 1000, true).is_err());
    assert_eq!(decimal::rescale(1, 1000, false).unwrap(), 0);
}
#[test]
fn timeout_and_restart_never_repeat_submission() {
    let t = Scratch::new();
    let mut v = Mock {
        timeout: true,
        ..Mock::default()
    };
    {
        let mut j = t.open();
        assert!(j.submit_once(&mut v, &intent(), 10 * SCALE).is_err());
        assert_eq!(j.orders().unwrap()[0].phase, "unknown");
    }
    let mut j = t.open();
    j.submit_once(&mut v, &intent(), 10 * SCALE).unwrap();
    assert_eq!(v.sends, 1);
    assert_eq!(v.query_count, 1);
}
#[test]
fn not_found_is_not_permission_to_resubmit() {
    let t = Scratch::new();
    let mut j = t.open();
    let mut v = Mock {
        timeout: true,
        missing: true,
        ..Mock::default()
    };
    assert!(j.submit_once(&mut v, &intent(), 10 * SCALE).is_err());
    assert!(j.submit_once(&mut v, &intent(), 10 * SCALE).is_err());
    assert_eq!(v.sends, 1);
    assert_eq!(j.orders().unwrap()[0].phase, "unknown");
}
#[test]
fn prepared_but_unsent_crash_fails_closed() {
    let t = Scratch::new();
    {
        let mut j = t.open();
        assert!(j.prepare(&intent(), 10 * SCALE).unwrap());
    }
    let mut j = t.open();
    let mut v = Mock {
        missing: true,
        ..Mock::default()
    };
    assert!(j.submit_once(&mut v, &intent(), 10 * SCALE).is_err());
    assert_eq!(v.sends, 0);
}
#[test]
fn cumulative_regression_identity_and_terminal_resurrection_are_rejected() {
    let t = Scratch::new();
    let mut j = t.open();
    j.prepare(&intent(), 10 * SCALE).unwrap();
    j.observe(&obs("0.005", VenueStatus::PartiallyFilled, 10))
        .unwrap();
    assert!(
        j.observe(&obs("0.001", VenueStatus::PartiallyFilled, 20))
            .is_err()
    );
    assert!(
        j.observe(&obs("0.006", VenueStatus::PartiallyFilled, 9))
            .is_err()
    );
    let mut wrong = obs("0.006", VenueStatus::PartiallyFilled, 20);
    wrong.order_id = 8;
    assert!(j.observe(&wrong).is_err());
    j.observe(&obs("0.01", VenueStatus::Filled, 30)).unwrap();
    assert!(j.observe(&obs("0", VenueStatus::New, 40)).is_err());
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
fn conflicting_intent_and_unresolved_orders_block_new_ids() {
    let t = Scratch::new();
    let mut j = t.open();
    j.prepare(&intent(), 10 * SCALE).unwrap();
    let mut i = intent();
    i.price = "101".into();
    assert!(j.prepare(&i, 10 * SCALE).is_err());
    i.client_order_id = "kaze-test-2".into();
    assert!(j.prepare(&i, 10 * SCALE).is_err());
    assert!(j.prepare(&intent(), SCALE / 2).is_err());
}
#[test]
fn duplicate_trades_do_not_double_count_fees_and_conflicts_roll_back() {
    let t = Scratch::new();
    let mut j = t.open();
    j.prepare(&intent(), 10 * SCALE).unwrap();
    j.observe(&obs("0.01", VenueStatus::Filled, 20)).unwrap();
    j.record_account(&account()).unwrap();
    assert!(
        !j.audit().unwrap()["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    j.record_trades(&[trade(), trade()]).unwrap();
    let a = j.audit().unwrap();
    assert!(a["problems"].as_array().unwrap().is_empty());
    assert_eq!(a["trades"].as_array().unwrap().len(), 1);
    let mut bad = trade();
    bad.commission = "0.1".into();
    assert!(j.record_trades(&[bad]).is_err());
    assert_eq!(j.audit().unwrap(), a);
}
#[test]
fn cancel_timeout_resolves_using_query_and_external_orders_block() {
    let t = Scratch::new();
    let mut j = t.open();
    let mut v = Mock::default();
    j.submit_once(&mut v, &intent(), 10 * SCALE).unwrap();
    j.cancel_once(&mut v, "kaze-test-1").unwrap();
    assert_eq!(j.orders().unwrap()[0].phase, "terminal");
    assert_eq!(v.known_queries, vec![Some(7), Some(7)]);
    v.external = true;
    assert!(j.reconcile(&mut v).is_err());
}
#[test]
fn one_writer_lock_protects_journal() {
    let t = Scratch::new();
    let _j = t.open();
    assert!(ExecutionJournal::open(&t.0.join("external.db")).is_err());
}
#[test]
fn venue_economics_cannot_exceed_buy_limit() {
    let t = Scratch::new();
    let mut j = t.open();
    j.prepare(&intent(), 10 * SCALE).unwrap();
    let mut bad = obs("0.005", VenueStatus::PartiallyFilled, 10);
    bad.cummulative_quote_qty = "999".into();
    assert!(j.observe(&bad).is_err());
    assert!(j.orders().unwrap()[0].observation.is_none());
}
