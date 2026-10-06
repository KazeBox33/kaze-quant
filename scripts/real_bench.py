#!/usr/bin/env python3
"""测量真实报价端到端持久回放、进程 RSS、检查点恢复与全量审计。"""
import argparse
import csv
import json
import platform
from pathlib import Path
import subprocess
import time

def measured(command, stem):
    samples = []
    start = time.perf_counter_ns()
    with stem.with_suffix(".stderr").open("w") as error:
        child = subprocess.Popen(command, stdout=subprocess.DEVNULL, stderr=error)
        while child.poll() is None:
            usage = subprocess.run(["ps", "-o", "rss=", "-p", str(child.pid)], capture_output=True, text=True)
            if usage.returncode == 0 and usage.stdout.strip():
                samples.append((time.perf_counter_ns() - start, int(usage.stdout.strip()) * 1024))
            time.sleep(0.1)
        status = child.wait()
    elapsed = time.perf_counter_ns() - start
    if status:
        raise RuntimeError(stem.with_suffix(".stderr").read_text())
    with stem.with_suffix(".rss.csv").open("w", newline="") as f:
        writer = csv.writer(f); writer.writerow(["elapsed_ns", "rss_bytes"]); writer.writerows(samples)
    return dict(process_wall_ns=elapsed, sample_peak_rss_bytes=max((s[1] for s in samples), default=0),
                rss_samples=len(samples), rss_rule="ps rss KiB * 1024, 100ms sampling; may miss transient peaks")

def run(output, quotes, repeats, modes):
    output.mkdir(parents=True, exist_ok=False)
    binary = str(Path("target/release/kaze-run").resolve())
    result = dict(platform=platform.platform(), machine=platform.machine(), repeats=repeats,
                  normalized_file=str(quotes), runs=[])
    expected = {}
    for mode in modes:
        for i in range(repeats):
            stem = output / f"{mode}-{i}"
            db = stem.with_suffix(".db")
            report = stem.with_suffix(".json")
            common = [binary, "--config", f"configs/btc-{mode}.json", "--db", str(db), "--quiet"]
            measurement = measured(common + ["--quotes", str(quotes), "--batch-size", "256", "--report", str(report)], stem)
            data = json.loads(report.read_text())
            recovered = output / f"{mode}-{i}-recovery.json"
            audit_measurement = measured(common + ["--recover-only", "--verify-full", "--report", str(recovered)], output / f"{mode}-{i}-audit")
            recovery = json.loads(recovered.read_text())
            if recovery["state"] != data["state"] or recovery["audit_chain_sha256"] != data["audit_chain_sha256"]:
                raise ValueError("recovery ledger differs")
            fingerprint = (data["state"], data["audit_chain_sha256"])
            if mode in expected and expected[mode] != fingerprint:
                raise ValueError("repeated run changed state or audit chain")
            expected[mode] = fingerprint
            result["runs"].append(dict(mode=mode, run=i, **measurement,
                run_stats=data["run"], recovery_ns=recovery["recovery_ns"], audit_measurement=audit_measurement,
                verified_commands=recovery["verified_commands"], storage_stats=data["storage_stats"],
                binary_sha256=data["binary_sha256"], config_sha256=data["config_sha256"],
                input_sha256=data["input_sha256"], audit_chain_sha256=data["audit_chain_sha256"], state=data["state"]))
            print(f"{mode} run={i} commands={data['run']['received_commands']} ns={data['run']['elapsed_ns']} rss={measurement['sample_peak_rss_bytes']}", flush=True)
    (output / "summary.json").write_text(json.dumps(result, indent=2)+"\n")

if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--quotes", type=Path, default=Path("reports/datasets/btc-quotes-1000000.csv"))
    p.add_argument("--repeats", type=int, default=3)
    p.add_argument("--modes", nargs="+", choices=["passive", "mean-reversion"], default=["passive", "mean-reversion"])
    a = p.parse_args()
    if not 1 <= a.repeats <= 20: p.error("repeats 1..20")
    run(a.output, a.quotes, a.repeats, a.modes)
