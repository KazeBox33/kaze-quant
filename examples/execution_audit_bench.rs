//! 同输入、同诊断、release 对照原来的逐订单全表扫描与分组核对。
use kaze_quant::{decimal, execution::*, types::Side};
use std::{hint::black_box, time::Instant};
fn reference(orders: &[TrackedOrder], trades: &[TradeObservation]) -> Vec<String> {
    let mut problems = Vec::new();
    for o in orders {
        let obs = o.observation.as_ref().unwrap();
        let mut qty = 0;
        let mut quote = 0;
        for t in trades
            .iter()
            .filter(|t| t.symbol == obs.symbol && t.order_id == obs.order_id)
        {
            assert_eq!(t.is_buyer, obs.side == "BUY");
            qty += decimal::parse(&t.qty).unwrap();
            quote += decimal::parse(&t.quote_qty).unwrap();
        }
        if qty != decimal::parse(&obs.executed_qty).unwrap()
            || quote != decimal::parse(&obs.cummulative_quote_qty).unwrap()
        {
            problems.push(format!(
                "{}: order/trade economics mismatch",
                o.intent.client_order_id
            ));
        }
    }
    problems
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("use --release");
        std::process::exit(2);
    }
    let n: usize = std::env::args()
        .nth(1)
        .map(|n| n.parse().unwrap())
        .unwrap_or(2000);
    assert!((100..=10000).contains(&n));
    let mut orders = Vec::new();
    let mut trades = Vec::new();
    for id in 1..=n as u64 {
        let client = format!("kaze-bench-{id}");
        orders.push(TrackedOrder {
            intent: OrderIntent {
                client_order_id: client.clone(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                price: "100".into(),
                quantity: "0.1".into(),
            },
            phase: "terminal".into(),
            observation: Some(OrderObservation {
                symbol: "BTCUSDT".into(),
                client_order_id: client,
                order_id: id,
                reported_client_order_id: None,
                price: "100".into(),
                orig_qty: "0.1".into(),
                executed_qty: if id % 13 == 0 { "0.09" } else { "0.1" }.into(),
                cummulative_quote_qty: if id % 13 == 0 { "9" } else { "10" }.into(),
                status: if id % 13 == 0 {
                    VenueStatus::Canceled
                } else {
                    VenueStatus::Filled
                },
                side: "BUY".into(),
                update_time: id,
            }),
        });
        for f in 0..10 {
            trades.push(TradeObservation {
                symbol: "BTCUSDT".into(),
                id: id * 10 + f,
                order_id: id,
                price: "100".into(),
                qty: "0.01".into(),
                quote_qty: "1".into(),
                commission: "0.001".into(),
                commission_asset: "USDT".into(),
                is_buyer: true,
            });
        }
    }
    // 交错顺序，不能利用每个订单的成交在输入中连续这一偶然条件。
    trades.sort_by_key(|t| ((t.id * 7919) % 100003, t.id));
    let expected = reference(&orders, &trades);
    assert_eq!(expected, order_trade_problems(&orders, &trades).unwrap());
    let mut records = Vec::new();
    for round in 0..8 {
        let mut times = [0u128; 2];
        // 交替测量顺序，计时包含聚合分配与诊断构造；生成输入在计时外。
        for which in if round % 2 == 0 { [0, 1] } else { [1, 0] } {
            let start = Instant::now();
            let result = if which == 0 {
                reference(black_box(&orders), black_box(&trades))
            } else {
                order_trade_problems(black_box(&orders), black_box(&trades)).unwrap()
            };
            times[which] = start.elapsed().as_nanos();
            assert_eq!(result, expected);
            black_box(result);
        }
        if round > 0 {
            records.push(serde_json::json!({"round":round,"scan_ns":times[0].to_string(),"grouped_ns":times[1].to_string()}));
        }
    }
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"mode":"order-trade-audit-component","orders":n,"trades":trades.len(),"mismatches":expected.len(),"results_equal":true,"rounds":records,"conditions":"release, deterministic synthetic shuffled trades, one warmup, seven alternating measured rounds; includes decimal parsing, allocation and diagnostics; excludes SQLite, asset replay, network, strategy; no CPU affinity, background soak running; reference supports this known-order BUY input only, not all new safety checks; not a framework benchmark"})).unwrap());
}
