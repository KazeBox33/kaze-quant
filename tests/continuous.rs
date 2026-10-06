#[path = "support/recovery_fixture.rs"]
mod fixture;
use fixture::*;
use kaze_quant::{continuous::*, execution::*, recovery::RecoveryOptions, user_stream::UserEvent};
use std::time::Duration;
struct Idle;
impl PrivateSource for Idle {
    fn receive(&mut self) -> Result<Option<UserEvent>, VenueError> {
        std::thread::sleep(Duration::from_millis(10));
        Ok(None)
    }
}
struct Broken;
impl PrivateSource for Broken {
    fn receive(&mut self) -> Result<Option<UserEvent>, VenueError> {
        Err(VenueError::Unavailable)
    }
}
#[test]
fn stale_generation_and_reopened_connection_cannot_change_ledger() {
    let s = Scratch::new();
    let mut j = bound(&s);
    let first = j.begin_stream_epoch().unwrap();
    assert!(j.begin_stream_epoch().is_err());
    assert!(
        j.ingest_stream_epoch(first, &UserEvent::AccountChanged)
            .is_err()
    );
    j.stream_subscribed(first).unwrap();
    assert!(j.stream_recovered(first).is_err());
    j.recover_history(&mut Fixture::new(0), RecoveryOptions::default())
        .unwrap();
    j.stream_recovered(first).unwrap();
    j.end_stream_epoch(first, "transport lost", false, false)
        .unwrap();
    let second = j.begin_stream_epoch().unwrap();
    assert!(
        j.ingest_stream_epoch(first, &UserEvent::ExternalBalanceChanged)
            .is_err()
    );
    assert!(j.stream_subscribed(first).is_err());
    assert_eq!(
        j.stream_audit().unwrap()["epochs"][1]["status"],
        "connecting"
    );
    drop(j);
    let mut j = s.open();
    assert_eq!(j.stream_audit().unwrap()["epochs"][1]["status"], "gap");
    assert!(j.stream_recovered(second).is_err());
    assert!(j.require_reconciled().is_err());
    assert!(j.begin_stream_epoch().unwrap() > second);
}
fn options() -> MonitorOptions {
    MonitorOptions {
        duration: Duration::from_secs(5),
        recovery_interval: Duration::from_secs(5),
        max_reconnects: 2,
    }
}
#[test]
fn monitor_recovers_transport_failure_but_never_sends_orders() {
    // 首次订阅连接失败后，新代次必须再次 REST 补查，静默私有账户不被误判断线。
    let s = Scratch::new();
    let mut j = bound(&s);
    let mut v = Fixture::new(0);
    let mut attempts = 0;
    let mut report = MonitorReport::default();
    monitor(
        &mut j,
        &mut v,
        || {
            attempts += 1;
            if attempts == 1 {
                Err(VenueError::Unavailable)
            } else {
                Ok(Idle)
            }
        },
        options(),
        &mut report,
    )
    .unwrap();
    assert_eq!(report.epochs, 2);
    assert_eq!(report.reconnects, 1);
    assert!(report.success);
    assert_eq!(v.posts, 0);
    assert_eq!(report.events, 0);
    assert_eq!(report.recovery_rounds, 2);
    assert!(j.require_reconciled().is_err());
    assert_eq!(j.stream_audit().unwrap()["epochs"][1]["status"], "stopped");
}
#[test]
fn producer_failure_and_bad_subscription_cannot_be_successful_shutdown() {
    let s = Scratch::new();
    let mut j = bound(&s);
    let mut v = Fixture::new(0);
    let mut r = MonitorReport::default();
    let mut o = options();
    o.max_reconnects = 0;
    assert!(monitor(&mut j, &mut v, || Ok(Broken), o, &mut r).is_err());
    assert!(!r.success);
    assert_eq!(r.gaps, ["private transport failed"]);
    assert!(j.require_reconciled().is_err());
    let mut r = MonitorReport::default();
    assert!(
        monitor(
            &mut j,
            &mut v,
            || Err::<Idle, _>(VenueError::Protocol("wrong subscription identity")),
            options(),
            &mut r
        )
        .is_err()
    );
    assert_eq!(r.reconnects, 0);
    assert_eq!(
        j.audit().unwrap()["external_ledger"]["external_activity_blocked"],
        true
    );
}
struct Disconnect {
    once: bool,
}
impl PrivateSource for Disconnect {
    fn receive(&mut self) -> Result<Option<UserEvent>, VenueError> {
        if self.once {
            Err(VenueError::Unavailable)
        } else {
            std::thread::sleep(Duration::from_millis(10));
            Ok(None)
        }
    }
}
#[test]
fn connected_producer_transport_gap_creates_new_recovered_epoch() {
    let s = Scratch::new();
    let mut j = bound(&s);
    let mut v = Fixture::new(0);
    let mut connections = 0;
    let mut r = MonitorReport::default();
    monitor(
        &mut j,
        &mut v,
        || {
            connections += 1;
            Ok(Disconnect {
                once: connections == 1,
            })
        },
        options(),
        &mut r,
    )
    .unwrap();
    assert_eq!(r.reconnects, 1);
    assert_eq!(r.epochs, 2);
    assert_eq!(r.gaps, ["private transport failed"]);
    assert_eq!(v.posts, 0);
    assert_eq!(j.stream_audit().unwrap()["epochs"][0]["status"], "gap");
    assert_eq!(j.stream_audit().unwrap()["epochs"][1]["status"], "stopped");
}
