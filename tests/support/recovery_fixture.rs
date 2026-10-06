#![allow(dead_code)]
use kaze_quant::{
    decimal::{self, SCALE},
    execution::*,
    external_state::*,
    recovery::*,
    types::Side,
};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
pub struct Scratch(pub PathBuf);
impl Scratch {
    pub fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "kaze-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
    pub fn open(&self) -> ExecutionJournal {
        ExecutionJournal::open(&self.0.join("external.db")).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn identity() -> ExecutionIdentity {
    ExecutionIdentity {
        venue: "binance-spot-testnet".into(),
        account_hash: "a".repeat(64),
    }
}
pub fn binding() -> InstrumentBinding {
    InstrumentBinding {
        symbol: "BTCUSDT".into(),
        base_asset: "BTC".into(),
        quote_asset: "USDT".into(),
    }
}
pub fn intent(id: u64) -> OrderIntent {
    OrderIntent {
        client_order_id: format!("kaze-history-{id}"),
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        price: "100".into(),
        quantity: "0.01".into(),
    }
}
pub fn observation(id: u64) -> OrderObservation {
    OrderObservation {
        symbol: "BTCUSDT".into(),
        client_order_id: intent(id).client_order_id,
        order_id: id,
        reported_client_order_id: None,
        price: "100.00000000".into(),
        orig_qty: "0.01000000".into(),
        executed_qty: "0.01000000".into(),
        cummulative_quote_qty: "1.00000000".into(),
        status: VenueStatus::Filled,
        side: "BUY".into(),
        update_time: id,
    }
}
pub fn trade(id: u64) -> TradeObservation {
    TradeObservation {
        symbol: "BTCUSDT".into(),
        id,
        order_id: id,
        price: "100.00000000".into(),
        qty: "0.01000000".into(),
        quote_qty: "1.00000000".into(),
        commission: "0.00100000".into(),
        commission_asset: "USDT".into(),
        is_buyer: true,
    }
}
pub fn account(n: u64) -> AccountObservation {
    AccountObservation {
        can_trade: true,
        update_time: n + 1,
        balances: vec![
            Balance {
                asset: "BTC".into(),
                free: decimal::format(i128::from(n) * 1_000_000).unwrap(),
                locked: "0".into(),
            },
            Balance {
                asset: "USDT".into(),
                free: decimal::format(200000 * SCALE - i128::from(n) * 100100000).unwrap(),
                locked: "0".into(),
            },
        ],
    }
}
#[derive(Clone)]
pub struct Fixture {
    pub orders: Vec<OrderObservation>,
    pub trades: Vec<TradeObservation>,
    pub account: AccountObservation,
    pub identity: ExecutionIdentity,
    pub calls: u64,
    pub posts: u64,
    pub queries: u64,
    pub head_override: Option<HistoryHead>,
    pub missing_trade: bool,
    pub reversed: bool,
    pub fail_trade: bool,
    pub foreign_open: bool,
    pub missing_query: bool,
    by_client: BTreeMap<String, OrderObservation>,
    by_id: BTreeMap<u64, OrderObservation>,
    by_trade_order: BTreeMap<u64, Vec<TradeObservation>>,
}
impl Fixture {
    pub fn new(n: u64) -> Self {
        let orders: Vec<_> = (1..=n).map(observation).collect();
        let trades: Vec<_> = (1..=n).map(trade).collect();
        Self {
            by_client: orders
                .iter()
                .map(|o| (o.client_order_id.clone(), o.clone()))
                .collect(),
            by_id: orders.iter().map(|o| (o.order_id, o.clone())).collect(),
            by_trade_order: trades
                .iter()
                .map(|t| (t.order_id, vec![t.clone()]))
                .collect(),
            orders,
            trades,
            account: account(n),
            identity: identity(),
            calls: 0,
            posts: 0,
            queries: 0,
            head_override: None,
            missing_trade: false,
            reversed: false,
            fail_trade: false,
            foreign_open: false,
            missing_query: false,
        }
    }
}
impl ExecutionVenue for Fixture {
    fn history_head(&mut self, s: &str) -> Result<HistoryHead, VenueError> {
        self.calls += 2;
        Ok(self.head_override.clone().unwrap_or(HistoryHead {
            symbol: s.into(),
            order_id: self.orders.last().map(|o| o.order_id),
            trade_id: self.trades.last().map(|t| t.id),
        }))
    }
    fn order_history(
        &mut self,
        _: &str,
        from: u64,
        limit: u16,
    ) -> Result<Vec<OrderObservation>, VenueError> {
        self.calls += 1;
        let start = self.orders.partition_point(|o| o.order_id < from);
        let mut rows = self.orders[start..(start + limit as usize).min(self.orders.len())].to_vec();
        if self.reversed {
            rows.reverse();
        }
        Ok(rows)
    }
    fn trade_history(
        &mut self,
        _: &str,
        from: u64,
        limit: u16,
    ) -> Result<Vec<TradeObservation>, VenueError> {
        self.calls += 1;
        if self.fail_trade {
            return Err(VenueError::Unavailable);
        }
        let start = self.trades.partition_point(|t| t.id < from);
        let mut rows = self.trades[start..(start + limit as usize).min(self.trades.len())].to_vec();
        if self.missing_trade {
            rows.pop();
        }
        Ok(rows)
    }
    fn validate_intent(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
        Ok(())
    }
    fn submit(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.posts += 1;
        self.query(i)
    }
    fn query(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.calls += 1;
        self.queries += 1;
        if self.missing_query {
            return Err(VenueError::NotFound);
        }
        self.by_client
            .get(&i.client_order_id)
            .cloned()
            .ok_or(VenueError::NotFound)
    }
    fn query_known(
        &mut self,
        i: &OrderIntent,
        id: Option<u64>,
    ) -> Result<OrderObservation, VenueError> {
        if let Some(id) = id {
            self.calls += 1;
            self.queries += 1;
            return self.by_id.get(&id).cloned().ok_or(VenueError::NotFound);
        }
        self.query(i)
    }
    fn cancel(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
        Err(VenueError::Unavailable)
    }
    fn account_identity(&mut self) -> Result<(ExecutionIdentity, AccountObservation), VenueError> {
        self.calls += 1;
        Ok((self.identity.clone(), self.account.clone()))
    }
    fn account(&mut self) -> Result<AccountObservation, VenueError> {
        self.calls += 1;
        Ok(self.account.clone())
    }
    fn account_open_orders(&mut self) -> Result<Vec<OrderObservation>, VenueError> {
        self.calls += 1;
        Ok(if self.foreign_open {
            vec![observation(999)]
        } else {
            vec![]
        })
    }
    fn open_orders(&mut self, _: &str) -> Result<Vec<OrderObservation>, VenueError> {
        self.calls += 1;
        Ok(vec![])
    }
    fn trades(&mut self, o: &OrderObservation) -> Result<Vec<TradeObservation>, VenueError> {
        self.calls += 1;
        Ok(self
            .by_trade_order
            .get(&o.order_id)
            .cloned()
            .unwrap_or_default())
    }
}
pub fn bound(s: &Scratch) -> ExecutionJournal {
    let mut j = s.open();
    j.bind_account_with_history(
        &identity(),
        &account(0),
        &[binding()],
        &[HistoryHead {
            symbol: "BTCUSDT".into(),
            order_id: None,
            trade_id: None,
        }],
    )
    .unwrap();
    let mut v = Fixture::new(0);
    j.recover_history(&mut v, RecoveryOptions::default())
        .unwrap();
    j
}
/// 计时外预装一个可达的终态历史；只用于基准/测试，不是公开下单入口。
pub fn preload(s: &Scratch, j: &ExecutionJournal, n: u64) {
    let mut c = rusqlite::Connection::open(s.0.join("external.db")).unwrap();
    let tx = c.transaction().unwrap();
    for id in 1..=n {
        tx.execute(
            "INSERT INTO intents VALUES(?1,?2,'terminal',?3)",
            rusqlite::params![
                intent(id).client_order_id,
                serde_json::to_string(&intent(id)).unwrap(),
                serde_json::to_string(&observation(id)).unwrap()
            ],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO trades VALUES('BTCUSDT',?1,?2)",
            rusqlite::params![id as i64, serde_json::to_string(&trade(id)).unwrap()],
        )
        .unwrap();
    }
    tx.execute(
        "INSERT INTO asset_movements VALUES('BTC',?1)",
        [(i128::from(n) * 1_000_000).to_string()],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO asset_movements VALUES('USDT',?1)",
        [(-i128::from(n) * 100100000).to_string()],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO accounts(body) VALUES(?1)",
        [serde_json::to_string(&account(n)).unwrap()],
    )
    .unwrap();
    tx.commit().unwrap();
    assert!(
        j.audit().unwrap()["problems"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
