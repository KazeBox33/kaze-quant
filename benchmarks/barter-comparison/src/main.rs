//! 独立组件实验：真实上游依赖固定到提交；不把组件结果推广为整个交易系统排名。
use barter::engine::state::order::{Orders, manager::OrderManager};
use barter_execution::order::{
    Order, OrderKey, OrderKind, TimeInForce,
    id::{ClientOrderId, OrderId, StrategyId},
    state::{Open, OrderState},
};
use barter_integration::collection::snapshot::Snapshot;
use chrono::{TimeZone, Utc};
use kaze_quant::config::InstrumentUnits;
use kaze_quant::execution::{ExecutionJournal, OrderIntent, OrderObservation, VenueStatus};
use kaze_quant::feed::FeedNormalizer;
use rust_decimal::{Decimal, prelude::ToPrimitive};
use std::hint::black_box;
use std::time::Instant;
fn barter_order(filled: i64, time: i64) -> Order<(), (), OrderState<(), ()>> {
    Order {
        key: OrderKey {
            exchange: (),
            instrument: (),
            strategy: StrategyId::new("test"),
            cid: ClientOrderId::new("kaze-test"),
        },
        side: barter_instrument::Side::Buy,
        price: Decimal::from(100),
        quantity: Decimal::from(10),
        kind: OrderKind::Limit,
        time_in_force: TimeInForce::GoodUntilCancelled { post_only: false },
        state: OrderState::active(Open {
            id: OrderId::new("7"),
            time_exchange: Utc.timestamp_millis_opt(time).unwrap(),
            filled_quantity: Decimal::from(filled),
        }),
    }
}
fn kaze_order(filled: i64, time: u64, status: VenueStatus) -> OrderObservation {
    OrderObservation {
        symbol: "BTCUSDT".into(),
        client_order_id: "kaze-test".into(),
        order_id: 7,
        price: "100".into(),
        orig_qty: "10".into(),
        executed_qty: filled.to_string(),
        cummulative_quote_qty: (filled * 100).to_string(),
        status,
        side: "BUY".into(),
        update_time: time,
    }
}
fn main() {
    assert!(!cfg!(debug_assertions), "use --release");
    let rows: usize = std::env::args()
        .nth(1)
        .unwrap_or("100000".into())
        .parse()
        .unwrap();
    assert!((1000..=1000000).contains(&rows));
    let path = std::env::temp_dir().join(format!("kaze-barter-{}", std::process::id()));
    std::fs::create_dir(&path).unwrap();
    let mut k = ExecutionJournal::open(&path.join("orders.db")).unwrap();
    let intent = OrderIntent {
        client_order_id: "kaze-test".into(),
        symbol: "BTCUSDT".into(),
        side: kaze_quant::types::Side::Buy,
        price: "100".into(),
        quantity: "10".into(),
    };
    k.prepare(&intent, 1000 * kaze_quant::decimal::SCALE)
        .unwrap();
    k.observe(&kaze_order(5, 10, VenueStatus::PartiallyFilled))
        .unwrap();
    let k_regression = k
        .observe(&kaze_order(1, 20, VenueStatus::PartiallyFilled))
        .is_err();
    let mut b = Orders::<(), ()>::default();
    b.update_from_order_snapshot(Snapshot(&barter_order(5, 10)));
    b.update_from_order_snapshot(Snapshot(&barter_order(1, 20)));
    let b_regression =
        b.0.values()
            .next()
            .unwrap()
            .state
            .open_meta()
            .unwrap()
            .filled_quantity
            == Decimal::from(5);
    k.observe(&kaze_order(10, 30, VenueStatus::Filled)).unwrap();
    let k_resurrection = k.observe(&kaze_order(0, 40, VenueStatus::New)).is_err();
    let mut terminal = barter_order(10, 30);
    terminal.state = OrderState::fully_filled();
    b.update_from_order_snapshot(Snapshot(&terminal));
    b.update_from_order_snapshot(Snapshot(&barter_order(0, 40)));
    let b_resurrection = b.0.is_empty();
    let source = std::env::args().nth(2);
    let fixtures: Vec<String> = if let Some(path) = &source {
        use std::io::BufRead;
        std::io::BufReader::new(std::fs::File::open(path).unwrap())
            .lines()
            .take(rows)
            .map(Result::unwrap)
            .collect()
    } else {
        (1..=rows).map(|i|format!(r#"{{"u":{i},"s":"BTCUSDT","b":"100.01","B":"1.00000000","a":"100.02","A":"2.00000000","T":1704067200000}}"#)).collect()
    };
    assert_eq!(fixtures.len(), rows);
    let units = InstrumentUnits {
        currency: "USDT".into(),
        money_scale: 100000000,
        quantity_scale: 100000,
    };
    let mut samples = Vec::new();
    let mut reference = FeedNormalizer::new("BTCUSDT".into(), units.clone(), 0, 0).unwrap();
    let mut expected = 0u128;
    for (i, raw) in fixtures.iter().enumerate() {
        let q = reference.normalize(raw, i as u64).unwrap().unwrap();
        let b: barter_data::exchange::binance::book::l1::BinanceOrderBookL1 =
            serde_json::from_str(raw).unwrap();
        let values = [
            (b.best_bid_price * Decimal::from(1000)).to_u64().unwrap(),
            (b.best_ask_price * Decimal::from(1000)).to_u64().unwrap(),
            (b.best_bid_amount * Decimal::from(100000))
                .to_u64()
                .unwrap(),
            (b.best_ask_amount * Decimal::from(100000))
                .to_u64()
                .unwrap(),
        ];
        assert_eq!(
            values,
            [q.bid.units(), q.ask.units(), q.bid_quantity, q.ask_quantity]
        );
        expected += values.iter().map(|&v| u128::from(v)).sum::<u128>();
    }

    for repeat in 0..8 {
        let mut run = |which: &str| {
            let mut sum = 0u128;
            let mut f = FeedNormalizer::new("BTCUSDT".into(), units.clone(), 0, 0).unwrap();
            let start = Instant::now();
            for (i, raw) in fixtures.iter().enumerate() {
                if which == "kaze" {
                    let q = f.normalize(black_box(raw), i as u64).unwrap().unwrap();
                    sum +=
                        u128::from(q.bid.units() + q.ask.units() + q.bid_quantity + q.ask_quantity);
                } else {
                    let q: barter_data::exchange::binance::book::l1::BinanceOrderBookL1 =
                        serde_json::from_str(black_box(raw)).unwrap();
                    sum += (q.best_bid_price * Decimal::from(1000)).to_u128().unwrap()
                        + (q.best_ask_price * Decimal::from(1000)).to_u128().unwrap()
                        + (q.best_bid_amount * Decimal::from(100000))
                            .to_u128()
                            .unwrap()
                        + (q.best_ask_amount * Decimal::from(100000))
                            .to_u128()
                            .unwrap();
                }
            }
            let ns = start.elapsed().as_nanos();
            assert_eq!(sum, expected);
            if repeat != 0 {
                samples.push(serde_json::json!({"variant":which,"repeat":repeat,"rows":rows,"elapsed_ns":ns,"checksum":sum}));
            }
        };
        if repeat % 2 == 0 {
            run("kaze");
            run("barter");
        } else {
            run("barter");
            run("kaze");
        }
    }
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"barter_revision":"9770b27a83f844472b93b593b08063affc974b0d","source_path":source,"source_sha256":source.as_ref().map(|p|kaze_quant::journal::file_hash(std::path::Path::new(p)).unwrap()),"workload":"preloaded L1 JSON parse + exact integer scale conversion; one warmup + seven alternating runs; no network, strategy, persistence, matching, allocations tracking or tracing subscriber","contract_differences":"Kaze validates update IDs/symbol/bounds/crossed prices and assigns supplied monotonic receive time; Barter parses subscription metadata and supplied T into UTC DateTime with Decimal; not identical output types and not an engine throughput comparison","normalization_samples":samples,"order_properties":{"cumulative_regression_rejected":{"kaze":k_regression,"barter_orders_component":b_regression},"terminal_resurrection_rejected":{"kaze":k_resurrection,"barter_orders_component":b_resurrection}},"property_scope":"Kaze durable gateway versus standalone upstream Orders component. No claim about full Barter adapters, applications or intentional external-order adoption semantics."})).unwrap());
    drop(k);
    std::fs::remove_dir_all(path).unwrap();
}
