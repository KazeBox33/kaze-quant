#!/usr/bin/env python3
"""一笔小额虚拟买入，抑制 ACK 且不读取原订阅；补查漏收成交，禁止失败重发。"""
import argparse
import json
import subprocess
import time
import uuid
from pathlib import Path
from market_data import sha_file
from testnet_acceptance import make_intent, rules, units, check_balances


def run(binary, directory, symbol):
    directory.mkdir(parents=True, exist_ok=False)
    journal = directory / 'execution.db'
    steps = []

    def command(action, *args):
        argv = [str(binary), 'market', symbol] if action == 'market' else [str(binary), str(journal), action, *map(str, args)]
        start = time.monotonic()
        p = subprocess.run(argv, capture_output=True, text=True, timeout=180)
        (directory / f'{len(steps):02d}-{action}.json').write_text(p.stdout)
        steps.append(dict(action=action, exit_code=p.returncode, duration_seconds=time.monotonic() - start))
        (directory / 'steps.json').write_text(json.dumps(steps, indent=2) + '\n')
        print(f'{action}: exit={p.returncode}', flush=True)
        if p.returncode:
            raise RuntimeError(f'{action} failed; query/cancel original intent, never automatically resend: {p.stderr.strip()}')
        return json.loads(p.stdout)

    before = command('ledger-init', symbol)
    market = command('market')
    r = rules(market, symbol)
    intent = make_intent(market, symbol, 'kaze-gap-' + uuid.uuid4().hex[:16], 'Buy')
    path = directory / 'intent.json'
    path.write_text(json.dumps(intent) + '\n')
    recovered = command('gap-submit', path, '20')
    pilot = recovered['private_stream_pilot']
    mon = pilot['recovery_monitor']
    if not (pilot['accepted_ack_suppressed'] and pilot['unknown_before_recovery'] and
            pilot['private_receive_calls_before_disconnect'] == 0 and mon['success'] and
            mon['recovered_new_trades'] > 0 and recovered['pilot_transport']['submit_calls'] == 1):
        raise ValueError('missing consumer gap/recovery proof')
    if recovered['orders'][0]['observation']['status'] != 'FILLED':
        raise ValueError('small virtual order did not fully fill; stop and query/cancel original order')
    duplicate = command('submit', path, '20')
    if duplicate['pilot_transport']['submit_calls'] != 0:
        raise ValueError('same identity sent twice')
    after = command('reconcile')
    fresh = command('audit')
    balance = check_balances(before['account'], after['account'], fresh['trades'], r['base'], r['quote'])
    if fresh['problems'] or not balance['matched'] or fresh['orders'] != after['orders'] or fresh['trades'] != after['trades']:
        raise ValueError('economic audit/fresh process mismatch')
    fee = {}
    for t in fresh['trades']:
        fee[t['commissionAsset']] = fee.get(t['commissionAsset'], 0) + units(t['commission'])
    summary = dict(schema_version=1, environment='binance-spot-testnet', binary_sha256=sha_file(binary), success=True,
                   steps=steps, submitted_orders=1, filled_orders=1, unique_trades=len(fresh['trades']),
                   fees_units_1e8=fee, all_asset_deltas_match=True, economic_deltas=balance['economic_deltas'],
                   fresh_process_equal=True, same_intent_new_posts=duplicate['pilot_transport']['submit_calls'],
                   pilot=pilot, epochs=fresh['external_ledger']['private_stream']['epochs'],
                   history=fresh['external_ledger']['history_recovery'],
                   scope='one bounded virtual order; application ACK suppression and intentionally unread private subscription; actual missed trade REST recovery, not physical packet loss or fill-after-disconnect timing proof; no mainnet/alpha/24h claim')
    (directory / 'public-summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps({k: summary[k] for k in ['success', 'unique_trades', 'all_asset_deltas_match', 'same_intent_new_posts']}))


if __name__ == '__main__':
    p = argparse.ArgumentParser()
    p.add_argument('--binary', type=Path, required=True)
    p.add_argument('--directory', type=Path, required=True)
    p.add_argument('--symbol', default='BTCUSDT')
    args = p.parse_args()
    run(args.binary.resolve(), args.directory, args.symbol)
