import unittest
from market_data import exact_price,quantity
class ConversionTests(unittest.TestCase):
    def test_price_is_cost_per_integer_lot(self):
        self.assertEqual(exact_price("42314.00000000"),42314000)
        self.assertEqual(42314000*1000,42314000000)  # .01 BTC 的 423.14 USDT，金额 1e-8 USDT。
    def test_off_grid_decimal_price_never_silently_rounds(self):
        for value in ["0", "-1", "0.00000001", "1000000001"]:
            with self.assertRaises(ValueError):exact_price(value)
    def test_liquidity_quantization_only_rounds_down(self):
        self.assertEqual(quantity("2.90600000"),290600)
        self.assertEqual(quantity("0.00001999"),1)
        self.assertEqual(quantity("0"),0)
        for value in ["-0.01","10000.00001"]:
            with self.assertRaises(ValueError):quantity(value)
if __name__=="__main__":unittest.main()
