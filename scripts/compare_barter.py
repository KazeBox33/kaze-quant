#!/usr/bin/env python3
"""固定上游源码版本，运行真实组件对照；不改上游源码或模拟上游行为。"""
import argparse, csv, hashlib, json, shutil, subprocess, zipfile
from pathlib import Path
REVISION="9770b27a83f844472b93b593b08063affc974b0d"
ROOT=Path(__file__).resolve().parents[1]
def prepare(source):
    destination=ROOT/"benchmarks/barter-source"
    if source:
        source=source.resolve()
        revision=subprocess.check_output(["git","rev-parse","HEAD"],cwd=source,text=True).strip()
        dirty=subprocess.check_output(["git","status","--porcelain"],cwd=source,text=True).strip()
        if revision!=REVISION or dirty: raise ValueError("comparison requires clean exact upstream revision")
        if destination.exists(): shutil.rmtree(destination)
        shutil.copytree(source,destination,ignore=shutil.ignore_patterns("target"))
    elif not destination.exists():
        subprocess.run(["git","clone","https://github.com/barter-rs/barter-rs",str(destination)],check=True,timeout=180)
        subprocess.run(["git","checkout",REVISION],cwd=destination,check=True,timeout=30)
    if subprocess.check_output(["git","rev-parse","HEAD"],cwd=destination,text=True).strip()!=REVISION: raise ValueError("upstream revision mismatch")
    if subprocess.check_output(["git","status","--porcelain"],cwd=destination,text=True).strip(): raise ValueError("upstream checkout modified")
    return destination
if __name__=="__main__":
    p=argparse.ArgumentParser(description=__doc__);p.add_argument("--source",type=Path);p.add_argument("--input",type=Path);p.add_argument("--archive",type=Path);p.add_argument("--rows",type=int,default=100000);p.add_argument("--output",type=Path,default=ROOT/"reports/barter-comparison.json");a=p.parse_args()
    if a.archive:
        if a.input: p.error("input and archive are mutually exclusive")
        from market_data import PINNED_SHA,sha_file
        if sha_file(a.archive)!=PINNED_SHA: raise ValueError("expected pinned BTCUSDT 2024-01-01 archive")
        a.input=ROOT/"reports/barter-real-100000.jsonl"
        with zipfile.ZipFile(a.archive) as z,z.open(z.namelist()[0]) as f,a.input.open("w") as out:
            header=f.readline()
            if not header.startswith(b"update_id,best_bid_price"):raise ValueError("source schema changed")
            count=0
            for line in f:
                u,b,B,ask,A,T,E=next(csv.reader([line.decode()]))
                out.write(json.dumps(dict(u=int(u),s="BTCUSDT",b=b,B=B,a=ask,A=A,T=int(T)),separators=(",",":"))+"\n")
                count+=1
                if count==a.rows:break
            if count!=a.rows:raise ValueError("source too short")
    upstream=prepare(a.source)
    command=[str(ROOT/"scripts/cargo.sh"),"run","--locked","--release","--manifest-path",str(ROOT/"benchmarks/barter-comparison/Cargo.toml"),"--",str(a.rows)]
    if a.input: command.append(str(a.input.resolve()))
    result=subprocess.run(command,cwd=ROOT,stdout=subprocess.PIPE,text=True,check=True,timeout=900)
    report=json.loads(result.stdout)
    report["upstream_tracked_sources_sha256"]=hashlib.sha256(b"".join(name+b"\0"+(upstream/name.decode()).read_bytes()+b"\0" for name in sorted(subprocess.check_output(["git","ls-files","-z"],cwd=upstream).split(b"\0")[:-1]))).hexdigest()
    report["comparison_binary_sha256"]=hashlib.sha256((ROOT/"benchmarks/barter-comparison/target/release/kaze-barter-comparison").read_bytes()).hexdigest()
    report["kaze_feed_source_sha256"]=hashlib.sha256((ROOT/"src/feed.rs").read_bytes()).hexdigest()
    report["kaze_decimal_source_sha256"]=hashlib.sha256((ROOT/"src/decimal.rs").read_bytes()).hexdigest()
    report["source_archive_sha256"]=hashlib.sha256(a.archive.read_bytes()).hexdigest() if a.archive else None
    report["command"]=command
    a.output.write_text(json.dumps(report,indent=2)+"\n")
