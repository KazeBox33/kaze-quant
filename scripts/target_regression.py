#!/usr/bin/env python3
"""相同公开前缀、固定组合配置、旧/新可执行文件交替；逐回执比较及各自独立恢复。"""
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
import statistics
import struct
import subprocess
from real_bench import measured


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(old, new, quotes, config, output, repeats):
    output.mkdir(parents=True, exist_ok=False)
    expected = None
    results = []
    for i in range(repeats):
        for name, binary in ([('old', old), ('new', new)] if i % 2 == 0 else [('new', new), ('old', old)]):
            stem = output / f'{i}-{name}'
            db = stem.with_suffix('.db')
            report = stem.with_suffix('.json')
            common = [str(binary), '--config', str(config), '--db', str(db), '--quiet']
            measurement = measured(common + ['--quotes', str(quotes), '--batch-size', '256', '--report', str(report)], stem)
            data = json.loads(report.read_text())
            h = hashlib.sha256()
            with sqlite3.connect(f'file:{db.resolve()}?mode=ro', uri=True) as conn:
                count = 0
                for seq, receipt in conn.execute('SELECT seq,receipt FROM commands ORDER BY seq'):
                    count += 1
                    if count != seq:
                        raise ValueError('sequence gap')
                    h.update(struct.pack('<Q', len(receipt)))
                    h.update(receipt)
            fingerprint = (data['state'], h.hexdigest())
            if expected is not None and expected != fingerprint:
                raise ValueError('version/round changed raw receipt or final state')
            expected = fingerprint
            recovered = output / f'{i}-{name}-audit.json'
            subprocess.run(common + ['--recover-only', '--verify-full', '--report', str(recovered)], capture_output=True, check=True, timeout=180)
            audit = json.loads(recovered.read_text())
            if audit['state'] != data['state'] or audit['audit_chain_sha256'] != data['audit_chain_sha256'] or audit['verified_commands'] != count:
                raise ValueError('same-version full recovery differs')
            results.append(dict(round=i, version=name, **measurement, elapsed_ns=data['run']['elapsed_ns'],
                                run=data['run'], commands=count, receipt_sha256=h.hexdigest(),
                                binary_sha256=data['binary_sha256'], recovery_equal=True, verified_commands=count))
            print(name, i, data['run']['elapsed_ns'], flush=True)
    med = {name: statistics.median(r['elapsed_ns'] for r in results if r['version'] == name) for name in ('old', 'new')}
    summary = dict(schema_version=1, success=True, old_binary_sha256=sha(old), new_binary_sha256=sha(new),
                   input_sha256=sha(quotes), config_file_sha256=sha(config), repeats_each=repeats,
                   ordering='old/new on even rounds, new/old on odd rounds; fresh database every run',
                   median_ns=med, new_over_old_time=med['new'] / med['old'], final_state=expected[0],
                   receipt_sha256=expected[1], runs=results,
                   scope='own supplied old/new full durable paper workload, identical raw receipts; separate binary-bound recovery; no upstream/network/individual quote latency/production SLA comparison')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--old', type=Path, required=True)
    p.add_argument('--new', type=Path, required=True)
    p.add_argument('--quotes', type=Path, required=True)
    p.add_argument('--config', type=Path, default=Path('configs/btc-composition-v1.json'))
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=7)
    a = p.parse_args()
    if not 1 <= a.repeats <= 20:
        p.error('repeats 1..20')
    run(a.old.resolve(), a.new.resolve(), a.quotes.resolve(), a.config.resolve(), a.output, a.repeats)
