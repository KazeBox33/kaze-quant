#!/usr/bin/env python3
"""离线手算金额守恒与实时网格边界；不访问网络和密钥。"""
import unittest
from testnet_acceptance import units, grid, make_intent, check_balances

class Economics(unittest.TestCase):
    def snapshot(self):
        return dict(environment='binance-spot-testnet', book=dict(symbol='BTCUSDT',bidPrice='100.00',askPrice='100.01'),
            exchange_info=dict(symbols=[dict(symbol='BTCUSDT',baseAsset='BTC',quoteAsset='USDT',status='TRADING',filters=[
                dict(filterType='PRICE_FILTER',tickSize='0.01',minPrice='0.01',maxPrice='100000'),
                dict(filterType='LOT_SIZE',stepSize='0.001',minQty='0.001',maxQty='100'),
                dict(filterType='NOTIONAL',minNotional='5',maxNotional='1000')])]))
    def account(self, **values):
        return dict(balances=[dict(asset=k,free=v,locked='0') for k,v in values.items()])
    def test_precise_cap_and_rounding(self):
        s=self.snapshot();i=make_intent(s,'BTCUSDT','kaze-example','Buy')
        self.assertEqual(i['price'],'100.12000000')
        self.assertEqual(i['quantity'],'0.14900000')
        self.assertLessEqual(units(i['price'])*units(i['quantity']),20*10**16)
        self.assertEqual(grid(101,10),100);self.assertEqual(grid(101,10,True),110)
        with self.assertRaises(ValueError):make_intent(s,'BTCUSDT','kaze-example','Sell',quantity=units('0.01'))
        with self.assertRaises(ValueError):make_intent(s,'BTCUSDT','kaze-example','Buy',quantity=units('1'))
        with self.assertRaises(ValueError):units('0.000000001')
    def test_base_and_third_currency_fee_and_external_change(self):
        trades=[dict(symbol='BTCUSDT',qty='0.1',quoteQty='10',isBuyer=True,commission='0.001',commissionAsset='BTC'),
                dict(symbol='BTCUSDT',qty='0.099',quoteQty='9.8',isBuyer=False,commission='0.002',commissionAsset='BNB')]
        before=self.account(BTC='1',USDT='100',BNB='1')
        after=self.account(BTC='1',USDT='99.8',BNB='0.998')
        r=check_balances(before,after,trades,'BTC','USDT');self.assertTrue(r['matched'])
        self.assertEqual(r['economic_deltas'],dict(BTC='0.00000000',USDT='-0.20000000',BNB='-0.00200000'))
        changed=self.account(BTC='1',USDT='99.9',BNB='0.998')
        self.assertFalse(check_balances(before,changed,trades,'BTC','USDT')['matched'])
    def test_locked_is_part_of_total_and_duplicate_rejected(self):
        before=self.account(USDT='100');after=dict(balances=[dict(asset='USDT',free='90',locked='10')])
        self.assertTrue(check_balances(before,after,[],'BTC','USDT')['matched'])
        with self.assertRaises(ValueError):check_balances(before,dict(balances=after['balances']*2),[],'BTC','USDT')

if __name__=='__main__':unittest.main()
