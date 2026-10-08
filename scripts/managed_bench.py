#!/usr/bin/env python3
"""固定公开十万报价的管理层成本、1/2/8策略首次基线、逐原回执与恢复审计。"""
import argparse
import hashlib
import json
from pathlib import Path
import platform
import sqlite3
import statistics
import struct
import subprocess
import sys
from real_bench import measured

def sha(p): return hashlib.sha256(p.read_bytes()).hexdigest()
def dump(p,v): p.write_text(json.dumps(v,indent=2)+'\n')
def run(binary,direct,managed,quotes,output,repeats,durable_repeats):
    output.mkdir(parents=True,exist_ok=False)
    configs={0:Path('configs/btc-bar-atr-v1.json'),1:Path('configs/btc-managed-atr-v1.json'),2:Path('configs/btc-managed-atr-2-v1.json'),8:Path('configs/btc-managed-atr-8-v1.json')}
    lines=quotes.read_text().splitlines(); assert len(lines)==100001
    memory=[]; expected={}
    for members in [1,2,8]:
        for round_id in range(repeats):
            versions=([0,1]if round_id%2==0 else[1,0])if members==1 else[members]
            for n in versions:
                exe=direct if n==0 else managed
                result=json.loads(subprocess.run([str(exe),str(configs[n]),str(quotes)],capture_output=True,check=True,timeout=180).stdout)
                fingerprint=(result['receipt_sha256'],result['state'])
                if n in expected and expected[n]!=fingerprint:raise ValueError('memory round original receipts/state differ')
                expected[n]=fingerprint;result.update(members=n,round=round_id);memory.append(result)
                dump(output/f'memory-{n}-{round_id}.json',result)
                print('memory',n,round_id,result['elapsed_ns'],flush=True)
                if n in [0,1]and all(k in expected for k in [0,1]):
                    if expected[0][0]!=expected[1][0]:raise ValueError('one-member normalized original receipt differs from direct strategy')
                    a,b=[expected[k][1]['markets'][0]for k in [0,1]]
                    for key in ['cash_minor','position_units','fees_minor','reserved_cash_minor','equity_minor','pnl_minor','risk_rejections','metrics','strategy_decision']:
                        if a[key]!=b[key]:raise ValueError(f'one-member economic/report field differs: {key}')
    durable=[]
    for n in [1,2,8]:
        config=configs[n];offset=2*n;input_path=output/f'input-{n}.jsonl'
        with input_path.open('w')as f:
            seq=0
            for owner in range(n):
                for op in ['init','start']:
                    seq+=1;f.write(json.dumps(dict(seq=seq,command=dict(type='strategy_control',market=0,owner=owner,operation=op)),separators=(',',':'))+'\n')
            for row in lines[1:]:
                sequence,timestamp_ns,bid,ask,bid_quantity,ask_quantity=map(int,row.split(','));seq+=1
                f.write(json.dumps(dict(seq=seq,command=dict(type='quote',market=0,quote=dict(sequence=sequence,timestamp_ns=timestamp_ns,bid=bid,ask=ask,bid_quantity=bid_quantity,ask_quantity=ask_quantity))),separators=(',',':'))+'\n')
        for round_id in range(durable_repeats):
            stem=output/f'durable-{n}-{round_id}';db=stem.with_suffix('.db');report=stem.with_suffix('.json')
            common=[str(binary),'--config',str(config),'--db',str(db),'--quiet']
            measurement=measured(common+['--input',str(input_path),'--batch-size','256','--report',str(report)],stem)
            data=json.loads(report.read_text());raw=hashlib.sha256();normalized=hashlib.sha256();count=0
            with sqlite3.connect(f'file:{db.resolve()}?mode=ro',uri=True)as conn:
                for sequence,receipt in conn.execute('SELECT seq,receipt FROM commands ORDER BY seq'):
                    count+=1;assert sequence==count
                    raw.update(struct.pack('<Q',len(receipt)));raw.update(receipt)
                    if sequence>offset:
                        r=json.loads(receipt);r['seq']-=offset;r['notices']=[v for v in r['notices']if v['type']!='owned_action']
                        encoded=json.dumps(r,separators=(',',':')).encode();normalized.update(struct.pack('<Q',len(encoded)));normalized.update(encoded)
            if count!=100000+offset or normalized.hexdigest()!=expected[n][0]or data['state']!=expected[n][1]:raise ValueError('durable normalized receipts or whole managed state differs from memory')
            audit=output/f'audit-{n}-{round_id}.json';subprocess.run(common+['--recover-only','--verify-full','--report',str(audit)],capture_output=True,check=True,timeout=180)
            checked=json.loads(audit.read_text());assert checked['state']==data['state']and checked['audit_chain_sha256']==data['audit_chain_sha256']and checked['verified_commands']==count
            durable.append(dict(members=n,round=round_id,elapsed_ns=data['run']['elapsed_ns'],original_receipt_sha256=raw.hexdigest(),normalized_receipt_sha256=normalized.hexdigest(),original_rows_audit_equal=True,verified_commands=count,input_sha256=sha(input_path),run=data['run'],storage_stats=data['storage_stats'],**measurement))
            print('durable',n,round_id,data['run']['elapsed_ns'],measurement['sample_peak_rss_bytes'],flush=True)
    summary=dict(success=True,quotes=100000,platform=platform.platform(),python=sys.version,binary_sha256=sha(binary),direct_reference_sha256=sha(direct),managed_reference_sha256=sha(managed),csv_sha256=sha(quotes),config_sha256={n:sha(p)for n,p in configs.items()},memory_runs=memory,durable_runs=durable,memory_median_ns={n:statistics.median(r['elapsed_ns']for r in memory if r['members']==n)for n in[0,1,2,8]},durable_median_ns={n:statistics.median(r['elapsed_ns']for r in durable if r['members']==n)for n in[1,2,8]},one_member_normalized_raw_receipts_and_economic_fields_equal=True,scope='same final build direct versus one managed member alternating; memory includes CSV/core/strategy/normalized receipt hash/retention, explicit init/start excluded; durable includes exact original control prefix+quotes+256 batches SQLite FULL/checkpoints, same-build full audit excluded; 2/8 policies differ in budgets/signals and are first workload baselines, not economic/rate superiority comparisons, individual latency, upstream, alpha or external route')
    dump(output/'summary.json',summary)
if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--binary',type=Path,default=Path('target/release/kaze-run'));p.add_argument('--direct',type=Path,default=Path('target/release/examples/target_reference'));p.add_argument('--managed',type=Path,default=Path('target/release/examples/managed_reference'));p.add_argument('--quotes',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--repeats',type=int,default=7);p.add_argument('--durable-repeats',type=int,default=3);a=p.parse_args()
    if not 1<=a.repeats<=20 or not 1<=a.durable_repeats<=10:p.error('repeat bound exceeded')
    run(a.binary.resolve(),a.direct.resolve(),a.managed.resolve(),a.quotes.resolve(),a.output,a.repeats,a.durable_repeats)
