use kaze_quant::binance::BinanceTestnet;
use kaze_quant::decimal;
use kaze_quant::execution::*;
use kaze_quant::paper::PaperError;
use std::path::Path;

/// 验收故障只丢弃已收到的提交ACK；底层仍使用同一个固定测试网适配器。
/// 这模拟应用看不到提交结果，不冒称真实网络断包。
struct AcceptanceVenue<V> {
    inner: V,
    discard_ack: bool,
    submit_calls: u64,
    discarded: bool,
}
impl<V: ExecutionVenue> ExecutionVenue for AcceptanceVenue<V> {
    fn history_head(&mut self, s: &str) -> Result<kaze_quant::recovery::HistoryHead, VenueError> {
        self.inner.history_head(s)
    }
    fn order_history(
        &mut self,
        s: &str,
        f: u64,
        l: u16,
    ) -> Result<Vec<OrderObservation>, VenueError> {
        self.inner.order_history(s, f, l)
    }
    fn trade_history(
        &mut self,
        s: &str,
        f: u64,
        l: u16,
    ) -> Result<Vec<TradeObservation>, VenueError> {
        self.inner.trade_history(s, f, l)
    }

    fn capabilities(&self) -> kaze_quant::external_state::VenueCapabilities {
        self.inner.capabilities()
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
        self.inner.account_identity()
    }
    fn account_open_orders(&mut self) -> Result<Vec<OrderObservation>, VenueError> {
        self.inner.account_open_orders()
    }

    fn validate_intent(&mut self, i: &OrderIntent) -> Result<(), VenueError> {
        self.inner.validate_intent(i)
    }
    fn submit(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.submit_calls += 1;
        let observation = self.inner.submit(i)?;
        if self.discard_ack {
            self.discarded = true;
            Err(VenueError::Unknown)
        } else {
            Ok(observation)
        }
    }
    fn query(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
        self.inner.query(i)
    }
    fn query_known(
        &mut self,
        i: &OrderIntent,
        id: Option<u64>,
    ) -> Result<OrderObservation, VenueError> {
        self.inner.query_known(i, id)
    }
    fn cancel(&mut self, i: &OrderIntent) -> Result<(), VenueError> {
        self.inner.cancel(i)
    }
    fn account(&mut self) -> Result<AccountObservation, VenueError> {
        self.inner.account()
    }
    fn open_orders(&mut self, s: &str) -> Result<Vec<OrderObservation>, VenueError> {
        self.inner.open_orders(s)
    }
    fn trades(&mut self, o: &OrderObservation) -> Result<Vec<TradeObservation>, VenueError> {
        self.inner.trades(o)
    }
}
/// 只读验收：一次主动消费者终止，网络线程退出时释放真实连接。
struct PilotSource {
    inner: kaze_quant::binance::TestnetUserStream,
    disconnect_at: Option<std::time::Instant>,
}
impl kaze_quant::continuous::PrivateSource for PilotSource {
    fn receive(&mut self) -> Result<Option<kaze_quant::user_stream::UserEvent>, VenueError> {
        if self
            .disconnect_at
            .is_some_and(|t| std::time::Instant::now() >= t)
        {
            return Err(VenueError::Unavailable);
        }
        self.inner.receive()
    }
}
fn run() -> Result<(), PaperError> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--help") || args.is_empty() {
        println!(
            "Binance Spot TESTNET only; never real-money endpoint.\nUsage: kaze-testnet JOURNAL submit INTENT.json MAX_USDT\n       kaze-testnet JOURNAL submit-drop-ack INTENT.json MAX_USDT\n       kaze-testnet JOURNAL gap-submit INTENT.json MAX_USDT\n       kaze-testnet JOURNAL monitor SECONDS\n       kaze-testnet JOURNAL monitor-gap SECONDS\n       kaze-testnet JOURNAL stream-watch SECONDS\n       kaze-testnet JOURNAL stream-submit INTENT.json MAX_USDT SECONDS\n       kaze-testnet JOURNAL ledger-init SYMBOL\n       kaze-testnet JOURNAL target-preview TARGET.json\n       kaze-testnet JOURNAL target-init TARGET.json\n       kaze-testnet JOURNAL target-tick | target-tick-drop-ack\n       kaze-testnet JOURNAL plan-init PLAN.json\n       kaze-testnet JOURNAL plan-tick | plan-tick-drop-ack\n       kaze-testnet JOURNAL plan-pause | plan-resume\n       kaze-testnet JOURNAL reconcile\n       kaze-testnet JOURNAL cancel CLIENT_ID\n       kaze-testnet JOURNAL audit\n       kaze-testnet market SYMBOL\nCredentials: local env or configs/testnet.credentials.env.\nsubmit-drop-ack intentionally discards an accepted response; it is NOT a wire-level fault.\nUncertain submissions are never resent, even if query returns not-found."
        );
        return Ok(());
    }
    if args.len() < 2 {
        return Err("journal and action required".into());
    }
    if args[0] == "market" && args.len() == 2 {
        let venue = BinanceTestnet::from_env()?;
        println!(
            "{}",
            serde_json::to_string_pretty(
                &venue
                    .market_snapshot(&args[1])
                    .map_err(|e| PaperError(e.to_string()))?
            )?
        );
        return Ok(());
    }
    let mut journal = ExecutionJournal::open(Path::new(&args[0]))?;
    if args[1] == "audit" && args.len() == 2 {
        println!("{}", serde_json::to_string_pretty(&journal.audit()?)?);
        return Ok(());
    }
    let mut venue = AcceptanceVenue {
        inner: BinanceTestnet::from_env()?,
        discard_ack: matches!(args[1].as_str(), "submit-drop-ack" | "gap-submit"),
        submit_calls: 0,
        discarded: false,
    };
    let mut stream_report = serde_json::Value::Null;
    let mut target_preview = serde_json::Value::Null;
    let mut net_initialization = serde_json::Value::Null;
    let result = match args[1].as_str() {
        "monitor" | "monitor-gap" if args.len() == 3 => {
            let seconds = args[2]
                .parse::<u64>()
                .map_err(|_| "invalid monitor duration")?;
            if args[1] == "monitor-gap" && seconds < 15 {
                return Err("monitor-gap requires at least15 seconds".into());
            }
            let forced = args[1] == "monitor-gap";
            let mut connections = 0u64;
            let connector = BinanceTestnet::from_env()?;
            let mut report = kaze_quant::continuous::MonitorReport::default();
            let result = kaze_quant::continuous::monitor(
                &mut journal,
                &mut venue,
                || {
                    let inner = connector.user_stream()?;
                    connections += 1;
                    Ok(PilotSource {
                        inner,
                        disconnect_at: if forced && connections == 1 {
                            Some(std::time::Instant::now() + std::time::Duration::from_secs(3))
                        } else {
                            None
                        },
                    })
                },
                kaze_quant::continuous::MonitorOptions {
                    duration: std::time::Duration::from_secs(seconds),
                    recovery_interval: std::time::Duration::from_secs(30),
                    max_reconnects: 8,
                },
                &mut report,
            );
            stream_report = serde_json::to_value(report)?;
            stream_report["intentional_client_disconnect"] = serde_json::json!(forced);
            stream_report["fault_scope"] =
                serde_json::json!("one consumer termination; no physical packet loss");
            result
        }
        "stream-watch" if args.len() == 3 => {
            let seconds = args[2]
                .parse::<u64>()
                .map_err(|_| "invalid stream duration")?;
            stream_pilot(&mut journal, &mut venue, None, seconds, &mut stream_report)
        }
        "stream-submit" if args.len() == 5 => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])?
                .take(8193)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8192 {
                return Err("intent exceeds 8 KiB".into());
            }
            let i: OrderIntent = serde_json::from_slice(&bytes)?;
            let cap = decimal::parse(&args[3])?;
            if cap > 100 * decimal::SCALE {
                return Err("pilot cap must be <=100 USDT".into());
            }
            i.validate(cap)?;
            let seconds = args[4]
                .parse::<u64>()
                .map_err(|_| "invalid stream duration")?;
            stream_pilot(
                &mut journal,
                &mut venue,
                Some((&i, cap)),
                seconds,
                &mut stream_report,
            )
        }

        "gap-submit" if args.len() == 4 => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])?
                .take(8193)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8192 {
                return Err("intent exceeds 8 KiB".into());
            }
            let i: OrderIntent = serde_json::from_slice(&bytes)?;
            let cap = decimal::parse(&args[3])?;
            if cap > 100 * decimal::SCALE {
                return Err("testnet pilot cap must be <=100 USDT".into());
            }
            i.validate(cap)?;
            gap_pilot(&mut journal, &mut venue, &i, cap, &mut stream_report)
        }
        "ledger-init" if args.len() == 3 => {
            let caps = venue.capabilities();
            if !caps.account_identity
                || !caps.account_open_orders
                || !caps.original_currency_fees
                || !caps.spot_limit_gtc
                || !caps.cursor_history
            {
                return Err("venue lacks required ledger capabilities".into());
            }
            if !venue
                .account_open_orders()
                .map_err(|e| PaperError(e.to_string()))?
                .is_empty()
            {
                return Err("baseline requires no open exchange orders".into());
            }
            let info = venue
                .inner
                .market_snapshot(&args[2])
                .map_err(|e| PaperError(e.to_string()))?;
            let i = &info["exchange_info"]["symbols"][0];
            let binding: kaze_quant::external_state::InstrumentBinding = serde_json::from_value(
                serde_json::json!({"symbol":i["symbol"],"base_asset":i["baseAsset"],"quote_asset":i["quoteAsset"]}),
            )?;
            let (identity, account) = venue
                .account_identity()
                .map_err(|e| PaperError(e.to_string()))?;
            let first = venue
                .history_head(&args[2])
                .map_err(|e| PaperError(e.to_string()))?;
            let (again, snapshot) = venue
                .account_identity()
                .map_err(|e| PaperError(e.to_string()))?;
            let second = venue
                .history_head(&args[2])
                .map_err(|e| PaperError(e.to_string()))?;
            if identity != again
                || serde_json::to_value(&account)? != serde_json::to_value(&snapshot)?
                || first != second
                || !venue
                    .account_open_orders()
                    .map_err(|e| PaperError(e.to_string()))?
                    .is_empty()
            {
                return Err(
                    "account/history changed during baseline capture; use isolated account".into(),
                );
            }
            journal.bind_account_with_history(&identity, &snapshot, &[binding], &[second])?;
            journal.reconcile(&mut venue)
        }

        "submit" | "submit-drop-ack" if args.len() == 4 => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])?
                .take(8193)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8192 {
                return Err("intent exceeds 8 KiB".into());
            }
            let intent: OrderIntent = serde_json::from_slice(&bytes)?;
            let cap = decimal::parse(&args[3])?;
            if cap > 100 * decimal::SCALE {
                return Err("testnet pilot cap must be <=100 USDT per order".into());
            }
            journal.submit_once(&mut venue, &intent, cap)
        }
        "target-init" if args.len() == 3 => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])?
                .take(8193)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8192 {
                return Err("target request exceeds 8 KiB".into());
            }
            net_initialization =
                journal.init_net_target(&mut venue, serde_json::from_slice(&bytes)?)?;
            Ok(())
        }
        "target-preview" if args.len() == 3 => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])?
                .take(8193)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8192 {
                return Err("target request exceeds 8 KiB".into());
            }
            let request: kaze_quant::external_target::TargetRequest =
                serde_json::from_slice(&bytes)?;
            request.validate()?;
            let start = std::time::Instant::now();
            journal.reconcile(&mut venue)?;
            if start.elapsed().as_millis() > u128::from(request.max_reconciliation_ms) {
                journal.mark_execution_gap("target preview reconciliation stale", false)?;
                return Err("stale target preview reconciliation".into());
            }
            target_preview = journal.preview_target(&request)?;
            if start.elapsed().as_millis() > u128::from(request.max_reconciliation_ms) {
                journal.mark_execution_gap("target preview compilation stale", false)?;
                return Err("stale target preview compilation".into());
            }
            Ok(())
        }
        "plan-init" if args.len() == 3 => {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(&args[2])?
                .take(8193)
                .read_to_end(&mut bytes)?;
            if bytes.len() > 8192 {
                return Err("plan config exceeds 8 KiB".into());
            }
            journal.init_plan(serde_json::from_slice(&bytes)?)
        }
        "plan-tick" | "plan-tick-drop-ack" | "target-tick" | "target-tick-drop-ack"
            if args.len() == 2 =>
        {
            if args[1].starts_with("target-") {
                // 显式净目标入口不能悄悄退化为没有净目标约束的毛量执行。
                journal.net_target_status()?;
            }
            venue.discard_ack =
                args[1] == "plan-tick-drop-ack" || args[1] == "target-tick-drop-ack";
            let r = journal.tick_plan(&mut venue, kaze_quant::binance::now_ms())?;
            stream_report = serde_json::to_value(r)?;
            Ok(())
        }
        "plan-pause" if args.len() == 2 => journal.pause_plan("operator pause"),
        "plan-resume" if args.len() == 2 => journal.resume_plan(&mut venue),
        "reconcile" if args.len() == 2 => journal.reconcile(&mut venue),
        "cancel" if args.len() == 3 => journal.cancel_once(&mut venue, &args[2]),
        _ => Err("invalid action/arguments".into()),
    };
    let mut audit = journal.audit()?;
    audit["private_stream_pilot"] = stream_report;
    if !net_initialization.is_null() {
        audit["net_target_initialization"] = net_initialization;
    }
    if !target_preview.is_null() {
        audit["target_preview"] = target_preview;
    }
    audit["pilot_transport"] = serde_json::json!({
        "submit_calls":venue.submit_calls,
        "accepted_ack_discarded":venue.discarded,
        "fault_scope":"application response suppression, not wire-level packet loss"
    });
    println!("{}", serde_json::to_string_pretty(&audit)?);
    result
}
/// 测试消费者漏收，不声称成交发生在物理线路断开后。
fn gap_pilot(
    j: &mut ExecutionJournal,
    v: &mut AcceptanceVenue<BinanceTestnet>,
    i: &OrderIntent,
    cap: i128,
    report: &mut serde_json::Value,
) -> Result<(), PaperError> {
    let epoch = j.begin_stream_epoch()?;
    let stream = match v.inner.user_stream() {
        Ok(s) => s,
        Err(e) => {
            j.end_stream_epoch(epoch, "gap pilot subscription failed", false, false)?;
            return Err(PaperError(e.to_string()));
        }
    };
    j.stream_subscribed(epoch)?;
    if let Err(e) = j.reconcile(v) {
        j.end_stream_epoch(epoch, "gap pilot initial recovery failed", false, false)?;
        return Err(e);
    }
    j.stream_recovered(epoch)?;
    let submit = j.submit_once(v, i, cap);
    // 验收期间从未调用 receive；成功 ACK 在应用层抑制，连接由客户端主动关闭。
    drop(stream);
    j.end_stream_epoch(
        epoch,
        "intentional private consumer gap after suppressed ACK",
        false,
        false,
    )?;
    if !v.discarded || v.submit_calls != 1 {
        return submit.and(Err(
            "gap pilot requires one newly accepted suppressed ACK".into()
        ));
    }
    let unknown = j
        .orders()?
        .into_iter()
        .find(|o| o.intent.client_order_id == i.client_order_id)
        .is_some_and(|o| o.phase == "unknown" && o.observation.is_none());
    if !unknown {
        return Err("suppressed ACK did not leave unknown durable intent".into());
    }
    let connector = BinanceTestnet::from_env()?;
    let mut monitor = kaze_quant::continuous::MonitorReport::default();
    let result = kaze_quant::continuous::monitor(
        j,
        v,
        || connector.user_stream(),
        kaze_quant::continuous::MonitorOptions {
            duration: std::time::Duration::from_secs(5),
            recovery_interval: std::time::Duration::from_secs(30),
            max_reconnects: 2,
        },
        &mut monitor,
    );
    *report = serde_json::json!({"accepted_ack_suppressed":true,"unknown_before_recovery":unknown,"private_receive_calls_before_disconnect":0,"consumer_gap_epoch":epoch,"recovery_monitor":monitor,"scope":"application ACK suppression and intentional unread private consumer disconnect; no physical packet-loss or fill-timing claim"});
    result
}
/// 有界人工验收；网络与 SQLite 共用一个写入者，不宣称连续实盘节点。
fn stream_pilot(
    j: &mut ExecutionJournal,
    v: &mut AcceptanceVenue<BinanceTestnet>,
    intent: Option<(&OrderIntent, i128)>,
    seconds: u64,
    report: &mut serde_json::Value,
) -> Result<(), PaperError> {
    if !(5..=300).contains(&seconds) {
        return Err("stream pilot duration must be 5..=300 seconds".into());
    }
    if j.execution_identity()?.is_none() {
        return Err("run ledger-init on a fresh journal before private stream".into());
    }
    j.mark_execution_gap("private stream connecting", false)?;
    let mut stream = v
        .inner
        .user_stream()
        .map_err(|e| PaperError(e.to_string()))?;
    let start = std::time::Instant::now();
    let mut events = 0u64;
    let mut duplicates = 0u64;
    let result = (|| -> Result<(), PaperError> {
        j.reconcile(v)?;
        if let Some((i, cap)) = intent {
            j.submit_once(v, i, cap)?;
        }
        let end = start + std::time::Duration::from_secs(seconds);
        while std::time::Instant::now() < end {
            if let Some(e) = stream.receive().map_err(|e| PaperError(e.to_string()))? {
                if matches!(e, kaze_quant::user_stream::UserEvent::Terminated) {
                    return Err("private stream terminated".into());
                }
                let execution = matches!(e, kaze_quant::user_stream::UserEvent::Execution(_));
                let inserted = j.ingest_user_event(&e)?;
                events += 1;
                if execution && !inserted {
                    duplicates += 1;
                }
                if events > 10_000 {
                    return Err("private pilot event capacity exceeded".into());
                }
            }
        }
        if let Some((i, _)) = intent {
            let open = j
                .orders()?
                .into_iter()
                .find(|o| o.intent.client_order_id == i.client_order_id)
                .is_some_and(|o| o.observation.is_none_or(|v| !v.status.terminal()));
            if open {
                j.cancel_once(v, &i.client_order_id)?;
            }
        }
        // 撤单 REST 返回后再消费已到达的推送；超出时间的成交仍由最终 REST 补查。
        let drain_end = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while std::time::Instant::now() < drain_end {
            if let Some(e) = stream.receive().map_err(|e| PaperError(e.to_string()))? {
                if matches!(e, kaze_quant::user_stream::UserEvent::Terminated) {
                    return Err("private stream terminated".into());
                }
                let execution = matches!(e, kaze_quant::user_stream::UserEvent::Execution(_));
                let inserted = j.ingest_user_event(&e)?;
                events += 1;
                if execution && !inserted {
                    duplicates += 1;
                }
                if events > 10_000 {
                    return Err("private pilot event capacity exceeded".into());
                }
            }
        }
        j.reconcile(v)?;
        Ok(())
    })();
    if result.is_err() {
        j.mark_execution_gap("private pilot failed; REST recovery required", false)?;
    }
    *report = serde_json::json!({"authenticated_subscription":true,"success":result.is_ok(),"events_received":events,"duplicate_execution_events":duplicates,"duration_seconds":start.elapsed().as_secs_f64(),"scope":"bounded synchronous manual Testnet pilot; one optional intent, cancel open remainder, no automatic liquidation/reconnect; REST final audit, no feed latency benchmark"});
    result
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
    struct Fake {
        rejected: bool,
    }
    impl ExecutionVenue for Fake {
        fn validate_intent(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
            Ok(())
        }
        fn submit(&mut self, i: &OrderIntent) -> Result<OrderObservation, VenueError> {
            if self.rejected {
                return Err(VenueError::Rejected(-1013));
            }
            Ok(OrderObservation {
                symbol: i.symbol.clone(),
                client_order_id: i.client_order_id.clone(),
                order_id: 7,
                reported_client_order_id: None,
                price: i.price.clone(),
                orig_qty: i.quantity.clone(),
                executed_qty: "0".into(),
                cummulative_quote_qty: "0".into(),
                status: VenueStatus::New,
                side: "BUY".into(),
                update_time: 1,
            })
        }
        fn query(&mut self, _: &OrderIntent) -> Result<OrderObservation, VenueError> {
            unreachable!()
        }
        fn cancel(&mut self, _: &OrderIntent) -> Result<(), VenueError> {
            unreachable!()
        }
        fn account(&mut self) -> Result<AccountObservation, VenueError> {
            unreachable!()
        }
        fn open_orders(&mut self, _: &str) -> Result<Vec<OrderObservation>, VenueError> {
            unreachable!()
        }
        fn trades(&mut self, _: &OrderObservation) -> Result<Vec<TradeObservation>, VenueError> {
            unreachable!()
        }
    }
    #[test]
    fn only_successful_ack_is_discarded_and_rejection_is_preserved() {
        let i = OrderIntent {
            client_order_id: "kaze-fault".into(),
            symbol: "BTCUSDT".into(),
            side: kaze_quant::types::Side::Buy,
            price: "100".into(),
            quantity: "0.1".into(),
        };
        let mut v = AcceptanceVenue {
            inner: Fake { rejected: false },
            discard_ack: true,
            submit_calls: 0,
            discarded: false,
        };
        assert_eq!(v.submit(&i).unwrap_err(), VenueError::Unknown);
        assert!(v.discarded);
        assert_eq!(v.submit_calls, 1);
        let mut rejected = AcceptanceVenue {
            inner: Fake { rejected: true },
            discard_ack: true,
            submit_calls: 0,
            discarded: false,
        };
        assert_eq!(
            rejected.submit(&i).unwrap_err(),
            VenueError::Rejected(-1013)
        );
        assert!(!rejected.discarded);
        assert_eq!(rejected.submit_calls, 1);
        v.discard_ack = false;
        assert_eq!(v.submit(&i).unwrap().order_id, 7);
        assert_eq!(v.submit_calls, 2);
    }
}
