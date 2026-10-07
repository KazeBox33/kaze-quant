#!/usr/bin/env python3
"""固定vn.py BestLimit方法与Kaze完整纸面路径在共同无成交域逐下单/撤单意图对照。"""
import argparse
import ast
from enum import Enum
import hashlib
import json
from pathlib import Path
import platform
from random import uniform
import statistics
import struct
import subprocess
import sys
import time
from types import SimpleNamespace

REVISION = 'bee959dc464749f7cce66e766249ccdbb2d4869a'
EXPECTED_SOURCE_SHA256 = 'b1ee536daed9c662aaa35337ea50d1c28e91102953783939e8bd30789f0b70c3'


def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def run(binary, source, output, repeats, quotes):
    if sha(source) != EXPECTED_SOURCE_SHA256:
        raise ValueError("requires reviewed exact source revision/hash")
    output.mkdir(parents=True, exist_ok=False)
    config = json.loads(Path('configs/best-limit-demo.json').read_text())
    market = config['markets'][0]
    market['engine'].update(max_orders=256, max_active_orders=128)
    plan = market['strategy']['plan']
    plan.update(max_working_ns=86_400_000_000_000, max_child_lots=5)
    plan['sizing']['lots'] = 10
    config['max_commands'] = max(100000, quotes)
    config_path = output / 'config.json'
    config_path.write_text(json.dumps(config, indent=2) + '\n')
    input_path = output / 'quotes.csv'
    with input_path.open('w') as f:
        f.write('sequence,timestamp_ns,bid,ask,bid_quantity,ask_quantity\n')
        for seq in range(1, quotes + 1):
            bid = 100 + (seq // 2) % 2
            f.write(f'{seq},{seq},{bid},{bid+1},0,0\n')

    class Direction(Enum):
        LONG = 'long'
        SHORT = 'short'
    class Offset(Enum):
        OPEN = 'open'

    class AlgoTemplate:
        def __init__(self, engine, name, symbol, direction, offset, price, volume, setting):
            self.direction = direction
            self.offset = offset
            self.price = price
            self.volume = volume
            self.traded = 0.0
            self.next_id = 1
            self.active_id = None
            self.sequence = 0
            self.digest = hashlib.sha256()
            self.actions = 0
        def put_event(self):
            pass
        def finish(self):
            raise ValueError('unexpected finish in no-fill fixture')
        def write_log(self, message):
            pass
        def buy(self, price, volume, offset):
            if self.active_id is not None or volume != 5:
                raise ValueError('duplicate/unbounded child')
            order_id = self.next_id
            self.next_id += 1
            self.active_id = str(order_id)
            self.digest.update(struct.pack('<BQQBQQ', 1, self.sequence, order_id, 1, int(price), int(volume)))
            self.actions += 1
            return self.active_id
        def sell(self, price, volume, offset):
            raise ValueError('fixture is long entry only')
        def cancel_all(self):
            if self.active_id is None:
                raise ValueError('cancel without active order')
            self.digest.update(struct.pack('<BQQBQQ', 2, self.sequence, int(self.active_id), 0, 0, 0))
            self.actions += 1
            self.active_id = None
            self.on_order(SimpleNamespace(is_active=lambda: False))

    tree = ast.parse(source.read_text())
    cls = next(c for c in tree.body if isinstance(c, ast.ClassDef) and c.name == 'BestLimitAlgo')
    namespace = dict(AlgoTemplate=AlgoTemplate, Direction=Direction, Offset=Offset, uniform=uniform,
                     AlgoEngine=SimpleNamespace, TickData=SimpleNamespace, OrderData=SimpleNamespace, TradeData=SimpleNamespace)
    exec(compile(ast.Module(body=[cls], type_ignores=[]), str(source), 'exec'), namespace)
    runs = []
    expected = None
    for round_id in range(repeats):
        for version in (['kaze', 'vnpy-method'] if round_id % 2 == 0 else ['vnpy-method', 'kaze']):
            if version == 'kaze':
                result = json.loads(subprocess.run([str(binary), str(config_path), str(input_path)],
                                    capture_output=True, text=True, check=True, timeout=60).stdout)
                m = result['state']['markets'][0]
                if m['metrics']['fills'] or m['metrics']['rejected'] or m['cash_minor'] != '10000':
                    raise ValueError('unexpected economic event')
            else:
                algo = namespace['BestLimitAlgo'](None, 'BENCH', 'BENCH', Direction.LONG, Offset.OPEN,
                                                 120.0, 10.0, dict(min_volume=5, max_volume=5))
                start = time.perf_counter_ns()
                for seq in range(1, quotes + 1):
                    bid = float(100 + (seq // 2) % 2)
                    algo.sequence = seq
                    algo.on_tick(SimpleNamespace(bid_price_1=bid, ask_price_1=bid + 1))
                result = dict(elapsed_ns=time.perf_counter_ns() - start, actions=algo.actions,
                              trace_sha256=algo.digest.hexdigest(), next_id=algo.next_id, active_id=algo.active_id)
            fingerprint = (result['actions'], result['trace_sha256'])
            if expected is not None and expected != fingerprint:
                raise ValueError('original accepted-submit/confirmed-cancel trace differs')
            expected = fingerprint
            result.update(round=round_id, version=version)
            runs.append(result)
            print(version, round_id, result['elapsed_ns'], result['trace_sha256'], flush=True)
    med = {v: statistics.median(r['elapsed_ns'] for r in runs if r['version'] == v) for v in ('kaze', 'vnpy-method')}
    summary = dict(success=True, quotes=quotes, repeats_each=repeats, median_ns=med, trace_sha256=expected[1], actions=expected[0],
                   all_original_intent_traces_equal=True, binary_sha256=sha(binary), source_sha256=sha(source), revision=REVISION,
                   config_sha256=sha(config_path), input_sha256=sha(input_path), platform=platform.platform(), python=sys.version,
                   source_url=f'https://github.com/vnpy/vnpy_algotrading/blob/{REVISION}/vnpy_algotrading/algos/best_limit_algo.py', runs=runs,
                   scope='source exact AST BestLimit class, stub event publication/no gateway or account, deterministic min=max5, immediate terminal cancel feedback, zero liquidity/no fills; Kaze CSV+full paper risk/account/core/retention+same intent digest; timing costs differ; no full framework, realistic maker queue, Iceberg speed, external route or platform superiority comparison')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/examples/algo_reference'))
    p.add_argument('--source', type=Path, required=True, help='reviewed exact pinned BestLimit source')
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--repeats', type=int, default=7)
    p.add_argument('--quotes', type=int, default=100000)
    a = p.parse_args()
    if not 1 <= a.repeats <= 20 or not 1 <= a.quotes <= 1000000:
        p.error('repeats 1..20, quotes 1..1000000')
    run(a.binary.resolve(), a.source.resolve(), a.output, a.repeats, a.quotes)
