//! 两条完整本地恢复路径：模拟场所不测真实网络/签名/服务器延迟。
#[path = "../tests/support/recovery_fixture.rs"]
mod fixture;
use fixture::*;
use kaze_quant::{execution::*, recovery::*};
use std::time::Instant;
fn economic(j: &ExecutionJournal) -> serde_json::Value {
    let a = j.audit().unwrap();
    assert!(a["problems"].as_array().unwrap().is_empty());
    serde_json::json!({"orders":a["orders"],"trades":a["trades"],"account":a["account"],"problems":a["problems"],"expected_assets":j.expected_balances().unwrap(),"movement_replay_equal":a["external_ledger"]["movement_replay_equal"]})
}
fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}
fn main() {
    let mut rows = vec![];
    for n in [200, 2000] {
        let olddir = Scratch::new();
        let newdir = Scratch::new();
        let mut old = olddir.open();
        old.bind_account(&identity(), &account(0), &[binding()])
            .unwrap();
        preload(&olddir, &old, n);
        let mut new = bound(&newdir);
        preload(&newdir, &new, n);
        let mut ov = Fixture::new(n);
        let mut nv = Fixture::new(n);
        old.reconcile(&mut ov).unwrap();
        new.recover_history(&mut nv, RecoveryOptions::default())
            .unwrap();
        assert_eq!(economic(&old), economic(&new));
        let mut old_ms = vec![];
        let mut new_ms = vec![];
        let mut old_calls = vec![];
        let mut new_calls = vec![];
        for round in 0..7 {
            let mut legacy = || {
                ov.calls = 0;
                let t = Instant::now();
                old.reconcile(&mut ov).unwrap();
                old_ms.push(t.elapsed().as_secs_f64() * 1000.);
                old_calls.push(ov.calls);
            };
            let mut cursor = || {
                nv.calls = 0;
                let t = Instant::now();
                let stats = new
                    .recover_history(&mut nv, RecoveryOptions::default())
                    .unwrap();
                new_ms.push(t.elapsed().as_secs_f64() * 1000.);
                assert_eq!(stats.rest_operations(), nv.calls);
                new_calls.push(nv.calls);
            };
            if round % 2 == 0 {
                legacy();
                cursor();
            } else {
                cursor();
                legacy();
            }
            assert_eq!(economic(&old), economic(&new));
        }
        let a = median(&mut old_ms.clone());
        let b = median(&mut new_ms.clone());
        rows.push(serde_json::json!({"orders":n,"trades":n,"symbols":1,"rounds":7,"legacy_ms":old_ms,"cursor_ms":new_ms,"legacy_median_ms":a,"cursor_median_ms":b,"median_speedup":a/b,"legacy_rest_source_operations":old_calls,"cursor_rest_source_operations":new_calls,"economic_equality_every_round":true}));
    }
    println!("{}",serde_json::to_string_pretty(&serde_json::json!({"schema_version":1,"profile":"release thin LTO, codegen-units=1","scope":"self comparison, complete local recovery including SQLite WAL/FULL and full order/trade/asset audits; indexed mock venue, synthetic terminal history preload outside timing; no transport, signing clock GET, upstream platform or latency percentiles; cursor path also has stronger failure atomicity","cases":rows})).unwrap());
}
