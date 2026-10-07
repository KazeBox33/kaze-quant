#!/usr/bin/env python3
"""零网络纸面 TWAP 验收：已确认子单阶段 SIGKILL，重投去重与逐回执核对。"""
import argparse
import hashlib
import json
from pathlib import Path
import select
import signal
import sqlite3
import subprocess


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def rows(path):
    with sqlite3.connect(f'file:{path.resolve()}?mode=ro', uri=True) as db:
        return db.execute('SELECT seq, payload, receipt, chain FROM commands ORDER BY seq').fetchall()


def run(binary, config, inputs, output):
    output.mkdir(parents=True, exist_ok=False)
    commands = inputs.read_bytes().splitlines(keepends=True)
    if len(commands) != 20 or [json.loads(e)['seq'] for e in commands] != list(range(1, 21)):
        raise ValueError('acceptance requires fixed 20-quote teaching trace')

    def common(db):
        return [str(binary), '--config', str(config), '--db', str(db)]

    def finish(db, name):
        report = output / f'{name}.json'
        with (output / f'{name}.stderr').open('w') as err:
            subprocess.run(common(db) + ['--input', str(inputs), '--finish', '--verify-full', '--quiet', '--report', str(report)],
                           stdout=subprocess.DEVNULL, stderr=err, check=True, timeout=60)
        data = json.loads(report.read_text())
        recovered = output / f'{name}-audit.json'
        subprocess.run(common(db) + ['--recover-only', '--verify-full', '--quiet', '--report', str(recovered)],
                       capture_output=True, check=True, timeout=60)
        audit = json.loads(recovered.read_text())
        if data['state'] != audit['state'] or data['audit_chain_sha256'] != audit['audit_chain_sha256'] or audit['verified_commands'] != 21:
            raise ValueError('fresh full audit differs')
        return data

    reference_db = output / 'reference.db'
    reference = finish(reference_db, 'reference')
    expected = rows(reference_db)
    market = reference['state']['markets'][0]
    if (market['position_units'], market['cash_minor'], market['fees_minor']) != (10, '8980', '10'):
        raise ValueError('hand ledger: 10 one-lot fills at 101, one minor fee each')
    # 第一条报价只提交、不成交；后续三子单为 3/3/4，不能用结束仓位掩盖超前成交。
    receipts = [json.loads(r[2]) for r in expected]
    if any(n.get('type') == 'engine' and 'Fill' in n.get('event', {}) for n in receipts[0]['notices']):
        raise ValueError('same-quote fill')
    decisions = [n['decision'] for r in receipts for n in r['notices'] if n['type'] == 'strategy_decision']
    children = [d['action']['Submit']['quantity'] for d in decisions if d['reason'] == 'accepted']
    fills = [n['event']['Fill'] for r in receipts for n in r['notices'] if n['type'] == 'engine' and 'Fill' in n['event']]
    if children != [3, 3, 4] or len(fills) != 10 or any(f['quantity'] != 1 for f in fills):
        raise ValueError('integer release or partial-fill trace differs')
    cases = []
    for prefix in (1, 2, 4, 7, 10):
        db = output / f'kill-{prefix}.db'
        with (output / f'kill-{prefix}.stderr').open('w') as err:
            p = subprocess.Popen(common(db) + ['--batch-size', '1', '--flush-ms', '1'],
                                 stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err)
            try:
                for seq, line in enumerate(commands[:prefix], 1):
                    p.stdin.write(line)
                    p.stdin.flush()
                    if not select.select([p.stdout], [], [], 15)[0]:
                        raise TimeoutError('committed receipt timeout')
                    receipt = json.loads(p.stdout.readline())
                    if receipt['seq'] != seq or receipt['duplicate']:
                        raise ValueError('commit confirmation differs')
                p.send_signal(signal.SIGKILL)
                status = p.wait(timeout=15)
                if status != -signal.SIGKILL:
                    raise ValueError('child did not terminate via SIGKILL')
            finally:
                if p.poll() is None:
                    p.kill()
                    p.wait(timeout=15)
                p.stdin.close()
                p.stdout.close()
        if len(rows(db)) != prefix:
            raise ValueError('confirmed prefix not durable')
        result = finish(db, f'resumed-{prefix}')
        if result['state'] != reference['state'] or rows(db) != expected or result['audit_chain_sha256'] != reference['audit_chain_sha256']:
            raise ValueError('resumed parent/child trace differs')
        cases.append(dict(confirmed_prefix=prefix, signal='SIGKILL', restarted_recovered=result['recovered_commands'],
                          all_original_command_receipt_chain_rows_equal=True, final_state_equal=True, full_audit_commands=21))
    summary = dict(schema_version=1, success=True, environment='local-paper-no-network',
                   binary_sha256=sha(binary), config_file_sha256=sha(config), input_sha256=sha(inputs),
                   network_posts=0, child_quantities=children, one_lot_fills=len(fills), cases=cases, expected_state=reference['state'],
                   audit_chain_sha256=reference['audit_chain_sha256'],
                   scope='owned child process SIGKILL after committed receipt; submitted/partial/completed child stages; no mid-transaction timing, real venue partial-fill, external TWAP or 24h proof')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(dict(success=True, crash_cases=len(cases), position=market['position_units'], fees_minor=market['fees_minor'])))


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/kaze-run'))
    p.add_argument('--config', type=Path, default=Path('configs/target-twap-demo.json'))
    p.add_argument('--input', type=Path, default=Path('data/target-twap.jsonl'))
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    run(a.binary.resolve(), a.config.resolve(), a.input.resolve(), a.output)
