#!/usr/bin/env python3
"""条件选择组件对照：固定上游源码空触发路径、同容器Rust扫描/索引，不是平台排名。"""
import argparse
import ast
from enum import Enum
import hashlib
import json
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import time
from types import SimpleNamespace

UPSTREAM_REVISION = '7a8768de9784dda35a7b261a7ade1dbfbff50919'
EXPECTED_ENGINE_SHA256 = 'f4d70334efd0a8c748e9ebd02c8a0273c57ec38a85c54cd86a1d2132e4835d56'


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def upstream_method(source):
    if sha(source) != EXPECTED_ENGINE_SHA256:
        raise ValueError("requires reviewed exact source revision/hash")
    tree = ast.parse(source.read_text())
    cls = next(c for c in tree.body if isinstance(c, ast.ClassDef) and c.name == 'CtaEngine')
    method = next(f for f in cls.body if isinstance(f, ast.FunctionDef) and f.name == 'check_stop_order')
    class Direction(Enum):
        LONG = 'long'
        SHORT = 'short'
    namespace = dict(TickData=SimpleNamespace, Direction=Direction)
    exec(compile(ast.Module(body=[method], type_ignores=[]), str(source), 'exec'), namespace)
    return namespace['check_stop_order'], Direction, ast.get_source_segment(source.read_text(), method)


def upstream_run(method, direction, n, loops):
    # 稀疏分支无合约/网关/成交调用；一旦源代码意外进入触发分支，缺失mock属性会失败。
    orders = {i+1: SimpleNamespace(vt_symbol='BENCH', direction=direction.LONG if i % 2 == 0 else direction.SHORT,
                                  price=float((110 if i % 2 == 0 else 90) + i % 10)) for i in range(n)}
    context = SimpleNamespace(stop_orders=orders)
    tick = SimpleNamespace(vt_symbol='BENCH', last_price=100.0)
    start = time.perf_counter_ns()
    for _ in range(loops):
        method(context, tick)
    elapsed = time.perf_counter_ns() - start
    if len(orders) != n:
        raise ValueError('unexpected trigger')
    return dict(policy='vnpy-source-sparse', mode='sparse', orders=n, loops=loops, elapsed_ns=elapsed,
                selected=0, checksum=0, remaining=n)


def run(binary, source, output, repeats):
    output.mkdir(parents=True, exist_ok=False)
    method, direction, text = upstream_method(source)
    (output / 'check_stop_order.py.txt').write_text(text + '\n')
    results = []
    medians = []
    for n in (128, 1024, 4096):
        for mode, loops in (('sparse', 10000), ('dense', 2000)):
            policies = ['scan', 'indexed', 'adaptive'] + (['vnpy-source-sparse'] if mode == 'sparse' else [])
            for round_id in range(repeats):
                for policy in (policies if round_id % 2 == 0 else list(reversed(policies))):
                    if policy.startswith('vnpy'):
                        result = upstream_run(method, direction, n, loops)
                    else:
                        result = json.loads(subprocess.run([str(binary), policy, str(n), mode, str(loops)],
                                            capture_output=True, text=True, check=True, timeout=60).stdout)
                    expected = n * loops if mode == 'dense' else 0
                    checksum = loops * n * (n + 1) // 2 if mode == 'dense' else 0
                    if result['selected'] != expected or result['checksum'] != checksum:
                        raise ValueError('selection oracle mismatch')
                    result['round'] = round_id
                    results.append(result)
                    print(n, mode, policy, round_id, result['elapsed_ns'], flush=True)
            med = {p: statistics.median(r['elapsed_ns'] for r in results if r['orders'] == n and r['mode'] == mode and r['policy'] == p) for p in policies}
            medians.append(dict(orders=n, mode=mode, loops=loops, median_ns=med,
                                indexed_over_scan_time=med['indexed'] / med['scan'], adaptive_over_scan_time=med['adaptive'] / med['scan']))
    summary = dict(success=True, platform=platform.platform(), machine=platform.machine(), python=sys.version,
                   binary_sha256=sha(binary), source_sha256=sha(source), source_revision=UPSTREAM_REVISION,
                   source_url=f'https://github.com/vnpy/vnpy_ctastrategy/blob/{UPSTREAM_REVISION}/vnpy_ctastrategy/engine.py',
                   repeats_each=repeats, medians=medians, runs=results,
                   scope='single-symbol condition selection only; identical integer-exact thresholds/current reference value; upstream exact AST method sparse branch with inert dictionary/tick; no framework import, event loop, gateway, account, expiry, OCO, persistence or fills; Rust scan/index share indexed container; dense upstream unmeasured; not whole-platform/individual event SLA or economic proof')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('reports/binaries/conditional-v15'))
    p.add_argument('--source', type=Path, required=True, help='reviewed engine.py from pinned upstream revision')
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=7)
    a = p.parse_args()
    if not 1 <= a.repeats <= 20:
        p.error('repeats 1..20')
    run(a.binary.resolve(), a.source.resolve(), a.output, a.repeats)
