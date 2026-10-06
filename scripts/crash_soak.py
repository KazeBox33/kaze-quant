#!/usr/bin/env python3
"""确定种子强杀并重投；恢复高水位不得低于已观察确认，最终与完整运行一致。"""
import argparse
import json
import random
from pathlib import Path
import subprocess
import threading
import time


def run(output, quotes, cycles):
    output.mkdir(parents=True, exist_ok=False)
    binary = str(Path("target/release/kaze-run").resolve())
    db = output / "crashed.db"
    common = [binary, "--config", "configs/btc-mean-reversion.json", "--db", str(db)]
    known_ack = 0
    results = []
    rng = random.Random(0x4B415A45)
    for i in range(cycles):
        with (output / f"kill-{i}.stderr").open("w") as error:
            child = subprocess.Popen(common + ["--quotes", str(quotes)], stdout=subprocess.PIPE, stderr=error, text=True)
            acknowledgments = []
            def consume():
                for line in child.stdout:
                    acknowledgments.append(json.loads(line)["seq"])
            thread = threading.Thread(target=consume); thread.start()
            wait = rng.uniform(0.015, 0.22)
            time.sleep(wait)
            killed = child.poll() is None
            if killed: child.kill()
            child.wait(); thread.join()
        known_ack = max([known_ack] + acknowledgments)
        report = output / f"recovery-{i}.json"
        recovery = subprocess.run(common + ["--recover-only", "--quiet", "--verify-full", "--report", str(report)], capture_output=True, text=True)
        if recovery.returncode: raise RuntimeError(recovery.stderr)
        data = json.loads(report.read_text())
        if data["state"]["processed"] < known_ack: raise ValueError("lost acknowledged command")
        results.append(dict(cycle=i, kill_requested=killed, wait_seconds=wait, observed_ack=known_ack,
            recovered_commands=data["state"]["processed"], verified_commands=data["verified_commands"],
            recovery_ns=data["recovery_ns"]))
        print(f"cycle={i} acknowledged={known_ack} recovered={data['state']['processed']}", flush=True)
    final = output / "final.json"
    command = common + ["--quotes", str(quotes), "--quiet", "--finish", "--verify-full", "--report", str(final)]
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
    reference = output / "reference.json"
    command = [binary, "--config", "configs/btc-mean-reversion.json", "--db", str(output / "reference.db"), "--quotes", str(quotes), "--quiet", "--finish", "--verify-full", "--report", str(reference)]
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
    a, b = json.loads(final.read_text()), json.loads(reference.read_text())
    if a["state"] != b["state"] or a["audit_chain_sha256"] != b["audit_chain_sha256"]:
        raise ValueError("final replay differs from uninterrupted reference")
    (output / "summary.json").write_text(json.dumps(dict(seed="0x4B415A45",cycles=results,
        final_commands=a["state"]["processed"],final_state=a["state"],audit_chain_sha256=a["audit_chain_sha256"],
        binary_sha256=a["binary_sha256"],config_sha256=a["config_sha256"],final_reference_equal=True,
        limitation="process SIGKILL; no power loss/controller failure or authenticated tamper test"),indent=2)+"\n")

if __name__ == "__main__":
    p=argparse.ArgumentParser(description=__doc__);p.add_argument("--output",type=Path,required=True)
    p.add_argument("--quotes",type=Path,default=Path("reports/datasets/btc-quotes-1000000.csv"));p.add_argument("--cycles",type=int,default=20)
    a=p.parse_args()
    if not 1 <= a.cycles <= 1000: p.error("cycles 1..1000")
    run(a.output,a.quotes,a.cycles)
