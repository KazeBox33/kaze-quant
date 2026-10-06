#!/usr/bin/env python3
"""固定日期/资产列表、连续归档前缀；来源校验和随研究计划存档，不按收益挑数据。"""
import argparse, csv, hashlib, json, urllib.request, zipfile
from pathlib import Path
from decimal import Decimal
from market_data import exact_price, quantity, sha_file

def prepare(directory, symbol, day, rows, sample_ms=0):
    name=f"{symbol}-bookTicker-{day}.zip"
    base=f"https://data.binance.vision/data/futures/um/daily/bookTicker/{symbol}/"
    directory.mkdir(parents=True,exist_ok=True)
    checksum=urllib.request.urlopen(base+name+".CHECKSUM",timeout=45).read().decode()
    expected=checksum.split()[0]
    if len(expected)!=64 or any(c not in "0123456789abcdef" for c in expected): raise ValueError("invalid official checksum")
    archive=directory/name
    if not archive.exists():
        partial=archive.with_suffix(".partial")
        with urllib.request.urlopen(base+name,timeout=45) as r,partial.open("wb") as f:
            size=0
            while chunk:=r.read(1024*1024):
                size+=len(chunk)
                if size>512*1024*1024: raise ValueError("archive size cap")
                f.write(chunk)
        if sha_file(partial)!=expected: raise ValueError("archive checksum mismatch")
        partial.rename(archive)
    if sha_file(archive)!=expected: raise ValueError("cached archive changed")
    output=directory/f"{symbol}-{day}-{rows}-sample{sample_ms}ms.csv"
    if output.exists():
        m=json.loads(output.with_suffix(".manifest.json").read_text())
        if m["archive_sha256"]==expected and sha_file(output)==m["normalized_sha256"]: return m,output
        raise ValueError("existing converted dataset changed")
    partial=output.with_suffix(".partial")
    h=hashlib.sha256(); count=0; previous=0; first=None; rounds=0; source_rows=0; last_kept=0
    with zipfile.ZipFile(archive) as z:
        if z.namelist()!=[name.replace(".zip",".csv")] or z.infolist()[0].file_size>4*1024**3: raise ValueError("archive member mismatch/capacity")
        with z.open(z.namelist()[0]) as source,partial.open("w",newline="") as target:
            header=source.readline();h.update(header)
            if header.decode().strip()!="update_id,best_bid_price,best_bid_qty,best_ask_price,best_ask_qty,transaction_time,event_time": raise ValueError("source header mismatch")
            writer=csv.writer(target,lineterminator="\n");writer.writerow(["sequence","timestamp_ns","bid","ask","bid_quantity","ask_quantity"])
            for line in source:
                if len(line)>4096: raise ValueError("source row size cap")
                h.update(line); uid,b,bq,a,aq,tx,event=next(csv.reader([line.decode()]))
                clock=int(event)*1000000
                if int(tx)>int(event) or clock<previous or not 0<clock<2**64: raise ValueError("timestamp regression; rejected, never sorted")
                source_rows+=1;previous=clock
                if sample_ms and last_kept and clock-last_kept < sample_ms*1000000: continue
                last_kept=clock
                count+=1;first=clock if first is None else first
                for q in (bq,aq): rounds+=int(Decimal(q)*100000!=(Decimal(q)*100000).to_integral_value())
                bid,ask=exact_price(b),exact_price(a)
                if bid>ask: raise ValueError("crossed source quote")
                writer.writerow([count,clock,bid,ask,quantity(bq),quantity(aq)]);previous=clock
                if count==rows: break
    if (not sample_ms and count!=rows) or count<10000: raise ValueError("archive too short")
    partial.rename(output)
    m=dict(transform_version="bookticker-strict-prefix-v2",symbol=symbol,date=day,rows=count,source_rows_consumed=source_rows,sample_ms=sample_ms,first_event_ns=first,last_event_ns=last_kept,source_url=base+name,checksum_url=base+name+".CHECKSUM",archive_sha256=expected,source_prefix_sha256=h.hexdigest(),normalized_sha256=sha_file(output),quantity_rounds=rounds,clock_clamps=0,selection="predeclared dates/symbols; causal first observed quote at least sample_ms after last kept quote; EOF or row cap; no price filtering/sort",units=dict(currency="USDT",money_scale=100000000,quantity_scale=100000))
    output.with_suffix(".manifest.json").write_text(json.dumps(m,indent=2)+"\n")
    return m,output

def build(directory,rows,output,sample_ms=0):
    # 先声明日期和参数；此程序从不读取策略收益决定数据选择。
    dates=["2024-01-01","2024-02-01","2024-03-01","2024-04-01"]
    symbols=["BTCUSDT","ETHUSDT"]
    declarations=dict(dates=dates,symbols=symbols,rows=rows,sample_ms=sample_ms,selection="calendar month first day, not optimized by PnL")
    directory.mkdir(parents=True,exist_ok=True)
    (directory/"selection.json").write_text(json.dumps(declarations,indent=2)+"\n")
    datasets=[]; unavailable=[]
    for symbol in symbols:
        config=json.loads(Path("configs/btc-mean-reversion.json").read_text());config["max_commands"]=1000000;config["markets"][0]["symbol"]=symbol
        config["markets"][0]["price_tick"]=1
        config["markets"][0]["engine"]["max_position"]=100000
        config["markets"][0]["engine"]["fee_bps"]=10
        cp=directory/f"{symbol}-research-config.json";cp.write_text(json.dumps(config,indent=2)+"\n")
        for day in dates:
            print(f"prepare {symbol} {day}",flush=True)
            try:
                m,path=prepare(directory,symbol,day,rows,sample_ms)
            except urllib.error.HTTPError as error:
                if error.code != 404: raise
                unavailable.append(dict(symbol=symbol,date=day,reason="official archive HTTP 404; no substituted date"))
                print(f"unavailable {symbol} {day} (recorded)",flush=True)
                continue
            start,end=m["first_event_ns"],m["last_event_ns"]
            cut=start+(end-start)//2
            datasets.append(dict(path=str(path),sha256=m["normalized_sha256"],source_manifest=str(path.with_suffix(".manifest.json")),config=str(cp),folds=[dict(train_start_ns=start,train_end_ns=cut,test_start_ns=cut+1,test_end_ns=end)]))
    candidates=[dict(type="sma_cross",fast=f,slow=s,band_bps=b,quantity=1000) for f,s,b in [(16,128,2),(32,256,5),(64,512,10)]]+[dict(type="mean_reversion",window=w,entry_bps=b,exit_bps=0,quantity=1000) for w,b in [(256,5),(1024,10),(4096,20)]]
    plan=dict(schema_version=1,availability=dict(declarations=declarations,unavailable=unavailable),datasets=datasets,candidates=candidates,costs=[dict(name="spot-10bps-10ms",fee_bps=10,latency_ns=10000000),dict(name="cost-stress-20bps-100ms",fee_bps=20,latency_ns=100000000),dict(name="slip-1bps-delta-budget",fee_bps=10,latency_ns=10000000,slippage_bps=1,liquidity_model="delta_budget")])
    output.write_text(json.dumps(plan,indent=2)+"\n")
if __name__=="__main__":
    p=argparse.ArgumentParser(description=__doc__);p.add_argument("--directory",type=Path,default=Path("reports/research-datasets"));p.add_argument("--rows",type=int,default=200000);p.add_argument("--sample-ms",type=int,default=0);p.add_argument("--plan",type=Path,default=Path("reports/research-plan.json"));a=p.parse_args()
    if not 10000<=a.rows<=500000:p.error("rows must be 10000..500000")
    if not 0<=a.sample_ms<=60000:p.error("sample-ms must be 0..60000")
    build(a.directory,a.rows,a.plan,a.sample_ms)
