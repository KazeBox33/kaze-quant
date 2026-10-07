//! 零网络验收和完整mock恢复负载；mock REST调用不等于真实网络延迟。
#[path = "../tests/support/plan_fixture.rs"]
mod support;
use kaze_quant::{execution::ExecutionJournal, external_plan::Phase, recovery::HistoryHead};
use std::{fs, path::PathBuf, time::Instant};
use support::fixture::*;
use support::*;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("external_plan_demo demo|before-post|recover-before-post DIRECTORY".into());
    }
    let dir = PathBuf::from(&args[1]);
    fs::create_dir_all(&dir)?;
    let db = dir.join("external.db");
    let mut j = ExecutionJournal::open(&db)?;
    let mut v = Sim::new();
    if args[0] == "recover-before-post" {
        let result = j.tick_plan(&mut v, 1100);
        let status = j.plan_status()?;
        if result.is_ok() || v.posts != 0 || status.phase != Phase::SubmitUnknown {
            return Err("prepared identity was resent".into());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"success":true,"post_calls":v.posts,"status":status,"scope":"mock unsent durable child; query not-found is not resend permission"})
            )?
        );
        return Ok(());
    }
    j.bind_account_with_history(
        &identity(),
        &account(0),
        &[binding()],
        &[HistoryHead {
            symbol: "BTCUSDT".into(),
            order_id: None,
            trade_id: None,
        }],
    )?;
    j.reconcile(&mut v)?;
    j.init_plan(config())?;
    if args[0] == "before-post" {
        v.block_before_post = true;
        j.tick_plan(&mut v, 1000)?;
        return Err("unreachable send permit".into());
    }
    if args[0] != "demo" {
        return Err("unknown mode".into());
    }
    let mut results = vec![];
    let start = Instant::now();
    v.drop_ack = true;
    results.push(j.tick_plan(&mut v, 1000)?);
    drop(j);
    j = ExecutionJournal::open(&db)?;
    v.drop_ack = false;
    results.push(j.tick_plan(&mut v, 1000)?);
    results.push(j.tick_plan(&mut v, 1100)?);
    results.push(j.tick_plan(&mut v, 1200)?);
    for t in 1201..2201 {
        j.tick_plan(&mut v, t)?;
    }
    let elapsed_ns = start.elapsed().as_nanos();
    let status = j.plan_status()?;
    if status.phase != Phase::Completed || v.posts != 3 {
        return Err("mock plan did not complete".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"success":true,"elapsed_ns":elapsed_ns,"ticks":1004,"post_calls":v.posts,"fixture_source_operations":v.f.calls,"status":status,"first_steps":results,"audit":j.audit()?,"scope":"full bound recovery + plan ticks + SQLite WAL/FULL, 3 mock filled children and 1000 completed ticks; process reopen after suppressed ACK; no real network/upstream comparison"})
        )?
    );
    Ok(())
}
