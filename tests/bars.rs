use kaze_quant::{bars::*, types::*};
fn q(seq: u64, time: u64, bid: u64) -> Quote {
    Quote {
        sequence: seq,
        timestamp_ns: time,
        bid: Price::new(bid).unwrap(),
        ask: Price::new(bid + 1).unwrap(),
        bid_quantity: 100,
        ask_quantity: 100,
    }
}
fn bar(t: u64, h: u64, l: u64, c: u64) -> Bar {
    Bar {
        start_ns: t,
        open: Price::new(l).unwrap(),
        high: Price::new(h).unwrap(),
        low: Price::new(l).unwrap(),
        close: Price::new(c).unwrap(),
        observations: 3,
    }
}
#[test]
fn boundary_quote_is_excluded_from_closed_bar_and_no_eof_flush() {
    let mut b = QuoteBars::new(100).unwrap();
    for (seq, t, p) in [(1, 0, 100), (2, 50, 105), (3, 99, 98)] {
        assert!(b.push(q(seq, t, p)).unwrap().closed.is_none());
    }
    let u = b.push(q(4, 100, 200)).unwrap();
    let c = u.closed.unwrap();
    assert_eq!(
        (
            c.open.units(),
            c.high.units(),
            c.low.units(),
            c.close.units(),
            c.observations
        ),
        (100, 105, 98, 98, 3)
    );
    assert_eq!(b.pending().unwrap().close.units(), 200);
    assert_eq!(u.missing_buckets, 0);
    b.validate_state(100).unwrap();
}
#[test]
fn time_gap_reports_missing_buckets_without_allocating_empty_bars() {
    let mut b = QuoteBars::new(10).unwrap();
    b.push(q(1, 1, 100)).unwrap();
    let u = b.push(q(2, 1_000_000_000, 105)).unwrap();
    assert_eq!(u.missing_buckets, 99_999_999);
    assert_eq!(u.closed.unwrap().observations, 1);
    assert_eq!(b.observations(), 2);
}
#[test]
fn malformed_or_old_input_is_failure_atomic_and_max_clock_is_bounded() {
    let mut b = QuoteBars::new(10).unwrap();
    b.push(q(1, 11, 100)).unwrap();
    let state = serde_json::to_value(&b).unwrap();
    for quote in [
        q(1, 12, 101),
        q(2, 10, 101),
        Quote {
            ask: Price::new(99).unwrap(),
            ..q(2, 12, 100)
        },
    ] {
        assert!(b.push(quote).is_err());
        assert_eq!(serde_json::to_value(&b).unwrap(), state);
    }
    b.push(q(2, u64::MAX, 101)).unwrap();
    b.validate_state(10).unwrap();
}
#[test]
fn wilder_seed_gap_range_and_conservative_rounding_have_hand_oracle() {
    let mut a = WilderAtr::new(2).unwrap();
    assert_eq!(a.push(&bar(0, 102, 100, 102)).unwrap(), None);
    assert_eq!(a.push(&bar(100, 106, 104, 106)).unwrap(), Some(3)); // TR2,4平均3
    assert_eq!(a.push(&bar(200, 120, 108, 110)).unwrap(), Some(9)); // TR14，(3+14)/2向上9
    assert_eq!(a.push(&bar(300, 111, 110, 111)).unwrap(), Some(5));
    a.validate_state(2).unwrap();
}
#[test]
fn flat_atr_is_zero_and_duplicate_or_invalid_bar_is_atomic() {
    let mut a = WilderAtr::new(1).unwrap();
    assert_eq!(a.push(&bar(0, 100, 100, 100)).unwrap(), Some(0));
    let before = serde_json::to_value(&a).unwrap();
    for b in [bar(0, 100, 100, 100), bar(1, 99, 100, 100)] {
        assert!(a.push(&b).is_err());
        assert_eq!(serde_json::to_value(&a).unwrap(), before);
    }
    assert!(WilderAtr::new(0).is_err());
    assert!(QuoteBars::new(0).is_err());
}
