#!/usr/bin/env python3
"""冻结旧/新 release 可执行文件，交替七轮；每轮校验完整状态而非只计时。"""
import argparse, hashlib, json, platform, statistics, subprocess
from pathlib import Path

def sha(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()

def run(old, new, output, repeats):
    output.mkdir(parents=True, exist_ok=False)
    rows=[]
    for mode in ('cancel','churn','lookup','quote','snapshot'):
        for n in (128,4096,32768):
            expected=None
            for i in range(repeats):
                for label,binary in ([('old',old),('new',new)] if i%2==0 else [('new',new),('old',old)]):
                    stem=output/f'{mode}-{n}-{i}-{label}'
                    command=[str(binary),mode,str(n)]
                    # macOS time -l 的进程峰值包含准备/审计/输出；不是热表独占内存。
                    time_cmd=['/usr/bin/time','-l'] if platform.system()=='Darwin' else []
                    p=subprocess.run(time_cmd+command,capture_output=True,check=True)
                    stem.with_suffix('.json').write_bytes(p.stdout)
                    stem.with_suffix('.stderr').write_bytes(p.stderr)
                    d=json.loads(p.stdout)
                    fingerprint=(d['checksum'],d['state'])
                    if expected is not None and fingerprint!=expected:
                        raise ValueError(f'{stem}: full state/checksum mismatch')
                    expected=fingerprint
                    peak=None
                    for line in p.stderr.decode().splitlines():
                        if 'maximum resident set size' in line:peak=int(line.split()[0])
                    rows.append(dict(mode=mode,orders=n,round=i,version=label,elapsed_ns=d['elapsed_ns'],operations=d['operations'],checksum=d['checksum'],state_sha256=hashlib.sha256(json.dumps(d['state'],sort_keys=True,separators=(',',':')).encode()).hexdigest(),raw_sha256=sha(stem.with_suffix('.json')),process_peak_rss_bytes=peak))
            med={v:statistics.median(x['elapsed_ns'] for x in rows if x['mode']==mode and x['orders']==n and x['version']==v) for v in ['old','new']}
            print(mode,n,med,round(med['old']/med['new'],3),flush=True)
    summaries=[]
    for mode in ('cancel','churn','lookup','quote','snapshot'):
        for n in (128,4096,32768):
            r=[x for x in rows if x['mode']==mode and x['orders']==n]
            med={v:statistics.median(x['elapsed_ns'] for x in r if x['version']==v) for v in ['old','new']}
            rss={v:statistics.median(x['process_peak_rss_bytes'] for x in r if x['version']==v) if all(x['process_peak_rss_bytes'] is not None for x in r) else None for v in ['old','new']}
            summaries.append(dict(mode=mode,orders=n,operations=r[0]['operations'],median_ns=med,new_over_old_time=med['new']/med['old'],speed_ratio=med['old']/med['new'],process_peak_rss_bytes=rss))
    result=dict(schema_version=1,success=True,repeats_each=repeats,platform=platform.platform(),machine=platform.machine(),old_sha256=sha(old),new_sha256=sha(new),fixture_sha256=sha(Path('examples/order_store_bench.rs')),summaries=summaries,runs=rows,scope='own legacy v13/new v14 synthetic fixed single-asset workload; setup and audit outside timing; HashMap seeded separately in each process; whole-process peak RSS includes setup/audit/output, no real exchange or upstream ranking')
    (output/'summary.json').write_text(json.dumps(result,indent=2)+'\n')

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--old',type=Path,required=True);p.add_argument('--new',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--repeats',type=int,default=7)
    a=p.parse_args()
    if not 1<=a.repeats<=20:p.error('repeats 1..20')
    run(a.old.resolve(),a.new.resolve(),a.output,a.repeats)
