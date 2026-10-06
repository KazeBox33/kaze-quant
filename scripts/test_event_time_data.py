#!/usr/bin/env python3
"""乱序离线事件重建只选每秒最早行；不按未来价格选择，不能混用线上接收顺序。"""
import hashlib,io,tempfile,unittest,zipfile
from pathlib import Path
from unittest.mock import patch
from event_time_data import prepare
class EventGrid(unittest.TestCase):
    def test_interleaved_archive_reconstructs_first_event_without_price_selection(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp);name='BTCUSDT-bookTicker-2024-01-01.zip';p=d/name;base=1704067200000
            rows=[]
            for i in range(10000):
                # 首次看到的是稍晚、价格更好的行；必须取同秒更早的较差价格。
                rows.append(f'{i*2+2},100,1,101,1,{base+i*1000+500},{base+i*1000+500}\n')
            for i in range(10000):rows.append(f'{i*2+1},99,1,102,1,{base+i*1000+1},{base+i*1000+1}\n')
            rows.append(f'999999,99,1,102,1,{base+86400000-2},{base+86400000+3}\n')
            with zipfile.ZipFile(p,'w',compression=zipfile.ZIP_DEFLATED) as z:z.writestr(name.replace('.zip','.csv'),'update_id,best_bid_price,best_bid_qty,best_ask_price,best_ask_qty,transaction_time,event_time\n'+''.join(rows))
            sha=hashlib.sha256(p.read_bytes()).hexdigest()
            with patch('urllib.request.urlopen',return_value=io.BytesIO((sha+'  '+name).encode())):m,out=prepare(d,'BTCUSDT','2024-01-01')
            self.assertEqual(m['source_rows_consumed'],20001)
            self.assertEqual(m['boundary_event_rows_excluded'],1);self.assertEqual(m['rows'],10000)
            self.assertEqual(m['source_order_regressions'],1)
            self.assertEqual(out.read_text().splitlines()[1].split(',')[1:4],[str((base+1)*1000000),'99000','102000'])
            self.assertEqual(m['normalized_sha256'],hashlib.sha256(out.read_bytes()).hexdigest())
if __name__=='__main__':unittest.main()
