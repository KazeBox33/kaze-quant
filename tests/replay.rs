use kaze_quant::engine::{Engine, EngineConfig};
use kaze_quant::replay::{CSV_HEADER, QuoteReader, TraceWriter, replay};
use kaze_quant::strategy::{RollingMean, ThresholdStrategy};
use kaze_quant::types::*;
use std::io::Cursor;

fn p(v: u64) -> Price {
    Price::new(v).unwrap()
}
fn strategy() -> ThresholdStrategy {
    ThresholdStrategy {
        buy_below: p(10_000),
        sell_above: p(10_500),
        quantity: Quantity::new(10).unwrap(),
    }
}

#[test]
fn demo_has_hand_calculated_cash_fees_and_drawdown() {
    let reader = QuoteReader::new(Cursor::new(include_str!("../data/demo.csv"))).unwrap();
    let mut engine = Engine::new(EngineConfig::default()).unwrap();
    let mut events = Vec::new();
    let summary = replay(reader, &mut engine, &mut strategy(), &mut events).unwrap();
    // 两轮买卖的独立算术账本：成本 198630，收入 213110，费用 415。
    let buy_notional = 4 * 9920 + 6 * 9950 + 5 * 9920 + 5 * 9930;
    let sell_notional = 3 * 10600 + 7 * 10610 + 6 * 10700 + 4 * 10710;
    assert_eq!(
        engine.account().cash(),
        1_000_000 - buy_notional + sell_notional - 415
    );
    assert_eq!(engine.account().cash(), 1_014_065);
    assert_eq!(engine.account().fees_paid(), 415);
    assert_eq!(engine.metrics().max_drawdown, 255);
    assert_eq!(engine.metrics().accepted, 4);
    assert_eq!(engine.metrics().fills, 8);
    assert_eq!(engine.account().position(), 0);
    assert_eq!(summary.strategy_rejections, 0);
    assert_eq!(events.len(), 12);
    engine.check_invariants().unwrap();
}

#[test]
fn deterministic_replay_produces_byte_identical_trace() {
    let run = || {
        let mut engine = Engine::new(EngineConfig::default()).unwrap();
        let mut bytes = Vec::new();
        let mut trace = TraceWriter::new(&mut bytes).unwrap();
        let reader = QuoteReader::new(Cursor::new(include_str!("../data/demo.csv"))).unwrap();
        replay(reader, &mut engine, &mut strategy(), &mut trace).unwrap();
        trace.finish().unwrap();
        bytes
    };
    let first = run();
    assert_eq!(first, run());
    assert!(
        String::from_utf8(first)
            .unwrap()
            .lines()
            .all(|line| line.split(',').count() == 9)
    );
}

#[test]
fn parsing_rejects_invalid_fields_and_ends_after_error() {
    for row in [
        "1,1,1,2,1",
        "1,1,1,2,1,1,9",
        "-1,1,1,2,1,1",
        "1,1,0,2,1,1",
        "1,1,3,2,1,1",
        "1,1,1.5,2,1,1",
        "1,1,1,2,1,1000000001",
        "",
    ] {
        let csv = format!("{CSV_HEADER}\n{row}\n1,2,1,2,1,1\n");
        let mut reader = QuoteReader::new(Cursor::new(csv)).unwrap();
        let error = reader.next().unwrap().unwrap_err();
        assert_eq!(error.line, 2);
        assert!(reader.next().is_none());
    }
}

#[test]
fn parsing_accepts_crlf_and_final_line_without_newline() {
    let csv = format!("{CSV_HEADER}\r\n1,1,1,2,0,0");
    let mut reader = QuoteReader::new(Cursor::new(csv)).unwrap();
    assert_eq!(reader.next().unwrap().unwrap().ask.units(), 2);
    assert!(reader.next().is_none());
}

#[test]
fn invalid_header_and_oversized_line_are_rejected() {
    assert!(QuoteReader::new(Cursor::new("wrong\n")).is_err());
    let csv = format!("{CSV_HEADER}\n{}\n", "1".repeat(4097));
    assert!(
        QuoteReader::new(Cursor::new(csv))
            .unwrap()
            .next()
            .unwrap()
            .is_err()
    );
}

#[test]
fn replay_rejects_empty_and_out_of_order_inputs() {
    for csv in [
        format!("{CSV_HEADER}\n"),
        format!("{CSV_HEADER}\n2,2,1,2,1,1\n1,3,1,2,1,1\n"),
    ] {
        let reader = QuoteReader::new(Cursor::new(csv)).unwrap();
        let mut engine = Engine::new(EngineConfig::default()).unwrap();
        assert!(replay(reader, &mut engine, &mut strategy(), &mut ()).is_err());
    }
}

#[test]
fn replay_stops_when_order_history_is_full() {
    let config = EngineConfig {
        max_orders: 1,
        max_active_orders: 1,
        ..EngineConfig::default()
    };
    let mut engine = Engine::new(config).unwrap();
    let reader = QuoteReader::new(Cursor::new(include_str!("../data/demo.csv"))).unwrap();
    let err = replay(reader, &mut engine, &mut strategy(), &mut ()).unwrap_err();
    assert!(err.message.contains("capacity"));
    assert_eq!(engine.orders().len(), 1);
}

#[test]
fn rolling_mean_matches_independent_full_window_sum() {
    for window in [1, 2, 7, 64, 100] {
        let mut rolling = RollingMean::new(window).unwrap();
        let mut values = Vec::new();
        let mut seed = 19u64;
        for _ in 0..1000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let value = 1 + seed % Price::MAX;
            values.push(value);
            let expected = if values.len() >= window {
                Some(
                    (values[values.len() - window..]
                        .iter()
                        .map(|&x| u128::from(x))
                        .sum::<u128>()
                        / window as u128) as u64,
                )
            } else {
                None
            };
            assert_eq!(rolling.push(p(value)), expected);
        }
    }
    assert!(RollingMean::new(0).is_err());
    assert!(RollingMean::new(1_000_001).is_err());
}

#[test]
fn trace_surfaces_write_failures() {
    struct Broken {
        remaining: usize,
    }
    impl std::io::Write for Broken {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("disk full"));
            }
            let count = data.len().min(self.remaining);
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    use kaze_quant::engine::EventSink;
    let mut trace = TraceWriter::new(Broken { remaining: 110 }).unwrap();
    trace.emit(Event::Accepted {
        order_id: OrderId(1),
        sequence: 1,
    });
    trace.emit(Event::Accepted {
        order_id: OrderId(2),
        sequence: 2,
    });
    assert!(trace.finish().is_err());
}
