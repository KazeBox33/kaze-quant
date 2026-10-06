//! 外部现货组合账本：期初资产 + 去重成交 = 应有总资产。锁定资金仍属于账户。
//! 没有汇率时不把第三币种手续费转换成 PnL，也不自动吸收存款或外部交易。
use crate::decimal;
use crate::execution::{AccountObservation, ExecutionJournal, OrderIntent, TradeObservation};
use crate::paper::PaperError;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionIdentity {
    pub venue: String,
    /// 交易所稳定 UID 的域分离 SHA256；不是 API key，也不是身份认证签名。
    pub account_hash: String,
}
impl ExecutionIdentity {
    pub fn validate(&self) -> Result<(), PaperError> {
        if self.venue != "binance-spot-testnet"
            || self.account_hash.len() != 64
            || !self
                .account_hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err("unsupported execution identity".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Default, Serialize)]
pub struct VenueCapabilities {
    pub spot_limit_gtc: bool,
    pub account_identity: bool,
    pub account_open_orders: bool,
    pub original_currency_fees: bool,
    pub private_stream: bool,
    pub cursor_history: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentBinding {
    pub symbol: String,
    pub base_asset: String,
    pub quote_asset: String,
}
impl InstrumentBinding {
    fn validate(&self) -> Result<(), PaperError> {
        asset(&self.base_asset)?;
        asset(&self.quote_asset)?;
        if self.base_asset == self.quote_asset
            || self.quote_asset != "USDT"
            || self.symbol != format!("{}{}", self.base_asset, self.quote_asset)
        {
            return Err("instrument identity conflicts with supported spot market".into());
        }
        let i = OrderIntent {
            client_order_id: "kaze-binding".into(),
            symbol: self.symbol.clone(),
            side: crate::types::Side::Buy,
            price: "1".into(),
            quantity: "1".into(),
        };
        i.validate(decimal::SCALE)
    }
}
pub(crate) fn asset(a: &str) -> Result<(), PaperError> {
    if a.is_empty() || a.len() > 64 || !a.chars().all(|c| c.is_alphanumeric() || "-_".contains(c)) {
        return Err("invalid asset identity".into());
    }
    Ok(())
}
pub(crate) fn create_schema(c: &Connection) -> Result<(), PaperError> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS external_control (singleton INTEGER PRIMARY KEY CHECK(singleton=1), identity TEXT NOT NULL, health TEXT NOT NULL, reason TEXT NOT NULL, blocked INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS instruments(symbol TEXT PRIMARY KEY, base TEXT NOT NULL, quote TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS ledger_baseline(asset TEXT PRIMARY KEY, units TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS asset_movements(asset TEXT PRIMARY KEY, units TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS private_events(symbol TEXT NOT NULL, order_id TEXT NOT NULL, execution_id TEXT NOT NULL, body TEXT NOT NULL, PRIMARY KEY(symbol,order_id,execution_id));")?;
    // 打开进程只恢复证据，不能把上一次 Ready 当成当前连接健康。
    c.execute("UPDATE external_control SET health='needs_reconciliation',reason='process opened' WHERE blocked=0",[])?;
    Ok(())
}
pub(crate) fn totals(a: &AccountObservation) -> Result<BTreeMap<String, i128>, PaperError> {
    if !a.can_trade || a.balances.len() > 4096 {
        return Err("account unavailable or balance capacity exceeded".into());
    }
    let mut out = BTreeMap::new();
    for b in &a.balances {
        asset(&b.asset)?;
        let n = decimal::parse(&b.free)?
            .checked_add(decimal::parse(&b.locked)?)
            .ok_or("balance overflow")?;
        if n > 10i128.pow(30) || out.insert(b.asset.clone(), n).is_some() {
            return Err("duplicate or oversized account asset".into());
        }
    }
    Ok(out)
}
fn units(c: &Connection, table: &str) -> Result<BTreeMap<String, i128>, PaperError> {
    // table 只来自内部静态字符串，不来自协议输入。
    let mut s = c.prepare(&format!("SELECT asset,units FROM {table} ORDER BY asset"))?;
    s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
        .map(|r| {
            let (a, n) = r?;
            asset(&a)?;
            let n = n.parse::<i128>().map_err(|_| "invalid ledger integer")?;
            if n.unsigned_abs() > 10u128.pow(30) {
                return Err("ledger bound exceeded".into());
            }
            Ok((a, n))
        })
        .collect()
}
fn add(m: &mut BTreeMap<String, i128>, a: &str, n: i128) -> Result<(), PaperError> {
    asset(a)?;
    let v = m.entry(a.into()).or_default();
    *v = v.checked_add(n).ok_or("asset movement overflow")?;
    if v.unsigned_abs() > 10u128.pow(30) || m.len() > 4096 {
        return Err("asset movement capacity exceeded".into());
    }
    Ok(())
}
fn movement(c: &Connection, t: &TradeObservation) -> Result<BTreeMap<String, i128>, PaperError> {
    let (base, quote): (String, String) = c.query_row(
        "SELECT base,quote FROM instruments WHERE symbol=?1",
        [&t.symbol],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let body:String=c.query_row("SELECT intent FROM intents WHERE observation IS NOT NULL AND json_extract(observation,'$.symbol')=?1 AND json_extract(observation,'$.orderId')=?2",
        params![t.symbol,i64::try_from(t.order_id).map_err(|_|"order identity exceeds SQLite bound")?],|r|r.get(0))?;
    let i: OrderIntent = serde_json::from_str(&body)?;
    let buy = i.side == crate::types::Side::Buy;
    let qty = decimal::parse(&t.qty)?;
    let amount = decimal::parse(&t.quote_qty)?;
    let price = decimal::parse(&t.price)?;
    let limit = decimal::parse(&i.price)?;
    if buy != t.is_buyer
        || qty > decimal::parse(&i.quantity)?
        || amount == 0
        || (buy && price > limit)
        || (!buy && price < limit)
        || (buy
            && amount
                .checked_mul(decimal::SCALE)
                .ok_or("trade amount overflow")?
                > limit.checked_mul(qty).ok_or("trade amount overflow")?)
        || (!buy
            && amount
                .checked_mul(decimal::SCALE)
                .ok_or("trade amount overflow")?
                < limit.checked_mul(qty).ok_or("trade amount overflow")?)
    {
        return Err("trade identity/limit conflict".into());
    }
    // 单笔成交报价金额应在 8 位小数截断误差内，不凭成交价重新覆盖交易所金额。
    let product = price.checked_mul(qty).ok_or("trade notional overflow")? / decimal::SCALE;
    if product.abs_diff(amount) > 1 {
        return Err("trade price/quote conflict".into());
    }
    let mut m = BTreeMap::new();
    add(&mut m, &base, if buy { qty } else { -qty })?;
    add(&mut m, &quote, if buy { -amount } else { amount })?;
    let fee = decimal::parse(&t.commission)?;
    if fee > 0 {
        add(&mut m, &t.commission_asset, -fee)?;
    }
    Ok(m)
}
pub(crate) fn record_trade_movement(
    c: &Connection,
    t: &TradeObservation,
) -> Result<(), PaperError> {
    if identity(c)?.is_none() {
        return Ok(());
    }
    let mut all = units(c, "asset_movements")?;
    for (a, n) in movement(c, t)? {
        add(&mut all, &a, n)?;
        c.execute("INSERT INTO asset_movements VALUES(?1,?2) ON CONFLICT(asset) DO UPDATE SET units=excluded.units",params![a,all[&a].to_string()])?;
    }
    c.execute("UPDATE external_control SET health='needs_reconciliation',reason='new trade evidence' WHERE blocked=0",[])?;
    Ok(())
}
fn identity(c: &Connection) -> Result<Option<ExecutionIdentity>, PaperError> {
    let s: Option<String> = c
        .query_row(
            "SELECT identity FROM external_control WHERE singleton=1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    s.map(|s| Ok(serde_json::from_str(&s)?)).transpose()
}
impl ExecutionJournal {
    pub fn execution_identity(&self) -> Result<Option<ExecutionIdentity>, PaperError> {
        identity(&self.conn)
    }
    /// 只允许为全新账本设期初余额；旧 journal 必须保留原义，不能偷偷迁移资产。
    pub fn bind_account(
        &mut self,
        id: &ExecutionIdentity,
        a: &AccountObservation,
        instruments: &[InstrumentBinding],
    ) -> Result<(), PaperError> {
        self.bind_account_inner(id, a, instruments, None)
    }
    pub fn bind_account_with_history(
        &mut self,
        id: &ExecutionIdentity,
        a: &AccountObservation,
        instruments: &[InstrumentBinding],
        heads: &[crate::recovery::HistoryHead],
    ) -> Result<(), PaperError> {
        self.bind_account_inner(id, a, instruments, Some(heads))
    }
    fn bind_account_inner(
        &mut self,
        id: &ExecutionIdentity,
        a: &AccountObservation,
        instruments: &[InstrumentBinding],
        heads: Option<&[crate::recovery::HistoryHead]>,
    ) -> Result<(), PaperError> {
        id.validate()?;
        if instruments.is_empty() || instruments.len() > 64 {
            return Err("instrument capacity invalid".into());
        }
        let baseline = totals(a)?;
        let tx = self.conn.transaction()?;
        if identity(&tx)?.is_some() {
            return Err("account already bound".into());
        }
        let count: i64 = tx.query_row(
            "SELECT (SELECT count(*) FROM intents)+(SELECT count(*) FROM trades)",
            [],
            |r| r.get(0),
        )?;
        if count != 0 {
            return Err("cannot attach a baseline to legacy execution history".into());
        }
        tx.execute(
            "INSERT INTO external_control VALUES(1,?1,'needs_reconciliation','initial baseline',0)",
            [serde_json::to_string(id)?],
        )?;
        for i in instruments {
            i.validate()?;
            tx.execute(
                "INSERT INTO instruments VALUES(?1,?2,?3)",
                params![i.symbol, i.base_asset, i.quote_asset],
            )?;
        }
        for (a, n) in baseline {
            tx.execute(
                "INSERT INTO ledger_baseline VALUES(?1,?2)",
                params![a, n.to_string()],
            )?;
        }
        if let Some(heads) = heads {
            crate::recovery::bind_cursors_tx(&tx, instruments, heads)?;
        }
        tx.commit()?;
        Ok(())
    }
    pub fn require_reconciled(&self) -> Result<(), PaperError> {
        let state: Option<(String, i64)> = self
            .conn
            .query_row("SELECT health,blocked FROM external_control", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()?;
        if state.is_some_and(|(s, b)| s != "ready" || b != 0) {
            return Err("external account requires reconciliation before new intent".into());
        }
        Ok(())
    }
    pub fn mark_execution_gap(&mut self, reason: &str, sticky: bool) -> Result<(), PaperError> {
        if reason.len() > 128 || !reason.is_ascii() {
            return Err("invalid gap reason".into());
        }
        self.conn.execute("UPDATE external_control SET health='needs_reconciliation',reason=?1,blocked=max(blocked,?2)",params![reason,i64::from(sticky)])?;
        Ok(())
    }
    pub fn expected_balances(&self) -> Result<BTreeMap<String, String>, PaperError> {
        let mut m = units(&self.conn, "ledger_baseline")?;
        for (a, n) in units(&self.conn, "asset_movements")? {
            add(&mut m, &a, n)?;
        }
        m.into_iter()
            .map(|(a, n)| Ok((a, decimal::format(n)?)))
            .collect()
    }
    /// 只比较每个币种的总资产，不能凭此证明外部活动不存在，也不计算策略收益。
    pub fn reconcile_balances(&mut self, a: &AccountObservation) -> Result<(), PaperError> {
        if self.execution_identity()?.is_none() {
            return Err("account baseline absent".into());
        }
        let actual = totals(a)?;
        let expected = self.expected_balances()?;
        let mut all = actual.clone();
        for a in expected.keys() {
            all.entry(a.clone()).or_default();
        }
        for (a, n) in all {
            let want = expected
                .get(&a)
                .map(|v| decimal::parse(v))
                .transpose()?
                .unwrap_or(0);
            if n != want {
                self.mark_execution_gap("asset totals conflict", false)?;
                return Err("asset totals conflict; external activity or missing trades".into());
            }
        }
        Ok(())
    }
    pub(crate) fn mark_reconciled(&mut self) -> Result<(), PaperError> {
        let blocked: Option<i64> = self
            .conn
            .query_row("SELECT blocked FROM external_control", [], |r| r.get(0))
            .optional()?;
        if blocked == Some(1) {
            return Err("external activity requires manual classification".into());
        }
        self.conn.execute("UPDATE external_control SET health='ready',reason='REST orders/trades/assets reconciled' WHERE blocked=0",[])?;
        Ok(())
    }
    pub fn external_audit(&self) -> Result<serde_json::Value, PaperError> {
        let Some(id) = self.execution_identity()? else {
            return Ok(serde_json::Value::Null);
        };
        let (health, reason, blocked): (String, String, i64) = self.conn.query_row(
            "SELECT health,reason,blocked FROM external_control",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        let materialized = units(&self.conn, "asset_movements")?;
        let mut replay = BTreeMap::new();
        let mut stmt = self
            .conn
            .prepare("SELECT body FROM trades ORDER BY symbol,id")?;
        for body in stmt.query_map([], |r| r.get::<_, String>(0))? {
            let t: TradeObservation = serde_json::from_str(&body?)?;
            for (a, n) in movement(&self.conn, &t)? {
                add(&mut replay, &a, n)?;
            }
        }
        if replay != materialized {
            return Err("materialized asset movements conflict with trade replay".into());
        }
        Ok(
            serde_json::json!({"identity":id,"health":health,"reason":reason,"external_activity_blocked":blocked!=0,
            "expected_total_assets":self.expected_balances()?,"movement_replay_equal":true,"history_recovery":self.history_audit()?,"private_stream":self.stream_audit()?,"private_execution_events":self.conn.query_row("SELECT count(*) FROM private_events",[],|r|r.get::<_,i64>(0))?,
            "scope":"original asset totals, fixed baseline; no FX valuation, deposit adoption, strategy PnL or proof of absence of external activity"}),
        )
    }
}
