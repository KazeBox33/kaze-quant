use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::paper::*;
use kaze_quant::types::*;

fn config() -> PaperConfig {
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
    for m in &mut c.markets {
        m.strategy = StrategyConfig::Passive {};
    }
    c
}
fn quote(market: usize, sequence: u64, timestamp_ns: u64, bid: u64, ask: u64, qty: u64) -> Command {
    Command::Quote {
        market,
        quote: Quote {
            sequence,
            timestamp_ns,
            bid: Price::new(bid).unwrap(),
            ask: Price::new(ask).unwrap(),
            bid_quantity: qty,
            ask_quantity: qty,
        },
    }
}
fn buy(quantity: u64, limit: u64) -> Command {
    Command::Submit {
        market: 0,
        request: OrderRequest {
            side: Side::Buy,
            quantity: Quantity::new(quantity).unwrap(),
            limit: Price::new(limit).unwrap(),
            time_in_force: TimeInForce::GoodTilCancelled,
        },
    }
}
fn run(r: &mut PaperRuntime, c: Command) -> Receipt {
    r.process(&Envelope {
        seq: r.processed() + 1,
        command: c,
    })
    .unwrap()
}
fn rejected(receipt: &Receipt, expected: RiskReject) {
    assert!(
        receipt
            .notices
            .iter()
            .any(|n| matches!(n, Notice::RiskRejected { reason, .. } if *reason == expected)),
        "{receipt:?}"
    );
}

#[test]
fn budgets_routing_and_fees_follow_hand_ledger() {
    let mut r = PaperRuntime::new(config()).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 2));
    run(&mut r, buy(3, 100));
    run(&mut r, quote(1, 1, 1, 200, 205, 2));
    assert_eq!(r.engine(0).unwrap().account().position(), 0);
    run(&mut r, quote(0, 2, 2, 99, 100, 2));
    assert_eq!(r.engine(0).unwrap().account().cash(), 999_799);
    assert_eq!(r.engine(0).unwrap().account().position(), 2);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 101);
    assert_eq!(r.engine(1).unwrap().account().cash(), 500_000);
    run(&mut r, Command::Finish {});
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 0);
    r.check_invariants().unwrap();
}
#[test]
fn duplicate_or_gap_commands_are_atomic_in_memory() {
    let mut r = PaperRuntime::new(config()).unwrap();
    let before = r.report();
    for seq in [0, 2, 100] {
        assert!(
            r.process(&Envelope {
                seq,
                command: Command::Halt {}
            })
            .is_err()
        );
        assert_eq!(r.report(), before);
    }
}
#[test]
fn bad_quote_and_unknown_market_do_not_advance_clock_or_state() {
    let mut r = PaperRuntime::new(config()).unwrap();
    run(&mut r, quote(0, 1, 10, 99, 100, 2));
    let before = r.report();
    for command in [
        quote(0, 1, 11, 99, 100, 2),
        quote(0, 2, 9, 99, 100, 2),
        quote(1, 1, 11, 99, 101, 2),
        quote(64, 1, 11, 99, 100, 2),
        Command::Advance { timestamp_ns: 9 },
    ] {
        assert!(r.process(&Envelope { seq: 2, command }).is_err());
        assert_eq!(r.report(), before);
    }
}
#[test]
fn order_notional_price_collar_and_grid_are_checked() {
    let mut c = config();
    c.markets[0].risk.max_order_notional = 300;
    c.markets[0].quantity_step = 2;
    let mut r = PaperRuntime::new(c).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 2));
    rejected(&run(&mut r, buy(1, 100)), RiskReject::Grid);
    rejected(&run(&mut r, buy(4, 100)), RiskReject::Notional);
    rejected(&run(&mut r, buy(2, 111)), RiskReject::PriceCollar);
    assert_eq!(r.engine(0).unwrap().orders().len(), 0);
    run(&mut r, buy(2, 110)); // 恰好 10% 可接受。
    assert_eq!(r.engine(0).unwrap().active_count(), 1);
}
#[test]
fn no_market_is_a_business_rejection() {
    let mut r = PaperRuntime::new(config()).unwrap();
    rejected(&run(&mut r, buy(1, 100)), RiskReject::NoMarket);
    assert_eq!(r.processed(), 1);
}
#[test]
fn quote_gap_cancels_before_new_quote_can_fill_resting_order() {
    let mut c = config();
    c.markets[0].risk.max_quote_age_ns = 10;
    let mut r = PaperRuntime::new(c).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 2));
    run(&mut r, buy(2, 100));
    run(&mut r, quote(0, 2, 12, 99, 100, 2));
    assert_eq!(r.halted(0), Some(HaltReason::StaleFeed));
    assert_eq!(r.engine(0).unwrap().account().position(), 0);
    assert_eq!(r.engine(0).unwrap().account().reserved_cash(), 0);
    rejected(&run(&mut r, buy(1, 100)), RiskReject::Halted);
}
#[test]
fn watchdog_boundary_and_cross_asset_timeout_are_deterministic() {
    let mut c = config();
    c.markets[0].risk.max_quote_age_ns = 10;
    let mut r = PaperRuntime::new(c).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 2));
    run(&mut r, buy(1, 100));
    run(&mut r, Command::Advance { timestamp_ns: 11 });
    assert_eq!(r.halted(0), None);
    run(&mut r, quote(1, 1, 12, 200, 205, 2));
    assert_eq!(r.halted(0), Some(HaltReason::StaleFeed));
    assert_eq!(r.halted(1), None);
}
#[test]
fn drawdown_trip_preserves_filled_position_and_releases_rest() {
    let mut c = config();
    c.markets[0].risk.max_drawdown = 2;
    let mut r = PaperRuntime::new(c).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 1));
    run(&mut r, buy(2, 100));
    run(&mut r, quote(0, 2, 2, 99, 100, 1)); // 花100+1费，bid估值99，回撤2。
    assert_eq!(r.halted(0), Some(HaltReason::Drawdown));
    let e = r.engine(0).unwrap();
    assert_eq!(e.account().position(), 1);
    assert_eq!(e.account().cash(), 999899);
    assert_eq!(e.account().reserved_cash(), 0);
    assert_eq!(e.orders()[0].status, OrderStatus::Cancelled);
    run(&mut r, quote(0, 3, 3, 90, 91, 1));
    assert_eq!(r.engine(0).unwrap().equity(), 999989);
    r.check_invariants().unwrap();
}
#[test]
fn history_capacity_rejects_without_consuming_ids_or_engine_metrics() {
    let mut c = config();
    c.markets[0].engine.max_orders = 1;
    c.markets[0].engine.max_active_orders = 1;
    let mut r = PaperRuntime::new(c).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 1));
    run(&mut r, buy(1, 100));
    run(
        &mut r,
        Command::Cancel {
            market: 0,
            order_id: OrderId(1),
        },
    );
    rejected(&run(&mut r, buy(1, 100)), RiskReject::HistoryCapacity);
    assert_eq!(r.engine(0).unwrap().metrics().rejected, 0);
    assert_eq!(r.report().markets[0].risk_rejections, 1);
}
#[test]
fn finish_seals_session_and_operator_halt_is_irreversible() {
    let mut r = PaperRuntime::new(config()).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 1));
    run(&mut r, buy(1, 100));
    run(&mut r, Command::Halt {});
    rejected(&run(&mut r, buy(1, 100)), RiskReject::Halted);
    run(&mut r, Command::Finish {});
    let before = r.report();
    assert!(
        r.process(&Envelope {
            seq: r.processed() + 1,
            command: Command::Halt {}
        })
        .is_err()
    );
    assert_eq!(r.report(), before);
}
#[test]
fn config_and_wire_types_fail_on_invalid_bounds_and_typos() {
    assert!(serde_json::from_str::<Price>("0").is_err());
    assert!(serde_json::from_str::<Quantity>("0").is_err());
    let good = serde_json::to_value(config()).unwrap();
    for bad in ["schema_version", "quantity_step", "max_quote_age_ns"] {
        let mut v = good.clone();
        if bad == "schema_version" {
            v[bad] = 0.into();
        } else if bad == "quantity_step" {
            v["markets"][0][bad] = 0.into();
        } else {
            v["markets"][0]["risk"][bad] = 0.into();
        }
        assert!(
            serde_json::from_value::<PaperConfig>(v)
                .unwrap()
                .validate()
                .is_err()
        );
    }
    let mut v = good.clone();
    v["markets"][0]["engine"]["initial_cash"] = 1000000.into();
    assert!(serde_json::from_value::<PaperConfig>(v).is_err());
    let mut v = good;
    v["typo"] = true.into();
    assert!(serde_json::from_value::<PaperConfig>(v).is_err());
    let mut c = config();
    c.markets[1].symbol = c.markets[0].symbol.clone();
    assert!(c.validate().is_err());
}
#[test]
fn mean_reversion_warms_window_and_executes_on_next_quote() {
    let mut c = config();
    c.markets.truncate(1);
    c.markets[0].strategy = StrategyConfig::MeanReversion {
        window: 3,
        entry_bps: 100,
        exit_bps: 0,
        quantity: Quantity::new(2).unwrap(),
    };
    let mut r = PaperRuntime::new(c).unwrap();
    for (sequence, bid) in [(1, 100), (2, 100), (3, 90)] {
        run(&mut r, quote(0, sequence, sequence, bid, bid + 1, 2));
    }
    assert_eq!(r.engine(0).unwrap().account().position(), 0);
    assert_eq!(r.engine(0).unwrap().active_count(), 1);
    run(&mut r, quote(0, 4, 4, 90, 91, 2));
    assert_eq!(r.engine(0).unwrap().account().position(), 2);
    run(&mut r, quote(0, 5, 5, 100, 101, 2));
    run(&mut r, quote(0, 6, 6, 100, 101, 2));
    assert_eq!(r.engine(0).unwrap().account().position(), 0);
    r.check_invariants().unwrap();
}

#[test]
fn custom_batch_strategy_shares_liquidity_and_receives_execution_callbacks() {
    use kaze_quant::strategy::{ActionBuffer, Strategy, StrategyView};
    use std::{cell::Cell, rc::Rc};
    struct SplitBuyer {
        fired: bool,
        filled: Rc<Cell<u64>>,
    }
    impl Strategy for SplitBuyer {
        fn on_quote_batch(&mut self, v: StrategyView<'_>, actions: &mut ActionBuffer) {
            if self.fired {
                return;
            }
            self.fired = true;
            for _ in 0..2 {
                actions
                    .push(Action::Submit(OrderRequest {
                        side: Side::Buy,
                        limit: v.quote.ask,
                        quantity: Quantity::new(2).unwrap(),
                        time_in_force: TimeInForce::ImmediateOrCancel,
                    }))
                    .unwrap();
            }
        }
        fn on_event(&mut self, event: Event) {
            if let Event::Fill(f) = event {
                self.filled.set(self.filled.get() + f.quantity);
            }
        }
    }
    let filled = Rc::new(Cell::new(0));
    let mut c = config();
    c.markets.truncate(1);
    let mut r = PaperRuntime::with_strategies(
        c,
        vec![Box::new(SplitBuyer {
            fired: false,
            filled: filled.clone(),
        })],
    )
    .unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 3));
    assert_eq!(r.engine(0).unwrap().active_count(), 2);
    run(&mut r, quote(0, 2, 2, 99, 100, 3));
    assert_eq!(filled.get(), 3);
    assert_eq!(r.engine(0).unwrap().account().position(), 3);
    assert_eq!(r.engine(0).unwrap().active_count(), 0);
    r.check_invariants().unwrap();
}
#[test]
fn action_overflow_halts_before_applying_any_signal_even_if_strategy_ignores_error() {
    use kaze_quant::strategy::{ActionBuffer, Strategy, StrategyView};
    struct Bad;
    impl Strategy for Bad {
        fn on_quote_batch(&mut self, v: StrategyView<'_>, actions: &mut ActionBuffer) {
            for _ in 0..65 {
                let _ = actions.push(Action::Submit(OrderRequest {
                    side: Side::Buy,
                    limit: v.quote.ask,
                    quantity: Quantity::new(1).unwrap(),
                    time_in_force: TimeInForce::GoodTilCancelled,
                }));
            }
        }
    }
    let mut c = config();
    c.markets.truncate(1);
    let mut r = PaperRuntime::with_strategies(c, vec![Box::new(Bad)]).unwrap();
    run(&mut r, quote(0, 1, 1, 99, 100, 100));
    assert_eq!(r.halted(0), Some(HaltReason::StrategyCapacity));
    assert!(r.engine(0).unwrap().orders().is_empty());
}
#[test]
fn passive_config_rejects_unknown_variant_fields() {
    assert!(
        serde_json::from_str::<StrategyConfig>("{\"type\":\"passive\",\"typo\":true}").is_err()
    );
}
