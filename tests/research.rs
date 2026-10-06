use kaze_quant::config::{PaperConfig, StrategyConfig};
use kaze_quant::research::*;
use kaze_quant::types::{Price, Quantity, Quote};
fn config() -> PaperConfig {
    let mut c: PaperConfig = serde_json::from_str(include_str!("../configs/paper.json")).unwrap();
    c.markets.truncate(1);
    c.markets[0].price_tick = 1;
    c.markets[0].risk.max_quote_age_ns = 1000000000;
    c
}
fn quotes() -> Vec<Quote> {
    (1..=120)
        .map(|i| Quote {
            sequence: i,
            timestamp_ns: i * 1000,
            bid: Price::new(100 + (i % 20) * 5).unwrap(),
            ask: Price::new(101 + (i % 20) * 5).unwrap(),
            bid_quantity: 100,
            ask_quantity: 100,
        })
        .collect()
}
fn cost() -> CostScenario {
    CostScenario {
        name: "base".into(),
        fee_bps: 10,
        latency_ns: 0,
        slippage_bps: 0,
        liquidity_model: kaze_quant::engine::LiquidityModel::QuoteRefresh,
    }
}
fn fold() -> Fold {
    Fold {
        train_start_ns: 1000,
        train_end_ns: 60000,
        test_start_ns: 61000,
        test_end_ns: 120000,
    }
}
fn strategy() -> StrategyConfig {
    StrategyConfig::SmaCross {
        fast: 2,
        slow: 5,
        band_bps: 0,
        quantity: Quantity::new(1).unwrap(),
    }
}
#[test]
fn boundary_is_strict_and_training_never_reads_holdout() {
    let f = fold();
    f.validate().unwrap();
    let mut invalid = f.clone();
    invalid.test_start_ns = f.train_end_ns;
    assert!(invalid.validate().is_err());
    let input = quotes();
    let a = evaluate(
        &config(),
        &strategy(),
        &f,
        false,
        &cost(),
        input.iter().copied().map(Ok),
    )
    .unwrap();
    let mut changed = input.clone();
    for q in &mut changed[60..] {
        q.bid = Price::new(1).unwrap();
        q.ask = Price::new(2).unwrap();
    }
    let b = evaluate(
        &config(),
        &strategy(),
        &f,
        false,
        &cost(),
        changed.into_iter().map(Ok),
    )
    .unwrap();
    assert_eq!(a.state, b.state);
    assert_eq!(a.quote_count, 60);
}
#[test]
fn holdout_accounts_start_flat_and_fee_penalty_is_explicit() {
    let r = evaluate(
        &config(),
        &strategy(),
        &fold(),
        true,
        &cost(),
        quotes().into_iter().map(Ok),
    )
    .unwrap();
    assert_eq!(r.quote_count, 60);
    assert_eq!(r.first_ns, Some(61000));
    assert_eq!(
        r.liquidation_adjusted_pnl_minor,
        r.state.pnl_minor - r.exit_fee_estimate_minor
    );
    assert!(r.state.metrics.fills > 0);
}
#[test]
fn strategy_state_survives_restore_at_every_split() {
    use kaze_quant::paper::{Command, Envelope, PaperRuntime};
    let mut c = config();
    c.markets[0].strategy = strategy();
    let mut whole = PaperRuntime::new(c.clone()).unwrap();
    let mut resumed = PaperRuntime::new(c.clone()).unwrap();
    for q in quotes() {
        let e = Envelope {
            seq: q.sequence,
            command: Command::Quote {
                market: 0,
                quote: q,
            },
        };
        assert_eq!(whole.process(&e).unwrap(), resumed.process(&e).unwrap());
        resumed = PaperRuntime::restore(c.clone(), resumed.snapshot().unwrap()).unwrap();
    }
    assert_eq!(whole.report(), resumed.report());
}
#[test]
fn selection_is_training_only_and_ties_keep_declared_order() {
    let a = evaluate(
        &config(),
        &strategy(),
        &fold(),
        false,
        &cost(),
        quotes().into_iter().map(Ok),
    )
    .unwrap();
    assert_eq!(choose_training(&[a.clone(), a]).unwrap(), 0);
    assert!(choose_training(&[]).is_err());
}
#[test]
fn malformed_tail_is_not_hidden_by_fold_end() {
    let mut input: Vec<_> = quotes().into_iter().map(Ok).collect();
    input.push(Ok(Quote {
        sequence: 1,
        timestamp_ns: 1,
        bid: Price::new(1).unwrap(),
        ask: Price::new(1).unwrap(),
        bid_quantity: 0,
        ask_quantity: 0,
    }));
    assert!(evaluate(&config(), &strategy(), &fold(), false, &cost(), input).is_err());
}
