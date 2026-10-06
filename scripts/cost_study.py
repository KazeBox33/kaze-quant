#!/usr/bin/env python3
"""固定信号参数，仅改变假设费率；研究成本敏感度，不筛选盈利参数。"""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

def run(quotes, output):
    config=json.loads(Path("configs/btc-mean-reversion.json").read_text())
    h=hashlib.sha256()
    with quotes.open("rb") as f:
        while chunk:=f.read(1024*1024):h.update(chunk)
    study=[]
    with tempfile.TemporaryDirectory(prefix="kaze-cost-") as directory:
        p=Path(directory)/"config.json"
        for fee in [0,2,5]:
            config["markets"][0]["engine"]["fee_bps"]=fee
            p.write_text(json.dumps(config))
            data=subprocess.check_output(["target/release/examples/real_reference",str(p),str(quotes)],text=True)
            study.append(dict(fee_bps_per_fill=fee,state=json.loads(data)))
    output.parent.mkdir(parents=True,exist_ok=True)
    with output.open("x") as f:json.dump(dict(input_sha256=h.hexdigest(),model="unlevered long-only best-quote shared liquidity, 1ms delay; fees are hypothetical assumptions",strategy="window4096 entry1bps exit0bps quantity1000 lots; engineering fixture, no alpha tuning",runs=study),f,indent=2);f.write("\n")
if __name__=="__main__":
    p=argparse.ArgumentParser(description=__doc__);p.add_argument("--quotes",type=Path,default=Path("reports/datasets/btc-quotes-1000000.csv"));p.add_argument("--output",type=Path,required=True);a=p.parse_args();run(a.quotes,a.output)
