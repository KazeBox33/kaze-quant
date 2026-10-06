//! 网络线程只传递有界原始帧；唯一写入者拥有账本与策略。
use kaze_quant::config::PaperConfig;
use kaze_quant::feed::FeedNormalizer;
use kaze_quant::paper::{Command, Envelope, PaperError};
use kaze_quant::storage::publish_new;
use kaze_quant::store::{SqliteSession, StoreOptions};
use kaze_quant::telemetry::{LiveLatency, QuoteTiming, validate_commit_window};
use sha2::{Digest, Sha256};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tungstenite::{Message, client_tls_with_config};
const FEED_QUEUE_CAPACITY: usize = 4096;
const FRAME_MAX_BYTES: usize = 8192;
/// pending包含队列帧及最多一个生产者正在发送的帧；不作为真实交易延迟指标。
#[derive(Default)]
struct QueueMeter {
    pending: AtomicUsize,
    high_water: AtomicUsize,
}
impl QueueMeter {
    fn send(&self, tx: &mpsc::SyncSender<Frame>, frame: Frame) -> Result<(), PaperError> {
        let pending = self.pending.fetch_add(1, Ordering::Relaxed) + 1;
        match tx.try_send(frame) {
            Ok(()) => {
                self.high_water.fetch_max(pending, Ordering::Relaxed);
                Ok(())
            }
            Err(error) => {
                self.pending.fetch_sub(1, Ordering::Relaxed);
                Err(match error {
                    mpsc::TrySendError::Full(_) => "feed queue capacity exceeded",
                    mpsc::TrySendError::Disconnected(_) => "feed consumer closed",
                }
                .into())
            }
        }
    }
    fn consumed(&self) {
        self.pending.fetch_sub(1, Ordering::Relaxed);
    }
}
struct Frame {
    raw: String,
    received: Instant,
}
fn elapsed_ns(start: Instant, at: Instant) -> Result<u64, PaperError> {
    u64::try_from(at.duration_since(start).as_nanos())
        .map_err(|_| "monotonic receive time overflow".into())
}
/// 检查整批时效后才触碰SQLite；统计只发生于实际确认之后。
fn commit_pending(
    session: &mut SqliteSession,
    batch: &[Envelope],
    timings: &[QuoteTiming],
    start: Instant,
    max_age_ns: u64,
    latency: &mut LiveLatency,
) -> Result<u128, PaperError> {
    if batch.len() != timings.len() {
        return Err("live batch timing count conflicts".into());
    }
    let before = Instant::now();
    let begin_ns = elapsed_ns(start, before)?;
    validate_commit_window(timings, begin_ns, max_age_ns)?;
    session.execute_batch(batch)?;
    let ack = Instant::now();
    latency.observe_commit(timings, begin_ns, elapsed_ns(start, ack)?)?;
    Ok(ack.duration_since(before).as_nanos())
}
fn terminal_error(
    outcome: Result<(), PaperError>,
    worker_panicked: bool,
    worker_failure: Option<String>,
    committed_quotes: u64,
    risk_halted: bool,
) -> Option<String> {
    outcome
        .err()
        .map(|e| e.to_string())
        .or_else(|| {
            if worker_panicked {
                Some("feed worker panicked: fail closed".into())
            } else {
                worker_failure
            }
        })
        .or_else(|| risk_halted.then(|| "paper risk halt latched".into()))
        .or_else(|| {
            (committed_quotes == 0).then(|| "live run confirmed no quotes: fail closed".into())
        })
}
/// 只忽略主动关闭接收者引起的错误；真实网络/过载故障在收尾阶段也必须保留。
fn expected_shutdown_error(error: &PaperError, stopped: bool) -> bool {
    stopped && error.0 == "feed consumer closed"
}
fn run() -> Result<(), PaperError> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 5 || args[0] == "--help" {
        println!(
            "Usage: kaze-live-paper CONFIG DB SYMBOL SECONDS|audit REPORT_NEW\nPublic Binance Spot bookTicker; all executions simulated.\nFresh/open session only. Disconnect, overload, malformed/stale data latch a durable halt.\nNo automatic re-enable. L1 lacks exchange timestamps; simulation uses monotonic local receive time."
        );
        return if args.first().is_some_and(|s| s == "--help") {
            Ok(())
        } else {
            Err("five arguments required".into())
        };
    }
    let config: PaperConfig = serde_json::from_slice(&std::fs::read(&args[0])?)?;
    config.validate()?;
    let symbol = &args[2];
    if config.markets.len() != 1
        || config.markets[0].symbol != *symbol
        || config.markets[0].units.currency != "USDT"
    {
        return Err("one market with matching Binance symbol and USDT units required".into());
    }
    let audit_only = args[3] == "audit";
    let seconds: u64 = if audit_only {
        1
    } else {
        args[3].parse().map_err(|_| "invalid duration")?
    };
    if !(1..=86400).contains(&seconds) {
        return Err("duration must be 1..86400 seconds".into());
    }
    let report = PathBuf::from(&args[4]);
    if report.exists() {
        return Err("report already exists".into());
    }
    let db = PathBuf::from(&args[1]);
    if let Some(p) = db.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(p)?;
    }
    let options = StoreOptions {
        max_database_bytes: 16 * 1024 * 1024 * 1024,
        ..StoreOptions::default()
    };
    let mut session = SqliteSession::open(&db, config.clone(), options)?;
    if audit_only {
        let verified = session.verify_full()?;
        let body = serde_json::json!({"schema_version":1,"mode":"live-paper-recover-audit","verified_commands":verified,"binary_sha256":session.binary_sha256,"audit_chain_sha256":session.chain_sha256(),"state":session.runtime().report()});
        publish_new(&report, &serde_json::to_vec_pretty(&body)?)?;
        println!("{}", serde_json::to_string(&body)?);
        return Ok(());
    }
    if session.runtime().closed() || session.runtime().halted(0).is_some() {
        return Err("session closed/halted; explicit new paper session required".into());
    }
    // 重开连接不延续历史委托，避免把断线时漏掉的报价当作连续行情。
    if session.runtime().processed() != 0 {
        return Err("live connection continuity unavailable; start a fresh session (use the same binary with duration argument audit to recover/verify old DB)".into());
    }
    let mut normalizer =
        FeedNormalizer::new(symbol.clone(), config.markets[0].units.clone(), 0, 0)?;
    let (tx, rx) = mpsc::sync_channel::<Frame>(FEED_QUEUE_CAPACITY);
    let queue_meter = Arc::new(QueueMeter::default());
    let producer_meter = queue_meter.clone();
    let failed = Arc::new(AtomicBool::new(false));
    // 单个终止原因有界保存，先发布原因再置位；主线程不再把网络失败与过载混为一谈。
    let (failure_tx, failure_rx) = mpsc::sync_channel::<String>(1);
    let stop = Arc::new(AtomicBool::new(false));
    let failure = failed.clone();
    let stopped = stop.clone();
    let wire_symbol = symbol.to_ascii_lowercase();
    let start = Instant::now();
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock before epoch")?
        .as_nanos();
    let producer = std::thread::spawn(move || {
        let result = (|| -> Result<(), PaperError> {
            let address = ("data-stream.binance.vision", 443)
                .to_socket_addrs()?
                .next()
                .ok_or("feed DNS has no address")?;
            let tcp = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
            tcp.set_read_timeout(Some(Duration::from_secs(2)))?;
            tcp.set_write_timeout(Some(Duration::from_secs(2)))?;
            let config = tungstenite::protocol::WebSocketConfig::default()
                .max_message_size(Some(FRAME_MAX_BYTES))
                .max_frame_size(Some(FRAME_MAX_BYTES));
            let (mut socket, _) = client_tls_with_config(
                format!("wss://data-stream.binance.vision/ws/{wire_symbol}@bookTicker"),
                tcp,
                Some(config),
                None,
            )
            .map_err(|_| "feed handshake failed")?;
            while !stopped.load(Ordering::Acquire) {
                match socket.read() {
                    Ok(Message::Text(raw)) => {
                        producer_meter.send(
                            &tx,
                            Frame {
                                raw: raw.to_string(),
                                received: Instant::now(),
                            },
                        )?;
                    }
                    Ok(Message::Ping(_)) => {
                        socket.flush().map_err(|_| "feed pong failed")?;
                    }
                    Ok(Message::Close(_)) => return Err("feed disconnected".into()),
                    Ok(_) => (),
                    Err(tungstenite::Error::Io(e))
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                        ) => {}
                    Err(tungstenite::Error::Io(error)) => {
                        return Err(PaperError(format!("feed transport IO: {:?}", error.kind())));
                    }
                    Err(tungstenite::Error::Capacity(_)) => {
                        return Err("feed message/frame capacity exceeded".into());
                    }
                    Err(tungstenite::Error::Protocol(_)) => {
                        return Err("feed websocket protocol failed".into());
                    }
                    Err(tungstenite::Error::Tls(_)) => return Err("feed TLS failed".into()),
                    Err(
                        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed,
                    ) => return Err("feed connection closed".into()),
                    Err(_) => return Err("feed transport failed (other class)".into()),
                }
            }
            Ok(())
        })();
        if let Err(error) = result
            && !expected_shutdown_error(&error, stopped.load(Ordering::Acquire))
        {
            let _ = failure_tx.try_send(error.to_string());
            failure.store(true, Ordering::Release);
        }
    });
    let mut batch = Vec::with_capacity(256);
    let mut arrivals = Vec::with_capacity(256);
    let mut deadline = start + Duration::from_millis(5);
    let mut last_frame = start;
    let mut committed = 0u64;
    let mut latency = LiveLatency::default();
    let mut commit_max_ns = 0u128;
    let mut wire_hash = Sha256::new();
    let mut raw_frames = 0u64;
    let outcome = (|| -> Result<(), PaperError> {
        while start.elapsed() < Duration::from_secs(seconds) {
            if failed.load(Ordering::Acquire) {
                return Err(PaperError(
                    failure_rx
                        .try_recv()
                        .unwrap_or_else(|_| "feed worker failed without detail".into()),
                ));
            }
            if last_frame.elapsed() > Duration::from_nanos(config.markets[0].risk.max_quote_age_ns)
            {
                return Err("feed stale: fail closed".into());
            }
            match rx.recv_timeout(Duration::from_millis(1)) {
                Ok(frame) => {
                    queue_meter.consumed();
                    let dequeued = Instant::now();
                    // 缓冲只吸收短时突发，不授权在积压后用过期报价产生策略订单。
                    if frame.received.elapsed()
                        > Duration::from_nanos(config.markets[0].risk.max_quote_age_ns)
                    {
                        return Err("feed queued frame stale: fail closed".into());
                    }
                    wire_hash.update(frame.raw.as_bytes());
                    wire_hash.update(b"\n");
                    raw_frames += 1;
                    let received_ns = elapsed_ns(start, frame.received)?;
                    let ns = u64::try_from(epoch + received_ns as u128)
                        .map_err(|_| "receive time overflow")?;
                    if let Some(quote) = normalizer.normalize(&frame.raw, ns)? {
                        last_frame = frame.received;
                        if batch.is_empty() {
                            deadline = Instant::now() + Duration::from_millis(5);
                        }
                        batch.push(Envelope {
                            seq: session.runtime().processed() + batch.len() as u64 + 1,
                            command: Command::Quote { market: 0, quote },
                        });
                        arrivals.push(QuoteTiming {
                            received_ns,
                            dequeued_ns: elapsed_ns(start, dequeued)?,
                        });
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(PaperError(
                        failure_rx
                            .try_recv()
                            .unwrap_or_else(|_| "feed worker closed without detail".into()),
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
            if !batch.is_empty() && (batch.len() == 256 || Instant::now() >= deadline) {
                commit_max_ns = commit_max_ns.max(commit_pending(
                    &mut session,
                    &batch,
                    &arrivals,
                    start,
                    config.markets[0].risk.max_quote_age_ns,
                    &mut latency,
                )?);
                committed += batch.len() as u64;
                arrivals.clear();
                batch.clear();
                if session.runtime().halted(0).is_some() {
                    return Err("paper risk halt latched".into());
                }
            }
        }
        if !batch.is_empty() {
            commit_max_ns = commit_max_ns.max(commit_pending(
                &mut session,
                &batch,
                &arrivals,
                start,
                config.markets[0].risk.max_quote_age_ns,
                &mut latency,
            )?);
            committed += batch.len() as u64;
        }
        Ok(())
    })();
    let observation_duration_seconds = start.elapsed().as_secs_f64();
    stop.store(true, Ordering::Release);
    drop(rx);
    // 必须先收尾工作线程，再判定成功；末批确认期间发生的故障与panic不能被Finish覆盖。
    let worker_panicked = producer.join().is_err();
    let worker_failure = if failed.load(Ordering::Acquire) {
        failure_rx
            .try_recv()
            .ok()
            .or_else(|| Some("feed worker failed without remaining detail".into()))
    } else {
        None
    };
    let error = terminal_error(
        outcome,
        worker_panicked,
        worker_failure.clone(),
        committed,
        session.runtime().halted(0).is_some(),
    );
    if error.is_some() {
        session.execute_batch(&[Envelope {
            seq: session.runtime().processed() + 1,
            command: Command::Halt {},
        }])?;
    } else {
        session.execute_batch(&[Envelope {
            seq: session.runtime().processed() + 1,
            command: Command::Finish {},
        }])?;
    }
    let verified = session.verify_full()?;
    let telemetry = latency.report();
    let body = serde_json::json!({"schema_version":2,"mode":"public-live-spot-paper","success":error.is_none(),"error":error,"symbol":symbol,"duration_seconds":start.elapsed().as_secs_f64(),"observation_duration_seconds":observation_duration_seconds,"worker_panicked":worker_panicked,"worker_failure":worker_failure,"quotes_committed":committed,"last_observed_exchange_update_id":normalizer.exchange_update_id(),"raw_frames_consumed":raw_frames,"raw_consumed_jsonl_sha256":kaze_quant::journal::hex(&wire_hash.finalize()),"binary_sha256":session.binary_sha256,"config_sha256":session.config_sha256,"audit_chain_sha256":session.chain_sha256(),"verified_commands":verified,"commit_max_ns":commit_max_ns,"feed_queue":{"capacity":FEED_QUEUE_CAPACITY,"frame_max_bytes":FRAME_MAX_BYTES,"raw_payload_capacity_bytes":FEED_QUEUE_CAPACITY*FRAME_MAX_BYTES,"pending_at_stop":queue_meter.pending.load(Ordering::Relaxed),"high_water_pending":queue_meter.high_water.load(Ordering::Relaxed),"scope":"pending includes at most one producer frame in flight; successful send high-water is an upper estimate, not latency"},"receive_to_durable_ack":telemetry["receive_to_durable_ack"],"telemetry":telemetry,"state":session.runtime().report(),"limits":"local receive timestamp; no real orders, queue position or market impact; stop/disconnect does not liquidate filled positions"});
    publish_new(&report, &serde_json::to_vec_pretty(&body)?)?;
    println!("{}", serde_json::to_string(&body)?);
    if error.is_some() {
        return Err("live paper run failed; inspect saved report".into());
    }
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(n: usize) -> Frame {
        Frame {
            raw: n.to_string(),
            received: Instant::now(),
        }
    }
    #[test]
    fn bounded_burst_preserves_order_and_overflow_never_overwrites() {
        let (tx, rx) = mpsc::sync_channel(4);
        let meter = QueueMeter::default();
        for n in 0..4 {
            meter.send(&tx, frame(n)).unwrap();
        }
        assert_eq!(meter.pending.load(Ordering::Relaxed), 4);
        assert!(meter.send(&tx, frame(4)).is_err());
        assert_eq!(meter.pending.load(Ordering::Relaxed), 4);
        for n in 0..4 {
            assert_eq!(rx.recv().unwrap().raw, n.to_string());
            meter.consumed();
        }
        assert_eq!(meter.pending.load(Ordering::Relaxed), 0);
        assert_eq!(meter.high_water.load(Ordering::Relaxed), 4);
        drop(rx);
        assert!(meter.send(&tx, frame(5)).is_err());
        assert_eq!(meter.pending.load(Ordering::Relaxed), 0);
    }
    #[test]
    fn shutdown_failure_and_worker_panic_cannot_be_success() {
        assert_eq!(
            terminal_error(Ok(()), false, Some("feed disconnected".into()), 1, false).as_deref(),
            Some("feed disconnected")
        );
        assert_eq!(
            terminal_error(Ok(()), true, None, 1, false).as_deref(),
            Some("feed worker panicked: fail closed")
        );
        assert_eq!(
            terminal_error(
                Err("risk halt".into()),
                true,
                Some("late fault".into()),
                1,
                false
            )
            .as_deref(),
            Some("risk halt")
        );
        assert!(terminal_error(Ok(()), false, None, 1, false).is_none());
        // 最后部分批次触发风控也不能被正常Finish掩盖。
        assert_eq!(
            terminal_error(Ok(()), false, None, 1, true).as_deref(),
            Some("paper risk halt latched")
        );
    }
    #[test]
    fn stale_pending_batch_does_not_change_durable_state_or_latency() {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "kaze-live-guard-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        let config: PaperConfig =
            serde_json::from_str(include_str!("../../configs/binance-live-passive.json")).unwrap();
        let db = path.join("guard.db");
        {
            use kaze_quant::types::{Price, Quote};
            let mut session = SqliteSession::open(&db, config, StoreOptions::default()).unwrap();
            let before = session.runtime().report();
            let batch = [Envelope {
                seq: 1,
                command: Command::Quote {
                    market: 0,
                    quote: Quote {
                        sequence: 1,
                        timestamp_ns: 1,
                        bid: Price::new(100).unwrap(),
                        ask: Price::new(110).unwrap(),
                        bid_quantity: 1,
                        ask_quantity: 1,
                    },
                },
            }];
            let timings = [QuoteTiming {
                received_ns: 0,
                dequeued_ns: 0,
            }];
            let mut latency = LiveLatency::default();
            let start = Instant::now() - Duration::from_secs(2);
            assert!(
                commit_pending(
                    &mut session,
                    &batch,
                    &timings,
                    start,
                    1_000_000_000,
                    &mut latency
                )
                .is_err()
            );
            assert_eq!(session.runtime().report(), before);
            assert_eq!(session.verify_full().unwrap(), 0);
            assert_eq!(latency.receive_to_ack.samples(), 0);
            let start = Instant::now();
            commit_pending(
                &mut session,
                &batch,
                &timings,
                start,
                10_000_000_000,
                &mut latency,
            )
            .unwrap();
            assert_eq!(session.verify_full().unwrap(), 1);
            assert_eq!(latency.receive_to_ack.samples(), 1);
            assert_eq!(latency.commit.samples(), 1);
        }
        std::fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn planned_consumer_close_is_the_only_ignored_shutdown_error() {
        assert!(expected_shutdown_error(
            &"feed consumer closed".into(),
            true
        ));
        assert!(!expected_shutdown_error(
            &"feed consumer closed".into(),
            false
        ));
        for message in [
            "feed queue capacity exceeded",
            "feed connection closed",
            "feed handshake failed",
        ] {
            assert!(!expected_shutdown_error(&message.into(), true));
        }
        assert!(terminal_error(Ok(()), false, None, 0, false).is_some());
    }
}
