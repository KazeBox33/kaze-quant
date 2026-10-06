#!/usr/bin/env python3
"""新账本上的有界 Testnet 私有流验收；失败不重发，原始账户只保留本机。"""
import argparse
import json
import sqlite3
import subprocess
import time
import uuid
from pathlib import Path
from market_data import sha_file
from testnet_acceptance import make_intent, rules, units, grid, balance_totals, check_balances


def run(binary, directory, symbol):
    directory.mkdir(parents=True, exist_ok=False)
    journal = directory / 'execution.db'
    steps = []

    def command(action, *args):
        started = time.monotonic()
        argv = [str(binary), 'market', symbol] if action == 'market' else [str(binary), str(journal), action, *map(str, args)]
        p = subprocess.run(argv, text=True, capture_output=True, timeout=180)
        # 输出包含原始账户，不能直接放进公开证据目录。
        index = len(steps) + 1
        (directory / f'{index:02d}-{action}.json').write_text(p.stdout)
        steps.append(dict(action=action, exit_code=p.returncode, duration_seconds=time.monotonic() - started))
        (directory / 'steps.json').write_text(json.dumps(steps, indent=2) + '\n')
        print(f'{action}: exit={p.returncode}', flush=True)
        if p.returncode:
            raise RuntimeError(f'{action} failed; recover/query, never auto-resubmit; {p.stderr.strip()}')
        return json.loads(p.stdout)

    before = command('ledger-init', symbol)
    command('stream-watch', 5)
    prefix = 'kaze-' + uuid.uuid4().hex[:16]

    def submit(label, side, passive=False, quantity=None):
        snapshot = command('market')
        intent = make_intent(snapshot, symbol, prefix + '-' + label, side, passive, quantity)
        path = directory / (label + '.json')
        path.write_text(json.dumps(intent) + '\n')
        result = command('stream-submit', path, '20', 8)
        if result['pilot_transport']['submit_calls'] != 1:
            raise ValueError('one POST required')
        return result, intent, rules(snapshot, symbol)

    passive, _, _ = submit('passive', 'Buy', True)
    buy, buy_intent, r = submit('buy', 'Buy')
    fills = [t for t in buy['trades'] if t['orderId'] == buy['orders'][-1]['observation']['orderId']]
    qty = sum(units(t['qty']) for t in fills) - sum(units(t['commission']) for t in fills if t['commissionAsset'] == r['base'])
    if not fills or buy['orders'][-1]['observation']['status'] != 'FILLED':
        raise ValueError('buy not fully filled; stop, do not blindly resubmit')
    sell, _, _ = submit('sell', 'Sell', quantity=grid(qty, r['step']))
    recovered = command('reconcile')
    fresh = command('audit')
    ledger = fresh['external_ledger']
    balance_check = check_balances(before['account'], recovered['account'], fresh['trades'], r['base'], r['quote'])
    if (fresh['problems'] or not balance_check['matched'] or
            ledger['expected_total_assets'] != recovered['external_ledger']['expected_total_assets'] or
            fresh['orders'] != recovered['orders'] or fresh['trades'] != recovered['trades']):
        raise ValueError('fresh process/order/trade/asset audit mismatch')
    reports = [passive, buy, sell]
    with sqlite3.connect(journal) as conn:
        private = [json.loads(row[0]) for row in conn.execute('SELECT body FROM private_events')]
    private_fills = [e for e in private if e['trade'] is not None]
    if len(private_fills) < 2 or len(private) < 4:
        raise ValueError('REST success alone does not prove private execution delivery')

    fees = {}
    for t in fresh['trades']:
        if units(t['commission']):
            fees[t['commissionAsset']] = fees.get(t['commissionAsset'], 0) + units(t['commission'])
    summary = dict(schema_version=1, environment='binance-spot-testnet', binary_sha256=sha_file(binary),
                   success=True, steps=steps, submitted_orders=3, filled_orders=sum(o['observation']['status'] == 'FILLED' for o in fresh['orders']),
                   canceled_orders=sum(o['observation']['status'] == 'CANCELED' for o in fresh['orders']),
                   private_pilots=[x['private_stream_pilot'] for x in reports],
                   private_execution_events=ledger['private_execution_events'], unique_trades=len(fresh['trades']),
                   fee_units_1e8=fees, all_asset_deltas_match=balance_check['matched'],
                   fresh_process_equal=True, movement_replay_equal=ledger['movement_replay_equal'],
                   actual_partial_fill_observed=any(e['order']['status'] == 'PARTIALLY_FILLED' for e in private),
                   private_fill_reports=len(private_fills),
                   private_statuses=sorted({e['order']['status'] for e in private}),
                   scope='three bounded virtual orders; no network fault injection, mainnet, production longevity, queue/alpha claim; original accounts remain local')
    (directory / 'public-summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps({k: summary[k] for k in ['success','private_execution_events','unique_trades','all_asset_deltas_match','fresh_process_equal']}))


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--directory', type=Path, required=True)
    p.add_argument('--symbol', default='BTCUSDT')
    a = p.parse_args()
    run(a.binary.resolve(), a.directory, a.symbol)
