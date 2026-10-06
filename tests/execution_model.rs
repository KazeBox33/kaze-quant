use kaze_quant::engine::{Engine, EngineConfig, LiquidityModel};
use kaze_quant::types::*;
fn quote(seq: u64, ask: u64, qty: u64) -> Quote {
    Quote {
        sequence: seq,
        timestamp_ns: seq * 1000,
        bid: Price::new(ask).unwrap(),
        ask: Price::new(ask).unwrap(),
        bid_quantity: qty,
        ask_quantity: qty,
    }
}
fn order(side: Side, limit: u64, qty: u64, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        side,
        limit: Price::new(limit).unwrap(),
        quantity: Quantity::new(qty).unwrap(),
        time_in_force: tif,
    }
}
#[test]
fn unchanged_snapshot_does_not_replenish_consumed_liquidity() {
    let cfg = EngineConfig {
        initial_cash: 10000,
        fee_bps: 0,
        liquidity_model: LiquidityModel::DeltaBudget,
        ..EngineConfig::default()
    };
    let mut e = Engine::new(cfg.clone()).unwrap();
    e.on_quote(quote(1, 100, 10), &mut ()).unwrap();
    e.submit(
        order(Side::Buy, 100, 6, TimeInForce::GoodTilCancelled),
        &mut (),
    )
    .unwrap();
    e.on_quote(quote(2, 100, 10), &mut ()).unwrap();
    e.submit(
        order(Side::Buy, 100, 6, TimeInForce::GoodTilCancelled),
        &mut (),
    )
    .unwrap();
    e.on_quote(quote(3, 100, 10), &mut ()).unwrap();
    assert_eq!(e.account().position(), 10);
    let mut resumed = Engine::restore(cfg, e.snapshot()).unwrap();
    for q in [quote(4, 100, 11), quote(5, 100, 9), quote(6, 99, 9)] {
        e.on_quote(q, &mut ()).unwrap();
        resumed.on_quote(q, &mut ()).unwrap();
        assert_eq!(e.account().cash(), resumed.account().cash());
        assert_eq!(e.account().position(), resumed.account().position());
        assert_eq!(e.orders(), resumed.orders());
    }
    assert_eq!(e.account().position(), 12);
    assert_eq!(e.account().cash(), 8801);
    e.check_invariants().unwrap();
}
#[test]
fn slippage_obeys_limit_and_hand_calculated_round_trip() {
    let mut e = Engine::new(EngineConfig {
        initial_cash: 10000,
        fee_bps: 0,
        slippage_bps: 100,
        ..EngineConfig::default()
    })
    .unwrap();
    e.on_quote(quote(1, 100, 10), &mut ()).unwrap();
    e.submit(
        order(Side::Buy, 102, 2, TimeInForce::ImmediateOrCancel),
        &mut (),
    )
    .unwrap();
    e.on_quote(quote(2, 100, 10), &mut ()).unwrap();
    assert_eq!(e.account().cash(), 9798);
    e.submit(
        order(Side::Sell, 99, 2, TimeInForce::ImmediateOrCancel),
        &mut (),
    )
    .unwrap();
    e.on_quote(quote(3, 100, 10), &mut ()).unwrap();
    assert_eq!(e.account().cash(), 9996);
    assert_eq!(e.account().position(), 0);
    let id = e
        .submit(
            order(Side::Buy, 100, 1, TimeInForce::ImmediateOrCancel),
            &mut (),
        )
        .unwrap();
    e.on_quote(quote(4, 100, 10), &mut ()).unwrap();
    assert_eq!(e.order(id).unwrap().status, OrderStatus::Cancelled);
    assert_eq!(e.account().cash(), 9996);
    assert_eq!(e.account().reserved_cash(), 0);
}
#[test]
fn slipped_price_over_bound_cancels_without_panicking_or_filling() {
    let mut e = Engine::new(EngineConfig {
        initial_cash: 10i128.pow(20),
        fee_bps: 0,
        slippage_bps: 1000,
        ..EngineConfig::default()
    })
    .unwrap();
    e.on_quote(quote(1, Price::MAX, 1), &mut ()).unwrap();
    e.submit(
        order(Side::Buy, Price::MAX, 1, TimeInForce::ImmediateOrCancel),
        &mut (),
    )
    .unwrap();
    e.on_quote(quote(2, Price::MAX, 1), &mut ()).unwrap();
    assert_eq!(e.account().position(), 0);
    assert_eq!(e.account().reserved_cash(), 0);
}
#[test]
fn invalid_budget_checkpoint_is_rejected() {
    let mut e = Engine::new(EngineConfig::default()).unwrap();
    e.on_quote(quote(1, 100, 10), &mut ()).unwrap();
    let mut value = serde_json::to_value(e.snapshot()).unwrap();
    value["liquidity_ask_remaining"] = serde_json::json!(11);
    assert!(
        Engine::restore(
            EngineConfig::default(),
            serde_json::from_value(value).unwrap()
        )
        .is_err()
    );
}
