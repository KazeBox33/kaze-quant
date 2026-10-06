//! 全程固定内存延迟统计；分位数返回桶上下界，不把近似统计伪装成精确纳秒。
use serde::Serialize;

const SUB_BUCKETS: usize = 32;
const BUCKETS: usize = 1 + 64 * SUB_BUCKETS;

#[derive(Clone)]
pub struct LatencyHistogram {
    counts: [u64; BUCKETS],
    samples: u64,
    sum: u128,
    minimum: u64,
    maximum: u64,
}
impl Default for LatencyHistogram {
    fn default() -> Self {
        Self {
            counts: [0; BUCKETS],
            samples: 0,
            sum: 0,
            minimum: u64::MAX,
            maximum: 0,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LatencyRange {
    pub lower_ns: u64,
    pub upper_ns: u64,
}
fn bucket(value: u64) -> usize {
    if value == 0 {
        return 0;
    }
    let exponent = 63 - value.leading_zeros();
    let base = 1u64 << exponent;
    let sub = (((value - base) as u128 * SUB_BUCKETS as u128) >> exponent) as usize;
    1 + exponent as usize * SUB_BUCKETS + sub
}
fn bounds(index: usize) -> LatencyRange {
    if index == 0 {
        return LatencyRange {
            lower_ns: 0,
            upper_ns: 0,
        };
    }
    let index = index - 1;
    let base = 1u128 << (index / SUB_BUCKETS);
    let sub = index % SUB_BUCKETS;
    let lower = (base * (SUB_BUCKETS + sub) as u128).div_ceil(SUB_BUCKETS as u128);
    let upper = (base * (SUB_BUCKETS + sub + 1) as u128).div_ceil(SUB_BUCKETS as u128) - 1;
    LatencyRange {
        lower_ns: lower as u64,
        upper_ns: upper as u64,
    }
}
impl LatencyHistogram {
    pub fn samples(&self) -> u64 {
        self.samples
    }
    pub fn record(&mut self, ns: u64) -> Result<(), &'static str> {
        let index = bucket(ns);
        let samples = self
            .samples
            .checked_add(1)
            .ok_or("latency sample capacity exceeded")?;
        let sum = self
            .sum
            .checked_add(ns as u128)
            .ok_or("latency sum overflow")?;
        let count = self.counts[index]
            .checked_add(1)
            .ok_or("latency bucket capacity exceeded")?;
        self.samples = samples;
        self.sum = sum;
        self.counts[index] = count;
        self.minimum = self.minimum.min(ns);
        self.maximum = self.maximum.max(ns);
        Ok(())
    }
    /// 最近秩分位数：ceil(samples * percent / 100)，全程整数计算。
    pub fn percentile(&self, percent: u8) -> Option<LatencyRange> {
        if self.samples == 0 || !(1..=100).contains(&percent) {
            return None;
        }
        let rank = (self.samples as u128 * percent as u128).div_ceil(100);
        let mut cumulative = 0u128;
        for (index, count) in self.counts.iter().enumerate() {
            cumulative += *count as u128;
            if cumulative >= rank {
                let mut range = bounds(index);
                range.lower_ns = range.lower_ns.max(self.minimum);
                range.upper_ns = range.upper_ns.min(self.maximum);
                return Some(range);
            }
        }
        None
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"samples":self.samples,"min_ns":(self.samples>0).then_some(self.minimum),
            "max_ns":(self.samples>0).then_some(self.maximum),"sum_ns":self.sum.to_string(),
            "mean_floor_ns":(self.samples>0).then(|| (self.sum / self.samples as u128) as u64),
            "p50":self.percentile(50),"p95":self.percentile(95),"p99":self.percentile(99),
            "method":"nearest rank, 32 subdivisions per power of two; integer bucket interval, no sample truncation",
            "buckets":self.counts.iter().enumerate().filter(|(_,c)|**c!=0).map(|(i,c)|serde_json::json!({"range":bounds(i),"count":c})).collect::<Vec<_>>()})
    }
}

#[derive(Clone, Copy, Debug)]
pub struct QuoteTiming {
    pub received_ns: u64,
    pub dequeued_ns: u64,
}
/// 使用单调连接时钟；待提交的每条报价都必须仍新鲜，不能只检查最后一条。
pub fn validate_commit_window(
    timings: &[QuoteTiming],
    begin_ns: u64,
    max_age_ns: u64,
) -> Result<(), &'static str> {
    if timings.is_empty() {
        return Err("empty live commit window");
    }
    for t in timings {
        if t.received_ns > t.dequeued_ns || t.dequeued_ns > begin_ns {
            return Err("live timing order conflicts");
        }
        if begin_ns - t.received_ns > max_age_ns {
            return Err("feed pending batch stale: fail closed");
        }
    }
    Ok(())
}
#[derive(Default)]
pub struct LiveLatency {
    pub queue: LatencyHistogram,
    pub batch_wait: LatencyHistogram,
    pub receive_to_ack: LatencyHistogram,
    pub commit: LatencyHistogram,
    minute: Option<u64>,
    minute_receive_to_ack: LatencyHistogram,
    worst_minute: Option<MinuteLatency>,
    last_ack_ns: Option<u64>,
}
#[derive(Clone, Debug, Serialize)]
struct MinuteLatency {
    index_since_connection_start: u64,
    samples: u64,
    p99: LatencyRange,
}
fn minute_summary(index: Option<u64>, histogram: &LatencyHistogram) -> Option<MinuteLatency> {
    Some(MinuteLatency {
        index_since_connection_start: index?,
        samples: histogram.samples(),
        p99: histogram.percentile(99)?,
    })
}
fn worse_minute(a: Option<MinuteLatency>, b: Option<MinuteLatency>) -> Option<MinuteLatency> {
    match (a, b) {
        (Some(a), Some(b)) if (b.p99.upper_ns, b.samples) > (a.p99.upper_ns, a.samples) => Some(b),
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}
impl LiveLatency {
    /// 只有持久提交成功之后调用；commit每事务一次，其余每已确认报价一次。
    pub fn observe_commit(
        &mut self,
        timings: &[QuoteTiming],
        begin_ns: u64,
        ack_ns: u64,
    ) -> Result<(), &'static str> {
        validate_commit_window(timings, begin_ns, u64::MAX)?;
        if ack_ns < begin_ns || self.last_ack_ns.is_some_and(|old| ack_ns < old) {
            return Err("durable acknowledgement clock regressed");
        }
        let count = u64::try_from(timings.len()).map_err(|_| "live timing capacity exceeded")?;
        for h in [&self.queue, &self.batch_wait, &self.receive_to_ack] {
            h.samples
                .checked_add(count)
                .ok_or("latency sample capacity exceeded")?;
        }
        self.commit
            .samples
            .checked_add(1)
            .ok_or("latency sample capacity exceeded")?;
        let minute = ack_ns / 60_000_000_000;
        if self.minute != Some(minute) {
            self.worst_minute = worse_minute(
                self.worst_minute.take(),
                minute_summary(self.minute, &self.minute_receive_to_ack),
            );
            self.minute = Some(minute);
            self.minute_receive_to_ack = LatencyHistogram::default();
        }
        // 所有时钟与计数先验证。u64值/u64计数的总和必定处于u128范围。
        for t in timings {
            self.queue.record(t.dequeued_ns - t.received_ns)?;
            self.batch_wait.record(begin_ns - t.dequeued_ns)?;
            self.receive_to_ack.record(ack_ns - t.received_ns)?;
            self.minute_receive_to_ack.record(ack_ns - t.received_ns)?;
        }
        self.commit.record(ack_ns - begin_ns)?;
        self.last_ack_ns = Some(ack_ns);
        Ok(())
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({"receive_to_durable_ack":self.receive_to_ack.report(),"queue_wait":self.queue.report(),
            "batch_wait":self.batch_wait.report(),"commit_per_transaction":self.commit.report(),
            "scope":"all durably committed quotes including final partial batch; receive is socket-read time, excludes exchange/network/kernel pre-read; commit counts transactions, not quote latencies",
            "fixed_collector_bytes":std::mem::size_of::<Self>(),
            "worst_minute":worse_minute(self.worst_minute.clone(),minute_summary(self.minute,&self.minute_receive_to_ack)),
            "window_scope":"60s buckets since connection start, assigned by durable ACK time; worst p99 upper bound, sample-count tie-break; current final partial minute included, no fabricated idle samples or coordinated-omission correction"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capacity_failure_preserves_histogram() {
        let mut counts = [0; BUCKETS];
        counts[0] = u64::MAX;
        let mut h = LatencyHistogram {
            samples: u64::MAX,
            counts,
            minimum: 0,
            ..LatencyHistogram::default()
        };
        let before = h.report();
        assert!(h.record(1).is_err());
        assert_eq!(h.report(), before);
    }
}
