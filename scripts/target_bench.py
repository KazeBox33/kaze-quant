#!/usr/bin/env python3
"""冻结组合配置的公开报价工程负载；同内核逐回执对照，不是上游或盈利排名。"""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import sqlite3
import statistics
import struct
import subprocess
from real_bench import measured

EXPECTED = '4725608a261cbff4a2879bef7d7cff7984eb1f70248ba4e627b56ab0eeeafd31'


def sha(path):
    h = hashlib.sha256()
    with path.open('rb') as f:
        for b in iter(lambda: f.read(1024 * 1024), b''):
            h.update(b)
    return h.hexdigest()


def run(output, quotes, config, binary, reference, memory_repeats, durable_repeats):
    if sha(quotes) != EXPECTED:
        raise ValueError('requires declared one-million quote prefix; rerun market_data.py')
    output.mkdir(parents=True, exist_ok=False)
    prefix = output / 'first-100000.csv'
    with quotes.open('rb') as source, prefix.open('wb') as dest:
        dest.write(source.readline())
        for _ in range(100000):
            line = source.readline()
            if not line:
                raise ValueError('short prefix')
            dest.write(line)
    memory = []
    baseline = None
    for i in range(memory_repeats):
        result = subprocess.run([str(reference), str(config), str(quotes)], capture_output=True, text=True, check=True, timeout=180)
        data = json.loads(result.stdout)
        fingerprint = (data['state'], data['receipt_sha256'])
        if baseline is not None and baseline != fingerprint:
            raise ValueError('memory repeat differs')
        baseline = fingerprint
        memory.append(data)
        print(f'memory run={i} ns={data["elapsed_ns"]}', flush=True)
    prefix_result = json.loads(subprocess.run([str(reference), str(config), str(prefix)], capture_output=True, text=True, check=True, timeout=180).stdout)
    durable = []
    expected = None
    for i in range(durable_repeats):
        stem = output / f'durable-{i}'
        db = stem.with_suffix('.db')
        report = stem.with_suffix('.json')
        common = [str(binary), '--config', str(config), '--db', str(db), '--quiet']
        measure = measured(common + ['--quotes', str(prefix), '--batch-size', '256', '--report', str(report)], stem)
        data = json.loads(report.read_text())
        recovered = output / f'audit-{i}.json'
        audit_measure = measured(common + ['--recover-only', '--verify-full', '--report', str(recovered)], output / f'audit-{i}')
        audit = json.loads(recovered.read_text())
        h = hashlib.sha256()
        count = 0
        with sqlite3.connect(f'file:{db.resolve()}?mode=ro', uri=True) as conn:
            for seq, receipt in conn.execute('SELECT seq, receipt FROM commands ORDER BY seq'):
                count += 1
                if seq != count:
                    raise ValueError('command gap')
                h.update(struct.pack('<Q', len(receipt)))
                h.update(receipt)
        if count != 100000 or h.hexdigest() != prefix_result['receipt_sha256'] or data['state'] != prefix_result['state']:
            raise ValueError('durable/shared-core original receipts or state differ')
        if audit['verified_commands'] != count or audit['state'] != data['state'] or audit['audit_chain_sha256'] != data['audit_chain_sha256']:
            raise ValueError('fresh full audit differs')
        fingerprint = (data['state'], data['audit_chain_sha256'])
        if expected is not None and expected != fingerprint:
            raise ValueError('durable repeat differs')
        expected = fingerprint
        durable.append(dict(run=i, **measure, run_stats=data['run'], audit_measurement=audit_measure,
                            recovery_ns=audit['recovery_ns'], verified_commands=count, storage_stats=data['storage_stats'],
                            binary_sha256=data['binary_sha256'], config_sha256=data['config_sha256'], input_sha256=data['input_sha256'],
                            audit_chain_sha256=data['audit_chain_sha256'], receipt_sha256=h.hexdigest(),
                            original_receipts_match_memory=True, state=data['state']))
        print(f'durable run={i} ns={data["run"]["elapsed_ns"]} rss={measure["sample_peak_rss_bytes"]}', flush=True)
    mem_ns = [r['elapsed_ns'] for r in memory]
    dur_ns = [r['run_stats']['elapsed_ns'] for r in durable]
    summary = dict(schema_version=1, success=True, platform=platform.platform(), machine=platform.machine(),
                   binary_sha256=sha(binary), reference_binary_sha256=sha(reference), config_file_sha256=sha(config),
                   million_input_sha256=EXPECTED, durable_prefix_sha256=sha(prefix),
                   selection='fixed first 100000 contiguous quotes; config frozen before result; no price/PnL filtering',
                   memory_runs=memory, durable_runs=durable,
                   memory_median_ns=statistics.median(mem_ns), memory_quotes_per_second=1000000 * 1e9 / statistics.median(mem_ns),
                   durable_median_ns=statistics.median(dur_ns), durable_quotes_per_second=100000 * 1e9 / statistics.median(dur_ns),
                   scope='million CSV+shared-core+serialized decision receipt hash; 100k SQLite WAL/FULL+bounded 256 batch+checkpoint; sampled RSS, same-kernel reference not independent economic oracle; futures quotes in spot-style paper model; no alpha/upstream/full-platform superiority/24h claim')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--quotes', type=Path, default=Path('reports/datasets/btc-quotes-1000000.csv'))
    p.add_argument('--config', type=Path, default=Path('configs/btc-composition-v1.json'))
    p.add_argument('--binary', type=Path, default=Path('target/release/kaze-run'))
    p.add_argument('--reference', type=Path, default=Path('target/release/examples/target_reference'))
    p.add_argument('--memory-repeats', type=int, default=7)
    p.add_argument('--durable-repeats', type=int, default=3)
    a = p.parse_args()
    if not 1 <= a.memory_repeats <= 20 or not 1 <= a.durable_repeats <= 20:
        p.error('repeats must be in 1..20')
    run(a.output, a.quotes.resolve(), a.config.resolve(), a.binary.resolve(), a.reference.resolve(), a.memory_repeats, a.durable_repeats)
