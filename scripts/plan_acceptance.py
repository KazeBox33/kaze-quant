#!/usr/bin/env python3
"""一个固定测试网父计划、三片小额虚拟买入；首片丢ACK后新进程恢复，禁止失败重发。"""
import argparse
import json
from pathlib import Path
import subprocess
import time
import uuid
from market_data import sha_file
from testnet_acceptance import rules, make_intent, decimal, units, check_balances


def run(binary, output, symbol):
    output.mkdir(parents=True, exist_ok=False)
    db = output / 'execution.db'
    steps = []

    def call(action, *args):
        command = [str(binary), 'market', symbol] if action == 'market' else [str(binary), str(db), action, *map(str, args)]
        start = time.monotonic_ns()
        r = subprocess.run(command, capture_output=True, text=True, timeout=120)
        (output / f'{len(steps):02}-{action}.json').write_text(r.stdout)
        steps.append(dict(action=action, exit_code=r.returncode, elapsed_ns=time.monotonic_ns() - start))
        (output / 'steps.json').write_text(json.dumps(steps, indent=2) + '\n')
        print(f'{action} exit={r.returncode}', flush=True)
        if r.returncode:
            raise RuntimeError(f'{action} failed; stop and query/pause original plan; no automatic resend: {r.stderr.strip()}')
        return json.loads(r.stdout)

    before = call('ledger-init', symbol)
    snapshot = call('market')
    r = rules(snapshot, symbol)
    i = make_intent(snapshot, symbol, 'kaze-probe', 'Buy')
    child = units(i['quantity'])
    price = units(i['price'])
    if price * child * 3 > 60 * 100_000_000 ** 2:
        raise ValueError('pilot parent exceeds 60 USDT')
    cfg = dict(version=1, plan_id='kaze-twap-' + uuid.uuid4().hex[:12], symbol=symbol, side='Buy',
               limit_price=i['price'], total_quantity=decimal(child * 3), quantity_step=decimal(r['step']),
               min_child_quantity=decimal(child), max_child_quantity=decimal(child), slices=3,
               interval_ms=10000, max_working_ms=30000, max_reconciliation_ms=60000, max_children=3)
    path = output / 'plan.json'
    path.write_text(json.dumps(cfg, indent=2) + '\n')
    call('plan-init', path)
    first = call('plan-tick-drop-ack')
    if first['pilot_transport']['submit_calls'] != 1 or first['execution_plan']['phase'] != 'submit_unknown':
        raise ValueError('first child not left durably unknown')
    # 独立新进程按历史查询恢复原子单，不用 plan-init 或任何新client ID绕开未知状态。
    recovered = call('reconcile')
    if units(recovered['execution_plan']['executed_gross_quantity']) != child:
        call('plan-pause')
        call('plan-tick')
        raise ValueError('first small virtual child did not fully fill; original plan paused')
    tick_posts = 1
    for index in (2, 3):
        target = first['execution_plan']['start_ms'] + (index - 1) * cfg['interval_ms']
        remaining = target / 1000 - time.time()
        if remaining > 0:
            time.sleep(min(remaining + 0.05, 11))
        result = call('plan-tick')
        posts = result['pilot_transport']['submit_calls']
        tick_posts += posts
        if posts != 1:
            call('plan-pause')
            raise ValueError('expected one new child at declared release, stop')
        recovered = call('reconcile')
        if units(recovered['execution_plan']['executed_gross_quantity']) != child * index:
            call('plan-pause')
            call('plan-tick')
            raise ValueError('child did not fully fill; plan paused, no new child')
    repeated = call('plan-tick')
    after = call('reconcile')
    fresh = call('audit')
    if repeated['pilot_transport']['submit_calls'] or fresh['execution_plan']['phase'] != 'needs_reconciliation':
        raise ValueError('fresh open must require reconciliation and repeat must not POST')
    balance = check_balances(before['account'], after['account'], fresh['trades'], r['base'], r['quote'])
    if tick_posts != 3 or after['execution_plan']['phase'] != 'completed' or fresh['problems'] or not balance['matched']:
        raise ValueError('parent completion/original asset audit failed')
    if fresh['orders'] != after['orders'] or fresh['trades'] != after['trades']:
        raise ValueError('fresh evidence differs')
    summary = dict(schema_version=1, success=True, environment='binance-spot-testnet-virtual',
                   binary_sha256=sha_file(binary), config=cfg, steps=steps, parent_plans=1, submit_calls=tick_posts,
                   child_orders=len(fresh['orders']), unique_trades=len(fresh['trades']),
                   first_ack_suppressed=True, first_child_recovered_in_new_process=True, repeated_completed_plan_posts=0,
                   completed_status=after['execution_plan'], fresh_open_requires_reconciliation=True,
                   original_asset_deltas_match=True, economic_deltas=balance['economic_deltas'],
                   original_currency_fees=[dict(asset=t['commissionAsset'], amount=t['commission']) for t in fresh['trades']],
                   scope='one bounded fixed-limit three-child gross quantity TWAP; virtual testnet; application ACK suppression, no physical packet fault/actual partial-fill/continuous target signal/mainnet/alpha/24h proof')
    (output / 'public-summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(dict(success=True, child_orders=len(fresh['orders']), submit_calls=tick_posts, all_asset_deltas_match=True)))


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--symbol', default='BTCUSDT')
    a = p.parse_args()
    run(a.binary.resolve(), a.output, a.symbol)
