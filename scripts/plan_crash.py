#!/usr/bin/env python3
"""mock进程在父子意图提交后、网络发送前 SIGKILL；新进程查不到也不重发。"""
import argparse
import hashlib
import json
from pathlib import Path
import select
import signal
import sqlite3
import subprocess


def run(binary, output):
    output.mkdir(parents=True, exist_ok=False)
    with (output / 'first.stderr').open('w') as err:
        p = subprocess.Popen([str(binary), 'before-post', str(output)], stdout=subprocess.PIPE, stderr=err)
        try:
            if not select.select([p.stdout], [], [], 15)[0] or p.stdout.readline().strip() != b'PREPARED_NO_POST':
                raise RuntimeError('did not reach durable-before-network boundary')
            p.send_signal(signal.SIGKILL)
            if p.wait(timeout=15) != -signal.SIGKILL:
                raise RuntimeError('child not killed')
        finally:
            if p.poll() is None:
                p.kill()
                p.wait(timeout=15)
            p.stdout.close()
    with sqlite3.connect(f'file:{(output / "external.db").resolve()}?mode=ro', uri=True) as c:
        unknown = c.execute("SELECT count(*) FROM intents WHERE phase='unknown' AND observation IS NULL").fetchone()[0]
        linked = c.execute('SELECT count(*) FROM plan_children').fetchone()[0]
        if unknown != 1 or linked != 1:
            raise ValueError('parent/child intent missing after crash')
    r = subprocess.run([str(binary), 'recover-before-post', str(output)], capture_output=True, text=True, check=True, timeout=30)
    data = json.loads(r.stdout)
    if data['post_calls'] != 0 or data['status']['phase'] != 'submit_unknown':
        raise ValueError('unsafe replay')
    summary = dict(schema_version=1, success=True, binary_sha256=hashlib.sha256(binary.read_bytes()).hexdigest(),
                   signal='SIGKILL', boundary='intent and child linkage committed; mock submit not invoked',
                   durable_unknown_intents=unknown, durable_child_links=linked, fresh_process_posts=0,
                   recovered=data, scope='owned process, deterministic no-network fixture; not actual unsent venue proof or physical network fault')
    (output / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    print(json.dumps(dict(success=True, linked_unknown_children=linked, fresh_process_posts=0)))


if __name__ == '__main__':
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--binary', type=Path, default=Path('target/release/examples/external_plan_demo'))
    p.add_argument('--output', type=Path, required=True)
    a = p.parse_args()
    run(a.binary.resolve(), a.output)
