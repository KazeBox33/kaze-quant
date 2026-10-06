//! 外部订单使用独立状态机：本地提交事务不等于交易所已接受。
//! 意图先同步，再允许一次发送；未知结果只能查询，不能盲目重发。
use crate::decimal::{self, SCALE};
use crate::paper::PaperError;
use crate::storage::lock_file;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrderIntent {
    pub client_order_id: String,
    pub symbol: String,
    pub side: crate::types::Side,
    pub price: String,
    pub quantity: String,
}
impl OrderIntent {
    pub fn validate(&self, max_notional: i128) -> Result<(), PaperError> {
        if !(1..=36).contains(&self.client_order_id.len())
            || !self
                .client_order_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !self.client_order_id.starts_with("kaze-")
        {
            return Err("client ID must be kaze- prefixed ASCII, at most 36 bytes".into());
        }
        if !(3..=20).contains(&self.symbol.len())
            || !self
                .symbol
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            || !self.symbol.ends_with("USDT")
        {
            return Err("testnet supports uppercase USDT spot symbols".into());
        }
        let p = decimal::parse(&self.price)?;
        let q = decimal::parse(&self.quantity)?;
        let n = p.checked_mul(q).ok_or("notional overflow")?;
        if p == 0
            || q == 0
            || max_notional <= 0
            || n > max_notional
                .checked_mul(SCALE)
                .ok_or("risk bound overflow")?
        {
            return Err("order outside configured notional cap".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VenueStatus {
    New,
    PartiallyFilled,
    Filled,
    Canceled,
    Rejected,
    Expired,
    ExpiredInMatch,
}
impl VenueStatus {
    pub fn terminal(&self) -> bool {
        !matches!(self, Self::New | Self::PartiallyFilled)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderObservation {
    pub symbol: String,
    pub client_order_id: String,
    pub order_id: u64,
    /// 按稳定 orderId 查询时，保留交易所撤单后更换的 client ID。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_client_order_id: Option<String>,
    pub price: String,
    pub orig_qty: String,
    pub executed_qty: String,
    pub cummulative_quote_qty: String,
    pub status: VenueStatus,
    pub side: String,
    #[serde(default, alias = "transactTime")]
    pub update_time: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TradeObservation {
    pub symbol: String,
    pub id: u64,
    pub order_id: u64,
    pub price: String,
    pub qty: String,
    pub quote_qty: String,
    pub commission: String,
    pub commission_asset: String,
    pub is_buyer: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Balance {
    pub asset: String,
    pub free: String,
    pub locked: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountObservation {
    pub can_trade: bool,
    pub balances: Vec<Balance>,
    pub update_time: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct TrackedOrder {
    pub intent: OrderIntent,
    pub phase: String,
    pub observation: Option<OrderObservation>,
}

pub struct ExecutionJournal {
    conn: Connection,
    _lock: File,
}
impl ExecutionJournal {
    pub fn open(path: &Path) -> Result<Self, PaperError> {
        let path = if path.exists() {
            path.canonicalize()?
        } else {
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."))
                .canonicalize()?
                .join(path.file_name().ok_or("journal needs a name")?)
        };
        let lock = lock_file(&path.with_extension("execution.lock"))?;
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_millis(100))?;
        let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if mode != "wal" {
            return Err("execution journal requires WAL".into());
        }
        conn.execute_batch("PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA trusted_schema=OFF; PRAGMA max_page_count=262144;
        CREATE TABLE IF NOT EXISTS execution_meta (revision TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS intents (id TEXT PRIMARY KEY, intent TEXT NOT NULL, phase TEXT NOT NULL, observation TEXT);
        CREATE TABLE IF NOT EXISTS observations (seq INTEGER PRIMARY KEY, id TEXT NOT NULL, body TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS trades (symbol TEXT NOT NULL, id INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY(symbol,id));
        CREATE TABLE IF NOT EXISTS accounts (seq INTEGER PRIMARY KEY, body TEXT NOT NULL);")?;
        let revision: Option<String> = conn
            .query_row("SELECT revision FROM execution_meta", [], |r| r.get(0))
            .optional()?;
        match revision {
            None => {
                conn.execute(
                    "INSERT INTO execution_meta VALUES ('binance-testnet-execution-v1')",
                    [],
                )?;
            }
            Some(r) if r == "binance-testnet-execution-v1" => (),
            _ => return Err("execution journal revision mismatch".into()),
        }
        Ok(Self { conn, _lock: lock })
    }
    /// 返回 false 表示身份已存在，调用方不得再次发送。
    pub fn prepare(&mut self, intent: &OrderIntent, cap: i128) -> Result<bool, PaperError> {
        intent.validate(cap)?;
        let json = serde_json::to_string(intent)?;
        let tx = self.conn.transaction()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT intent FROM intents WHERE id=?1",
                [&intent.client_order_id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            if old != json {
                return Err("client ID reused with a different intent".into());
            }
            return Ok(false);
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM intents", [], |r| r.get(0))?;
        let unresolved: i64 = tx.query_row(
            "SELECT count(*) FROM intents WHERE phase != 'terminal'",
            [],
            |r| r.get(0),
        )?;
        if count >= 10_000 || unresolved != 0 {
            return Err(
                "execution capacity or unresolved order: reconcile before new submission".into(),
            );
        }
        tx.execute(
            "INSERT INTO intents VALUES (?1,?2,'unknown',NULL)",
            params![intent.client_order_id, json],
        )?;
        tx.commit()?;
        Ok(true)
    }
    pub fn orders(&self) -> Result<Vec<TrackedOrder>, PaperError> {
        let mut stmt = self
            .conn
            .prepare("SELECT intent,phase,observation FROM intents ORDER BY rowid")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (intent, phase, observation) = row?;
            Ok(TrackedOrder {
                intent: serde_json::from_str(&intent)?,
                phase,
                observation: observation.map(|v| serde_json::from_str(&v)).transpose()?,
            })
        })
        .collect()
    }
    pub fn observe(&mut self, obs: &OrderObservation) -> Result<(), PaperError> {
        let tx = self.conn.transaction()?;
        let (intent, old): (String, Option<String>) = tx.query_row(
            "SELECT intent,observation FROM intents WHERE id=?1",
            [&obs.client_order_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let intent: OrderIntent = serde_json::from_str(&intent)?;
        let filled = decimal::parse(&obs.executed_qty)?;
        let quote = decimal::parse(&obs.cummulative_quote_qty)?;
        let side = match intent.side {
            crate::types::Side::Buy => "BUY",
            crate::types::Side::Sell => "SELL",
        };
        if intent.symbol != obs.symbol
            || obs.side != side
            || decimal::parse(&intent.price)? != decimal::parse(&obs.price)?
            || decimal::parse(&intent.quantity)? != decimal::parse(&obs.orig_qty)?
            || obs.order_id == 0
            || filled > decimal::parse(&intent.quantity)?
            || (obs.status == VenueStatus::Filled && filled != decimal::parse(&intent.quantity)?)
            || (obs.status == VenueStatus::New && filled != 0)
            || (obs.status == VenueStatus::PartiallyFilled
                && (filled == 0 || filled == decimal::parse(&intent.quantity)?))
            || (filled == 0 && quote != 0)
        {
            return Err("venue observation conflicts with order intent/economics".into());
        }
        let filled_notional = decimal::parse(&intent.price)?
            .checked_mul(filled)
            .ok_or("observation notional overflow")?;
        let reported = quote
            .checked_mul(SCALE)
            .ok_or("observation quote overflow")?;
        if (intent.side == crate::types::Side::Buy && reported > filled_notional)
            || (intent.side == crate::types::Side::Sell && reported < filled_notional)
            || (obs.status == VenueStatus::Rejected && filled != 0)
        {
            return Err("venue fill economics violate limit or rejected status".into());
        }
        if let Some(old) = old {
            let old: OrderObservation = serde_json::from_str(&old)?;
            if old.order_id != obs.order_id
                || filled < decimal::parse(&old.executed_qty)?
                || quote < decimal::parse(&old.cummulative_quote_qty)?
                || obs.update_time < old.update_time
                || (old.status.terminal()
                    && (old.status != obs.status
                        || decimal::parse(&old.executed_qty)? != filled
                        || decimal::parse(&old.cummulative_quote_qty)? != quote))
            {
                return Err("regressing or conflicting venue observation".into());
            }
        }
        let json = serde_json::to_string(obs)?;
        tx.execute(
            "INSERT INTO observations(id,body) VALUES (?1,?2)",
            params![obs.client_order_id, json],
        )?;
        tx.execute(
            "UPDATE intents SET phase=?2,observation=?3 WHERE id=?1",
            params![
                obs.client_order_id,
                if obs.status.terminal() {
                    "terminal"
                } else {
                    "open"
                },
                json
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn record_trades(&mut self, trades: &[TradeObservation]) -> Result<(), PaperError> {
        if trades.len() > 1000 {
            return Err("trade page capacity exceeded".into());
        }
        let tx = self.conn.transaction()?;
        for t in trades {
            if t.id > i64::MAX as u64
                || t.order_id == 0
                || decimal::parse(&t.qty)? == 0
                || decimal::parse(&t.price)? == 0
                || t.commission_asset.is_empty()
                || t.commission_asset.len() > 20
                || !t
                    .commission_asset
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric())
            {
                return Err("invalid trade economics or identity".into());
            }
            decimal::parse(&t.quote_qty)?;
            decimal::parse(&t.commission)?;
            let json = serde_json::to_string(t)?;
            let old: Option<String> = tx
                .query_row(
                    "SELECT body FROM trades WHERE symbol=?1 AND id=?2",
                    params![t.symbol, t.id as i64],
                    |r| r.get(0),
                )
                .optional()?;
            if old.as_ref().is_some_and(|s| s != &json) {
                return Err("conflicting duplicate trade ID".into());
            }
            tx.execute(
                "INSERT OR IGNORE INTO trades VALUES (?1,?2,?3)",
                params![t.symbol, t.id as i64, json],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn record_account(&mut self, a: &AccountObservation) -> Result<(), PaperError> {
        if a.balances.len() > 4096 {
            return Err("balance capacity exceeded".into());
        }
        let mut assets = std::collections::HashSet::new();
        for b in &a.balances {
            if b.asset.is_empty() || !assets.insert(&b.asset) {
                return Err("duplicate balance asset".into());
            }
            decimal::parse(&b.free)?;
            decimal::parse(&b.locked)?;
        }
        self.conn.execute(
            "INSERT INTO accounts(body) VALUES (?1)",
            [serde_json::to_string(a)?],
        )?;
        Ok(())
    }
    /// 用真实成交数量核对订单累计量；手续费按原币种保留，不能冒充已换算 PnL。
    pub fn audit(&self) -> Result<serde_json::Value, PaperError> {
        let mut stmt = self
            .conn
            .prepare("SELECT body FROM trades ORDER BY symbol,id")?;
        let trades = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| -> Result<TradeObservation, PaperError> { Ok(serde_json::from_str(&r?)?) })
            .collect::<Result<Vec<_>, _>>()?;
        let mut problems = Vec::new();
        for o in self.orders()? {
            let Some(obs) = o.observation else {
                problems.push(format!(
                    "{}: unknown submission outcome",
                    o.intent.client_order_id
                ));
                continue;
            };
            let selected: Vec<_> = trades
                .iter()
                .filter(|t| t.symbol == obs.symbol && t.order_id == obs.order_id)
                .collect();
            let mut quantity = 0i128;
            let mut quote = 0i128;
            for t in selected {
                if t.is_buyer != (obs.side == "BUY") {
                    return Err("trade side conflicts with order".into());
                }
                quantity = quantity
                    .checked_add(decimal::parse(&t.qty)?)
                    .ok_or("trade quantity overflow")?;
                quote = quote
                    .checked_add(decimal::parse(&t.quote_qty)?)
                    .ok_or("trade quote overflow")?;
            }
            if quantity != decimal::parse(&obs.executed_qty)?
                || quote != decimal::parse(&obs.cummulative_quote_qty)?
            {
                problems.push(format!(
                    "{}: order/trade economics mismatch",
                    o.intent.client_order_id
                ));
            }
        }
        let latest: Option<String> = self
            .conn
            .query_row(
                "SELECT body FROM accounts ORDER BY seq DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        if latest.is_none() {
            problems.push("account snapshot absent".into());
        }
        Ok(
            serde_json::json!({"schema_version":1,"environment":"binance-spot-testnet","problems":problems,"orders":self.orders()?,"trades":trades,"account":latest.map(|s| serde_json::from_str::<AccountObservation>(&s)).transpose()?,"scope":"observed exchange balances and per-order trade reconciliation; no portfolio PnL or authenticated journal signatures"}),
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum VenueError {
    Unknown,
    Rejected(i64),
    Unavailable,
    NotFound,
}
impl std::fmt::Display for VenueError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "venue outcome: {self:?}; query/reconcile before any new submission"
        )
    }
}
impl std::error::Error for VenueError {}
/// 传输适配器只返回交易所证据；本地订单 ID 和重投政策由网关统一管理。
pub trait ExecutionVenue {
    fn validate_intent(&mut self, intent: &OrderIntent) -> Result<(), VenueError>;
    fn submit(&mut self, intent: &OrderIntent) -> Result<OrderObservation, VenueError>;
    fn query(&mut self, intent: &OrderIntent) -> Result<OrderObservation, VenueError>;
    /// 已知交易所 ID 时优先用稳定身份查询；默认适配器可沿用 client ID。
    fn query_known(
        &mut self,
        intent: &OrderIntent,
        _order_id: Option<u64>,
    ) -> Result<OrderObservation, VenueError> {
        self.query(intent)
    }
    fn cancel(&mut self, intent: &OrderIntent) -> Result<(), VenueError>;
    fn account(&mut self) -> Result<AccountObservation, VenueError>;
    fn open_orders(&mut self, symbol: &str) -> Result<Vec<OrderObservation>, VenueError>;
    fn trades(
        &mut self,
        observation: &OrderObservation,
    ) -> Result<Vec<TradeObservation>, VenueError>;
}
fn venue_error(e: VenueError) -> PaperError {
    PaperError(e.to_string())
}
impl ExecutionJournal {
    pub fn submit_once(
        &mut self,
        venue: &mut impl ExecutionVenue,
        intent: &OrderIntent,
        cap: i128,
    ) -> Result<(), PaperError> {
        intent.validate(cap)?;
        // 再次收到同一意图只做查询，连 preflight 都不允许变成二次提交。
        if self
            .orders()?
            .iter()
            .any(|o| o.intent.client_order_id == intent.client_order_id)
        {
            self.prepare(intent, cap)?;
            let known = self
                .orders()?
                .into_iter()
                .find(|o| o.intent.client_order_id == intent.client_order_id)
                .and_then(|o| o.observation.map(|v| v.order_id));
            let obs = venue.query_known(intent, known).map_err(venue_error)?;
            return self.observe(&obs);
        }
        self.reconcile(venue)?;
        venue.validate_intent(intent).map_err(venue_error)?;
        if !self.prepare(intent, cap)? {
            return Err("intent already prepared".into());
        }
        let obs = venue.submit(intent).map_err(venue_error)?;
        self.observe(&obs)
    }
    pub fn cancel_once(
        &mut self,
        venue: &mut impl ExecutionVenue,
        id: &str,
    ) -> Result<(), PaperError> {
        let order = self
            .orders()?
            .into_iter()
            .find(|o| o.intent.client_order_id == id)
            .ok_or("unknown client ID")?;
        let before = venue
            .query_known(
                &order.intent,
                order.observation.as_ref().map(|o| o.order_id),
            )
            .map_err(venue_error)?;
        self.observe(&before)?;
        if before.status.terminal() {
            return Ok(());
        }
        self.conn.execute(
            "UPDATE intents SET phase='cancel_unknown' WHERE id=?1",
            [id],
        )?;
        // DELETE 可更换 client ID；之后通过不变的交易所 orderId 查询，原意图身份永不复用。
        let sent = venue.cancel(&order.intent);
        let queried = venue
            .query_known(&order.intent, Some(before.order_id))
            .map_err(venue_error)?;
        self.observe(&queried)?;
        if !queried.status.terminal() {
            return Err("cancel unresolved; order remains open".into());
        }
        // 查询到终态已提供更强证据，因此即使 DELETE 超时也可结束撤单流程。
        let _ = sent;
        Ok(())
    }
    pub fn reconcile(&mut self, venue: &mut impl ExecutionVenue) -> Result<(), PaperError> {
        let orders = self.orders()?;
        for o in &orders {
            let obs = venue
                .query_known(&o.intent, o.observation.as_ref().map(|v| v.order_id))
                .map_err(venue_error)?;
            self.observe(&obs)?;
            let trades = venue.trades(&obs).map_err(venue_error)?;
            self.record_trades(&trades)?;
        }
        let symbols: std::collections::BTreeSet<_> =
            orders.iter().map(|o| o.intent.symbol.as_str()).collect();
        for symbol in symbols {
            for remote in venue.open_orders(symbol).map_err(venue_error)? {
                if !orders.iter().any(|o| {
                    o.intent.client_order_id == remote.client_order_id
                        && o.intent.symbol == remote.symbol
                }) {
                    return Err("untracked exchange order: manual reconciliation required".into());
                }
            }
        }
        let account = venue.account().map_err(venue_error)?;
        if !account.can_trade {
            return Err("exchange account cannot trade".into());
        }
        self.record_account(&account)?;
        let audit = self.audit()?;
        if !audit["problems"]
            .as_array()
            .ok_or("invalid audit")?
            .is_empty()
        {
            return Err("order/trade/account reconciliation incomplete".into());
        }
        Ok(())
    }
}
