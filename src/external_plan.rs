//! 有界外部执行计划。计划只计算授权量，成交和原币费用以交易所证据为准。
//! 父子链接和 unknown 意图同事务；崩溃后从不把“可能没发出”当成重发许可。
use crate::{
    decimal::{self, SCALE},
    execution::*,
    paper::PaperError,
    types::Side,
};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
const DAY: u64 = 86_400_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanConfig {
    pub version: u32,
    pub plan_id: String,
    pub symbol: String,
    pub side: Side,
    pub limit_price: String,
    pub total_quantity: String,
    pub quantity_step: String,
    pub min_child_quantity: String,
    pub max_child_quantity: String,
    pub slices: u16,
    pub interval_ms: u64,
    pub max_working_ms: u64,
    pub max_reconciliation_ms: u64,
    pub max_children: u16,
}
impl PlanConfig {
    pub fn validate(&self) -> Result<(), PaperError> {
        if self.version != 1
            || !self.plan_id.starts_with("kaze-")
            || !(6..=27).contains(&self.plan_id.len())
            || !self
                .plan_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
            || !(1..=1024).contains(&self.slices)
            || !(1..=1024).contains(&self.max_children)
            || self.slices > self.max_children
            || !(1..=DAY).contains(&self.interval_ms)
            || self
                .interval_ms
                .checked_mul(u64::from(self.slices - 1))
                .is_none_or(|n| n > DAY)
            || !(1..=DAY).contains(&self.max_working_ms)
            || !(1..=60000).contains(&self.max_reconciliation_ms)
        {
            return Err("invalid external plan version/identity/time/capacity".into());
        }
        let total = decimal::parse(&self.total_quantity)?;
        let step = decimal::parse(&self.quantity_step)?;
        let min = decimal::parse(&self.min_child_quantity)?;
        let max = decimal::parse(&self.max_child_quantity)?;
        if step == 0
            || min == 0
            || max < min
            || max > total
            || total % step != 0
            || min % step != 0
            || max % step != 0
            || total < min * i128::from(self.slices)
        {
            return Err("invalid external quantity grid or slices below minimum child".into());
        }
        self.intent(1, total)?.validate(100 * SCALE)
    }
    fn intent(&self, child: u16, qty: i128) -> Result<OrderIntent, PaperError> {
        Ok(OrderIntent {
            client_order_id: format!("{}-{child:04}", self.plan_id),
            symbol: self.symbol.clone(),
            side: self.side,
            price: self.limit_price.clone(),
            quantity: decimal::format(qty)?,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    config: PlanConfig,
    start_ms: Option<u64>,
    last_ms: Option<u64>,
    ticks: u64,
    paused: bool,
    reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Child {
    number: u16,
    id: String,
    submitted_ms: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    NeedsReconciliation,
    Ready,
    Working,
    SubmitUnknown,
    CancelPending,
    Paused,
    Completed,
}
#[derive(Clone, Debug, Serialize)]
pub struct PlanStatus {
    pub config: PlanConfig,
    pub phase: Phase,
    pub start_ms: Option<u64>,
    pub last_ms: Option<u64>,
    pub ticks: u64,
    pub paused: bool,
    pub reason: String,
    pub children: usize,
    pub current_child: Option<String>,
    pub authorized_quantity: String,
    pub executed_gross_quantity: String,
    pub pending_quantity: String,
    pub scope: &'static str,
}
#[derive(Clone, Debug, Serialize)]
pub struct TickResult {
    pub operation: String,
    pub post_calls: u64,
    pub cancel_attempted: bool,
    pub status: PlanStatus,
}

pub(crate) fn create_schema(c: &Connection) -> Result<(), PaperError> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS execution_plan(singleton INTEGER PRIMARY KEY CHECK(singleton=1),state TEXT NOT NULL,checksum TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS plan_children(number INTEGER PRIMARY KEY,id TEXT NOT NULL UNIQUE,submitted_ms TEXT NOT NULL);")?;
    Ok(())
}
pub(crate) fn has_plan(c: &Connection) -> Result<bool, PaperError> {
    Ok(c.query_row("SELECT count(*) FROM execution_plan", [], |r| {
        r.get::<_, i64>(0)
    })? == 1)
}
fn children(c: &Connection) -> Result<Vec<Child>, PaperError> {
    let mut s = c.prepare("SELECT number,id,submitted_ms FROM plan_children ORDER BY number")?;
    s.query_map([], |r| {
        Ok((
            r.get::<_, u16>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?
    .map(|r| {
        let (n, id, t) = r?;
        Ok(Child {
            number: n,
            id,
            submitted_ms: t.parse().map_err(|_| "invalid child clock")?,
        })
    })
    .collect()
}
fn checksum(state: &str, children: &[Child]) -> Result<String, PaperError> {
    let mut h = Sha256::new();
    h.update(b"kaze-external-plan-v1");
    h.update((state.len() as u64).to_le_bytes());
    h.update(state);
    h.update(serde_json::to_vec(children)?);
    Ok(crate::journal::hex(&h.finalize()))
}
fn save(c: &Connection, s: &State) -> Result<(), PaperError> {
    let body = serde_json::to_string(s)?;
    c.execute("INSERT INTO execution_plan VALUES(1,?1,?2) ON CONFLICT(singleton) DO UPDATE SET state=excluded.state,checksum=excluded.checksum",params![body,checksum(&body,&children(c)?)?])?;
    Ok(())
}
fn load(c: &Connection) -> Result<(State, Vec<Child>), PaperError> {
    let (body, hash): (String, String) = c.query_row(
        "SELECT state,checksum FROM execution_plan WHERE singleton=1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let kids = children(c)?;
    if hash != checksum(&body, &kids)? {
        return Err("external plan checksum conflict".into());
    }
    let s: State = serde_json::from_str(&body)?;
    s.config.validate()?;
    if s.ticks > 10000
        || s.reason.len() > 256
        || s.start_ms.zip(s.last_ms).is_some_and(|(a, b)| a > b)
        || s.start_ms.is_some() != s.last_ms.is_some()
        || (s.ticks == 0) != s.last_ms.is_none()
        || kids.len() > usize::from(s.config.max_children)
    {
        return Err("invalid external plan checkpoint".into());
    }
    for (i, k) in kids.iter().enumerate() {
        if usize::from(k.number) != i + 1
            || k.id != format!("{}-{:04}", s.config.plan_id, k.number)
            || s.start_ms.is_none_or(|t| k.submitted_ms < t)
            || s.last_ms.is_none_or(|t| k.submitted_ms > t)
        {
            return Err("invalid external child identity/clock".into());
        }
    }
    Ok((s, kids))
}
fn progress(
    j: &ExecutionJournal,
    s: &State,
    kids: &[Child],
) -> Result<(i128, i128, Option<TrackedOrder>), PaperError> {
    let orders = j.orders()?;
    if orders.len() != kids.len() {
        return Err("managed plan has foreign or orphaned intents".into());
    }
    // 一次建立借用索引，避免每个子单交叉扫描整个订单历史；没有复制请求字符串。
    let by_id: std::collections::BTreeMap<_, _> = orders
        .iter()
        .map(|o| (o.intent.client_order_id.as_str(), o))
        .collect();
    let mut gross = 0i128;
    let mut pending = 0i128;
    let mut current = None;
    let step = decimal::parse(&s.config.quantity_step)?;
    for k in kids {
        let o = by_id
            .get(k.id.as_str())
            .copied()
            .ok_or("plan child missing intent")?;
        let qty = decimal::parse(&o.intent.quantity)?;
        if !matches!(
            o.phase.as_str(),
            "unknown" | "open" | "cancel_unknown" | "terminal"
        ) || (o.phase == "unknown") != o.observation.is_none()
            || o.observation
                .as_ref()
                .is_some_and(|v| v.status.terminal() != (o.phase == "terminal"))
        {
            return Err("plan child phase/evidence conflict".into());
        }
        if o.intent != s.config.intent(k.number, qty)?
            || qty < decimal::parse(&s.config.min_child_quantity)?
            || qty > decimal::parse(&s.config.max_child_quantity)?
            || qty % step != 0
        {
            return Err("plan child conflicts with frozen configuration".into());
        }
        let filled = o
            .observation
            .as_ref()
            .map(|o| decimal::parse(&o.executed_qty))
            .transpose()?
            .unwrap_or(0);
        gross = gross.checked_add(filled).ok_or("plan gross overflow")?;
        if o.phase != "terminal" {
            if current.is_some() || k != kids.last().ok_or("no child")? {
                return Err("multiple/non-last active plan children".into());
            }
            pending = qty - filled;
            current = Some(TrackedOrder {
                intent: o.intent.clone(),
                phase: o.phase.clone(),
                observation: o.observation.clone(),
            });
        }
    }
    if gross + pending > decimal::parse(&s.config.total_quantity)? {
        return Err("plan exceeds total quantity".into());
    }
    Ok((gross, pending, current))
}
fn authorized(s: &State, now: u64) -> Result<i128, PaperError> {
    let Some(start) = s.start_ms else {
        return Ok(0);
    };
    let due = (now.saturating_sub(start) / s.config.interval_ms)
        .saturating_add(1)
        .min(u64::from(s.config.slices));
    let step = decimal::parse(&s.config.quantity_step)?;
    Ok(
        (decimal::parse(&s.config.total_quantity)? / step) * i128::from(due)
            / i128::from(s.config.slices)
            * step,
    )
}
impl ExecutionJournal {
    pub fn init_plan(&mut self, config: PlanConfig) -> Result<(), PaperError> {
        config.validate()?;
        if has_plan(&self.conn)? {
            let (s, k) = load(&self.conn)?;
            progress(self, &s, &k)?;
            return if s.config == config {
                Ok(())
            } else {
                Err("plan configuration is immutable".into())
            };
        }
        if self.execution_identity()?.is_none()
            || !self.has_history_recovery()?
            || !self.orders()?.is_empty()
        {
            return Err("plan requires fresh bound history-enabled account journal".into());
        }
        let bound: i64 = self.conn.query_row(
            "SELECT count(*) FROM instruments WHERE symbol=?1",
            [&config.symbol],
            |r| r.get(0),
        )?;
        if bound != 1 {
            return Err("plan instrument is not bound".into());
        }
        let tx = self.conn.transaction()?;
        // 旧二进制只认识v1，必须拒绝打开受计划管理的账本，不能绕开父子身份。
        tx.execute(
            "UPDATE execution_meta SET revision='binance-testnet-execution-v2-plan'",
            [],
        )?;
        save(
            &tx,
            &State {
                config,
                start_ms: None,
                last_ms: None,
                ticks: 0,
                paused: false,
                reason: "initialized; awaiting fresh reconciliation".into(),
            },
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn plan_status(&self) -> Result<PlanStatus, PaperError> {
        let (s, kids) = load(&self.conn)?;
        let (gross, pending, current) = progress(self, &s, &kids)?;
        let (health, blocked): (String, i64) =
            self.conn
                .query_row("SELECT health,blocked FROM external_control", [], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })?;
        let phase = if let Some(o) = &current {
            match o.phase.as_str() {
                "unknown" => Phase::SubmitUnknown,
                "cancel_unknown" => Phase::CancelPending,
                _ => {
                    if health == "ready" && blocked == 0 {
                        Phase::Working
                    } else {
                        Phase::NeedsReconciliation
                    }
                }
            }
        } else if s.paused {
            Phase::Paused
        } else if health != "ready" || blocked != 0 {
            Phase::NeedsReconciliation
        } else if gross == decimal::parse(&s.config.total_quantity)? {
            Phase::Completed
        } else {
            Phase::Ready
        };
        Ok(PlanStatus {
            config: s.config.clone(),
            phase,
            start_ms: s.start_ms,
            last_ms: s.last_ms,
            ticks: s.ticks,
            paused: s.paused,
            reason: s.reason.clone(),
            children: kids.len(),
            current_child: current.map(|o| o.intent.client_order_id),
            authorized_quantity: decimal::format(authorized(&s, s.last_ms.unwrap_or(0))?)?,
            executed_gross_quantity: decimal::format(gross)?,
            pending_quantity: decimal::format(pending)?,
            scope: "bounded fixed-limit gross-quantity testnet execution; original-asset ledger authoritative, no net target/alpha/mainnet or continuous signal routing",
        })
    }
    pub fn pause_plan(&mut self, reason: &str) -> Result<(), PaperError> {
        if reason.is_empty() || reason.len() > 256 {
            return Err("invalid pause reason".into());
        }
        let (mut s, k) = load(&self.conn)?;
        progress(self, &s, &k)?;
        s.paused = true;
        s.reason = reason.into();
        let tx = self.conn.transaction()?;
        save(&tx, &s)?;
        tx.commit()?;
        Ok(())
    }
    /// 显式恢复只解除人工暂停，仍必须核对；不会重发任何已持久化的子单身份。
    pub fn resume_plan(&mut self, v: &mut impl ExecutionVenue) -> Result<(), PaperError> {
        self.reconcile(v)?;
        let (mut s, k) = load(&self.conn)?;
        let (_, _, current) = progress(self, &s, &k)?;
        if current
            .as_ref()
            .is_some_and(|o| o.phase == "unknown" || o.phase == "cancel_unknown")
        {
            return Err("unresolved child blocks resume".into());
        }
        s.paused = false;
        s.reason = "explicit resume; clock and original children retained".into();
        let tx = self.conn.transaction()?;
        save(&tx, &s)?;
        tx.commit()?;
        Ok(())
    }
    pub fn tick_plan(
        &mut self,
        v: &mut impl ExecutionVenue,
        now_ms: u64,
    ) -> Result<TickResult, PaperError> {
        let (mut s, kids) = load(&self.conn)?;
        progress(self, &s, &kids)?;
        if now_ms == 0 || s.last_ms.is_some_and(|t| now_ms < t) {
            self.pause_plan("clock regression")?;
            return Err("plan clock regression; paused".into());
        }
        if s.ticks >= 10000 {
            self.pause_plan("tick capacity")?;
            return Err("plan tick capacity reached".into());
        }
        let start = std::time::Instant::now();
        // 无缓存 Ready：每次操作前核对真实成交与所有账户资产。失败不授权下单。
        self.reconcile(v)?;
        if start.elapsed().as_millis() > u128::from(s.config.max_reconciliation_ms) {
            self.mark_execution_gap("plan reconciliation took too long", false)?;
            return Err("stale reconciliation; no plan operation".into());
        }
        let (gross, _, current) = progress(self, &s, &kids)?;
        s.start_ms.get_or_insert(now_ms);
        s.last_ms = Some(now_ms);
        s.ticks += 1;
        let mut op = "wait";
        let mut cancel = false;
        if let Some(o) = current {
            let child = kids.last().ok_or("active child missing")?;
            if o.phase == "unknown" || o.phase == "cancel_unknown" {
                op = "query_only";
            } else if s.paused
                || now_ms.saturating_sub(child.submitted_ms) >= s.config.max_working_ms
            {
                let tx = self.conn.transaction()?;
                s.reason = "cancel requested; no replacement until terminal evidence".into();
                save(&tx, &s)?;
                tx.commit()?;
                // cancel_once 持久化 cancel_unknown 后才调用 DELETE；失败不允许第二次 DELETE。
                cancel = true;
                let result = self.cancel_once(v, &o.intent.client_order_id);
                if result.is_err() {
                    return Ok(TickResult {
                        operation: "cancel_unresolved".into(),
                        post_calls: 0,
                        cancel_attempted: cancel,
                        status: self.plan_status()?,
                    });
                }
                self.reconcile(v)?;
                return Ok(TickResult {
                    operation: "cancel_confirmed".into(),
                    post_calls: 0,
                    cancel_attempted: cancel,
                    status: self.plan_status()?,
                });
            } else {
                op = "working";
            }
        } else if s.paused {
            op = "paused";
        } else if gross == decimal::parse(&s.config.total_quantity)? {
            op = "completed";
        } else {
            let step = decimal::parse(&s.config.quantity_step)?;
            let due = (authorized(&s, now_ms)? - gross).max(0);
            let qty = due.min(decimal::parse(&s.config.max_child_quantity)?) / step * step;
            if kids.len() >= usize::from(s.config.max_children) {
                s.paused = true;
                op = "child_capacity";
            } else if qty >= decimal::parse(&s.config.min_child_quantity)? {
                let number = kids.len() as u16 + 1;
                let intent = s.config.intent(number, qty)?;
                if let Err(e) = v.validate_intent(&intent) {
                    self.pause_plan(&format!("child preflight: {e}"))?;
                    return Err(PaperError(e.to_string()));
                }
                if start.elapsed().as_millis() > u128::from(s.config.max_reconciliation_ms) {
                    self.mark_execution_gap("plan preflight took too long", false)?;
                    return Err("stale reconciliation after preflight; no child prepared".into());
                }
                let tx = self.conn.transaction()?;
                if !prepare_tx(&tx, &intent, 100 * SCALE)? {
                    return Err("child identity already exists; no send permit".into());
                }
                tx.execute(
                    "INSERT INTO plan_children VALUES(?1,?2,?3)",
                    params![number, intent.client_order_id, now_ms.to_string()],
                )?;
                s.reason = "child intent committed before one POST".into();
                save(&tx, &s)?;
                tx.commit()?;
                if start.elapsed().as_millis() > u128::from(s.config.max_reconciliation_ms) {
                    self.mark_execution_gap(
                        "plan commit took too long; child outcome unknown",
                        false,
                    )?;
                    return Err("expired send permission after durable intent; query only".into());
                }
                // 该栈帧的首次提交权限不能序列化；恢复只能查原 ID。
                match v.submit(&intent) {
                    Ok(o) => self.observe(&o)?,
                    Err(e) => {
                        return Ok(TickResult {
                            operation: format!("submit_unresolved: {e}"),
                            post_calls: 1,
                            cancel_attempted: false,
                            status: self.plan_status()?,
                        });
                    }
                }
                return Ok(TickResult {
                    operation: "submitted".into(),
                    post_calls: 1,
                    cancel_attempted: false,
                    status: self.plan_status()?,
                });
            } else if authorized(&s, now_ms)? == decimal::parse(&s.config.total_quantity)?
                && gross < decimal::parse(&s.config.total_quantity)?
            {
                s.paused = true;
                op = "unexecutable_dust";
            }
        }
        s.reason = op.into();
        let tx = self.conn.transaction()?;
        save(&tx, &s)?;
        tx.commit()?;
        Ok(TickResult {
            operation: op.into(),
            post_calls: 0,
            cancel_attempted: cancel,
            status: self.plan_status()?,
        })
    }
}
