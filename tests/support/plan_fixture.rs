#![allow(dead_code)]
#[path = "recovery_fixture.rs"]
pub mod fixture;
use fixture::*;
use kaze_quant::{
    decimal::{self, SCALE},
    execution::*,
    external_plan::*,
    recovery::HistoryHead,
    types::Side,
};
pub fn config() -> PlanConfig {
    PlanConfig {
        version: 1,
        plan_id: "kaze-plan-test".into(),
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        limit_price: "100".into(),
        total_quantity: "0.10".into(),
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
pub struct Sim {
    pub f: Fixture,
    pub posts: u64,
    pub cancels: u64,
    pub fill: bool,
    pub drop_ack: bool,
    pub lost_cancel: bool,
    pub panic_submit: bool,
    pub block_before_post: bool,
    pub preflight_fail: bool,
    pub preflight_delay: bool,
    pub base_fee: bool,
    pub delay: bool,
}
impl Sim {
    pub fn new() -> Self {
        Self {
            f: Fixture::new(0),
            posts: 0,
            cancels: 0,
            fill: true,
            drop_ack: false,
            lost_cancel: false,
            panic_submit: false,
            block_before_post: false,
            preflight_fail: false,
            preflight_delay: false,
            base_fee: false,
            delay: false,
        }
    }
    pub fn fill_order(&mut self, id: u64, qty: i128) {
        let o = self.f.orders.iter_mut().find(|o| o.order_id == id).unwrap();
        let amount = decimal::parse(&o.price).unwrap() * qty / SCALE;
        let fee = if self.base_fee {
            (qty + 999) / 1000
        } else {
            (amount + 999) / 1000
        };
        let buy = o.side == "BUY";
        let sign = if buy { 1 } else { -1 };
        o.executed_qty = decimal::format(decimal::parse(&o.executed_qty).unwrap() + qty).unwrap();
        o.cummulative_quote_qty =
            decimal::format(decimal::parse(&o.cummulative_quote_qty).unwrap() + amount).unwrap();
        o.status =
            if decimal::parse(&o.executed_qty).unwrap() == decimal::parse(&o.orig_qty).unwrap() {
                VenueStatus::Filled
            } else {
                VenueStatus::PartiallyFilled
            };
        o.update_time += 1;
        let asset = if self.base_fee { "BTC" } else { "USDT" };
        self.f.trades.push(TradeObservation {
            symbol: o.symbol.clone(),
            id: self.f.trades.len() as u64 + 1,
            order_id: id,
            price: o.price.clone(),
            qty: decimal::format(qty).unwrap(),
            quote_qty: decimal::format(amount).unwrap(),
            commission: decimal::format(fee).unwrap(),
            commission_asset: asset.into(),
            is_buyer: buy,
        });
        for b in &mut self.f.account.balances {
            let delta = match b.asset.as_str() {
                "BTC" => sign * qty,
                "USDT" => -sign * amount,
                _ => 0,
            } - if b.asset == asset { fee } else { 0 };
            b.free = decimal::format(decimal::parse(&b.free).unwrap() + delta).unwrap();
        }
        self.f.account.update_time += 1;
    }
}
impl ExecutionVenue for Sim {
    fn capabilities(&self) -> kaze_quant::external_state::VenueCapabilities {
        self.f.capabilities()
    }
    fn history_head(&mut self, s: &str) -> Result<HistoryHead, VenueError> {
        self.f.history_head(s)
    }
    fn order_history(
        &mut self,
        s: &str,
        f: u64,
        l: u16,
    ) -> Result<Vec<OrderObservation>, VenueError> {
        self.f.order_history(s, f, l)
    }
    fn trade_history(
        &mut self,
        s: &str,
        f: u64,
        l: u16,
    ) -> Result<Vec<TradeObservation>, VenueError> {
        self.f.trade_history(s, f, l)
    }
    fn account_identity(
        &mut self,
    ) -> Result<
        (
            kaze_quant::external_state::ExecutionIdentity,
            AccountObservation,
        ),
        VenueError,
    > {
        if self.delay {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.f.account_identity()
    }
    fn account_open_orders(&mut self) -> Result<Vec<OrderObservation>, VenueError> {
        let mut o: Vec<_> = self
            .f
            .orders
            .iter()
            .filter(|o| !o.status.terminal())
            .cloned()
            .collect();
        if self.f.foreign_open {
            o.push(observation(999));
        }
        Ok(o)
    }
    fn validate_intent(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
        if self.preflight_delay {
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        if self.preflight_fail {
            Err(VenueError::Rejected(-1013))
        } else {
            Ok(())
        }
    }
    fn submit(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        assert!(
            !self.panic_submit,
            "injected crash after durable child intent before POST"
        );
        if self.block_before_post {
            use std::io::Write;
            println!("PREPARED_NO_POST");
            std::io::stdout().flush().unwrap();
            loop {
                std::thread::park();
            }
        }
        self.posts += 1;
        let id = self.f.orders.len() as u64 + 1;
        self.f.orders.push(OrderObservation {
            symbol: i.symbol.clone(),
            client_order_id: i.client_order_id.clone(),
            reported_client_order_id: None,
            order_id: id,
            price: i.price.clone(),
            orig_qty: i.quantity.clone(),
            executed_qty: "0".into(),
            cummulative_quote_qty: "0".into(),
            status: VenueStatus::New,
            side: if i.side == Side::Buy {
                "BUY".into()
            } else {
                "SELL".into()
            },
            update_time: 1,
        });
        if self.fill {
            self.fill_order(id, decimal::parse(&i.quantity).unwrap());
        }
        if self.drop_ack {
            Err(VenueError::Unknown)
        } else {
            self.query(i)
        }
    }
    fn query(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.f
            .orders
            .iter()
            .find(|o| o.client_order_id == i.client_order_id)
            .cloned()
            .ok_or(VenueError::NotFound)
    }
    fn cancel(&mut self, i: &OrderIntent) -> Result<(), VenueError> {
        self.cancels += 1;
        if self.lost_cancel {
            return Err(VenueError::Unknown);
        }
        let o = self
            .f
            .orders
            .iter_mut()
            .find(|o| o.client_order_id == i.client_order_id)
            .unwrap();
        o.status = VenueStatus::Canceled;
        o.update_time += 1;
        Ok(())
    }
    fn account(&mut self) -> Result<AccountObservation, VenueError> {
        self.f.account()
    }
    fn open_orders(&mut self, _: &str) -> Result<Vec<OrderObservation>, VenueError> {
        self.account_open_orders()
    }
    fn trades(&mut self, o: &OrderObservation) -> Result<Vec<TradeObservation>, VenueError> {
        Ok(self
            .f
            .trades
            .iter()
            .filter(|t| t.order_id == o.order_id)
            .cloned()
            .collect())
    }
}
pub fn initialized() -> (Scratch, ExecutionJournal, Sim) {
    let t = Scratch::new();
    let mut j = bound(&t);
    j.init_plan(config()).unwrap();
    (t, j, Sim::new())
}

impl Default for Sim {
    fn default() -> Self {
        Self::new()
    }
}
