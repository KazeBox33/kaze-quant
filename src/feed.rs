//! L1 bookTicker 是更新通知，不是完整逐档流；update ID 允许跳号。
use crate::config::InstrumentUnits;
use crate::decimal;
use crate::paper::PaperError;
use crate::types::{Price, Quote};
use serde::Deserialize;
#[derive(Deserialize)]
/// 字段借用整帧字符串；normalize 返回整数值 Quote 后即可释放原帧。
struct Ticker<'a> {
    u: u64,
    s: &'a str,
    b: &'a str,
    #[serde(rename = "B")]
    bid_qty: &'a str,
    a: &'a str,
    #[serde(rename = "A")]
    ask_qty: &'a str,
}
pub struct FeedNormalizer {
    symbol: String,
    units: InstrumentUnits,
    last_id: u64,
    last_time: u64,
    sequence: u64,
    last_quote: Option<Quote>,
}
impl FeedNormalizer {
    pub fn new(
        symbol: String,
        units: InstrumentUnits,
        sequence: u64,
        timestamp_ns: u64,
    ) -> Result<Self, PaperError> {
        units.validate()?;
        if symbol.is_empty()
            || symbol.len() > 20
            || !symbol
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return Err("invalid feed symbol".into());
        }
        Ok(Self {
            symbol,
            units,
            last_id: 0,
            last_time: timestamp_ns,
            sequence,
            last_quote: None,
        })
    }
    pub fn normalize(&mut self, raw: &str, received_ns: u64) -> Result<Option<Quote>, PaperError> {
        if raw.len() > 8192 {
            return Err("feed frame exceeds 8 KiB".into());
        }
        let t: Ticker = serde_json::from_str(raw)?;
        if t.s != self.symbol || t.u == 0 || received_ns < self.last_time {
            return Err("feed symbol/update ID/clock mismatch".into());
        }
        if t.u < self.last_id {
            return Err("regressing exchange update ID".into());
        }
        let price = |s: &str| -> Result<Price, PaperError> {
            let numerator = decimal::parse(s)?
                .checked_mul(i128::from(self.units.money_scale))
                .ok_or("feed price overflow")?;
            let denominator = decimal::SCALE * i128::from(self.units.quantity_scale);
            if numerator % denominator != 0 {
                return Err("feed price outside exact instrument scale".into());
            }
            Price::new(u64::try_from(numerator / denominator).map_err(|_| "feed price overflow")?)
                .map_err(Into::into)
        };
        let q = Quote {
            sequence: self
                .sequence
                .checked_add(1)
                .ok_or("feed sequence overflow")?,
            timestamp_ns: received_ns,
            bid: price(t.b)?,
            ask: price(t.a)?,
            bid_quantity: decimal::rescale(
                decimal::parse(t.bid_qty)?,
                self.units.quantity_scale,
                false,
            )?,
            ask_quantity: decimal::rescale(
                decimal::parse(t.ask_qty)?,
                self.units.quantity_scale,
                false,
            )?,
        };
        q.validate()?;
        if t.u == self.last_id {
            let old = self.last_quote.ok_or("missing duplicate identity")?;
            if (old.bid, old.ask, old.bid_quantity, old.ask_quantity)
                != (q.bid, q.ask, q.bid_quantity, q.ask_quantity)
            {
                return Err("conflicting duplicate exchange update ID".into());
            }
            self.last_time = received_ns;
            return Ok(None);
        }
        self.last_quote = Some(q);
        self.sequence = q.sequence;
        self.last_id = t.u;
        self.last_time = received_ns;
        Ok(Some(q))
    }
    pub fn exchange_update_id(&self) -> u64 {
        self.last_id
    }
}
