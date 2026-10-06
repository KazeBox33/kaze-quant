#!/usr/bin/env python3
"""离线研究专用：归档可能交错多段时间流；每UTC秒选最早交易所事件，不冒充线上接收顺序。"""
import csv
import datetime
import hashlib
import json
import shutil
import urllib.request
import zipfile
from pathlib import Path
from market_data import exact_price, quantity, sha_file


def prepare(directory, symbol, day, rows=200000, sample_ms=1000, clock_policy=None, max_bad_ppm=None):
    if sample_ms != 1000 or rows < 86400:
        raise ValueError('event grid requires one second / complete day capacity')
    directory.mkdir(parents=True, exist_ok=True)
    name=f'{symbol}-bookTicker-{day}.zip'
    base=f'https://data.binance.vision/data/futures/um/daily/bookTicker/{symbol}/'
    expected=urllib.request.urlopen(base+name+'.CHECKSUM',timeout=45).read(1024).decode().split()[0]
    if len(expected)!=64 or any(c not in '0123456789abcdef' for c in expected):raise ValueError('invalid checksum')
    archive=directory/name
    if not archive.exists():
        # 只复用已下载的原始归档，仍重新核对官方哈希；不复用其他采样结果。
        cached=Path('reports/holdout-week-v3/archives')/name
        if not cached.exists():cached=Path('reports/holdout-week-v2/archives')/name
        if cached.exists() and sha_file(cached)==expected:
            shutil.copyfile(cached,archive)
        else:
            partial=archive.with_suffix('.download.partial')
            with urllib.request.urlopen(base+name,timeout=45) as r,partial.open('wb') as f:
                size=0
                while chunk:=r.read(1024*1024):
                    size+=len(chunk)
                    if size>512*1024*1024:raise ValueError('archive capacity')
                    f.write(chunk)
            if sha_file(partial)!=expected:raise ValueError('download checksum mismatch')
            partial.rename(archive)
    if sha_file(archive)!=expected:raise ValueError('archive checksum mismatch')
    output=directory/f'{symbol}-{day}-utc-second-first-event.csv'
    if output.exists():
        m=json.loads(output.with_suffix('.manifest.json').read_text())
        if m['archive_sha256']==expected and sha_file(output)==m['normalized_sha256']:return m,output
        raise ValueError('cached output changed')
    start=int(datetime.datetime.fromisoformat(day).replace(tzinfo=datetime.timezone.utc).timestamp())*1000
    end=start+86400000
    selected={}; source_rows=0; backward=0; boundary_tail=0; previous=0; h=hashlib.sha256()
    with zipfile.ZipFile(archive) as z:
        if z.namelist()!=[name.replace('.zip','.csv')] or z.infolist()[0].file_size>4*1024**3:raise ValueError('member identity/capacity')
        with z.open(z.namelist()[0]) as f:
            header=f.readline();h.update(header)
            if header.decode().strip()!='update_id,best_bid_price,best_bid_qty,best_ask_price,best_ask_qty,transaction_time,event_time':raise ValueError('header mismatch')
            for raw in f:
                if len(raw)>4096:raise ValueError('source row capacity')
                h.update(raw);source_rows+=1
                uid,b,bq,a,aq,tx,event=next(csv.reader([raw.decode()]))
                uid,tx,event=int(uid),int(tx),int(event)
                if uid<=0 or not (start<=tx<end and tx<=event<=tx+60000):raise ValueError('transaction outside declared day / invalid event lag')
                # 归档以交易时间划日；毫秒级发布延迟可跨日。保留哈希/计数，但下一UTC日事件不进入本日网格。
                if event>=end:
                    boundary_tail+=1
                    continue
                backward+=event<previous;previous=event
                second=(event-start)//1000
                # 只保留每秒最早的事件，无高低价/末价；同毫秒按更新ID、源行号稳定取序。
                key=(event,uid,source_rows)
                old=selected.get(second)
                if old is None or key<old[0]:selected[second]=(key,(b,bq,a,aq))
    if len(selected)<10000:raise ValueError('insufficient seconds')
    partial=output.with_suffix('.partial')
    first=last=None
    with partial.open('x',newline='') as f:
        w=csv.writer(f,lineterminator='\n');w.writerow(['sequence','timestamp_ns','bid','ask','bid_quantity','ask_quantity'])
        for seq,second in enumerate(sorted(selected),1):
            key,prices=selected[second];b,bq,a,aq=prices
            bid,ask=exact_price(b),exact_price(a)
            if bid>ask:raise ValueError('crossed quote')
            clock=key[0]*1000000;first=clock if first is None else first;last=clock
            w.writerow([seq,clock,bid,ask,quantity(bq),quantity(aq)])
    partial.rename(output)
    m=dict(transform_version='offline-utc-second-first-event-v1',symbol=symbol,date=day,rows=len(selected),
           first_event_ns=first,last_event_ns=last,source_rows_consumed=source_rows,source_order_regressions=backward,boundary_event_rows_excluded=boundary_tail,max_event_lag_ms=60000,
           archive_sha256=expected,source_prefix_sha256=h.hexdigest(),normalized_sha256=sha_file(output),
           source_url=base+name,checksum_url=base+name+'.CHECKSUM',archive_complete=True,
           selection='offline chronological reconstruction: earliest event per UTC second; event/update ID/source row ties; no return filtering',
           limits='exchange event chronology, not original file/arrival order; cannot estimate venue-to-client latency; futures quotes in unlevered spot-like model',
           units=dict(currency='USDT',money_scale=100000000,quantity_scale=100000))
    output.with_suffix('.manifest.json').write_text(json.dumps(m,indent=2)+'\n')
    return m,output
