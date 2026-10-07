#!/usr/bin/env python3
"""同一冻结二进制交替运行有/无净目标约束，报告完整持久负载成本。"""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess


def run(binary, output, repeats):
    output.mkdir(parents=True, exist_ok=False)
    rows, expected = [], None
    for i in range(repeats):
        for mode in (['gross', 'target'] if i % 2 == 0 else ['target', 'gross']):
            r = subprocess.run([str(binary), mode, str(output / f'{i}-{mode}')], capture_output=True, text=True, check=True, timeout=90)
            data = json.loads(r.stdout)
            (output / f'{i}-{mode}.json').write_text(json.dumps(data, indent=2) + '\n')
            proof = {k: v for k, v in data.items() if k not in ('elapsed_ns', 'initialization_ns', 'mode', 'net_target')}
            proof['audit'] = dict(proof['audit'])
            proof['audit'].pop('net_target', None)
            if expected is not None and proof != expected:
                raise ValueError('net policy changed underlying gross plan/ledger/fee evidence')
            expected = proof
            if not data['success'] or data['post_calls'] != 3 or data['ticks'] != 1004 or data['audit']['problems']:
                raise ValueError('incomplete workload')
            if mode == 'target' and (data['net_target']['phase'] != 'satisfied' or data['net_target']['current_net_quantity'] != '0.09990000'):
                raise ValueError('target feedback incorrect')
            rows.append(dict(round=i, mode=mode, elapsed_ns=data['elapsed_ns'], initialization_ns=data['initialization_ns']))
            print(mode, i, data['elapsed_ns'], flush=True)
    med = {mode: statistics.median(r['elapsed_ns'] for r in rows if r['mode'] == mode) for mode in ('gross', 'target')}
    init = {mode: statistics.median(r['initialization_ns'] for r in rows if r['mode'] == mode) for mode in ('gross', 'target')}
    summary = dict(schema_version=1, success=True, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(), repeats_each=repeats,
                   ordering='gross/target even, target/gross odd; fresh database each run', median_ns=med, initialization_median_ns=init,
                   target_over_gross_time=med['target'] / med['gross'], runs=rows, same_underlying_execution_evidence=True,
                   evidence_sha256=hashlib.sha256(json.dumps(expected, sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
                   scope='same binary, 1004 full fixture REST + SQLite WAL/FULL ticks including process reopen, original base fee and three children; net policy cost not prior-version speedup or upstream/network/alpha proof')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/examples/net_execution_demo'))
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=7)
    a = p.parse_args()
    if not 1 <= a.repeats <= 20:
        p.error('repeats must be 1..20')
    run(a.binary.resolve(), a.output, a.repeats)
