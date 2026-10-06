//! 第一堂课：只观察一张买单的冻结、两次成交和账户变化。
use kaze_quant::engine::{Engine, EngineConfig};
use kaze_quant::types::*;

fn quote(sequence: u64, bid: u64, ask: u64, ask_quantity: u64) -> Quote {
    Quote {
        sequence,
        timestamp_ns: sequence * 100,
        bid: Price::new(bid).unwrap(),
        ask: Price::new(ask).unwrap(),
        bid_quantity: 10,
        ask_quantity,
    }
}
fn show(label: &str, engine: &Engine) {
    let a = engine.account();
    println!(
        "{label}: cash={} reserved={} available={} position={} fees={}",
        a.cash(),
        a.reserved_cash(),
        a.available_cash(),
        a.position(),
        a.fees_paid()
    );
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = Engine::new(EngineConfig {
        initial_cash: 10_000,
        ..EngineConfig::default()
    })?;
    let mut events = Vec::new();
    engine.on_quote(quote(1, 999, 1000, 10), &mut events)?;
    let request = OrderRequest {
        side: Side::Buy,
        limit: Price::new(1000)?,
        quantity: Quantity::new(3)?,
        time_in_force: TimeInForce::GoodTilCancelled,
    };
    let id = engine
        .submit(request, &mut events)
        .map_err(|r| format!("rejected: {r:?}"))?;
    show("提交后", &engine);
    engine.on_quote(quote(2, 989, 990, 1), &mut events)?;
    show("第一次成交后", &engine);
    engine.on_quote(quote(3, 994, 995, 2), &mut events)?;
    show("第二次成交后", &engine);
    assert_eq!(engine.account().cash(), 7017);
    assert_eq!(engine.account().position(), 3);
    assert_eq!(engine.order(id).unwrap().status, OrderStatus::Filled);
    engine.check_invariants()?;
    for event in events {
        println!("{event:?}");
    }
    Ok(())
}
