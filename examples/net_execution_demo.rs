//! 固定手续费的完整持久化对照；target/gross共用成交负载，但只有target强制净目标。
#[path = "../tests/support/plan_fixture.rs"]
mod support;
use kaze_quant::{
    execution::ExecutionJournal, external_target::TargetRequest, recovery::HistoryHead,
};
use std::{fs, path::PathBuf, time::Instant};
use support::{fixture::*, *};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("net_execution_demo target|gross|budget|residual|before-post|recover-before-post DIRECTORY".into());
    }
    let mode = args[0].as_str();
    if ![
        "target",
        "gross",
        "budget",
        "residual",
        "before-post",
        "recover-before-post",
    ]
    .contains(&mode)
    {
        return Err("unknown mode".into());
    }
    let dir = PathBuf::from(&args[1]);
    fs::create_dir_all(&dir)?;
    let db = dir.join("external.db");
    if mode != "recover-before-post" && db.exists() {
        return Err("fresh workload requires absent database".into());
    }
    let mut j = ExecutionJournal::open(&db)?;
    let mut v = Sim::new();
    v.base_fee = true;
    if mode == "recover-before-post" {
        let result = j.tick_plan(&mut v, 1100);
        let status = j.plan_status()?;
        let net = j.net_target_status()?;
        if result.is_ok() || v.posts != 0 || net["phase"] != "submit_unknown" {
            return Err("prepared target child was resent".into());
        }
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({"success":true,"post_calls":v.posts,"status":status,"net_target":net})
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
    let init_start = Instant::now();
    if mode == "gross" {
        j.reconcile(&mut v)?;
        let mut c = config();
        // 两种模式使用相同的规范十进制表示，完整诊断逐字段对照不掩盖格式差异。
        c.total_quantity = "0.10000000".into();
        c.max_child_quantity = "0.04000000".into();
        j.init_plan(c)?;
    } else {
        let mut r: TargetRequest =
            serde_json::from_str(include_str!("../configs/net-target-preview.json"))?;
        r.plan_id = config().plan_id;
        if mode == "budget" {
            r.base_fee_reserve = "0.00001".into();
        }
        if mode == "residual" {
            r.tolerance_quantity = "0".into();
        }
        j.init_net_target(&mut v, r)?;
    }
    let initialization_ns = init_start.elapsed().as_nanos();
    if mode == "before-post" {
        v.block_before_post = true;
        j.tick_plan(&mut v, 1000)?;
        return Err("unreachable send permit".into());
    }
    let mut results = vec![];
    let start = Instant::now();
    v.drop_ack = true;
    results.push(j.tick_plan(&mut v, 1000)?);
    drop(j);
    j = ExecutionJournal::open(&db)?;
    v.drop_ack = false;
    for t in [1000, 1100, 1200] {
        results.push(j.tick_plan(&mut v, t)?);
    }
    for t in 1201..2201 {
        j.tick_plan(&mut v, t)?;
    }
    let elapsed_ns = start.elapsed().as_nanos();
    let status = j.plan_status()?;
    let audit = j.audit()?;
    if v.posts != if mode == "budget" { 1 } else { 3 }
        || !audit["problems"]
            .as_array()
            .ok_or("audit format")?
            .is_empty()
    {
        return Err("mock target workload failed".into());
    }
    let net = if mode == "gross" {
        serde_json::Value::Null
    } else {
        j.net_target_status()?
    };
    let expected = match mode {
        "budget" => "budget_or_bound_violation",
        "residual" => "completed_with_residual",
        _ => "satisfied",
    };
    if mode != "gross" && net["phase"] != expected {
        return Err("unexpected net target phase".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"success":true,"mode":mode,"initialization_ns":initialization_ns,"elapsed_ns":elapsed_ns,"ticks":1004,"post_calls":v.posts,"fixture_source_operations":v.f.calls,"status":status,"first_steps":results,"audit":audit,"net_target":net,"scope":"full bound recovery + SQLite WAL/FULL, original base fee 0.1%, one reopen after suppressed ACK and 1000 finished ticks; fixture not network latency or alpha"})
        )?
    );
    Ok(())
}
