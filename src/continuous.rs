//! 只读测试网观察节点：网络线程拥有连接，主线程独占 SQLite 写入。
//! 连接代次不是交易所全局序号；每次代次切换都必须 REST 补齐。
use crate::{execution::*, paper::PaperError, recovery::RecoveryOptions, user_stream::UserEvent};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

pub(crate) fn create_schema(c: &rusqlite::Connection) -> Result<(), PaperError> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS stream_epochs(id INTEGER PRIMARY KEY,status TEXT NOT NULL,reason TEXT NOT NULL);
    UPDATE stream_epochs SET status='gap',reason='process reopened; old connection cannot be resumed' WHERE status IN ('connecting','recovering','ready');")?;
    Ok(())
}
impl ExecutionJournal {
    pub fn begin_stream_epoch(&mut self) -> Result<i64, PaperError> {
        if !self.has_history_recovery()? {
            return Err("continuous stream requires seeded history baseline".into());
        }
        self.history_cursors()?;
        let blocked: i64 =
            self.conn
                .query_row("SELECT blocked FROM external_control", [], |r| r.get(0))?;
        if blocked != 0 {
            return Err("external activity requires manual classification".into());
        }
        let tx = self.conn.transaction()?;
        let active:i64=tx.query_row("SELECT count(*) FROM stream_epochs WHERE status IN ('connecting','recovering','ready')",[],|r|r.get(0))?;
        let count: i64 = tx.query_row("SELECT count(*) FROM stream_epochs", [], |r| r.get(0))?;
        if active != 0 || count >= 10000 {
            return Err("stream epoch already active or capacity exhausted".into());
        }
        tx.execute("INSERT INTO stream_epochs(status,reason) VALUES('connecting','authenticated subscription required')",[])?;
        let id = tx.last_insert_rowid();
        tx.execute("UPDATE external_control SET health='needs_reconciliation',reason='private stream connecting'",[])?;
        tx.commit()?;
        Ok(id)
    }
    fn epoch_state(&self, id: i64) -> Result<String, PaperError> {
        let row: Option<(i64, String)> = self
            .conn
            .query_row(
                "SELECT id,status FROM stream_epochs ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (current, state) = row.ok_or("private stream epoch absent")?;
        if current != id {
            return Err("stale private stream epoch".into());
        }
        Ok(state)
    }
    pub fn stream_subscribed(&mut self, id: i64) -> Result<(), PaperError> {
        if self.epoch_state(id)? != "connecting" {
            return Err("invalid stream subscription transition".into());
        }
        self.conn.execute("UPDATE stream_epochs SET status='recovering',reason='subscription authenticated; REST recovery required' WHERE id=?1",[id])?;
        Ok(())
    }
    pub fn stream_recovered(&mut self, id: i64) -> Result<(), PaperError> {
        let state = self.epoch_state(id)?;
        if state != "recovering" && state != "ready" {
            return Err("inactive stream cannot recover".into());
        }
        self.require_reconciled()?;
        self.conn.execute("UPDATE stream_epochs SET status='ready',reason='subscription and REST recovery completed' WHERE id=?1",[id])?;
        Ok(())
    }
    pub fn ingest_stream_epoch(&mut self, id: i64, e: &UserEvent) -> Result<bool, PaperError> {
        if self.epoch_state(id)? != "ready" {
            return Err("stream must recover before ingestion".into());
        }
        self.ingest_user_event(e)
    }
    pub fn end_stream_epoch(
        &mut self,
        id: i64,
        reason: &str,
        sticky: bool,
        stopped: bool,
    ) -> Result<(), PaperError> {
        let state = self.epoch_state(id)?;
        if state == "gap" || state == "stopped" {
            return Err("stream epoch already ended".into());
        }
        if reason.len() > 128 || !reason.is_ascii() {
            return Err("invalid stream reason".into());
        }
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE stream_epochs SET status=?2,reason=?3 WHERE id=?1",
            params![id, if stopped { "stopped" } else { "gap" }, reason],
        )?;
        tx.execute("UPDATE external_control SET health='needs_reconciliation',reason=?1,blocked=max(blocked,?2)",params![reason,i64::from(sticky)])?;
        tx.commit()?;
        Ok(())
    }
    pub fn stream_audit(&self) -> Result<serde_json::Value, PaperError> {
        let mut s = self
            .conn
            .prepare("SELECT id,status,reason FROM stream_epochs ORDER BY id")?;
        let rows=s.query_map([],|r|Ok(serde_json::json!({"id":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"reason":r.get::<_,String>(2)?})))?;
        Ok(
            serde_json::json!({"epochs":rows.collect::<Result<Vec<_>,_>>()?,"scope":"local connection generations; no exchange-wide event sequence"}),
        )
    }
}
/// receive 必须有有界读取超时，允许 stop 后 join；源只由网络线程拥有。
pub trait PrivateSource: Send + 'static {
    fn receive(&mut self) -> Result<Option<UserEvent>, VenueError>;
}
#[derive(Clone, Copy, Debug)]
struct Fault {
    reason: &'static str,
    sticky: bool,
}
struct Frame {
    at: Instant,
    event: UserEvent,
}
struct Worker {
    rx: Receiver<Frame>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<Option<Fault>>>,
}
impl Worker {
    fn start(mut source: impl PrivateSource, capacity: usize) -> Self {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let join = std::thread::spawn(move || {
            while !flag.load(Ordering::Acquire) {
                match source.receive() {
                    Ok(Some(UserEvent::Terminated)) => {
                        return Some(Fault {
                            reason: "private stream terminated",
                            sticky: false,
                        });
                    }
                    Ok(Some(event)) => {
                        // 排队有界、满则断代，绝不默默丢帧后宣称持续完整。
                        if let UserEvent::Execution(e) = &event
                            && serde_json::to_vec(e).map_or(true, |v| v.len() > 65536)
                        {
                            return Some(Fault {
                                reason: "private decoded event capacity exceeded",
                                sticky: true,
                            });
                        }
                        match tx.try_send(Frame {
                            at: Instant::now(),
                            event,
                        }) {
                            Ok(()) => {}
                            Err(mpsc::TrySendError::Full(_)) => {
                                return Some(Fault {
                                    reason: "private queue overflow",
                                    sticky: false,
                                });
                            }
                            Err(mpsc::TrySendError::Disconnected(_)) => {
                                return Some(Fault {
                                    reason: "private consumer unexpectedly closed",
                                    sticky: true,
                                });
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(VenueError::Protocol(_)) => {
                        return Some(Fault {
                            reason: "private event protocol failed",
                            sticky: true,
                        });
                    }
                    Err(_) => {
                        return Some(Fault {
                            reason: "private transport failed",
                            sticky: false,
                        });
                    }
                }
            }
            None
        });
        Self {
            rx,
            stop,
            join: Some(join),
        }
    }
    fn finish(&mut self) -> Option<Fault> {
        self.stop.store(true, Ordering::Release);
        self.join.take().and_then(|j| match j.join() {
            Ok(f) => f,
            Err(_) => Some(Fault {
                reason: "private producer panicked",
                sticky: true,
            }),
        })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.finish();
    }
}
#[derive(Clone, Copy)]
pub struct MonitorOptions {
    pub duration: Duration,
    pub recovery_interval: Duration,
    pub max_reconnects: u8,
}
impl MonitorOptions {
    fn validate(self) -> Result<(), PaperError> {
        if !(5..=86400).contains(&self.duration.as_secs())
            || !(5..=300).contains(&self.recovery_interval.as_secs())
            || self.max_reconnects > 8
        {
            return Err("invalid monitor duration/recovery/reconnect limits".into());
        }
        Ok(())
    }
}
#[derive(Default, Serialize)]
pub struct MonitorReport {
    pub epochs: u64,
    pub reconnects: u64,
    pub events: u64,
    pub duplicates: u64,
    pub recovery_rounds: u64,
    pub recovered_new_trades: u64,
    pub rest_source_operations: u64,
    pub gaps: Vec<String>,
    pub success: bool,
    pub duration_seconds: f64,
}
/// 没有下单/撤单入口；重连预算耗尽或协议/身份冲突立即失败。
pub fn monitor<S: PrivateSource>(
    j: &mut ExecutionJournal,
    v: &mut impl ExecutionVenue,
    mut connect: impl FnMut() -> Result<S, VenueError>,
    options: MonitorOptions,
    report: &mut MonitorReport,
) -> Result<(), PaperError> {
    options.validate()?;
    let start = Instant::now();
    let deadline = start + options.duration;
    let result = (|| -> Result<(), PaperError> {
        loop {
            let id = j.begin_stream_epoch()?;
            report.epochs += 1;
            let mut worker = match connect() {
                Ok(s) => Worker::start(s, 256),
                Err(e) => {
                    let sticky = matches!(e, VenueError::Protocol(_) | VenueError::Rejected(_));
                    j.end_stream_epoch(id, "private subscription failed", sticky, false)?;
                    report.gaps.push("private subscription failed".into());
                    retry(options, report, deadline, sticky)?;
                    continue;
                }
            };
            j.stream_subscribed(id)?;
            let mut fault = None;
            let active = (|| -> Result<(), PaperError> {
                let stats = j.recover_history(v, RecoveryOptions::default())?;
                report.recovery_rounds += 1;
                report.recovered_new_trades += stats.new_trades;
                report.rest_source_operations += stats.rest_operations();
                j.stream_recovered(id)?;
                let mut next = Instant::now() + options.recovery_interval;
                while Instant::now() < deadline {
                    if worker.join.as_ref().is_some_and(|h| h.is_finished()) {
                        fault = worker.finish();
                        break;
                    }
                    match worker.rx.try_recv() {
                        Ok(f) => {
                            if f.at.elapsed() > Duration::from_secs(5) {
                                fault = Some(Fault {
                                    reason: "private queued event stale",
                                    sticky: false,
                                });
                                break;
                            }
                            let execution = matches!(f.event, UserEvent::Execution(_));
                            let added = j.ingest_stream_epoch(id, &f.event)?;
                            report.events += 1;
                            if execution && !added {
                                report.duplicates += 1;
                            }
                        }
                        Err(TryRecvError::Disconnected) => {
                            fault = worker.finish().or(Some(Fault {
                                reason: "private producer ended unexpectedly",
                                sticky: true,
                            }));
                            break;
                        }
                        Err(TryRecvError::Empty) => std::thread::sleep(Duration::from_millis(10)),
                    }
                    if Instant::now() >= next {
                        let stats = j.recover_history(v, RecoveryOptions::default())?;
                        report.recovery_rounds += 1;
                        report.recovered_new_trades += stats.new_trades;
                        report.rest_source_operations += stats.rest_operations();
                        j.stream_recovered(id)?;
                        next = Instant::now() + options.recovery_interval;
                    }
                }
                Ok(())
            })();
            let producer = worker.finish();
            // 收尾时线程真实失败优先保留，不能被主动 stop 或较早的容量错误覆盖。
            if producer.is_some_and(|f| f.sticky) || fault.is_none() {
                fault = producer.or(fault);
            }
            if let Err(e) = active {
                j.end_stream_epoch(id, "private monitor recovery/ingestion failed", true, false)?;
                return Err(e);
            }
            if let Some(f) = fault {
                j.end_stream_epoch(id, f.reason, f.sticky, false)?;
                report.gaps.push(f.reason.into());
                retry(options, report, deadline, f.sticky)?;
                continue;
            }
            // 停止网络线程之后用 REST 覆盖队列尾部；并不冒称未消费事件已逐条收到。
            let final_result = j.recover_history(v, RecoveryOptions::default());
            if let Ok(stats) = &final_result {
                report.recovery_rounds += 1;
                report.recovered_new_trades += stats.new_trades;
                report.rest_source_operations += stats.rest_operations();
            }
            j.end_stream_epoch(
                id,
                "private monitor stopped; restart requires new subscription",
                false,
                true,
            )?;
            final_result?;
            return Ok(());
        }
    })();
    report.success = result.is_ok();
    report.duration_seconds = start.elapsed().as_secs_f64();
    result
}
fn retry(
    o: MonitorOptions,
    r: &mut MonitorReport,
    deadline: Instant,
    sticky: bool,
) -> Result<(), PaperError> {
    if sticky || r.reconnects >= u64::from(o.max_reconnects) || Instant::now() >= deadline {
        return Err("private monitor cannot recover within reconnect budget".into());
    }
    r.reconnects += 1;
    let wait = Duration::from_secs((1u64 << (r.reconnects - 1)).min(30));
    if Instant::now() + wait >= deadline {
        return Err("private monitor deadline before reconnect".into());
    }
    std::thread::sleep(wait);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Flood;
    impl PrivateSource for Flood {
        fn receive(&mut self) -> Result<Option<UserEvent>, VenueError> {
            Ok(Some(UserEvent::AccountChanged))
        }
    }
    struct Bad;
    impl PrivateSource for Bad {
        fn receive(&mut self) -> Result<Option<UserEvent>, VenueError> {
            Err(VenueError::Protocol("bad subscription"))
        }
    }
    struct Panic;
    impl PrivateSource for Panic {
        fn receive(&mut self) -> Result<Option<UserEvent>, VenueError> {
            panic!("controlled producer panic")
        }
    }
    fn ended(w: &Worker) {
        let until = Instant::now() + Duration::from_secs(2);
        while !w.join.as_ref().unwrap().is_finished() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    #[test]
    fn overflow_protocol_and_panic_survive_stop_and_join() {
        let mut w = Worker::start(Flood, 2);
        ended(&w);
        let f = w.finish().unwrap();
        assert_eq!(f.reason, "private queue overflow");
        assert!(!f.sticky);
        assert_eq!(w.rx.try_iter().count(), 2);
        let mut w = Worker::start(Bad, 2);
        ended(&w);
        assert!(w.finish().unwrap().sticky);
        let mut w = Worker::start(Panic, 2);
        ended(&w);
        let f = w.finish().unwrap();
        assert_eq!(f.reason, "private producer panicked");
        assert!(f.sticky);
    }
}
