use kaze_quant::{
    bar_strategy::*,
    config::*,
    paper::*,
    store::{SqliteSession, StoreOptions},
    strategy::{Strategy, StrategyView},
    target::*,
    types::*,
};
fn parameters() -> Parameters {
    Parameters {
        bar_interval_ns: 100,
        atr_period: 2,
        trend_window: 2,
        trend_band_bps: 0,
        distance_budget_minor: 120,
        atr_multiple_bps: 20000,
        min_distance_units: 1,
        max_target_lots: 30,
        execution: ExecutionPolicy {
            max_spread_bps: 10000,
            cooldown_ns: 0,
            min_rebalance_lots: 1,
            max_child_lots: 30,
            max_working_ns: 1000,
            schedule: Schedule::Immediate,
        },
    }
}
fn q(seq: u64, t: u64, bid: u64) -> Quote {
    Quote {
        sequence: seq,
        timestamp_ns: t,
        bid: Price::new(bid).unwrap(),
        ask: Price::new(bid + 1).unwrap(),
        bid_quantity: 100,
        ask_quantity: 100,
    }
}
fn observe(s: &mut BarAtrStrategy, quote: Quote) {
    let engine =
        kaze_quant::engine::Engine::new(kaze_quant::engine::EngineConfig::default()).unwrap();
    assert_eq!(
        s.on_quote(StrategyView {
            quote,
            account: engine.account(),
            active_orders: 0
        }),
        Action::None
    );
}
fn config() -> PaperConfig {
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
    c.markets.truncate(1);
    let m = &mut c.markets[0];
    m.engine.initial_cash = 100000;
    m.engine.latency_ns = 0;
    m.engine.fee_bps = 10;
    m.price_tick = 1;
    m.quantity_step = 1;
    m.strategy = StrategyConfig::Registered {
        name: "bar-atr".into(),
        version: 1,
        parameters: serde_json::to_value(parameters()).unwrap(),
    };
    c
}
fn stream() -> Vec<Envelope> {
    (0..16)
        .flat_map(|b| {
            let p = if b < 8 {
                100 + b * 2
            } else {
                116 - (b - 8) * 4
            };
            [q(b * 2 + 1, b * 100 + 1, p), q(b * 2 + 2, b * 100 + 50, p)]
        })
        .map(|quote| Envelope {
            seq: quote.sequence,
            command: Command::Quote { market: 0, quote },
        })
        .collect()
}
#[test]
fn incomplete_bar_does_not_change_signal_and_atr_growth_reduces_target() {
    let mut s = BarAtrStrategy::new(parameters()).unwrap();
    for (seq, t, bid) in [
        (1, 1, 100),
        (2, 2, 102),
        (3, 100, 104),
        (4, 101, 106),
        (5, 200, 108),
    ] {
        observe(&mut s, q(seq, t, bid));
    }
    assert_eq!(s.distance_units(), Some(6));
    assert_eq!(s.requested_target(), 20);
    observe(&mut s, q(6, 201, 120));
    observe(&mut s, q(7, 202, 110));
    assert_eq!(s.requested_target(), 20);
    observe(&mut s, q(8, 300, 200));
    assert_eq!(s.distance_units(), Some(18));
    assert_eq!(s.requested_target(), 6);
    assert_eq!(s.diagnostics()["last_closed"]["close"], 110);
}
#[test]
fn gap_resets_warmup_and_no_future_fill_is_synthesized() {
    let mut s = BarAtrStrategy::new(parameters()).unwrap();
    for (seq, t, bid) in [(1, 1, 100), (2, 101, 102), (3, 201, 104)] {
        observe(&mut s, q(seq, t, bid));
    }
    assert!(s.requested_target() > 0);
    observe(&mut s, q(4, 601, 105));
    assert_eq!(s.requested_target(), 0);
    assert_eq!(s.diagnostics()["missing_buckets"], 3);
    assert_eq!(s.diagnostics()["ready"], false);
    observe(&mut s, q(5, 701, 106));
    assert_eq!(s.requested_target(), 0);
    observe(&mut s, q(6, 801, 107));
    assert!(s.requested_target() > 0);
}
#[test]
fn state_factory_rejects_identity_window_source_and_future_conflicts() {
    let p = parameters();
    let mut s = BarAtrStrategy::new(p.clone()).unwrap();
    for seq in 1..=5 {
        observe(&mut s, q(seq, seq * 100, 100 + seq));
    }
    let v = s.custom_checkpoint().unwrap().state;
    for field in [
        "parameters",
        "mean",
        "empty_mean",
        "last_closed",
        "segment_bars",
    ] {
        let mut bad = v.clone();
        match field {
            "parameters" => bad[field]["atr_period"] = serde_json::json!(3),
            "mean" => bad[field]["sum"] = serde_json::json!(0),
            "empty_mean" => {
                bad["mean"] = serde_json::json!({"values":[],"next":0,"count":0,"sum":0})
            }
            "last_closed" => bad[field]["start_ns"] = serde_json::json!(10000),
            _ => bad[field] = serde_json::json!(0),
        }
        assert!(
            factory(&serde_json::to_value(&p).unwrap(), Some(&bad)).is_err(),
            "{field}"
        );
    }
}
#[test]
fn every_quote_restore_is_identical_and_generates_both_buy_and_sell() {
    let c = config();
    let mut reference = PaperRuntime::new(c.clone()).unwrap();
    let mut resumed = PaperRuntime::new(c.clone()).unwrap();
    for e in stream() {
        assert_eq!(reference.process(&e).unwrap(), resumed.process(&e).unwrap());
        resumed = PaperRuntime::restore(c.clone(), resumed.snapshot().unwrap()).unwrap();
    }
    assert_eq!(reference.report(), resumed.report());
    assert!(reference.report().markets[0].metrics.fills >= 2);
    assert_eq!(reference.report().markets[0].position_units, 0);
    reference.check_invariants().unwrap();
}
#[test]
fn sqlite_restarts_with_open_bar_and_indicators_and_failed_batch_rolls_back() {
    let dir = std::env::temp_dir().join(format!("kaze-bar-atr-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    let db = dir.join("run.db");
    let c = config();
    let quotes = stream();
    for chunk in quotes.chunks(3) {
        let mut s = SqliteSession::open(&db, c.clone(), StoreOptions::default()).unwrap();
        s.execute_batch(chunk).unwrap();
    }
    let mut s = SqliteSession::open(&db, c, StoreOptions::default()).unwrap();
    assert_eq!(s.verify_full().unwrap(), 32);
    let before = serde_json::to_value(s.runtime().snapshot().unwrap()).unwrap();
    let first = Envelope {
        seq: 33,
        command: Command::Quote {
            market: 0,
            quote: q(33, 1601, 110),
        },
    };
    let invalid = Envelope {
        seq: 35,
        command: Command::Halt {},
    };
    assert!(s.execute_batch(&[first, invalid]).is_err());
    assert_eq!(
        serde_json::to_value(s.runtime().snapshot().unwrap()).unwrap(),
        before
    );
    assert_eq!(s.verify_full().unwrap(), 32);
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn sizing_caps_and_integer_distance_prevent_flat_price_division_by_zero() {
    let mut p = parameters();
    p.atr_period = 1;
    p.min_distance_units = 10;
    p.max_target_lots = 3;
    let mut s = BarAtrStrategy::new(p.clone()).unwrap();
    for seq in 1..=3 {
        observe(&mut s, q(seq, seq * 100, 100 + seq));
    }
    assert_eq!(s.distance_units(), Some(10));
    assert_eq!(s.requested_target(), 3);
    p.min_distance_units = 0;
    assert!(BarAtrStrategy::new(p).is_err());
}

#[test]
fn market_position_grid_and_hard_history_risk_still_bound_generated_orders() {
    let mut c = config();
    let m = &mut c.markets[0];
    m.quantity_step = 3;
    m.engine.max_position = 9;
    m.engine.max_orders = 1;
    m.engine.max_active_orders = 1;
    let mut r = PaperRuntime::new(c).unwrap();
    for mut e in stream() {
        if let Command::Quote { quote, .. } = &mut e.command {
            quote.bid_quantity = 99;
            quote.ask_quantity = 99;
        }
        let receipt = r.process(&e).unwrap();
        for notice in receipt.notices {
            if let Notice::StrategyDecision { decision, .. } = notice {
                assert!(decision.target_lots <= 9);
                assert!(decision.target_lots.is_multiple_of(3));
            }
        }
    }
    assert_eq!(r.engine(0).unwrap().orders().len(), 1);
    assert_eq!(r.report().markets[0].position_units, 9);
    assert!(r.report().markets[0].risk_rejections > 0);
    r.check_invariants().unwrap();
}

#[test]
fn future_pending_bar_is_rejected_by_runtime_restore() {
    let c = config();
    let mut r = PaperRuntime::new(c.clone()).unwrap();
    r.process(&stream()[0]).unwrap();
    let mut snapshot = serde_json::to_value(r.snapshot().unwrap()).unwrap();
    let bars = &mut snapshot["markets"][0]["strategy"]["registered"]["state"]["bars"];
    bars["last_timestamp_ns"] = serde_json::json!(101);
    bars["current"]["start_ns"] = serde_json::json!(100);
    assert!(PaperRuntime::restore(c, serde_json::from_value(snapshot).unwrap()).is_err());
}
