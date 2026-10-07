#!/usr/bin/env python3
"""零网络条件策略验收：等待、激活、成交、OCO后SIGKILL；逐原始回执恢复。"""
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
    if len(commands) != 5 or [json.loads(e)['seq'] for e in commands] != list(range(1, 6)):
        raise ValueError('acceptance requires fixed 5-quote bracket teaching trace')

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
        if data['state'] != audit['state'] or data['audit_chain_sha256'] != audit['audit_chain_sha256'] or audit['verified_commands'] != 6:
            raise ValueError('fresh full audit differs')
        return data

    reference_db = output / 'reference.db'
    reference = finish(reference_db, 'reference')
    expected = rows(reference_db)
    market = reference['state']['markets'][0]
    if (market['position_units'], market['cash_minor'], market['fees_minor']) != (0, '10028', '2'):
        raise ValueError('hand ledger: buy 3@101, sell 3@111, one minor fee each')
    receipts = [json.loads(r[2]) for r in expected]
    fills = [n['event']['Fill'] for r in receipts for n in r['notices'] if n['type'] == 'engine' and 'Fill' in n['event']]
    conditionals = [n['event'] for r in receipts for n in r['notices'] if n['type'] == 'conditional']
    if len(fills) != 2 or [f['quantity'] for f in fills] != [3, 3]:
        raise ValueError('fill trace differs')
    if sum(e['type'] == 'triggered' for e in conditionals) != 2 or sum(e['type'] == 'cancelled' for e in conditionals) != 1:
        raise ValueError('conditional activation/OCO differs')
    if any(n['type'] == 'engine' and 'Fill' in n['event'] for r in receipts[:2] for n in r['notices']):
        raise ValueError('entry filled on condition trigger quote')
    cases = []
    for prefix in (1, 2, 3, 4, 5):
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
                          all_original_command_receipt_chain_rows_equal=True, final_state_equal=True, full_audit_commands=6))
    summary = dict(schema_version=1, success=True, environment='local-paper-no-network',
                   binary_sha256=sha(binary), config_file_sha256=sha(config), input_sha256=sha(inputs),
                   network_posts=0, fills=fills, conditional_events=conditionals, cases=cases, expected_state=reference['state'],
                   audit_chain_sha256=reference['audit_chain_sha256'],
                   scope='owned child process SIGKILL after committed receipt; pending/activated/filled/bracket/OCO/completed stages; no mid-transaction timing, real venue partial-fill, external conditional execution or 24h proof')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(dict(success=True, crash_cases=len(cases), position=market['position_units'], fees_minor=market['fees_minor'])))


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/kaze-run'))
    p.add_argument('--config', type=Path, default=Path('configs/breakout-bracket-demo.json'))
    p.add_argument('--input', type=Path, default=Path('data/breakout-bracket-demo.jsonl'))
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    run(a.binary.resolve(), a.config.resolve(), a.input.resolve(), a.output)
