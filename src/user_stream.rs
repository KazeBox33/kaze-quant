//! Binance 私有事件协议与原子入账。网络订阅与重连由适配层负责。
use crate::decimal;
use crate::execution::{
    ExecutionJournal, OrderObservation, TradeObservation, VenueStatus, observe_tx, record_trades_tx,
};
use crate::paper::PaperError;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionEvent {
    pub execution_id: u64,
    pub order: OrderObservation,
    pub trade: Option<TradeObservation>,
}
// 执行回报是主要变体；保留值所有权，避免为每笔回报额外 Box 分配。
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug)]
pub enum UserEvent {
    Execution(ExecutionEvent),
    /// 部分账户更新不是完整快照，不能用它覆盖全资产账本。
    AccountChanged,
    ExternalBalanceChanged,
    Terminated,
}
fn string(v: &Value, k: &str) -> Result<String, PaperError> {
    Ok(v[k].as_str().ok_or("private event string absent")?.into())
}
fn number(v: &Value, k: &str) -> Result<u64, PaperError> {
    v[k].as_u64()
        .ok_or_else(|| "private event integer absent".into())
}
pub fn parse_event(bytes: &[u8], subscription: u64) -> Result<UserEvent, PaperError> {
    if bytes.len() > 65_536 {
        return Err("private frame exceeds 64 KiB".into());
    }
    let envelope: Value =
        serde_json::from_slice(bytes).map_err(|_| "invalid private event JSON")?;
    if envelope["event"].is_null()
        && envelope["event"]["e"].is_null()
        && envelope["e"] == "serverShutdown"
    {
        return Ok(UserEvent::Terminated);
    }
    if envelope["subscriptionId"].as_u64() != Some(subscription) {
        return Err("private subscription identity conflict".into());
    }
    let v = &envelope["event"];
    match v["e"].as_str().ok_or("private event type absent")? {
        "outboundAccountPosition" => Ok(UserEvent::AccountChanged),
        "balanceUpdate" => Ok(UserEvent::ExternalBalanceChanged),
        "eventStreamTerminated" => Ok(UserEvent::Terminated),
        "executionReport" => {
            if v["o"] != "LIMIT" || v["f"] != "GTC" {
                return Err("unsupported private order type".into());
            }
            let status: VenueStatus = serde_json::from_value(v["X"].clone())
                .map_err(|_| "unsupported private order status")?;
            let client = if v["x"] == "CANCELED" && v["C"].as_str().is_some_and(|s| !s.is_empty()) {
                string(v, "C")?
            } else {
                string(v, "c")?
            };
            let order = OrderObservation {
                symbol: string(v, "s")?,
                client_order_id: client.clone(),
                order_id: number(v, "i")?,
                reported_client_order_id: if client != string(v, "c")? {
                    Some(string(v, "c")?)
                } else {
                    None
                },
                price: string(v, "p")?,
                orig_qty: string(v, "q")?,
                executed_qty: string(v, "z")?,
                cummulative_quote_qty: string(v, "Z")?,
                status,
                side: string(v, "S")?,
                update_time: number(v, "T")?,
            };
            let trade = if v["x"] == "TRADE" {
                let side = order.side.as_str();
                if side != "BUY" && side != "SELL" {
                    return Err("private trade side invalid".into());
                }
                Some(TradeObservation {
                    symbol: order.symbol.clone(),
                    id: number(v, "t")?,
                    order_id: order.order_id,
                    price: string(v, "L")?,
                    qty: string(v, "l")?,
                    quote_qty: string(v, "Y")?,
                    commission: string(v, "n")?,
                    commission_asset: v["N"].as_str().unwrap_or("").into(),
                    is_buyer: side == "BUY",
                })
            } else {
                if decimal::parse(&string(v, "l")?)? != 0 {
                    return Err("non-trade event has fill quantity".into());
                }
                None
            };
            Ok(UserEvent::Execution(ExecutionEvent {
                execution_id: number(v, "I")?,
                order,
                trade,
            }))
        }
        _ => Err("unsupported private event; reconciliation required".into()),
    }
}
impl ExecutionJournal {
    pub fn ingest_user_frame(
        &mut self,
        bytes: &[u8],
        subscription: u64,
    ) -> Result<bool, PaperError> {
        match parse_event(bytes, subscription) {
            Ok(e) => self.ingest_user_event(&e),
            Err(e) => {
                self.mark_execution_gap("invalid private protocol evidence", true)?;
                Err(e)
            }
        }
    }
    pub fn ingest_user_event(&mut self, event: &UserEvent) -> Result<bool, PaperError> {
        if self.execution_identity()?.is_none() {
            return Err("private events require bound account".into());
        }
        let result = match event {
            UserEvent::Execution(e) => self.ingest_execution(e),
            UserEvent::AccountChanged => self
                .mark_execution_gap("partial account update", false)
                .map(|_| false),
            UserEvent::ExternalBalanceChanged => self
                .mark_execution_gap("external balance activity", true)
                .map(|_| false),
            UserEvent::Terminated => self
                .mark_execution_gap("private stream terminated", false)
                .map(|_| false),
        };
        if result.is_err() {
            self.mark_execution_gap("invalid private execution evidence", true)?;
        }
        result
    }
    fn ingest_execution(&mut self, e: &ExecutionEvent) -> Result<bool, PaperError> {
        let mut e = e.clone();
        for v in [
            &mut e.order.price,
            &mut e.order.orig_qty,
            &mut e.order.executed_qty,
            &mut e.order.cummulative_quote_qty,
        ] {
            *v = decimal::format(decimal::parse(v)?)?;
        }
        e.trade = e
            .trade
            .as_ref()
            .map(crate::execution::canonical_trade)
            .transpose()?;
        if let Some(t) = &e.trade
            && (t.symbol != e.order.symbol || t.order_id != e.order.order_id)
        {
            return Err("private trade/order identity conflict".into());
        }
        let body = serde_json::to_string(&e)?;
        let tx = self.conn.transaction()?;
        let old:Option<String>=tx.query_row("SELECT body FROM private_events WHERE symbol=?1 AND order_id=?2 AND execution_id=?3",
            params![e.order.symbol,e.order.order_id.to_string(),e.execution_id.to_string()],|r|r.get(0)).optional()?;
        if let Some(old) = old {
            if old != body {
                return Err("conflicting private execution ID".into());
            }
            return Ok(false);
        }
        let count: i64 = tx.query_row("SELECT count(*) FROM private_events", [], |r| r.get(0))?;
        if count >= 100_000 {
            return Err("private event history capacity exceeded".into());
        }
        let old: Option<String> = tx
            .query_row(
                "SELECT observation FROM intents WHERE id=?1",
                [&e.order.client_order_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let mut stale = false;
        if let Some(old) = old {
            let old: OrderObservation = serde_json::from_str(&old)?;
            if old.status.terminal()
                && e.order.status.terminal()
                && (old.status != e.order.status
                    || decimal::parse(&old.executed_qty)? != decimal::parse(&e.order.executed_qty)?
                    || decimal::parse(&old.cummulative_quote_qty)?
                        != decimal::parse(&e.order.cummulative_quote_qty)?)
            {
                return Err("conflicting terminal private evidence".into());
            }
            stale = e.order.update_time <= old.update_time
                && decimal::parse(&e.order.executed_qty)? <= decimal::parse(&old.executed_qty)?
                && decimal::parse(&e.order.cummulative_quote_qty)?
                    <= decimal::parse(&old.cummulative_quote_qty)?
                && (!e.order.status.terminal() || e.order.status == old.status);
            // 迟到事件仍须验证原始意图和稳定 orderId，不能借 stale 绕过身份校验。
            if stale {
                let original: String = tx.query_row(
                    "SELECT intent FROM intents WHERE id=?1",
                    [&e.order.client_order_id],
                    |r| r.get(0),
                )?;
                let i: crate::execution::OrderIntent = serde_json::from_str(&original)?;
                if old.order_id != e.order.order_id
                    || i.symbol != e.order.symbol
                    || old.side != e.order.side
                    || decimal::parse(&i.price)? != decimal::parse(&e.order.price)?
                    || decimal::parse(&i.quantity)? != decimal::parse(&e.order.orig_qty)?
                {
                    return Err("stale private event identity conflict".into());
                }
                // 用原校验器验证迟到累计经济值；临时清除旧投影，在同一事务内立即恢复。
                tx.execute(
                    "UPDATE intents SET observation=NULL WHERE id=?1",
                    [&e.order.client_order_id],
                )?;
                observe_tx(&tx, &e.order)?;
                tx.execute(
                    "UPDATE intents SET observation=?2,phase=?3 WHERE id=?1",
                    params![
                        e.order.client_order_id,
                        serde_json::to_string(&old)?,
                        if old.status.terminal() {
                            "terminal"
                        } else {
                            "open"
                        }
                    ],
                )?;
            }
        }
        if !stale {
            observe_tx(&tx, &e.order)?;
        }
        if let Some(t) = e.trade {
            record_trades_tx(&tx, &[t])?;
        }
        tx.execute(
            "INSERT INTO private_events VALUES(?1,?2,?3,?4)",
            params![
                e.order.symbol,
                e.order.order_id.to_string(),
                e.execution_id.to_string(),
                body
            ],
        )?;
        tx.execute("UPDATE external_control SET health='needs_reconciliation',reason='private execution evidence' WHERE blocked=0",[])?;
        tx.commit()?;
        Ok(true)
    }
}
