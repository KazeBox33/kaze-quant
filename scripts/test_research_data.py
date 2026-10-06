#!/usr/bin/env python3
"""验证因果采样、固定前缀、源身份与异常时钟；全部离线。"""
import hashlib,io,json,tempfile,unittest,zipfile
from pathlib import Path
from unittest.mock import patch
from research_data import prepare
HEADER="update_id,best_bid_price,best_bid_qty,best_ask_price,best_ask_qty,transaction_time,event_time\n"
class Conversion(unittest.TestCase):
    def fixture(self,directory,regress=False):
        name="BTCUSDT-bookTicker-2024-01-01.zip"
        data=HEADER+"".join(f"{i},100.01,1.00001,100.02,2.00000,{1704067200000+i},{1704067200000+(i if not regress or i!=12345 else 1)}\n" for i in range(1,20002))
        path=directory/name
        with zipfile.ZipFile(path,"w",compression=zipfile.ZIP_DEFLATED) as z:z.writestr(name.replace(".zip",".csv"),data)
        checksum=hashlib.sha256(path.read_bytes()).hexdigest()
        return patch("urllib.request.urlopen",return_value=io.BytesIO((checksum+"  "+name).encode()))
    def test_prefix_identity_and_exact_units(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp)
            with self.fixture(d):m,p=prepare(d,"BTCUSDT","2024-01-01",10000)
            self.assertEqual(m["rows"],10000);self.assertEqual(m["source_rows_consumed"],10000)
            self.assertEqual(p.read_text().splitlines()[1].split(',')[2:],['100010','100020','100001','200000'])
            self.assertEqual(hashlib.sha256(p.read_bytes()).hexdigest(),m["normalized_sha256"])
    def test_sampling_keeps_first_observed_event_and_allows_eof(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp)
            with self.fixture(d):m,p=prepare(d,"BTCUSDT","2024-01-01",20000,2)
            lines=p.read_text().splitlines();self.assertEqual(m["rows"],10001)
            self.assertEqual(int(lines[2].split(',')[1])-int(lines[1].split(',')[1]),2000000)
            self.assertEqual(m["source_rows_consumed"],20001)
    def test_regression_is_rejected_instead_of_sorted_or_clamped(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp)
            with self.fixture(d,regress=True),self.assertRaisesRegex(ValueError,"timestamp regression"):prepare(d,"BTCUSDT","2024-01-01",20000)
            self.assertFalse((d/"BTCUSDT-2024-01-01-20000-sample0ms.csv").exists())
    def test_causal_quarantine_does_not_let_future_outlier_poison_watermark(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp)
            with self.fixture(d) as unused:
                pass
            archive=d/'BTCUSDT-bookTicker-2024-01-01.zip'
            with zipfile.ZipFile(archive) as z:data=z.read(z.namelist()[0]).decode()
            lines=data.splitlines();fields=lines[2].split(',');fields[-1]=str(int(fields[-1])+80000000);lines[2]=','.join(fields)
            with zipfile.ZipFile(archive,'w',compression=zipfile.ZIP_DEFLATED) as z:z.writestr(archive.name.replace('.zip','.csv'),'\n'.join(lines)+'\n')
            checksum=hashlib.sha256(archive.read_bytes()).hexdigest()
            with patch('urllib.request.urlopen',return_value=io.BytesIO((checksum+'  '+archive.name).encode())):
                m,p=prepare(d,'BTCUSDT','2024-01-01',20000,0,'causal_quarantine',100)
            self.assertEqual(m['quarantine']['rejected_rows'],1)
            self.assertEqual(m['quarantine']['reasons'],{'event_transaction_lag_over_60s':1})
            self.assertEqual(m['source_rows_consumed'],20001)
            self.assertEqual(int(p.read_text().splitlines()[2].split(',')[1]),1704067200003000000)
    def test_bad_clock_ratio_aborts_without_publishing_converted_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp)
            with self.fixture(d,regress=True),self.assertRaisesRegex(ValueError,'quality threshold'):
                prepare(d,'BTCUSDT','2024-01-01',20000,0,'causal_quarantine',0)
            self.assertFalse(list(d.glob('*.csv')))
if __name__=="__main__":unittest.main()
