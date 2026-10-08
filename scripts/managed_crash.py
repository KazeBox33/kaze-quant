#!/usr/bin/env python3
"""多策略纸面验收：生命周期、部分成交、越权拒绝、策略停止、现金转移与条件OCO确认后强杀。"""
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
        return db.execute('SELECT seq,payload,receipt,chain FROM commands ORDER BY seq').fetchall()


def run(binary, output):
    output.mkdir(parents=True, exist_ok=False)
    cases = []
    ledgers = []
    for name, expected, prefixes in [('managed', (10, '8986', '4'), (1, 4, 5, 6, 8, 10, 11, 12, 13)),
                                     ('managed-bracket', (0, '10056', '4'), (5, 6, 7, 8, 9))]:
        config = Path(f'configs/{name}-demo.json').resolve()
        inputs = Path(f'data/{name}-demo.jsonl').resolve()
        commands = inputs.read_bytes().splitlines(keepends=True)
        if [json.loads(e)['seq'] for e in commands] != list(range(1, len(commands) + 1)):
            raise ValueError('noncontiguous demo')
        folder = output / name
        folder.mkdir()

        def common(db):
            return [str(binary), '--config', str(config), '--db', str(db)]

        def finish(db, label):
            report = folder / f'{label}.json'
            subprocess.run(common(db) + ['--input', str(inputs), '--finish', '--verify-full', '--quiet', '--report', str(report)],
                           capture_output=True, check=True, timeout=60)
            data = json.loads(report.read_text())
            audit_path = folder / f'{label}-audit.json'
            subprocess.run(common(db) + ['--recover-only', '--verify-full', '--quiet', '--report', str(audit_path)],
                           capture_output=True, check=True, timeout=60)
            audit = json.loads(audit_path.read_text())
            if data['state'] != audit['state'] or data['audit_chain_sha256'] != audit['audit_chain_sha256'] or audit['verified_commands'] != len(commands) + 1:
                raise ValueError('new-process audit differs')
            return data

        reference_db = folder / 'reference.db'
        reference = finish(reference_db, 'reference')
        original = rows(reference_db)
        m = reference['state']['markets'][0]
        if (m['position_units'], m['cash_minor'], m['fees_minor']) != expected:
            raise ValueError('hand ledger differs')
        members=m['strategy_diagnostics']['members']
        if any(x['lifecycle']!='stopped' or x['active_orders'] or x['waiting_conditions'] for x in members):
            raise ValueError('finish left member work')
        if name=='managed' and [(x['cash_minor'],x['position_units']) for x in members]!=[('4193',5),('4793',5)]:
            raise ValueError('partial fill/transfer member ledger differs')
        if name=='managed-bracket' and [(x['cash_minor'],x['position_units']) for x in members]!=[('5028',0),('5028',0)]:
            raise ValueError('separate bracket member ledger differs')
        for prefix in prefixes:
            db = folder / f'kill-{prefix}.db'
            with (folder / f'kill-{prefix}.stderr').open('w') as err:
                child = subprocess.Popen(common(db) + ['--batch-size', '1', '--flush-ms', '1'],
                                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err)
                try:
                    for seq, line in enumerate(commands[:prefix], 1):
                        child.stdin.write(line)
                        child.stdin.flush()
                        if not select.select([child.stdout], [], [], 15)[0]:
                            raise TimeoutError('commit confirmation timeout')
                        receipt = json.loads(child.stdout.readline())
                        if receipt['seq'] != seq or receipt['duplicate']:
                            raise ValueError('unexpected receipt')
                    child.send_signal(signal.SIGKILL)
                    if child.wait(timeout=15) != -signal.SIGKILL:
                        raise ValueError('not killed')
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait(timeout=15)
                    child.stdin.close()
                    child.stdout.close()
            if len(rows(db)) != prefix:
                raise ValueError('lost confirmed prefix')
            resumed = finish(db, f'resumed-{prefix}')
            if rows(db) != original or resumed['state'] != reference['state'] or resumed['audit_chain_sha256'] != reference['audit_chain_sha256']:
                raise ValueError('duplicate child or recovery changed original trace')
            cases.append(dict(algorithm=name, confirmed_prefix=prefix, signal='SIGKILL', all_payload_receipt_chain_rows_equal=True,
                              state_equal=True, full_audit_commands=len(commands) + 1))
        ledgers.append(dict(algorithm=name, input_sha256=sha(inputs), config_sha256=sha(config),
                            members=members, state=reference['state'], audit_chain_sha256=reference['audit_chain_sha256']))
    summary = dict(success=True, binary_sha256=sha(binary), ledgers=ledgers, cases=cases, network_posts=0,
                   scope='owned process kill after committed control/partial fill/ownership rejection/stop/transfer/conditional phases; one shared paper Engine, no mid-transaction injection, physical power failure, authentication sandbox, external venue, 24h or alpha claim')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(dict(success=True, crash_cases=len(cases))))


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/kaze-run'))
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    run(a.binary.resolve(), a.output)
