use kaze_quant::account::fee;
use kaze_quant::engine::{Engine, EngineConfig, ScanPolicy};
use kaze_quant::types::*;

fn p(v: u64) -> Price {
    Price::new(v).unwrap()
}
fn q(seq: u64, bid: u64, ask: u64, bqty: u64, aqty: u64) -> Quote {
    Quote {
        sequence: seq,
        timestamp_ns: seq * 100,
        bid: p(bid),
        ask: p(ask),
        bid_quantity: bqty,
        ask_quantity: aqty,
    }
}
fn req(side: Side, price: u64, qty: u64, tif: TimeInForce) -> OrderRequest {
    OrderRequest {
        side,
        limit: p(price),
        quantity: Quantity::new(qty).unwrap(),
        time_in_force: tif,
    }
}
fn buy(price: u64, qty: u64) -> OrderRequest {
    req(Side::Buy, price, qty, TimeInForce::GoodTilCancelled)
}
fn config() -> EngineConfig {
    EngineConfig {
        initial_cash: 10_000,
        fee_bps: 0,
        ..EngineConfig::default()
    }
}
fn ready(config: EngineConfig) -> Engine {
    let mut e = Engine::new(config).unwrap();
    e.on_quote(q(1, 99, 100, 100, 100), &mut ()).unwrap();
    e
}

#[test]
fn validated_price_and_quantity_boundaries() {
    assert!(Price::new(0).is_err());
    assert!(Price::new(Price::MAX + 1).is_err());
    assert!(Quantity::new(0).is_err());
    assert!(Quantity::new(Quantity::MAX + 1).is_err());
    assert!(Price::new(Price::MAX).is_ok());
    assert!(Quantity::new(Quantity::MAX).is_ok());
    assert_eq!(p(12345).to_string(), "123.45");
}

#[test]
fn fee_rounding_is_per_fill() {
    assert_eq!(fee(0, 10), 0);
    assert_eq!(fee(1, 10), 1);
    assert_eq!(fee(10_001, 10), 11);
    assert_eq!(fee(100, 0), 0);
    assert_eq!(fee(123, 10_000), 123);
}

#[test]
fn no_market_rejects_without_reserving() {
    let mut e = Engine::new(config()).unwrap();
    let mut events = Vec::new();
    assert_eq!(
        e.submit(buy(100, 1), &mut events),
        Err(RejectReason::NoMarket)
    );
    assert_eq!(e.account().cash(), 10_000);
    assert_eq!(e.active_count(), 0);
    assert_eq!(
        events,
        vec![Event::Rejected {
            order_id: OrderId(1),
            reason: RejectReason::NoMarket
        }]
    );
    e.check_invariants().unwrap();
}

#[test]
fn zero_latency_still_requires_a_later_quote() {
    let mut e = ready(config());
    let mut events = Vec::new();
    let id = e.submit(buy(100, 3), &mut events).unwrap();
    assert_eq!(e.account().position(), 0);
    assert_eq!(e.account().cash(), 10_000);
    assert_eq!(e.account().reserved_cash(), 300);
    e.on_quote(q(2, 98, 99, 100, 100), &mut events).unwrap();
    assert_eq!(e.account().cash(), 9_703);
    assert_eq!(e.account().position(), 3);
    assert_eq!(e.order(id).unwrap().status, OrderStatus::Filled);
    assert_eq!(e.account().reserved_cash(), 0);
    assert_eq!(events.len(), 2);
}

#[test]
fn partial_fills_release_only_filled_reservations() {
    let mut e = ready(config());
    let id = e.submit(buy(100, 10), &mut ()).unwrap();
    e.on_quote(q(2, 98, 99, 20, 4), &mut ()).unwrap();
    assert_eq!(e.account().cash(), 9_604);
    assert_eq!(e.account().reserved_cash(), 600);
    assert_eq!(e.account().pending_buy(), 6);
    assert_eq!(e.order(id).unwrap().status, OrderStatus::PartiallyFilled);
    e.on_quote(q(3, 98, 99, 20, 6), &mut ()).unwrap();
    assert_eq!(e.account().position(), 10);
    assert_eq!(e.active_count(), 0);
    assert_eq!(e.account().cash(), 9_010);
    e.check_invariants().unwrap();
}

#[test]
fn fifo_and_shared_liquidity_across_orders() {
    let mut e = ready(config());
    let first = e.submit(buy(101, 4), &mut ()).unwrap();
    let second = e.submit(buy(100, 4), &mut ()).unwrap();
    e.on_quote(q(2, 99, 100, 20, 5), &mut ()).unwrap();
    assert_eq!(e.order(first).unwrap().remaining, 0);
    assert_eq!(e.order(second).unwrap().remaining, 3);
    assert_eq!(e.account().position(), 5);
    e.check_invariants().unwrap();
}

#[test]
fn compaction_keeps_submission_priority() {
    let mut e = ready(config());
    let a = e.submit(buy(100, 1), &mut ()).unwrap();
    let b = e.submit(buy(100, 2), &mut ()).unwrap();
    let c = e.submit(buy(100, 2), &mut ()).unwrap();
    assert!(e.cancel(a, &mut ()));
    e.on_quote(q(2, 99, 100, 10, 2), &mut ()).unwrap();
    assert_eq!(e.order(b).unwrap().status, OrderStatus::Filled);
    assert_eq!(e.order(c).unwrap().remaining, 2);
}

#[test]
fn non_crossing_limit_does_not_fill() {
    let mut e = ready(config());
    let id = e.submit(buy(99, 1), &mut ()).unwrap();
    e.on_quote(q(2, 100, 101, 10, 10), &mut ()).unwrap();
    assert_eq!(e.account().position(), 0);
    assert_eq!(e.order(id).unwrap().status, OrderStatus::Accepted);
    e.on_quote(q(3, 98, 99, 10, 10), &mut ()).unwrap();
    assert_eq!(e.account().position(), 1);
}

#[test]
fn latency_boundary_is_inclusive() {
    let mut e = ready(EngineConfig {
        latency_ns: 200,
        ..config()
    });
    e.submit(buy(100, 1), &mut ()).unwrap();
    e.on_quote(q(2, 99, 100, 10, 10), &mut ()).unwrap();
    assert_eq!(e.account().position(), 0);
    e.on_quote(q(3, 99, 100, 10, 10), &mut ()).unwrap();
    assert_eq!(e.account().position(), 1);
}

#[test]
fn latency_overflow_rejects_without_state_corruption() {
    let mut e = Engine::new(EngineConfig {
        latency_ns: 2,
        ..config()
    })
    .unwrap();
    let mut quote = q(1, 99, 100, 10, 10);
    quote.timestamp_ns = u64::MAX - 1;
    e.on_quote(quote, &mut ()).unwrap();
    assert_eq!(
        e.submit(buy(100, 1), &mut ()),
        Err(RejectReason::TimestampOverflow)
    );
    assert_eq!(e.account().reserved_cash(), 0);
    e.check_invariants().unwrap();
}

#[test]
fn ioc_partial_fill_then_cancel_and_release() {
    let mut e = ready(config());
    let mut events = Vec::new();
    let id = e
        .submit(
            req(Side::Buy, 100, 5, TimeInForce::ImmediateOrCancel),
            &mut events,
        )
        .unwrap();
    e.on_quote(q(2, 99, 100, 10, 2), &mut events).unwrap();
    assert_eq!(e.order(id).unwrap().status, OrderStatus::Cancelled);
    assert_eq!(e.order(id).unwrap().remaining, 3);
    assert_eq!(e.account().position(), 2);
    assert_eq!(e.account().reserved_cash(), 0);
    assert!(matches!(events[1], Event::Fill(_)));
    assert!(matches!(events[2], Event::Cancelled { .. }));
    e.check_invariants().unwrap();
}

#[test]
fn ioc_non_crossing_or_zero_liquidity_cancels() {
    for (price, liquidity) in [(99, 10), (100, 0)] {
        let mut e = ready(config());
        let id = e
            .submit(
                req(Side::Buy, price, 5, TimeInForce::ImmediateOrCancel),
                &mut (),
            )
            .unwrap();
        e.on_quote(q(2, 99, 100, 10, liquidity), &mut ()).unwrap();
        assert_eq!(e.order(id).unwrap().status, OrderStatus::Cancelled);
        assert_eq!(e.account().position(), 0);
        assert_eq!(e.account().reserved_cash(), 0);
    }
}

#[test]
fn ioc_waits_for_configured_latency() {
    let mut e = ready(EngineConfig {
        latency_ns: 200,
        ..config()
    });
    let id = e
        .submit(
            req(Side::Buy, 100, 1, TimeInForce::ImmediateOrCancel),
            &mut (),
        )
        .unwrap();
    e.on_quote(q(2, 99, 100, 10, 0), &mut ()).unwrap();
    assert_eq!(e.order(id).unwrap().status, OrderStatus::Accepted);
    e.on_quote(q(3, 99, 100, 10, 0), &mut ()).unwrap();
    assert_eq!(e.order(id).unwrap().status, OrderStatus::Cancelled);
}

#[test]
fn frozen_cash_prevents_double_spending() {
    let mut e = ready(EngineConfig {
        initial_cash: 500,
        ..config()
    });
    e.submit(buy(100, 4), &mut ()).unwrap();
    assert_eq!(
        e.submit(buy(100, 2), &mut ()),
        Err(RejectReason::InsufficientCash)
    );
    assert_eq!(e.account().available_cash(), 100);
    e.check_invariants().unwrap();
}

#[test]
fn pending_buys_count_toward_position_limit() {
    let mut e = ready(EngineConfig {
        max_position: 5,
        ..config()
    });
    e.submit(buy(100, 4), &mut ()).unwrap();
    assert_eq!(
        e.submit(buy(100, 2), &mut ()),
        Err(RejectReason::PositionLimit)
    );
}

#[test]
fn conservative_fee_reserve_covers_many_tiny_fills() {
    let mut e = ready(EngineConfig {
        initial_cash: 303,
        fee_bps: 1,
        ..config()
    });
    e.submit(buy(100, 3), &mut ()).unwrap();
    for seq in 2..=4 {
        e.on_quote(q(seq, 99, 100, 10, 1), &mut ()).unwrap();
    }
    assert_eq!(e.account().cash(), 0);
    assert_eq!(e.account().fees_paid(), 3);
    assert_eq!(e.account().reserved_cash(), 0);
    e.check_invariants().unwrap();
}

#[test]
fn sell_reservation_prevents_overselling() {
    let mut e = ready(config());
    e.submit(buy(100, 5), &mut ()).unwrap();
    e.on_quote(q(2, 99, 100, 10, 10), &mut ()).unwrap();
    let id = e
        .submit(
            req(Side::Sell, 100, 4, TimeInForce::GoodTilCancelled),
            &mut (),
        )
        .unwrap();
    assert_eq!(
        e.submit(
            req(Side::Sell, 100, 2, TimeInForce::GoodTilCancelled),
            &mut ()
        ),
        Err(RejectReason::InsufficientPosition)
    );
    e.on_quote(q(3, 101, 102, 2, 10), &mut ()).unwrap();
    assert_eq!(e.account().position(), 3);
    assert_eq!(e.account().reserved_sell(), 2);
    assert!(e.cancel(id, &mut ()));
    assert_eq!(e.account().reserved_sell(), 0);
    assert_eq!(e.account().cash(), 9_702);
    e.check_invariants().unwrap();
}

#[test]
fn cancel_is_idempotent_and_finish_releases_resources() {
    let mut e = ready(config());
    let id = e.submit(buy(100, 2), &mut ()).unwrap();
    assert!(!e.cancel(OrderId(0), &mut ()));
    assert!(!e.cancel(OrderId(u64::MAX), &mut ()));
    assert!(e.cancel(id, &mut ()));
    assert!(!e.cancel(id, &mut ()));
    e.submit(buy(100, 2), &mut ()).unwrap();
    e.finish(&mut ());
    e.finish(&mut ());
    assert_eq!(e.metrics().cancelled, 2);
    assert_eq!(e.active_count(), 0);
    assert_eq!(e.account().available_cash(), 10_000);
}

#[test]
fn capacities_are_enforced_and_cancel_frees_active_slot() {
    let mut e = ready(EngineConfig {
        max_active_orders: 1,
        max_orders: 3,
        ..config()
    });
    let a = e.submit(buy(100, 1), &mut ()).unwrap();
    assert_eq!(
        e.submit(buy(100, 1), &mut ()),
        Err(RejectReason::ActiveOrderLimit)
    );
    e.cancel(a, &mut ());
    e.submit(buy(100, 1), &mut ()).unwrap();
    assert_eq!(
        e.submit(buy(100, 1), &mut ()),
        Err(RejectReason::HistoryLimit)
    );
    assert_eq!(e.orders().len(), 3);
    e.check_invariants().unwrap();
}

#[test]
fn malformed_quote_is_atomic() {
    let mut e = ready(config());
    e.submit(buy(100, 1), &mut ()).unwrap();
    let cash = e.account().clone();
    let orders = e.orders().to_vec();
    let metrics = e.metrics();
    let invalids = [
        q(1, 99, 100, 10, 10),
        q(2, 101, 100, 10, 10),
        Quote {
            timestamp_ns: 99,
            ..q(2, 99, 100, 10, 10)
        },
        q(2, 99, 100, 10, Quantity::MAX + 1),
    ];
    let mut events = Vec::new();
    for bad in invalids {
        assert!(e.on_quote(bad, &mut events).is_err());
        assert_eq!(*e.account(), cash);
        assert_eq!(e.orders(), orders);
        assert_eq!(e.metrics(), metrics);
        assert_eq!(e.last_quote().unwrap().sequence, 1);
        assert!(events.is_empty());
    }
}

#[test]
fn equal_timestamps_with_increasing_sequences_are_supported() {
    let mut e = ready(config());
    e.submit(buy(100, 1), &mut ()).unwrap();
    e.on_quote(
        Quote {
            timestamp_ns: 100,
            ..q(2, 99, 100, 10, 10)
        },
        &mut (),
    )
    .unwrap();
    assert_eq!(e.account().position(), 1);
}

#[test]
fn round_trip_conservation_and_bid_marked_drawdown() {
    let mut e = ready(EngineConfig {
        fee_bps: 100,
        ..config()
    });
    e.submit(buy(100, 10), &mut ()).unwrap();
    e.on_quote(q(2, 90, 100, 100, 100), &mut ()).unwrap();
    assert_eq!(e.account().cash(), 8_990);
    assert_eq!(e.equity(), 9_890);
    assert_eq!(e.metrics().max_drawdown, 110);
    e.submit(
        req(Side::Sell, 110, 10, TimeInForce::GoodTilCancelled),
        &mut (),
    )
    .unwrap();
    e.on_quote(q(3, 110, 111, 100, 100), &mut ()).unwrap();
    assert_eq!(e.account().position(), 0);
    assert_eq!(e.account().cash(), 10_079);
    assert_eq!(e.account().fees_paid(), 21);
    assert_eq!(e.equity(), 10_079);
}

#[test]
fn maximum_price_quantity_and_cash_stay_in_bounds() {
    let mut e = Engine::new(EngineConfig {
        initial_cash: 1_000_000_000_000_000_000_000_000_000_000,
        max_position: Quantity::MAX,
        fee_bps: 10_000,
        ..config()
    })
    .unwrap();
    e.on_quote(
        q(1, Price::MAX, Price::MAX, Quantity::MAX, Quantity::MAX),
        &mut (),
    )
    .unwrap();
    e.submit(buy(Price::MAX, Quantity::MAX), &mut ()).unwrap();
    e.on_quote(
        q(2, Price::MAX, Price::MAX, Quantity::MAX, Quantity::MAX),
        &mut (),
    )
    .unwrap();
    e.check_invariants().unwrap();
    assert_eq!(e.account().position(), Quantity::MAX);
}

#[test]
fn invalid_engine_configuration_is_rejected() {
    for config in [
        EngineConfig {
            initial_cash: -1,
            ..config()
        },
        EngineConfig {
            fee_bps: 10001,
            ..config()
        },
        EngineConfig {
            max_position: 0,
            ..config()
        },
        EngineConfig {
            max_orders: 0,
            ..config()
        },
        EngineConfig {
            max_active_orders: 0,
            ..config()
        },
        EngineConfig {
            max_active_orders: 4,
            max_orders: 3,
            ..config()
        },
    ] {
        assert!(Engine::new(config).is_err());
    }
}

/// 扫描优化的对照只验证索引调度；另有手算测试检查双方共用的执行核算。
#[test]
fn seeded_mixed_actions_match_history_reference_exactly() {
    for initial_seed in 1u64..=12 {
        let conf = EngineConfig {
            latency_ns: 150,
            fee_bps: 17,
            max_active_orders: 20,
            max_orders: 400,
            ..config()
        };
        let mut fast = Engine::with_scan_policy(conf.clone(), ScanPolicy::Active).unwrap();
        let mut slow = Engine::with_scan_policy(conf, ScanPolicy::History).unwrap();
        let (mut fast_events, mut slow_events) = (Vec::new(), Vec::new());
        let mut seed = initial_seed;
        for seq in 1..=250 {
            seed = seed.wrapping_mul(6364136223846793005u64).wrapping_add(1);
            let bid = 90 + seed % 20;
            let quote = q(seq, bid, bid + 2, (seed >> 8) % 5, (seed >> 16) % 5);
            fast.on_quote(quote, &mut fast_events).unwrap();
            slow.on_quote(quote, &mut slow_events).unwrap();
            if seed % 7 == 0 {
                let id = OrderId(1 + (seed >> 32) % 250);
                assert_eq!(
                    fast.cancel(id, &mut fast_events),
                    slow.cancel(id, &mut slow_events)
                );
            } else {
                let request = req(
                    if seed & 1 == 0 { Side::Buy } else { Side::Sell },
                    95 + (seed >> 24) % 20,
                    1 + (seed >> 40) % 5,
                    if seed % 3 == 0 {
                        TimeInForce::ImmediateOrCancel
                    } else {
                        TimeInForce::GoodTilCancelled
                    },
                );
                assert_eq!(
                    fast.submit(request, &mut fast_events),
                    slow.submit(request, &mut slow_events)
                );
            }
            assert_eq!(fast.account(), slow.account());
            assert_eq!(fast.orders(), slow.orders());
            assert_eq!(fast_events, slow_events);
            assert_eq!(fast.equity(), slow.equity());
            fast.check_invariants().unwrap();
            slow.check_invariants().unwrap();
        }
        fast.finish(&mut fast_events);
        slow.finish(&mut slow_events);
        assert_eq!(fast_events, slow_events);
        assert_eq!(fast.account(), slow.account());
        assert!(fast.metrics().orders_examined < slow.metrics().orders_examined);
    }
}
