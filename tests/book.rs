use kaze_quant::book::{BookError, DepthUpdate, OrderBook};
use kaze_quant::engine::{Engine, EngineConfig};
use kaze_quant::types::*;
use std::collections::BTreeMap;

fn p(v: u64) -> Price {
    Price::new(v).unwrap()
}
fn update(sequence: u64, side: Side, price: u64, quantity: u64) -> DepthUpdate {
    DepthUpdate {
        sequence,
        timestamp_ns: sequence * 100,
        side,
        price: p(price),
        quantity,
    }
}
fn ready(capacity: usize) -> OrderBook {
    let mut book = OrderBook::new(capacity).unwrap();
    assert_eq!(book.apply(update(1, Side::Buy, 99, 5)).unwrap(), None);
    book.apply(update(2, Side::Sell, 101, 6)).unwrap();
    book
}

#[test]
fn book_builds_best_quote_and_replaces_existing_levels() {
    let mut book = ready(8);
    let q = book.quote().unwrap();
    assert_eq!(
        (q.bid.units(), q.ask.units(), q.bid_quantity, q.ask_quantity),
        (99, 101, 5, 6)
    );
    book.apply(update(3, Side::Buy, 98, 8)).unwrap();
    book.apply(update(4, Side::Buy, 100, 2)).unwrap();
    book.apply(update(5, Side::Sell, 102, 3)).unwrap();
    let q = book.apply(update(6, Side::Sell, 101, 9)).unwrap().unwrap();
    assert_eq!(
        (q.bid.units(), q.ask.units(), q.bid_quantity, q.ask_quantity),
        (100, 101, 2, 9)
    );
    assert_eq!(
        book.bids()
            .iter()
            .map(|l| l.price.units())
            .collect::<Vec<_>>(),
        vec![98, 99, 100]
    );
}

#[test]
fn deleting_best_promotes_next_level_and_empty_side_removes_quote() {
    let mut book = ready(8);
    book.apply(update(3, Side::Buy, 98, 1)).unwrap();
    book.apply(update(4, Side::Sell, 102, 1)).unwrap();
    book.apply(update(5, Side::Buy, 99, 0)).unwrap();
    book.apply(update(6, Side::Sell, 101, 0)).unwrap();
    assert_eq!(
        (
            book.best_bid().unwrap().price.units(),
            book.best_ask().unwrap().price.units()
        ),
        (98, 102)
    );
    assert!(book.apply(update(7, Side::Buy, 98, 0)).unwrap().is_none());
    assert!(book.quote().is_none());
}

#[test]
fn malformed_depth_updates_leave_book_and_clock_unchanged() {
    for bad in [
        update(2, Side::Buy, 98, 1),
        update(4, Side::Buy, 98, 1),
        update(3, Side::Buy, 102, 1),
        update(3, Side::Sell, 98, 1),
        update(3, Side::Buy, 98, Quantity::MAX + 1),
        DepthUpdate {
            timestamp_ns: 1,
            ..update(3, Side::Buy, 98, 1)
        },
    ] {
        let mut book = ready(8);
        let before = book.quote();
        let bids = book.bids().to_vec();
        let asks = book.asks().to_vec();
        assert!(book.apply(bad).is_err());
        assert_eq!(book.quote(), before);
        assert_eq!(book.bids(), bids);
        assert_eq!(book.asks(), asks);
        assert_eq!(book.last_sequence(), Some(2));
    }
}

#[test]
fn book_capacity_allows_replacement_and_deletion() {
    let mut book = ready(1);
    assert_eq!(
        book.apply(update(3, Side::Buy, 98, 1)),
        Err(BookError::Capacity)
    );
    book.apply(update(3, Side::Buy, 99, 7)).unwrap();
    book.apply(update(4, Side::Buy, 99, 0)).unwrap();
    book.apply(update(5, Side::Buy, 98, 1)).unwrap();
    assert_eq!(book.best_bid().unwrap().price.units(), 98);
}

#[test]
fn absent_delete_is_idempotent_and_consumes_sequence() {
    let mut book = ready(8);
    book.apply(update(3, Side::Buy, 90, 0)).unwrap();
    assert_eq!(book.last_sequence(), Some(3));
    assert_eq!(book.bids().len(), 1);
}

#[test]
fn locked_book_is_allowed_and_zero_sequence_is_not() {
    let mut book = ready(8);
    book.apply(update(3, Side::Buy, 101, 2)).unwrap();
    assert_eq!(book.quote().unwrap().bid, book.quote().unwrap().ask);
    assert_eq!(
        OrderBook::new(1)
            .unwrap()
            .apply(update(0, Side::Buy, 99, 1)),
        Err(BookError::InvalidSequence)
    );
    assert!(OrderBook::new(0).is_err());
    assert!(OrderBook::new(1_000_001).is_err());
}

#[test]
fn seeded_order_book_matches_independent_btree_model() {
    for initial in 1u64..=8 {
        let mut book = OrderBook::new(64).unwrap();
        let (mut bids, mut asks) = (BTreeMap::new(), BTreeMap::new());
        let mut seed = initial;
        for sequence in 1..=2000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let side = if seed & 1 == 0 { Side::Buy } else { Side::Sell };
            let price = if side == Side::Buy {
                90 + (seed >> 8) % 10
            } else {
                101 + (seed >> 8) % 10
            };
            let qty = (seed >> 24) % 6;
            let u = update(sequence, side, price, qty);
            book.apply(u).unwrap();
            let levels = if side == Side::Buy {
                &mut bids
            } else {
                &mut asks
            };
            if qty == 0 {
                levels.remove(&price);
            } else {
                levels.insert(price, qty);
            }
            let expected_bid = bids.last_key_value().map(|(&p, &q)| (p, q));
            let expected_ask = asks.first_key_value().map(|(&p, &q)| (p, q));
            assert_eq!(
                book.best_bid().map(|l| (l.price.units(), l.quantity)),
                expected_bid
            );
            assert_eq!(
                book.best_ask().map(|l| (l.price.units(), l.quantity)),
                expected_ask
            );
            assert_eq!(
                book.bids()
                    .iter()
                    .map(|l| (l.price.units(), l.quantity))
                    .collect::<Vec<_>>(),
                bids.iter().map(|(&p, &q)| (p, q)).collect::<Vec<_>>()
            );
            assert_eq!(
                book.asks()
                    .iter()
                    .map(|l| (l.price.units(), l.quantity))
                    .collect::<Vec<_>>(),
                asks.iter().map(|(&p, &q)| (p, q)).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn depth_quote_adapter_drives_engine_with_no_same_event_fill() {
    let mut book = ready(8);
    let mut engine = Engine::new(EngineConfig {
        initial_cash: 1000,
        fee_bps: 0,
        ..EngineConfig::default()
    })
    .unwrap();
    engine.on_quote(book.quote().unwrap(), &mut ()).unwrap();
    engine
        .submit(
            OrderRequest {
                side: Side::Buy,
                limit: p(101),
                quantity: Quantity::new(3).unwrap(),
                time_in_force: TimeInForce::GoodTilCancelled,
            },
            &mut (),
        )
        .unwrap();
    assert_eq!(engine.account().position(), 0);
    let quote = book.apply(update(3, Side::Sell, 101, 2)).unwrap().unwrap();
    engine.on_quote(quote, &mut ()).unwrap();
    assert_eq!(engine.account().position(), 2);
    assert_eq!(engine.account().cash(), 798);
    engine.check_invariants().unwrap();
}
