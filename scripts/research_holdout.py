#!/usr/bin/env python3
"""先冻结独立周研究协议，训练决定单量，随后才获取测试数据；不按收益改日期。"""
import argparse, csv, datetime, hashlib, json, subprocess
from pathlib import Path
from research_data import prepare as prepare_strict
from event_time_data import prepare as prepare_event_grid
from market_data import sha_file

def merge(parts, target):
    previous=0; sequence=0; first=None; last=None; maximum_ask=0
    partial=target.with_suffix('.partial')
    with partial.open('x',newline='') as output:
        writer=csv.writer(output,lineterminator='\n')
        writer.writerow(['sequence','timestamp_ns','bid','ask','bid_quantity','ask_quantity'])
        for manifest,path in parts:
            if sha_file(path)!=manifest['normalized_sha256']:raise ValueError('source changed during merge')
            with path.open(newline='') as source:
                for row in csv.DictReader(source):
                    clock=int(row['timestamp_ns'])
                    if clock<previous:raise ValueError('merge clock regression')
                    sequence+=1;first=clock if first is None else first;last=clock;previous=clock
                    maximum_ask=max(maximum_ask,int(row['ask']))
                    writer.writerow([sequence,clock,row['bid'],row['ask'],row['bid_quantity'],row['ask_quantity']])
    partial.rename(target)
    return dict(rows=sequence,first_ns=first,last_ns=last,max_ask=maximum_ask,normalized_sha256=sha_file(target))

def training_quantity(notional, maximum_ask):
    if maximum_ask<=0:raise ValueError('empty training maximum ask')
    quantity=int(notional)*100000000//maximum_ask
    if not 0<quantity<=100000:raise ValueError('training-derived quantity outside bounds')
    return quantity

def build(spec_path,directory,binary):
    spec_bytes=spec_path.read_bytes();spec=json.loads(spec_bytes)
    prepare=prepare_event_grid if spec.get('clock_policy')=='offline_event_grid' else prepare_strict
    if spec['schema_version']!=1 or spec['sample_ms']!=1000:raise ValueError('unsupported protocol')
    dates=spec['train_dates']+spec['test_dates']
    if sorted(set(dates))!=dates or spec['train_dates'][-1]>=spec['test_dates'][0]:raise ValueError('strict date split required')
    directory.mkdir(parents=True,exist_ok=False)
    frozen=directory/'frozen-spec.json';frozen.write_bytes(spec_bytes)
    identity=dict(spec_sha256=hashlib.sha256(spec_bytes).hexdigest(),frozen_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),binary_sha256=sha_file(binary),converter_sha256=sha_file(Path(__file__).with_name('event_time_data.py' if spec.get('clock_policy')=='offline_event_grid' else 'research_data.py')),driver_sha256=sha_file(Path(__file__)),spec=str(spec_path),rule='frozen before fetching any new archive; quantity only uses training data')
    (directory/'protocol.json').write_text(json.dumps(identity,indent=2)+'\n')
    completed=[]
    for symbol in spec['symbols']:
        parts=[]
        for day in spec['train_dates']:
            print('training',symbol,day,flush=True)
            parts.append(prepare(directory/'archives',symbol,day,200000,spec['sample_ms'],spec.get('clock_policy','strict'),spec.get('max_bad_ppm',100)))
        train=merge(parts,directory/f'{symbol}-train.csv')
        # 金额minor / 每lot价格，所有数值均为整数；测试价格不参与单量计算。
        quantity=training_quantity(spec['quantity_notional_usdt'],train['max_ask'])

        candidates=[dict(c,quantity=quantity) for c in spec['candidates']]
        (directory/f'{symbol}-frozen-selection.json').write_text(json.dumps(dict(training=train,quantity=quantity,candidates=candidates,selection_frozen_before_holdout=True),indent=2)+'\n')
        for day in spec['test_dates']:
            print('holdout',symbol,day,flush=True)
            parts.append(prepare(directory/'archives',symbol,day,200000,spec['sample_ms'],spec.get('clock_policy','strict'),spec.get('max_bad_ppm',100)))
        merged=directory/f'{symbol}-all.csv';metadata=merge(parts,merged)
        if metadata['rows']>1200000:raise ValueError('study row capacity exceeded')
        manifest=dict(**metadata,parts=[m for m,_ in parts],protocol=identity,symbol=symbol,quantity=quantity)
        mp=merged.with_suffix('.manifest.json');mp.write_text(json.dumps(manifest,indent=2)+'\n')
        config=json.loads(Path('configs/btc-mean-reversion.json').read_text());config['max_commands']=1000000
        market=config['markets'][0];market['symbol']=symbol;market['price_tick']=1;market['engine']['fee_bps']=10
        cp=directory/f'{symbol}-config.json';cp.write_text(json.dumps(config,indent=2)+'\n')
        fold=dict(train_start_ns=train['first_ns'],train_end_ns=train['last_ns'],test_start_ns=parts[len(spec['train_dates'])][0]['first_event_ns'],test_end_ns=metadata['last_ns'])
        plan=dict(schema_version=1,availability=dict(protocol=identity,missing_policy=spec['missing_data']),datasets=[dict(path=str(merged),sha256=metadata['normalized_sha256'],source_manifest=str(mp),config=str(cp),folds=[fold])],candidates=candidates,costs=spec['costs'])
        plan_path=directory/f'{symbol}-plan.json';plan_path.write_text(json.dumps(plan,indent=2)+'\n')
        rp=directory/f'{symbol}-result.json'
        subprocess.run([str(binary.resolve()),str(plan_path),str(rp)],check=True)
        result=json.loads(rp.read_text());completed.append(dict(symbol=symbol,screen_passed=result['screen_passed'],report=str(rp)))
    (directory/'summary.json').write_text(json.dumps(dict(protocol=identity,results=completed,screen_passed=all(r['screen_passed'] for r in completed),limits='single predeclared week; no claim of statistical profitability certification'),indent=2)+'\n')

if __name__=='__main__':
    p=argparse.ArgumentParser(description=__doc__);p.add_argument('--spec',type=Path,default=Path('configs/research-holdout-v4.json'));p.add_argument('--output',type=Path,required=True);p.add_argument('--binary',type=Path,default=Path('target/release/kaze-research'));a=p.parse_args()
    existed=a.output.exists()
    try:build(a.spec,a.output,a.binary)
    except Exception as error:
        if not existed and a.output.exists() and not (a.output/'failure.json').exists():
            (a.output/'failure.json').write_text(json.dumps(dict(completed=False,screen_passed=False,error_type=type(error).__name__,error=str(error),protocol=json.loads((a.output/'protocol.json').read_text()) if (a.output/'protocol.json').exists() else None),indent=2)+'\n')
        raise
