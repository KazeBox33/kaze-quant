use kaze_quant::{
    engine::{Engine, EngineConfig},
    types::*,
};
fn request(q: u64, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        side: Side::Buy,
        limit: Price::new(100).unwrap(),
        quantity: Quantity::new(q).unwrap(),
        time_in_force: tif,
    }
}
fn quote(s: u64, liquidity: u64) -> Quote {
    Quote {
        sequence: s,
        timestamp_ns: s,
        bid: Price::new(99).unwrap(),
        ask: Price::new(100).unwrap(),
        bid_quantity: 0,
        ask_quantity: liquidity,
    }
}
fn engine() -> Engine {
    let mut e = Engine::new(EngineConfig {
        initial_cash: 10000,
        fee_bps: 0,
        max_orders: 8,
        max_active_orders: 8,
        ..EngineConfig::default()
    })
    .unwrap();
    e.on_quote(quote(1, 0), &mut ()).unwrap();
    e
}
#[test]
fn reused_low_slot_fills_after_older_high_slot_and_stale_id_cannot_cancel_it() {
    let mut e = engine();
    let a = e
        .submit(request(1, TimeInForce::GoodTilCancelled), &mut ())
        .unwrap();
    let b = e
        .submit(request(2, TimeInForce::GoodTilCancelled), &mut ())
        .unwrap();
    assert!(e.cancel(a, &mut ()));
    assert_eq!(e.compact_terminal_orders(0), 1);
    let c = e
        .submit(request(2, TimeInForce::GoodTilCancelled), &mut ())
        .unwrap();
    assert!(!e.cancel(a, &mut ()));
    let mut events = vec![];
    e.on_quote(quote(2, 3), &mut events).unwrap();
    assert!(
        matches!(events.as_slice(),[Event::Fill(x),Event::Fill(y)] if x.order_id==b && x.quantity==2 && y.order_id==c && y.quantity==1)
    );
    assert_eq!(e.account().cash(), 9700);
    assert_eq!(e.account().position(), 3);
    assert_eq!(e.account().reserved_cash(), 100);
    let bytes = serde_json::to_vec(&e.snapshot()).unwrap();
    let mut restored =
        Engine::restore(e.config().clone(), serde_json::from_slice(&bytes).unwrap()).unwrap();
    let mut resumed = vec![];
    e.on_quote(quote(3, 1), &mut events).unwrap();
    restored.on_quote(quote(3, 1), &mut resumed).unwrap();
    assert_eq!(events.last(), resumed.last());
    assert_eq!(
        serde_json::to_vec(&restored.snapshot()).unwrap(),
        serde_json::to_vec(&e.snapshot()).unwrap()
    );
}
#[test]
fn ioc_removal_and_finish_keep_fifo_without_skipping_adjacent_nodes() {
    let mut e = engine();
    let a = e
        .submit(request(2, TimeInForce::ImmediateOrCancel), &mut ())
        .unwrap();
    let b = e
        .submit(request(1, TimeInForce::GoodTilCancelled), &mut ())
        .unwrap();
    let c = e
        .submit(request(1, TimeInForce::ImmediateOrCancel), &mut ())
        .unwrap();
    let mut events = vec![];
    e.on_quote(quote(2, 1), &mut events).unwrap();
    assert!(
        matches!(events.as_slice(),[Event::Fill(x),Event::Cancelled{order_id:y,..},Event::Cancelled{order_id:z,..}] if x.order_id==a && *y==a && *z==c)
    );
    assert_eq!(e.active_count(), 1);
    e.finish(&mut events);
    assert!(matches!(events.last(),Some(Event::Cancelled{order_id,..}) if *order_id==b));
    assert_eq!(e.account().reserved_cash(), 0);
    e.check_invariants().unwrap();
}
#[test]
fn duplicate_or_unordered_snapshot_ids_are_rejected_without_panicking() {
    let mut e = engine();
    e.submit(request(1, TimeInForce::GoodTilCancelled), &mut ())
        .unwrap();
    e.submit(request(1, TimeInForce::GoodTilCancelled), &mut ())
        .unwrap();
    let mut v = serde_json::to_value(e.snapshot()).unwrap();
    v["orders"][1]["id"] = v["orders"][0]["id"].clone();
    assert!(Engine::restore(e.config().clone(), serde_json::from_value(v).unwrap()).is_err());
}
