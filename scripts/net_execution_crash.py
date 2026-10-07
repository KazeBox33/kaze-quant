#!/usr/bin/env python3
"""继承真实进程强杀边界，并核对目标/父/子原子关系及恢复版本。"""
import argparse
import hashlib
import json
from pathlib import Path
import sqlite3
from plan_crash import run


def check(binary, output):
    run(binary, output)
    with sqlite3.connect(f'file:{(output / "external.db").resolve()}?mode=ro', uri=True) as c:
        body, checksum = c.execute('SELECT body,checksum FROM net_target WHERE singleton=1').fetchone()
        if hashlib.sha256(b'kaze-net-mandate-v1' + body.encode()).hexdigest() != checksum:
            raise ValueError('mandate checksum changed')
        if c.execute('SELECT count(*) FROM execution_plan').fetchone()[0] != 1:
            raise ValueError('missing parent')
        revision = c.execute('SELECT revision FROM execution_meta').fetchone()[0]
        if revision != 'binance-testnet-execution-v3-target':
            raise ValueError('target version lost')
    summary = json.loads((output / 'summary.json').read_text())
    if summary['recovered']['net_target']['phase'] != 'submit_unknown':
        raise ValueError('unknown target unexpectedly ready')
    summary.update(durable_net_mandates=1, mandate_checksum=checksum, revision=revision)
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/examples/net_execution_demo'))
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    check(a.binary.resolve(), a.output)
