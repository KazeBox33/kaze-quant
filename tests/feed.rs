use kaze_quant::config::InstrumentUnits;
use kaze_quant::feed::FeedNormalizer;
fn raw(id: u64) -> String {
    format!(r#"{{"u":{id},"s":"BTCUSDT","b":"100.01","B":"0.01000001","a":"100.02","A":"0.02"}}"#)
}
fn feed() -> FeedNormalizer {
    FeedNormalizer::new(
        "BTCUSDT".into(),
        InstrumentUnits {
            currency: "USDT".into(),
            money_scale: 100000000,
            quantity_scale: 100000,
        },
        0,
        0,
    )
    .unwrap()
}
#[test]
fn l1_ids_can_skip_but_cannot_regress() {
    let mut f = feed();
    let a = f.normalize(&raw(1), 100).unwrap().unwrap();
    let b = f.normalize(&raw(10), 101).unwrap().unwrap();
    assert_eq!(a.bid.units(), 100010);
    assert_eq!(a.bid_quantity, 1000);
    assert_eq!(b.sequence, 2);
    assert!(f.normalize(&raw(10), 102).unwrap().is_none());
    assert!(f.normalize(&raw(9), 102).is_err());
}
#[test]
fn malformed_frame_does_not_advance_state() {
    let mut f = feed();
    f.normalize(&raw(1), 100).unwrap();
    assert!(f.normalize(&raw(2).replace("100.02", "99"), 101).is_err());
    assert!(
        f.normalize(&raw(2).replace("BTCUSDT", "ETHUSDT"), 101)
            .is_err()
    );
    assert!(f.normalize(&raw(2), 99).is_err());
    assert_eq!(f.normalize(&raw(2), 101).unwrap().unwrap().sequence, 2);
}
#[test]
fn duplicate_id_cannot_hide_changed_prices_or_malformed_decimals() {
    let mut f = feed();
    f.normalize(&raw(1), 100).unwrap();
    assert!(
        f.normalize(&raw(1).replace("100.01", "100.00"), 101)
            .is_err()
    );
    assert!(
        f.normalize(&raw(1).replace("100.01", "garbage"), 101)
            .is_err()
    );
    assert!(f.normalize(&raw(1), 101).unwrap().is_none());
}
#[test]
fn duplicate_frame_updates_observed_clock_without_advancing_quote_sequence() {
    let mut f = feed();
    f.normalize(&raw(1), 100).unwrap();
    f.normalize(&raw(1), 105).unwrap();
    assert!(f.normalize(&raw(2), 104).is_err());
    assert_eq!(f.normalize(&raw(2), 105).unwrap().unwrap().sequence, 2);
}
