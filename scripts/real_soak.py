#!/usr/bin/env python3
"""单会话大规模真实报价持久运行；保留成本、资源、全量审计和恢复报告。"""
import argparse
import json
from pathlib import Path
from real_bench import measured

def run(quotes, output):
    output.mkdir(parents=True, exist_ok=False)
    binary = str(Path("target/release/kaze-run").resolve())
    common = [binary,"--config","configs/btc-mean-reversion.json","--store-options","configs/store-soak.json","--db",str(output/"session.db"),"--quiet"]
    run_report=output/"run.json"
    performance=measured(common+["--quotes",str(quotes),"--finish","--verify-full","--report",str(run_report)],output/"run")
    recovery_report=output/"recovery.json"
    recovery=measured(common+["--recover-only","--verify-full","--report",str(recovery_report)],output/"recovery")
    first,second=json.loads(run_report.read_text()),json.loads(recovery_report.read_text())
    if first["state"]!=second["state"] or first["audit_chain_sha256"]!=second["audit_chain_sha256"]:
        raise ValueError("soak and recovered ledger differ")
    data=dict(run=first,recovery=second,run_measurement=performance,recovery_measurement=recovery,
        state_and_chain_equal=True,limitation="accelerated historical replay, single repetition; no 24h wall-clock or venue SLA claim")
    (output/"summary.json").write_text(json.dumps(data,indent=2)+"\n")
    print(json.dumps(data,indent=2))
if __name__=="__main__":
    p=argparse.ArgumentParser(description=__doc__);p.add_argument("--quotes",type=Path,default=Path("reports/datasets/btc-quotes-10000000.csv"));p.add_argument("--output",type=Path,required=True);a=p.parse_args();run(a.quotes,a.output)
