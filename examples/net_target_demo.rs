//! 固定手算资产/费用案例，预览零POST，再显式驱动mock父计划。
#[path = "../tests/support/plan_fixture.rs"]
mod support;
use kaze_quant::{
    execution::ExecutionJournal, external_target::TargetRequest, recovery::HistoryHead,
};
use std::{fs, path::PathBuf};
use support::{fixture::*, *};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("fresh output directory required")?,
    );
    fs::create_dir(&dir)?;
    let template: TargetRequest =
        serde_json::from_str(include_str!("../configs/net-target-preview.json"))?;
    let mut cases = vec![];
    for name in [
        "buy_base_fee",
        "sell_base_fee",
        "hold",
        "resource_block",
        "grid_dust",
    ] {
        let db = dir.join(format!("{name}.db"));
        let mut j = ExecutionJournal::open(&db)?;
        let mut v = Sim::new();
        let mut request = template.clone();
        match name {
            "sell_base_fee" => {
                v.f.account.balances[0].free = "0.10".into();
                request.target_quantity = "0".into();
                request.tolerance_quantity = "0.01".into();
            }
            "hold" => request.target_quantity = "0.00005".into(),
            "resource_block" => v.f.account.balances[1].free = "0.01".into(),
            "grid_dust" => request.target_quantity = "0.02".into(),
            _ => {}
        }
        j.bind_account_with_history(
            &identity(),
            &v.f.account,
            &[binding()],
            &[HistoryHead {
                symbol: "BTCUSDT".into(),
                order_id: None,
                trade_id: None,
            }],
        )?;
        j.reconcile(&mut v)?;
        let preview = j.preview_target(&request)?;
        if v.posts != 0 || !j.orders()?.is_empty() {
            return Err("preview submitted/wrote an intent".into());
        }
        let mut ticks = vec![];
        if !preview["plan"].is_null() {
            j.init_plan(serde_json::from_value(preview["plan"].clone())?)?;
            v.base_fee = true;
            for t in [1000, 1100, 1200] {
                ticks.push(j.tick_plan(&mut v, t)?);
                drop(j);
                j = ExecutionJournal::open(&db)?;
            }
            j.reconcile(&mut v)?;
        }
        let balances = j.expected_balances()?;
        let audit = j.audit()?;
        if audit["problems"] != serde_json::json!([]) {
            return Err("mock ledger audit failed".into());
        }
        cases.push(serde_json::json!({"case":name,"preview":preview,"preview_posts":0,"explicit_mock_execution_posts":v.posts,"ticks":ticks,"final_balances":balances,"audit":audit}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"schema_version":1,"success":true,"cases":cases,"scope":"deterministic no-network net-target fixture; explicit caller starts mock plan; previews never authorize/send; no live net target strategy/mainnet/alpha proof"})
        )?
    );
    Ok(())
}
