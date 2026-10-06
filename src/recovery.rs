//! 先收集可核对的 REST 证据，再一次性提交订单、成交、资产与历史游标。
//! 游标来自期初之前的交易所历史，不能从已收到的 WebSocket 最大 ID 推断完整性。
use crate::{
    decimal,
    execution::*,
    external_state::{ExecutionIdentity, InstrumentBinding},
    paper::PaperError,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryHead {
    pub symbol: String,
    pub order_id: Option<u64>,
    pub trade_id: Option<u64>,
}
impl HistoryHead {
    fn validate(&self) -> Result<(), PaperError> {
        let i = OrderIntent {
            client_order_id: "kaze-cursor".into(),
            symbol: self.symbol.clone(),
            side: crate::types::Side::Buy,
            price: "1".into(),
            quantity: "1".into(),
        };
        i.validate(decimal::SCALE)?;
        if self.order_id == Some(0)
            || self.order_id.is_some_and(|v| v >= i64::MAX as u64)
            || self.trade_id.is_some_and(|v| v >= i64::MAX as u64)
        {
            return Err("invalid history head identity".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct HistoryCursor {
    pub seed: HistoryHead,
    pub confirmed: HistoryHead,
}
#[derive(Clone, Copy, Debug)]
pub struct RecoveryOptions {
    pub page_limit: u16,
    pub max_pages: u16,
    pub max_records: usize,
}
impl Default for RecoveryOptions {
    fn default() -> Self {
        Self {
            page_limit: 200,
            max_pages: 256,
            max_records: 100000,
        }
    }
}
impl RecoveryOptions {
    fn validate(self) -> Result<(), PaperError> {
        if !(1..=1000).contains(&self.page_limit)
            || !(1..=256).contains(&self.max_pages)
            || !(1..=100000).contains(&self.max_records)
        {
            return Err("invalid recovery limits".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct RecoveryStats {
    pub order_pages: u64,
    pub trade_pages: u64,
    pub pending_order_queries: u64,
    pub history_head_requests: u64,
    pub account_requests: u64,
    pub open_order_requests: u64,
    pub order_records: u64,
    pub trade_records: u64,
    pub new_trades: u64,
}
impl RecoveryStats {
    pub fn rest_operations(&self) -> u64 {
        self.order_pages
            + self.trade_pages
            + self.pending_order_queries
            + self.history_head_requests
            + self.account_requests
            + self.open_order_requests
    }
}
pub(crate) fn create_schema(c: &Connection) -> Result<(), PaperError> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS recovery_control(singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision TEXT NOT NULL, rounds INTEGER NOT NULL, stats TEXT, checkpoint TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS recovery_cursors(symbol TEXT PRIMARY KEY, seed_order TEXT, seed_trade TEXT, last_order TEXT, last_trade TEXT);")?;
    Ok(())
}
pub(crate) fn bind_cursors_tx(
    c: &Connection,
    instruments: &[InstrumentBinding],
    heads: &[HistoryHead],
) -> Result<(), PaperError> {
    let expected: BTreeSet<_> = instruments.iter().map(|v| v.symbol.as_str()).collect();
    let found: BTreeSet<_> = heads.iter().map(|v| v.symbol.as_str()).collect();
    if expected != found || heads.len() != found.len() {
        return Err("history heads must bind every instrument exactly once".into());
    }
    c.execute(
        "INSERT INTO recovery_control VALUES(1,'spot-history-v1',0,NULL,'')",
        [],
    )?;
    for h in heads {
        h.validate()?;
        c.execute(
            "INSERT INTO recovery_cursors VALUES(?1,?2,?3,?2,?3)",
            params![
                h.symbol,
                h.order_id.map(|v| v.to_string()),
                h.trade_id.map(|v| v.to_string())
            ],
        )?;
    }
    update_checkpoint(c)?;
    Ok(())
}
fn checkpoint(c: &Connection) -> Result<String, PaperError> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(b"kaze-spot-history-checkpoint-v1");
    for sql in [
        "SELECT identity FROM external_control",
        "SELECT json_array(symbol,base,quote) FROM instruments ORDER BY symbol",
        "SELECT json_array(asset,units) FROM ledger_baseline ORDER BY asset",
        "SELECT json_array(symbol,seed_order,seed_trade,last_order,last_trade) FROM recovery_cursors ORDER BY symbol",
        "SELECT json_array(revision,rounds,stats) FROM recovery_control",
    ] {
        let mut stmt = c.prepare(sql)?;
        for r in stmt.query_map([], |r| r.get::<_, String>(0))? {
            let s = r?;
            hash.update((s.len() as u64).to_le_bytes());
            hash.update(s.as_bytes());
        }
    }
    Ok(crate::journal::hex(&hash.finalize()))
}
fn update_checkpoint(c: &Connection) -> Result<(), PaperError> {
    c.execute(
        "UPDATE recovery_control SET checkpoint=?1",
        [checkpoint(c)?],
    )?;
    Ok(())
}
fn verify_checkpoint(c: &Connection) -> Result<(), PaperError> {
    let expected: String =
        c.query_row("SELECT checkpoint FROM recovery_control", [], |r| r.get(0))?;
    if expected != checkpoint(c)? {
        return Err("history checkpoint integrity conflict".into());
    }
    Ok(())
}
fn id(s: Option<String>) -> Result<Option<u64>, PaperError> {
    s.map(|s| {
        s.parse::<u64>()
            .map_err(|_| "invalid durable history ID".into())
    })
    .transpose()
}
fn at_least(new: Option<u64>, old: Option<u64>) -> bool {
    old.is_none_or(|v| new.is_some_and(|n| n >= v))
}
fn start(seed: Option<u64>, last: Option<u64>) -> u64 {
    // 初始旧历史不属于当前期初；后续包含最后确认项以检查重叠经济内容。
    match (seed, last) {
        (_, None) => 0,
        (s, Some(v)) if s == Some(v) => v + 1,
        (_, Some(v)) => v,
    }
}
fn venue(e: VenueError) -> PaperError {
    PaperError(e.to_string())
}
struct Collected {
    orders: Vec<OrderObservation>,
    trades: Vec<TradeObservation>,
    heads: Vec<HistoryHead>,
    account: AccountObservation,
    stats: RecoveryStats,
}
impl ExecutionJournal {
    pub fn has_history_recovery(&self) -> Result<bool, PaperError> {
        let revision: Option<String> = self
            .conn
            .query_row("SELECT revision FROM recovery_control", [], |r| r.get(0))
            .optional()?;
        match revision.as_deref() {
            None => Ok(false),
            Some("spot-history-v1") => Ok(true),
            _ => Err("history recovery revision conflict".into()),
        }
    }
    pub fn history_cursors(&self) -> Result<Vec<HistoryCursor>, PaperError> {
        if !self.has_history_recovery()? {
            return Ok(vec![]);
        }
        verify_checkpoint(&self.conn)?;
        let mut s=self.conn.prepare("SELECT symbol,seed_order,seed_trade,last_order,last_trade FROM recovery_cursors ORDER BY symbol")?;
        let rows = s.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (symbol, so, st, lo, lt) = r?;
            let seed = HistoryHead {
                symbol: symbol.clone(),
                order_id: id(so)?,
                trade_id: id(st)?,
            };
            let confirmed = HistoryHead {
                symbol,
                order_id: id(lo)?,
                trade_id: id(lt)?,
            };
            seed.validate()?;
            confirmed.validate()?;
            if !at_least(confirmed.order_id, seed.order_id)
                || !at_least(confirmed.trade_id, seed.trade_id)
            {
                return Err("regressing durable cursor".into());
            }
            out.push(HistoryCursor { seed, confirmed });
        }
        let registered: i64 = self
            .conn
            .query_row("SELECT count(*) FROM instruments", [], |r| r.get(0))?;
        if out.is_empty() || out.len() > 64 || out.len() != registered as usize {
            return Err("incomplete history cursor binding".into());
        }
        for c in &out {
            let exists: i64 = self.conn.query_row(
                "SELECT count(*) FROM instruments WHERE symbol=?1",
                [&c.seed.symbol],
                |r| r.get(0),
            )?;
            if exists != 1 {
                return Err("cursor instrument identity conflict".into());
            }
        }
        Ok(out)
    }
    pub fn history_audit(&self) -> Result<serde_json::Value, PaperError> {
        if !self.has_history_recovery()? {
            return Ok(serde_json::Value::Null);
        }
        let (rounds, stats): (i64, Option<String>) =
            self.conn
                .query_row("SELECT rounds,stats FROM recovery_control", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
        if !(0..=100000).contains(&rounds) {
            return Err("invalid recovery round count".into());
        }
        Ok(
            serde_json::json!({"schema_version":1,"confirmed_rounds":rounds,"cursors":self.history_cursors()?,"last_round":stats.map(|s|serde_json::from_str::<serde_json::Value>(&s)).transpose()?,"scope":"registered symbols only, durable REST cursors; server history retention/coverage and dedicated-account baseline required; no global private-stream sequence"}),
        )
    }
    /// 失败时整个候选事务回滚；已经收集的最大 ID 绝不成为确认高水位。
    pub fn recover_history(
        &mut self,
        v: &mut impl ExecutionVenue,
        options: RecoveryOptions,
    ) -> Result<RecoveryStats, PaperError> {
        options.validate()?;
        self.mark_execution_gap("history recovery started", false)?;
        let expected = self
            .execution_identity()?
            .ok_or("history recovery requires account binding")?;
        let collected = self.collect_history(v, &expected, options)?;
        let result = (|| -> Result<RecoveryStats, PaperError> {
            // unchecked_transaction 是安全 API；此处没有外层事务，借用连接供只读审计候选。
            let tx = self.conn.unchecked_transaction()?;
            let mut stats = collected.stats;
            for o in &collected.orders {
                observe_tx(&tx, o)?;
            }
            for trades in collected.trades.chunks(1000) {
                stats.new_trades += record_trades_tx(&tx, trades)? as u64;
            }
            record_account_tx(&tx, &collected.account)?;
            let expected = self.expected_balances()?;
            let actual = crate::external_state::totals(&collected.account)?;
            let assets: BTreeSet<_> = expected.keys().chain(actual.keys()).collect();
            for a in assets {
                let want = expected
                    .get(a)
                    .map(|v| decimal::parse(v))
                    .transpose()?
                    .unwrap_or(0);
                if actual.get(a).copied().unwrap_or(0) != want {
                    return Err("history recovery asset totals conflict".into());
                }
            }
            let audit = self.audit()?;
            if !audit["problems"]
                .as_array()
                .ok_or("invalid recovery audit")?
                .is_empty()
            {
                return Err("history recovery order/trade audit incomplete".into());
            }
            for h in &collected.heads {
                tx.execute(
                    "UPDATE recovery_cursors SET last_order=?2,last_trade=?3 WHERE symbol=?1",
                    params![
                        h.symbol,
                        h.order_id.map(|v| v.to_string()),
                        h.trade_id.map(|v| v.to_string())
                    ],
                )?;
            }
            let blocked: i64 =
                tx.query_row("SELECT blocked FROM external_control", [], |r| r.get(0))?;
            if blocked != 0 {
                return Err("external activity requires manual classification".into());
            }
            let rounds: i64 =
                tx.query_row("SELECT rounds FROM recovery_control", [], |r| r.get(0))?;
            if !(0..100000).contains(&rounds) {
                return Err("history recovery round capacity exceeded".into());
            }
            tx.execute(
                "UPDATE recovery_control SET rounds=rounds+1,stats=?1",
                [serde_json::to_string(&stats)?],
            )?;
            update_checkpoint(&tx)?;
            tx.execute("UPDATE external_control SET health='ready',reason='REST orders/trades/assets reconciled' WHERE blocked=0",[])?;
            tx.commit()?;
            Ok(stats)
        })();
        if result.is_err() {
            self.mark_execution_gap("history recovery failed; candidate rolled back", false)?;
        }
        result
    }
    fn collect_history(
        &mut self,
        v: &mut impl ExecutionVenue,
        expected: &ExecutionIdentity,
        options: RecoveryOptions,
    ) -> Result<Collected, PaperError> {
        let cursors = self.history_cursors()?;
        if cursors.is_empty() {
            return Err("history baseline absent; use new isolated journal".into());
        }
        let (actual, _) = v.account_identity().map_err(venue)?;
        if &actual != expected {
            self.mark_execution_gap("account identity conflict", true)?;
            return Err("account identity conflict".into());
        }
        let tracked = self.orders()?;
        let by_client: BTreeMap<_, _> = tracked
            .iter()
            .enumerate()
            .map(|(i, o)| (o.intent.client_order_id.clone(), i))
            .collect();
        let by_venue: BTreeMap<_, _> = tracked
            .iter()
            .enumerate()
            .filter_map(|(i, o)| {
                o.observation
                    .as_ref()
                    .map(|v| ((v.symbol.clone(), v.order_id), i))
            })
            .collect();
        let mut observations = BTreeMap::<usize, OrderObservation>::new();
        let mut trades = Vec::new();
        let mut heads = Vec::new();
        let mut stats = RecoveryStats {
            account_requests: 1,
            ..Default::default()
        };
        for cursor in cursors {
            let h = v.history_head(&cursor.seed.symbol).map_err(venue)?;
            h.validate()?;
            stats.history_head_requests += 2;
            if h.symbol != cursor.seed.symbol
                || !at_least(h.order_id, cursor.confirmed.order_id)
                || !at_least(h.trade_id, cursor.confirmed.trade_id)
            {
                self.mark_execution_gap("exchange history regressed or reset", true)?;
                return Err("exchange history regressed or reset".into());
            }
            let mut confirmed = cursor.confirmed.clone();
            let mut from = start(cursor.seed.order_id, confirmed.order_id);
            loop {
                check_budget(&stats, options)?;
                let page = v
                    .order_history(&cursor.seed.symbol, from, options.page_limit)
                    .map_err(venue)?;
                stats.order_pages += 1;
                if page.len() > options.page_limit as usize {
                    return Err("order page exceeds requested limit".into());
                }
                let complete = page.len() < options.page_limit as usize;
                let mut previous = None;
                for remote in page {
                    if remote.symbol != cursor.seed.symbol
                        || remote.order_id < from
                        || previous.is_some_and(|p| remote.order_id <= p)
                        || remote.order_id >= i64::MAX as u64
                    {
                        return Err("order page identity/order conflict".into());
                    }
                    previous = Some(remote.order_id);
                    let index = match bind_order(remote, &tracked, &by_client, &by_venue) {
                        Ok((i, o)) => {
                            confirmed.order_id =
                                Some(confirmed.order_id.unwrap_or(0).max(o.order_id));
                            observations.insert(i, o);
                            i
                        }
                        Err(e) => {
                            self.mark_execution_gap("untracked history order", true)?;
                            return Err(e);
                        }
                    };
                    let _ = index;
                    stats.order_records += 1;
                    check_records(&stats, options)?;
                }
                if complete {
                    break;
                }
                from = previous.ok_or("full order page did not advance")? + 1;
            }
            let mut from = start(cursor.seed.trade_id, confirmed.trade_id);
            loop {
                check_budget(&stats, options)?;
                let page = v
                    .trade_history(&cursor.seed.symbol, from, options.page_limit)
                    .map_err(venue)?;
                stats.trade_pages += 1;
                if page.len() > options.page_limit as usize {
                    return Err("trade page exceeds requested limit".into());
                }
                let complete = page.len() < options.page_limit as usize;
                let mut previous = None;
                for t in page {
                    if t.symbol != cursor.seed.symbol
                        || t.id < from
                        || previous.is_some_and(|p| t.id <= p)
                        || t.id >= i64::MAX as u64
                    {
                        return Err("trade page identity/order conflict".into());
                    }
                    previous = Some(t.id);
                    confirmed.trade_id = Some(confirmed.trade_id.unwrap_or(0).max(t.id));
                    trades.push(t);
                    stats.trade_records += 1;
                    check_records(&stats, options)?;
                }
                if complete {
                    break;
                }
                from = previous.ok_or("full trade page did not advance")? + 1;
            }
            // 已见 head 可能刚由交易所数据库返回，页仍有延迟；不能确认少于已见 head 的前缀。
            if !at_least(confirmed.order_id, h.order_id)
                || !at_least(confirmed.trade_id, h.trade_id)
            {
                return Err("history page incomplete relative to observed head".into());
            }
            heads.push(confirmed);
        }
        for (i, o) in tracked.iter().enumerate() {
            if !observations.contains_key(&i)
                && o.observation.as_ref().is_none_or(|v| !v.status.terminal())
            {
                let remote = v
                    .query_known(&o.intent, o.observation.as_ref().map(|v| v.order_id))
                    .map_err(venue)?;
                stats.pending_order_queries += 1;
                let (bound, remote) = bind_order(remote, &tracked, &by_client, &by_venue)?;
                if bound != i {
                    return Err("pending order query identity conflict".into());
                }
                observations.insert(i, remote);
            }
        }
        let remote_open = v.account_open_orders().map_err(venue)?;
        stats.open_order_requests += 1;
        if remote_open.len() > 10000 {
            return Err("open order capacity exceeded".into());
        }
        for remote in remote_open {
            let (i, remote) = match bind_order(remote, &tracked, &by_client, &by_venue) {
                Ok(v) => v,
                Err(e) => {
                    self.mark_execution_gap("untracked exchange order", true)?;
                    return Err(e);
                }
            };
            observations.insert(i, remote);
        }
        let (actual, account) = v.account_identity().map_err(venue)?;
        stats.account_requests += 1;
        if &actual != expected {
            self.mark_execution_gap("account identity conflict", true)?;
            return Err("account identity conflict".into());
        }
        Ok(Collected {
            orders: observations.into_values().collect(),
            trades,
            heads,
            account,
            stats,
        })
    }
}
fn check_budget(stats: &RecoveryStats, o: RecoveryOptions) -> Result<(), PaperError> {
    if stats.order_pages + stats.trade_pages >= u64::from(o.max_pages) {
        return Err("history page budget exhausted before complete prefix".into());
    }
    Ok(())
}
fn check_records(stats: &RecoveryStats, o: RecoveryOptions) -> Result<(), PaperError> {
    if stats.order_records + stats.trade_records > o.max_records as u64 {
        return Err("history record budget exhausted".into());
    }
    Ok(())
}
fn bind_order(
    mut remote: OrderObservation,
    tracked: &[TrackedOrder],
    clients: &BTreeMap<String, usize>,
    venue: &BTreeMap<(String, u64), usize>,
) -> Result<(usize, OrderObservation), PaperError> {
    let stable = venue
        .get(&(remote.symbol.clone(), remote.order_id))
        .copied();
    let by_client = clients.get(&remote.client_order_id).copied();
    if stable.is_some() && by_client.is_some() && stable != by_client {
        return Err("conflicting client/venue order identity".into());
    }
    let i = stable
        .or(by_client)
        .ok_or("foreign account history order; manual classification required")?;
    if remote.symbol != tracked[i].intent.symbol {
        return Err("history instrument conflict".into());
    }
    if remote.client_order_id != tracked[i].intent.client_order_id {
        if !remote.status.terminal() {
            return Err("open history client identity conflict".into());
        }
        remote.reported_client_order_id = Some(remote.client_order_id);
        remote.client_order_id = tracked[i].intent.client_order_id.clone();
    }
    Ok((i, remote))
}
