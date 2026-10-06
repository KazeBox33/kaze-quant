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
fn run() -> Result<(), PaperError> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "--help") || args.is_empty() {
        println!(
            "Binance Spot TESTNET only; never real-money endpoint.\nUsage: kaze-testnet JOURNAL submit INTENT.json MAX_USDT\n       kaze-testnet JOURNAL submit-drop-ack INTENT.json MAX_USDT\n       kaze-testnet JOURNAL stream-watch SECONDS\n       kaze-testnet JOURNAL stream-submit INTENT.json MAX_USDT SECONDS\n       kaze-testnet JOURNAL ledger-init SYMBOL\n       kaze-testnet JOURNAL reconcile\n       kaze-testnet JOURNAL cancel CLIENT_ID\n       kaze-testnet JOURNAL audit\n       kaze-testnet market SYMBOL\nCredentials: local env or configs/testnet.credentials.env.\nsubmit-drop-ack intentionally discards an accepted response; it is NOT a wire-level fault.\nUncertain submissions are never resent, even if query returns not-found."
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
        discard_ack: args[1] == "submit-drop-ack",
        submit_calls: 0,
        discarded: false,
    };
    let mut stream_report = serde_json::Value::Null;
    let result = match args[1].as_str() {
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

        "ledger-init" if args.len() == 3 => {
            let caps = venue.capabilities();
            if !caps.account_identity
                || !caps.account_open_orders
                || !caps.original_currency_fees
                || !caps.spot_limit_gtc
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
            journal.bind_account(&identity, &account, &[binding])?;
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
        "reconcile" if args.len() == 2 => journal.reconcile(&mut venue),
        "cancel" if args.len() == 3 => journal.cancel_once(&mut venue, &args[2]),
        _ => Err("invalid action/arguments".into()),
    };
    let mut audit = journal.audit()?;
    audit["private_stream_pilot"] = stream_report;
    audit["pilot_transport"] = serde_json::json!({
        "submit_calls":venue.submit_calls,
        "accepted_ack_discarded":venue.discarded,
        "fault_scope":"application response suppression, not wire-level packet loss"
    });
    println!("{}", serde_json::to_string_pretty(&audit)?);
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
