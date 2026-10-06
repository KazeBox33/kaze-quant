use kaze_quant::config::PaperConfig;
use kaze_quant::engine::{Engine, EngineConfig, EngineSnapshot};
use kaze_quant::paper::{Envelope, PaperRuntime, PaperSnapshot};
use kaze_quant::types::*;
fn config() -> PaperConfig {
    serde_json::from_str(include_str!("../configs/paper.json")).unwrap()
}
#[test]
fn every_split_preserves_warm_strategy_and_subsequent_execution() {
    let inputs: Vec<Envelope> = include_str!("../data/paper.jsonl")
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    for cut in 0..=inputs.len() {
        let mut expected = PaperRuntime::new(config()).unwrap();
        for c in &inputs[..cut] {
            expected.process(c).unwrap();
        }
        let snapshot = serde_json::to_vec(&expected.snapshot().unwrap()).unwrap();
        let state: PaperSnapshot = serde_json::from_slice(&snapshot).unwrap();
        let mut restored = PaperRuntime::restore(config(), state).unwrap();
        assert_eq!(expected.report(), restored.report());
        for c in &inputs[cut..] {
            assert_eq!(expected.process(c).unwrap(), restored.process(c).unwrap());
            assert_eq!(expected.report(), restored.report());
        }
    }
}
fn quote(seq: u64, qty: u64) -> Quote {
    Quote {
        sequence: seq,
        timestamp_ns: seq,
        bid: Price::new(100).unwrap(),
        ask: Price::new(100).unwrap(),
        bid_quantity: qty,
        ask_quantity: qty,
    }
}
fn order(side: Side, qty: u64) -> OrderRequest {
    OrderRequest {
        side,
        limit: Price::new(100).unwrap(),
        quantity: Quantity::new(qty).unwrap(),
        time_in_force: TimeInForce::GoodTilCancelled,
    }
}
#[test]
fn compacted_partial_orders_keep_fifo_reservations_and_monotonic_ids() {
    let c = EngineConfig {
        fee_bps: 0,
        max_orders: 6,
        max_active_orders: 6,
        ..Default::default()
    };
    let mut a = Engine::new(c.clone()).unwrap();
    let mut b = Engine::new(c.clone()).unwrap();
    for e in [&mut a, &mut b] {
        e.on_quote(quote(1, 1), &mut Vec::new()).unwrap();
        e.submit(order(Side::Buy, 2), &mut Vec::new()).unwrap();
        assert!(e.submit(order(Side::Sell, 1), &mut Vec::new()).is_err());
        e.submit(order(Side::Buy, 1), &mut Vec::new()).unwrap();
        e.on_quote(quote(2, 1), &mut Vec::new()).unwrap();
    }
    assert_eq!(b.compact_terminal_orders(0), 1);
    assert!(b.order(OrderId(2)).is_none());
    let mut b = Engine::restore(c, b.snapshot()).unwrap();
    for seq in 3..=4 {
        let mut ea = Vec::new();
        let mut eb = Vec::new();
        a.on_quote(quote(seq, 1), &mut ea).unwrap();
        b.on_quote(quote(seq, 1), &mut eb).unwrap();
        assert_eq!(ea, eb);
        assert_eq!(a.account().cash(), b.account().cash());
        assert_eq!(a.account().position(), b.account().position());
    }
    b.compact_terminal_orders(0);
    assert_eq!(
        b.submit(order(Side::Buy, 1), &mut Vec::new()).unwrap(),
        OrderId(4)
    );
}
#[test]
fn hostile_snapshot_numbers_return_errors_instead_of_panicking() {
    let e = Engine::new(EngineConfig::default()).unwrap();
    let original = serde_json::to_value(e.snapshot()).unwrap();
    for (key, value) in [
        ("pending_buy", serde_json::json!(u64::MAX)),
        ("reserved_cash", serde_json::json!(i128::MIN.to_string())),
        ("net_buy_notional", serde_json::json!(i128::MIN.to_string())),
        ("cash", serde_json::json!(i128::MIN.to_string())),
        ("position", serde_json::json!(u64::MAX)),
    ] {
        let mut v = original.clone();
        v["account"][key] = value;
        let state: EngineSnapshot = serde_json::from_value(v).unwrap();
        assert!(
            Engine::restore(EngineConfig::default(), state).is_err(),
            "{key}"
        );
    }
}
#[test]
fn corrupt_strategy_window_is_rejected_even_with_valid_engine() {
    let mut r = PaperRuntime::new(config()).unwrap();
    let c: Envelope =
        serde_json::from_str(include_str!("../data/paper.jsonl").lines().nth(1).unwrap()).unwrap();
    let mut c = c;
    c.seq = 1;
    r.process(&c).unwrap();
    let mut v = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    v["markets"][1]["strategy"]["MeanReversion"]["mean"]["sum"] = serde_json::json!(0);
    let state: PaperSnapshot = serde_json::from_value(v).unwrap();
    assert!(PaperRuntime::restore(config(), state).is_err());
}
#[test]
fn unit_scales_are_explicit_and_invalid_metadata_is_rejected() {
    let mut c = config();
    c.markets[0].units.money_scale = 3;
    assert!(c.validate().is_err());
    c.markets[0].units.money_scale = 100;
    c.markets[0].units.quantity_scale = 0;
    assert!(c.validate().is_err());
    let c: PaperConfig = serde_json::from_str(include_str!("../configs/btc-passive.json")).unwrap();
    c.validate().unwrap();
    assert_eq!(c.markets[0].units.money_scale, 100000000);
}

#[test]
fn hostile_execution_counters_cannot_overflow_after_restore() {
    let e = Engine::new(EngineConfig::default()).unwrap();
    let original = serde_json::to_value(e.snapshot()).unwrap();
    for field in [
        "quotes",
        "accepted",
        "rejected",
        "fills",
        "cancelled",
        "orders_examined",
    ] {
        let mut v = original.clone();
        v["metrics"][field] = serde_json::json!(u64::MAX);
        let state: EngineSnapshot = serde_json::from_value(v).unwrap();
        assert!(
            Engine::restore(EngineConfig::default(), state).is_err(),
            "{field}"
        );
    }
}
