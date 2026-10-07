//! 流式观测bid K线；下一桶的首个报价才确认上一桶，绝不补造无观测K线。
use crate::types::{Price, Quote};
use serde::{Deserialize, Serialize};
const MAX_OBSERVATIONS: u64 = 1_000_000_000_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bar {
    pub start_ns: u64,
    pub open: Price,
    pub high: Price,
    pub low: Price,
    pub close: Price,
    pub observations: u64,
}
impl Bar {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.low > self.open
            || self.low > self.close
            || self.high < self.open
            || self.high < self.close
            || self.low > self.high
            || !(1..=MAX_OBSERVATIONS).contains(&self.observations)
        {
            return Err("invalid observed OHLC bar");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QuoteBars {
    interval_ns: u64,
    current: Option<Bar>,
    last_sequence: Option<u64>,
    last_timestamp_ns: Option<u64>,
    seen: u64,
}
#[derive(Debug, PartialEq, Eq)]
pub struct BarUpdate {
    pub closed: Option<Bar>,
    pub missing_buckets: u64,
}
impl QuoteBars {
    pub fn new(interval_ns: u64) -> Result<Self, &'static str> {
        if !(1..=86_400_000_000_000).contains(&interval_ns) {
            return Err("bar interval outside one nanosecond..one day");
        }
        Ok(Self {
            interval_ns,
            current: None,
            last_sequence: None,
            last_timestamp_ns: None,
            seen: 0,
        })
    }
    pub fn validate_state(&self, interval_ns: u64) -> Result<(), &'static str> {
        Self::new(interval_ns)?;
        if self.interval_ns != interval_ns
            || self.seen > MAX_OBSERVATIONS
            || (self.seen == 0) != self.current.is_none()
            || self.current.is_none() != self.last_sequence.is_none()
            || self.current.is_none() != self.last_timestamp_ns.is_none()
            || self.last_sequence == Some(0)
        {
            return Err("invalid bar aggregation identity/count");
        }
        if let Some(b) = &self.current {
            b.validate()?;
            if b.observations > self.seen
                || b.start_ns != self.last_timestamp_ns.unwrap() / interval_ns * interval_ns
            {
                return Err("invalid pending bar timestamp/count");
            }
        }
        Ok(())
    }
    pub fn push(&mut self, q: Quote) -> Result<BarUpdate, &'static str> {
        q.validate()?;
        if self.seen >= MAX_OBSERVATIONS
            || self.last_sequence.is_some_and(|s| q.sequence <= s)
            || self.last_timestamp_ns.is_some_and(|t| q.timestamp_ns < t)
        {
            return Err("bar input sequence/time/count invalid");
        }
        let start = q.timestamp_ns / self.interval_ns * self.interval_ns;
        let mut closed = None;
        let mut missing = 0;
        if let Some(b) = &mut self.current {
            if b.start_ns == start {
                b.high = b.high.max(q.bid);
                b.low = b.low.min(q.bid);
                b.close = q.bid;
                b.observations += 1;
            } else {
                missing = (start - b.start_ns) / self.interval_ns - 1;
                closed = self.current.take();
            }
        }
        if self.current.is_none() {
            self.current = Some(Bar {
                start_ns: start,
                open: q.bid,
                high: q.bid,
                low: q.bid,
                close: q.bid,
                observations: 1,
            });
        }
        self.seen += 1;
        self.last_sequence = Some(q.sequence);
        self.last_timestamp_ns = Some(q.timestamp_ns);
        Ok(BarUpdate {
            closed,
            missing_buckets: missing,
        })
    }
    pub fn pending(&self) -> Option<&Bar> {
        self.current.as_ref()
    }
    pub fn observations(&self) -> u64 {
        self.seen
    }
    pub fn last_input(&self) -> Option<(u64, u64)> {
        self.last_sequence.zip(self.last_timestamp_ns)
    }
}

/// Wilder递推ATR，种子和递推均向上取整数，避免距离预算因小数截断而缩小。
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WilderAtr {
    period: usize,
    seeded: usize,
    seed_sum: u128,
    previous_close: Option<Price>,
    previous_start: Option<u64>,
    value: Option<u64>,
}
impl WilderAtr {
    pub fn new(period: usize) -> Result<Self, &'static str> {
        if !(1..=4096).contains(&period) {
            return Err("ATR period outside1..4096");
        }
        Ok(Self {
            period,
            seeded: 0,
            seed_sum: 0,
            previous_close: None,
            previous_start: None,
            value: None,
        })
    }
    pub fn push(&mut self, bar: &Bar) -> Result<Option<u64>, &'static str> {
        bar.validate()?;
        if self.previous_start.is_some_and(|t| bar.start_ns <= t) {
            return Err("ATR bars must increase in time");
        }
        let mut tr = bar.high.units() - bar.low.units();
        if let Some(c) = self.previous_close {
            tr = tr
                .max(bar.high.units().abs_diff(c.units()))
                .max(bar.low.units().abs_diff(c.units()));
        }
        self.previous_close = Some(bar.close);
        self.previous_start = Some(bar.start_ns);
        if self.seeded < self.period {
            self.seeded += 1;
            self.seed_sum += u128::from(tr);
            if self.seeded == self.period {
                self.value = Some(self.seed_sum.div_ceil(self.period as u128) as u64);
            }
        } else {
            self.value = Some(
                (u128::from(self.value.unwrap()) * (self.period - 1) as u128 + u128::from(tr))
                    .div_ceil(self.period as u128) as u64,
            );
        }
        Ok(self.value)
    }
    pub fn value(&self) -> Option<u64> {
        self.value
    }
    pub fn seeded(&self) -> usize {
        self.seeded
    }
    pub fn source(&self) -> Option<(u64, Price)> {
        self.previous_start.zip(self.previous_close)
    }
    pub fn validate_state(&self, period: usize) -> Result<(), &'static str> {
        Self::new(period)?;
        if self.period != period
            || self.seeded > period
            || self.seed_sum > self.seeded as u128 * u128::from(Price::MAX)
            || self.value.is_some_and(|v| v > Price::MAX)
            || self.value.is_some() != (self.seeded == period)
            || self.previous_close.is_none() != (self.seeded == 0)
            || self.previous_start.is_none() != (self.seeded == 0)
            || (self.seeded == 0 && self.seed_sum != 0)
        {
            return Err("invalid ATR checkpoint");
        }
        Ok(())
    }
}
