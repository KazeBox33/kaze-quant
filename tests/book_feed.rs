use kaze_quant::book::*;
use kaze_quant::types::*;
fn p(n: u64) -> Price {
    Price::new(n).unwrap()
}
fn l(price: u64, quantity: u64) -> Level {
    Level {
        price: p(price),
        quantity,
    }
}
fn c(side: Side, price: u64, quantity: u64) -> LevelChange {
    LevelChange {
        side,
        price: p(price),
        quantity,
    }
}
#[test]
fn gap_hides_quote_until_valid_new_snapshot() {
    let mut f = BookFeed::new(2, 4).unwrap();
    assert_eq!(f.state(), FeedState::AwaitingSnapshot);
    assert_eq!(f.delta(1, 1, &[]), Err(BookError::NeedsSnapshot));
    f.snapshot(10, 10, &[l(99, 1)], &[l(100, 2)]).unwrap();
    let before = f.book().quote();
    assert_eq!(f.delta(12, 12, &[]), Err(BookError::SequenceGap));
    assert_eq!(f.state(), FeedState::NeedsSnapshot);
    assert_eq!(f.quote(), None);
    assert_eq!(f.book().quote(), before);
    assert_eq!(f.delta(11, 11, &[]), Err(BookError::NeedsSnapshot));
    assert_eq!(
        f.snapshot(10, 12, &[l(98, 1)], &[l(100, 1)]),
        Err(BookError::InvalidSequence)
    );
    f.snapshot(20, 20, &[l(98, 1)], &[l(100, 1)]).unwrap();
    assert_eq!(f.state(), FeedState::Live);
    assert_eq!(f.quote().unwrap().sequence, 20);
}
#[test]
fn shared_sequence_frame_validates_final_spread_not_intermediate_state() {
    let mut f = BookFeed::new(2, 4).unwrap();
    f.snapshot(1, 1, &[l(99, 1)], &[l(100, 2)]).unwrap();
    // 先加入101 bid，再替换ask为102；中间交叉但整帧合法。
    let q = f
        .delta(
            2,
            2,
            &[
                c(Side::Buy, 101, 3),
                c(Side::Sell, 100, 0),
                c(Side::Sell, 102, 4),
            ],
        )
        .unwrap()
        .unwrap();
    assert_eq!((q.sequence, q.bid.units(), q.ask.units()), (2, 101, 102));
}
#[test]
fn failed_frame_and_snapshot_leave_depth_and_clock_unchanged() {
    let mut f = BookFeed::new(2, 4).unwrap();
    f.snapshot(1, 1, &[l(99, 1)], &[l(100, 2)]).unwrap();
    let q = f.quote();
    let bids = f.book().bids().to_vec();
    let asks = f.book().asks().to_vec();
    for changes in [
        vec![c(Side::Buy, 98, 1), c(Side::Sell, 97, 1)],
        vec![c(Side::Buy, 98, 1), c(Side::Buy, 98, 2)],
        vec![c(Side::Buy, 98, 1), c(Side::Sell, 102, Quantity::MAX + 1)],
        vec![c(Side::Buy, 98, 1), c(Side::Buy, 97, 1)],
    ] {
        assert!(f.delta(2, 2, &changes).is_err());
        assert_eq!(f.quote(), q);
        assert_eq!(f.book().bids(), bids);
        assert_eq!(f.book().asks(), asks);
    }
    for levels in [
        vec![l(99, 1), l(98, 1)],
        vec![l(99, 0)],
        vec![l(99, 1), l(99, 2)],
        vec![l(101, 1)],
    ] {
        assert!(f.snapshot(5, 5, &levels, &[l(100, 2)]).is_err());
        assert_eq!(f.quote(), q);
    }
    f.delta(2, 2, &[c(Side::Buy, 98, 1)]).unwrap();
    assert_eq!(f.book().bids().len(), 2);
}
#[test]
fn full_capacity_delete_and_insert_in_same_frame_and_empty_side() {
    let mut f = BookFeed::new(1, 2).unwrap();
    f.snapshot(1, 1, &[l(99, 1)], &[l(100, 2)]).unwrap();
    f.delta(2, 2, &[c(Side::Buy, 98, 2), c(Side::Buy, 99, 0)])
        .unwrap();
    assert_eq!(f.quote().unwrap().bid.units(), 98);
    assert_eq!(f.delta(3, 3, &[c(Side::Sell, 100, 0)]).unwrap(), None);
    assert_eq!(f.state(), FeedState::Live);
}
#[test]
fn frame_timestamp_sequence_and_limits_fail_before_commit() {
    let mut f = BookFeed::new(2, 1).unwrap();
    f.snapshot(1, 10, &[l(99, 1)], &[l(100, 2)]).unwrap();
    let before = f.quote();
    assert_eq!(f.delta(1, 10, &[]), Err(BookError::InvalidSequence));
    assert_eq!(f.delta(2, 9, &[]), Err(BookError::StaleTimestamp));
    assert_eq!(
        f.delta(2, 11, &[c(Side::Buy, 98, 1), c(Side::Sell, 101, 1)]),
        Err(BookError::Capacity)
    );
    assert_eq!(f.quote(), before);
}
