#!/usr/bin/env python3
"""旧/新冻结二进制交替完整mock计划；比较所有账户/交易/计划诊断，计时含FULL持久化。"""
import argparse
import hashlib
import json
from pathlib import Path
import statistics
import subprocess


def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def run(old, new, output, repeats):
    output.mkdir(parents=True, exist_ok=False)
    expected = None
    rows = []
    for i in range(repeats):
        for name, binary in ([('old', old), ('new', new)] if i % 2 == 0 else [('new', new), ('old', old)]):
            directory = output / f'{i}-{name}'
            r = subprocess.run([str(binary), 'demo', str(directory)], capture_output=True, text=True, check=True, timeout=90)
            data = json.loads(r.stdout)
            (output / f'{i}-{name}.json').write_text(json.dumps(data, indent=2) + '\n')
            proof = {k: v for k, v in data.items() if k != 'elapsed_ns'}
            if expected is not None and proof != expected:
                raise ValueError('version/round changed mock receipts or complete ledger/plan evidence')
            expected = proof
            if not data['success'] or data['post_calls'] != 3 or data['ticks'] != 1004 or data['audit']['problems']:
                raise ValueError('incomplete mock plan')
            rows.append(dict(round=i, version=name, elapsed_ns=data['elapsed_ns'], ticks=data['ticks'],
                             mock_posts=data['post_calls'], fixture_source_operations=data['fixture_source_operations']))
            print(name, i, data['elapsed_ns'], flush=True)
    med = {name: statistics.median(r['elapsed_ns'] for r in rows if r['version'] == name) for name in ('old', 'new')}
    summary = dict(schema_version=1, success=True, old_binary_sha256=sha(old), new_binary_sha256=sha(new),
                   repeats_each=repeats, ordering='old/new even, new/old odd; fresh database every run',
                   median_ns=med, new_over_old_time=med['new'] / med['old'], runs=rows,
                   evidence_sha256=hashlib.sha256(json.dumps(expected, sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
                   same_complete_mock_evidence=True,
                   scope='1004 full REST fixture+SQLite WAL/FULL plan ticks, three mock children, suppressed ACK then reopen; own versions, no real network/HFT/upstream/24h proof')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--old', type=Path, required=True)
    p.add_argument('--new', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=7)
    a = p.parse_args()
    if not 1 <= a.repeats <= 20:
        p.error('repeats must be 1..20')
    run(a.old.resolve(), a.new.resolve(), a.output, a.repeats)
