use kaze_quant::telemetry::*;
#[test]
fn quantiles_enclose_exact_sorted_nearest_rank_across_u64_domain() {
    let mut values = vec![0, 1, 2, 3, 31, 32, 33, 63, 64, 65, u64::MAX];
    for shift in 0..64 {
        let base = 1u64 << shift;
        for sub in 0..32 {
            let v = base as u128 + (base as u128 * sub / 32);
            if v <= u64::MAX as u128 {
                values.push(v as u64);
            }
        }
        values.push(base.saturating_sub(1));
        values.push(base.saturating_add(1));
    }
    values.sort_unstable();
    let mut h = LatencyHistogram::default();
    for &v in values.iter().rev() {
        h.record(v).unwrap();
    }
    for p in 1..=100 {
        let expected = values[(values.len() * p as usize).div_ceil(100) - 1];
        let range = h.percentile(p).unwrap();
        assert!(
            range.lower_ns <= expected && expected <= range.upper_ns,
            "p={p} expected={expected} {range:?}"
        );
    }
    assert!(h.percentile(0).is_none());
    assert!(h.percentile(101).is_none());
    assert!(LatencyHistogram::default().percentile(99).is_none());
}
#[test]
fn late_degradation_after_first_100k_is_included_with_constant_memory() {
    let mut h = LatencyHistogram::default();
    let bytes = std::mem::size_of_val(&h);
    for _ in 0..100000 {
        h.record(1000).unwrap();
    }
    for _ in 0..10000 {
        h.record(1000000000).unwrap();
    }
    assert_eq!(h.samples(), 110000);
    assert_eq!(std::mem::size_of_val(&h), bytes);
    assert!(h.percentile(99).unwrap().lower_ns > 900000000);
    assert_eq!(h.report()["sum_ns"], "10000100000000");
}
#[test]
fn stage_times_and_final_single_quote_commit_have_hand_calculated_totals() {
    let mut s = LiveLatency::default();
    s.observe_commit(
        &[
            QuoteTiming {
                received_ns: 10,
                dequeued_ns: 20,
            },
            QuoteTiming {
                received_ns: 12,
                dequeued_ns: 25,
            },
        ],
        30,
        50,
    )
    .unwrap();
    s.observe_commit(
        &[QuoteTiming {
            received_ns: 60,
            dequeued_ns: 61,
        }],
        62,
        65,
    )
    .unwrap();
    assert_eq!(s.queue.report()["sum_ns"], "24");
    assert_eq!(s.batch_wait.report()["sum_ns"], "16");
    assert_eq!(s.receive_to_ack.report()["sum_ns"], "83");
    assert_eq!(s.commit.report()["sum_ns"], "23");
    assert_eq!(s.commit.samples(), 2);
    assert_eq!(s.receive_to_ack.samples(), 3);
}
#[test]
fn oldest_quote_age_and_clock_conflicts_reject_before_metrics_change() {
    let t = [
        QuoteTiming {
            received_ns: 1,
            dequeued_ns: 2,
        },
        QuoteTiming {
            received_ns: 99,
            dequeued_ns: 100,
        },
    ];
    assert!(validate_commit_window(&t, 101, 99).is_err());
    assert!(validate_commit_window(&t, 100, 99).is_ok());
    let mut s = LiveLatency::default();
    let before = s.report();
    assert!(s.observe_commit(&t, 100, 99).is_err());
    assert_eq!(s.report(), before);
    assert!(
        s.observe_commit(
            &[QuoteTiming {
                received_ns: 100,
                dequeued_ns: 99
            }],
            100,
            101
        )
        .is_err()
    );
    assert_eq!(s.report(), before);
    assert!(s.observe_commit(&[], 0, 0).is_err());
    assert_eq!(s.report(), before);
}

#[test]
fn worst_minute_exposes_short_stall_hidden_by_lifetime_p99() {
    let mut metrics = LiveLatency::default();
    let timings = vec![
        QuoteTiming {
            received_ns: 0,
            dequeued_ns: 1
        };
        100000
    ];
    metrics.observe_commit(&timings, 2, 1000).unwrap();
    metrics
        .observe_commit(
            &[QuoteTiming {
                received_ns: 60_000_000_000,
                dequeued_ns: 60_000_000_001,
            }],
            60_000_000_002,
            69_000_000_000,
        )
        .unwrap();
    let lifetime = metrics.receive_to_ack.percentile(99).unwrap();
    assert!(lifetime.lower_ns <= 1000 && lifetime.upper_ns >= 1000 && lifetime.upper_ns < 2000);
    let report = metrics.report();
    assert_eq!(report["worst_minute"]["index_since_connection_start"], 1);
    assert_eq!(report["worst_minute"]["samples"], 1);
    assert!(report["worst_minute"]["p99"]["lower_ns"].as_u64().unwrap() > 8_000_000_000);
    let before = metrics.report();
    assert!(
        metrics
            .observe_commit(
                &[QuoteTiming {
                    received_ns: 1,
                    dequeued_ns: 2
                }],
                3,
                1000
            )
            .is_err()
    );
    assert_eq!(metrics.report(), before);
}
