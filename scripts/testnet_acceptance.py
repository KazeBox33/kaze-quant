#!/usr/bin/env python3
"""真实测试网有界验收；每步启动新进程，所有金额为整数，禁止未知提交重发。"""
import argparse
import datetime
import json
import subprocess
import time
import uuid
from pathlib import Path
from decimal import Decimal
from market_data import sha_file

SCALE = 100_000_000
CAP = 20 * SCALE


def units(value):
    parsed = Decimal(value) * SCALE
    if not parsed.is_finite() or parsed != parsed.to_integral_value() or parsed < 0:
        raise ValueError('invalid exact nonnegative decimal')
    return int(parsed)


def decimal(value):
    sign = '-' if value < 0 else ''
    value = abs(value)
    return f'{sign}{value // SCALE}.{value % SCALE:08d}'


def grid(value, step, up=False):
    if step <= 0 or value <= 0:
        raise ValueError('invalid grid')
    return ((value + step - 1) // step if up else value // step) * step


def rules(snapshot, symbol):
    if snapshot['environment'] != 'binance-spot-testnet':
        raise ValueError('testnet required')
    markets = snapshot['exchange_info']['symbols']
    if len(markets) != 1:
        raise ValueError('ambiguous market')
    m = markets[0]
    if m['symbol'] != symbol or m['quoteAsset'] != 'USDT' or m['status'] != 'TRADING':
        raise ValueError('invalid market identity')
    f = {r['filterType']: r for r in m['filters']}
    p, q = f['PRICE_FILTER'], f['LOT_SIZE']
    n = f.get('NOTIONAL', f.get('MIN_NOTIONAL'))
    if n is None:
        raise ValueError('notional filter absent')
    return dict(base=m['baseAsset'], quote=m['quoteAsset'], tick=units(p['tickSize']),
                step=units(q['stepSize']), min_qty=units(q['minQty']), max_qty=units(q['maxQty']),
                min_price=units(p['minPrice']), max_price=units(p['maxPrice']),
                min_notional=units(n['minNotional']), max_notional=units(n.get('maxNotional', '0')))


def make_intent(snapshot, symbol, client_id, side, passive=False, quantity=None):
    r = rules(snapshot, symbol)
    book = snapshot['book']
    if book['symbol'] != symbol:
        raise ValueError('book identity mismatch')
    bid, ask = units(book['bidPrice']), units(book['askPrice'])
    if not 0 < bid <= ask:
        raise ValueError('invalid spread')
    # 被动买单留1%距离；穿价单只给0.1%限价缓冲，仍受交易所动态过滤。
    price = grid(bid * 9900 // 10000 if passive else
                 ((ask * 10010 + 9999) // 10000 if side == 'Buy' else bid * 9990 // 10000),
                 r['tick'], up=(not passive and side == 'Buy'))
    qty = grid(15 * SCALE * SCALE // price if quantity is None else quantity, r['step'])
    notional = price * qty
    if (price < r['min_price'] or (r['max_price'] and price > r['max_price']) or
            not r['min_qty'] <= qty <= r['max_qty'] or
            notional < r['min_notional'] * SCALE or notional > CAP * SCALE or
            (r['max_notional'] and notional > r['max_notional'] * SCALE)):
        raise ValueError('intent outside current filters / 20 USDT cap')
    return dict(client_order_id=client_id, symbol=symbol, side=side, price=decimal(price), quantity=decimal(qty))


def balance_totals(account):
    result = {}
    for b in account['balances']:
        if b['asset'] in result:
            raise ValueError('duplicate asset')
        result[b['asset']] = units(b['free']) + units(b['locked'])
    return result


def check_balances(before, after, trades, base, quote):
    expected = {}
    for t in trades:
        if t['symbol'] != base + quote:
            raise ValueError('unexpected trade symbol')
        sign = 1 if t['isBuyer'] else -1
        expected[base] = expected.get(base, 0) + sign * units(t['qty'])
        expected[quote] = expected.get(quote, 0) - sign * units(t['quoteQty'])
        asset = t['commissionAsset']
        expected[asset] = expected.get(asset, 0) - units(t['commission'])
    a, b = balance_totals(before), balance_totals(after)
    assets = a.keys() | b.keys() | expected.keys()
    mismatches = {s: dict(actual=decimal(b.get(s, 0) - a.get(s, 0)), expected=decimal(expected.get(s, 0)))
                  for s in sorted(assets) if b.get(s, 0) - a.get(s, 0) != expected.get(s, 0)}
    return dict(matched=not mismatches, mismatches=mismatches,
                economic_deltas={s: decimal(v) for s, v in sorted(expected.items())})


class Pilot:
    def __init__(self, binary, directory, symbol):
        self.binary = binary.resolve()
        self.directory = directory
        self.symbol = symbol
        self.db = directory / 'execution.db'
        self.steps = []
        self.index = 0
        self.saw_partial = False
        self.prefix = 'kaze-' + uuid.uuid4().hex[:16]

    def command(self, action, *args, expected=0):
        self.index += 1
        started = time.monotonic()
        cmd = [str(self.binary), 'market', self.symbol] if action == 'market' else [str(self.binary), str(self.db), action, *map(str, args)]
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=180)
        record = dict(action=action, exit_code=result.returncode, duration_seconds=time.monotonic() - started)
        if result.stdout:
            audit = json.loads(result.stdout)
            self.saw_partial |= any((o.get('observation') or {}).get('status') == 'PARTIALLY_FILLED' for o in audit.get('orders', []))
            (self.directory / f'{self.index:03d}-{action}.json').write_text(json.dumps(audit, indent=2) + '\n')
            record['result_file'] = f'{self.index:03d}-{action}.json'
        else:
            audit = None
        # Rust适配器保证错误没有密钥、签名和请求URL；只保留受控错误类别。
        record['error'] = result.stderr.strip()
        self.steps.append(record)
        (self.directory / 'steps.json').write_text(json.dumps(self.steps, indent=2) + '\n')
        print(f'{self.index:03d} {action}: exit={result.returncode}, {record["duration_seconds"]:.2f}s', flush=True)
        if result.returncode != expected or audit is None:
            raise RuntimeError(f'{action} failed; inspect local step evidence, never auto-resubmit')
        return audit

    def submit(self, label, side, passive=False, quantity=None, drop=False):
        snapshot = self.command('market')
        intent = make_intent(snapshot, self.symbol, self.prefix + '-' + label, side, passive, quantity)
        path = self.directory / (label + '-intent.json')
        path.write_text(json.dumps(intent, indent=2) + '\n')
        result = self.command('submit-drop-ack' if drop else 'submit', path, '20', expected=1 if drop else 0)
        if result['pilot_transport']['submit_calls'] != 1:
            raise RuntimeError('submission call count mismatch')
        if drop:
            if not result['pilot_transport']['accepted_ack_discarded']:
                raise RuntimeError('accepted ACK suppression not observed')
            row = next(o for o in result['orders'] if o['intent']['client_order_id'] == intent['client_order_id'])
            if row['phase'] != 'unknown' or row['observation'] is not None:
                raise RuntimeError('fault did not leave durable unknown outcome')
        # 同一身份由新进程再次请求，证明只查询。期间不向外部策略开放提交。
        restored = self.command('submit', path, '20')
        if restored['pilot_transport']['submit_calls'] != 0:
            raise RuntimeError('duplicate external submit detected')
        return intent, snapshot

    def terminal(self, intent, expect_fill=False):
        for _ in range(8):
            a = self.command('reconcile')
            o = next(o for o in a['orders'] if o['intent']['client_order_id'] == intent['client_order_id'])
            if o['phase'] == 'terminal':
                if expect_fill and o['observation']['status'] != 'FILLED':
                    raise RuntimeError('crossing order did not fill completely')
                return a, o
            if not expect_fill:
                self.command('cancel', intent['client_order_id'])
            else:
                time.sleep(1)
        self.command('cancel', intent['client_order_id'])
        raise RuntimeError('fill deadline exceeded; tracked order cancellation attempted')

    def run(self):
        initial = self.command('reconcile')
        if initial['orders'] or initial['problems']:
            raise ValueError('pilot requires new isolated journal')
        for label, drop in [('cancel', False), ('lostack', True)]:
            intent, _ = self.submit(label, 'Buy', passive=True, drop=drop)
            _, row = self.terminal(intent)
            if row['observation']['status'] != 'CANCELED' or units(row['observation']['executedQty']):
                raise RuntimeError('passive order unexpectedly filled; retain evidence and stop')
        buy, snapshot = self.submit('buy', 'Buy')
        a, row = self.terminal(buy, expect_fill=True)
        r = rules(snapshot, self.symbol)
        received = units(row['observation']['executedQty'])
        received -= sum(units(t['commission']) for t in a['trades']
                        if t['orderId'] == row['observation']['orderId'] and t['commissionAsset'] == r['base'])
        sell, _ = self.submit('sell', 'Sell', quantity=received)
        a, _ = self.terminal(sell, expect_fill=True)
        # 再次对账验证trade去重；不同进程读取SQLite，不能只比内存状态。
        repeated = self.command('reconcile')
        recovered = self.command('audit')
        consistent = a['orders'] == repeated['orders'] == recovered['orders'] and a['trades'] == repeated['trades'] == recovered['trades']
        balance = check_balances(initial['account'], recovered['account'], recovered['trades'], r['base'], r['quote'])
        fees = {}
        for t in recovered['trades']:
            s = t['commissionAsset']; fees[s] = fees.get(s, 0) + units(t['commission'])
        report = dict(schema_version=1, environment='binance-spot-testnet', binary_sha256=sha_file(self.binary),
                      completed_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                      baseline_passed=consistent and balance['matched'] and not recovered['problems'],
                      per_order_cap_usdt='20', orders=len(recovered['orders']), trade_count=len(recovered['trades']),
                      recovery_equal=consistent, balance_reconciliation=balance,
                      fees={s: decimal(v) for s, v in fees.items()},
                      accepted_ack_loss_recovered_without_second_submit=True,
                      actual_partial_fill_observed=self.saw_partial,
                      nonzero_fee_observed=any(fees.values()),
                      statuses=[o['observation']['status'] for o in recovered['orders']],
                      limits=['ACK loss is explicit application response suppression, not wire-level disconnect',
                              'No private WebSocket or strategy external-routing acceptance',
                              'No forced partial-fill guarantee; zero fees cannot validate fee economics',
                              'Independent testnet balance delta check assumes no concurrent external account activity'])
        (self.directory / 'summary.json').write_text(json.dumps(report, indent=2) + '\n')
        if not report['baseline_passed']:
            raise RuntimeError('baseline failed; summary retained')
        return report


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/kaze-testnet'))
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--symbol', default='BTCUSDT', choices=['BTCUSDT', 'ETHUSDT'])
    a = p.parse_args()
    a.output.mkdir(parents=True, exist_ok=False)
    (a.output / 'protocol.json').write_text(json.dumps(dict(binary_sha256=sha_file(a.binary), symbol=a.symbol,
       max_new_orders=4, per_order_usdt_cap='20', frozen_utc=datetime.datetime.now(datetime.timezone.utc).isoformat()), indent=2) + '\n')
    pilot = Pilot(a.binary, a.output, a.symbol)
    try:
        print(json.dumps(pilot.run(), indent=2))
    except Exception as error:
        # 只查询/撤销本账本身份，不清理外部订单；未知结果绝不再POST。
        cleanup = []
        try:
            audit = pilot.command('audit')
            for order in audit['orders']:
                if order['phase'] != 'terminal':
                    pilot.command('cancel', order['intent']['client_order_id'])
            pilot.command('reconcile')
        except Exception as cleanup_error:
            cleanup.append(type(cleanup_error).__name__)
        (a.output / 'failed.json').write_text(json.dumps(dict(baseline_passed=False, error=str(error), cleanup_errors=cleanup), indent=2) + '\n')
        raise

if __name__ == '__main__':
    main()
