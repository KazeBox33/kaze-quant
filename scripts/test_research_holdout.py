#!/usr/bin/env python3
"""独立研究训练单量、合并哈希及时间边界的离线验证。"""
import csv,tempfile,unittest
from pathlib import Path
from market_data import sha_file
from research_holdout import merge,training_quantity
class Holdout(unittest.TestCase):
    def part(self,directory,name,clock):
        path=directory/name
        path.write_text('sequence,timestamp_ns,bid,ask,bid_quantity,ask_quantity\n'+f'1,{clock},999,1000,10,10\n')
        return dict(normalized_sha256=sha_file(path)),path
    def test_training_quantity_and_capacity(self):
        self.assertEqual(training_quantity('100',50000000),200)
        with self.assertRaises(ValueError):training_quantity('100',0)
        with self.assertRaises(ValueError):training_quantity('100',1)
    def test_chronological_merge_renumbers_and_checks_hash(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp);a=self.part(d,'a.csv',10);b=self.part(d,'b.csv',20)
            result=merge([a,b],d/'all.csv')
            self.assertEqual((result['rows'],result['first_ns'],result['last_ns'],result['max_ask']),(2,10,20,1000))
            with (d/'all.csv').open() as f:self.assertEqual([r['sequence'] for r in csv.DictReader(f)],['1','2'])
            a[1].write_text(a[1].read_text()+'corruption')
            with self.assertRaisesRegex(ValueError,'source changed'):merge([a],d/'changed.csv')
            self.assertFalse((d/'changed.csv').exists())
    def test_backward_days_are_rejected_instead_of_sorted(self):
        with tempfile.TemporaryDirectory() as tmp:
            d=Path(tmp);a=self.part(d,'a.csv',20);b=self.part(d,'b.csv',10)
            with self.assertRaisesRegex(ValueError,'regression'):merge([a,b],d/'all.csv')
            self.assertFalse((d/'all.csv').exists())
if __name__=='__main__':unittest.main()
