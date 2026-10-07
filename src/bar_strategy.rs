//! 已确认bid K线趋势 + ATR距离预算；复用目标仓位执行器，不直接修改账户。
use crate::{
    bars::{Bar, QuoteBars, WilderAtr},
    engine::Engine,
    paper::PaperError,
    registry::CustomCheckpoint,
    strategy::{ActionBuffer, RollingMean, Strategy, StrategyView},
    target::{
        CompositionConfig, CompositionStrategy, Constraints, Decision, Schedule, SignalConfig,
        SizingConfig,
    },
    types::*,
};
use serde::{Deserialize, Serialize};
const NAME: &str = "bar-atr";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPolicy {
    pub max_spread_bps: u32,
    pub cooldown_ns: u64,
    pub min_rebalance_lots: u64,
    pub max_child_lots: u64,
    pub max_working_ns: u64,
    pub schedule: Schedule,
}
impl ExecutionPolicy {
    fn composition(&self) -> CompositionConfig {
        CompositionConfig {
            version: 1,
            signal: SignalConfig::Constant {
                exposure_bps: 10000,
            },
            sizing: SizingConfig::FixedLots { lots: 1 },
            max_spread_bps: self.max_spread_bps,
            cooldown_ns: self.cooldown_ns,
            min_rebalance_lots: self.min_rebalance_lots,
            max_child_lots: self.max_child_lots,
            max_working_ns: self.max_working_ns,
            schedule: self.schedule.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Parameters {
    pub bar_interval_ns: u64,
    pub atr_period: usize,
    pub trend_window: usize,
    pub trend_band_bps: u32,
    #[serde(with = "crate::config::money")]
    pub distance_budget_minor: i128,
    pub atr_multiple_bps: u32,
    pub min_distance_units: u64,
    pub max_target_lots: u64,
    pub execution: ExecutionPolicy,
}
impl Parameters {
    pub fn validate(&self) -> Result<(), &'static str> {
        QuoteBars::new(self.bar_interval_ns)?;
        WilderAtr::new(self.atr_period)?;
        if !(2..=2048).contains(&self.trend_window)
            || self.trend_band_bps > 1000
            || !(1..=10i128.pow(30)).contains(&self.distance_budget_minor)
            || !(1..=100000).contains(&self.atr_multiple_bps)
            || !(1..=Price::MAX).contains(&self.min_distance_units)
            || !(1..=Quantity::MAX).contains(&self.max_target_lots)
        {
            return Err("invalid bar ATR sizing/trend parameters");
        }
        self.execution.composition().validate()
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BarAtrStrategy {
    parameters: Parameters,
    bars: QuoteBars,
    atr: WilderAtr,
    mean: RollingMean,
    last_closed: Option<Bar>,
    closed_bars: u64,
    segment_bars: u64,
    missing_buckets: u64,
    exposure: bool,
    faulted: bool,
    execution: CompositionStrategy,
}
impl BarAtrStrategy {
    pub fn new(p: Parameters) -> Result<Self, &'static str> {
        p.validate()?;
        Ok(Self {
            bars: QuoteBars::new(p.bar_interval_ns)?,
            atr: WilderAtr::new(p.atr_period)?,
            mean: RollingMean::new(p.trend_window)?,
            execution: CompositionStrategy::new(p.execution.composition())?,
            parameters: p,
            last_closed: None,
            closed_bars: 0,
            segment_bars: 0,
            missing_buckets: 0,
            exposure: false,
            faulted: false,
        })
    }
    fn observe(&mut self, q: Quote) -> Result<(), &'static str> {
        if self.faulted {
            return Ok(());
        }
        let update = self.bars.push(q)?;
        if update.closed.is_some() {
            self.closed_bars += 1;
        }
        if update.missing_buckets > 0 {
            // 时间缺口不补造bar，也不把缺口前缓存继续当已预热指标；目标降为零。
            self.missing_buckets = self.missing_buckets.saturating_add(update.missing_buckets);
            self.atr = WilderAtr::new(self.parameters.atr_period)?;
            self.mean = RollingMean::new(self.parameters.trend_window)?;
            self.last_closed = None;
            self.segment_bars = 0;
            self.exposure = false;
        } else if let Some(b) = update.closed {
            self.segment_bars += 1;
            let atr = self.atr.push(&b)?;
            let mean = self.mean.push(b.close);
            if let (Some(_), Some(mean)) = (atr, mean) {
                let close = u128::from(b.close.units()) * 10000;
                if close > u128::from(mean) * u128::from(10000 + self.parameters.trend_band_bps) {
                    self.exposure = true;
                } else if close
                    < u128::from(mean) * u128::from(10000 - self.parameters.trend_band_bps)
                {
                    self.exposure = false;
                }
            }
            self.last_closed = Some(b);
        }
        Ok(())
    }
    pub fn distance_units(&self) -> Option<u64> {
        self.atr.value().map(|a| {
            ((u128::from(a) * u128::from(self.parameters.atr_multiple_bps))
                .div_ceil(10000)
                .max(u128::from(self.parameters.min_distance_units))) as u64
        })
    }
    pub fn requested_target(&self) -> u64 {
        if self.faulted || !self.exposure || self.mean.value().is_none() {
            return 0;
        }
        self.distance_units()
            .map(|d| {
                (self.parameters.distance_budget_minor / i128::from(d))
                    .min(i128::from(self.parameters.max_target_lots)) as u64
            })
            .unwrap_or(0)
    }
    pub fn diagnostics(&self) -> serde_json::Value {
        let mut diagnostics = serde_json::json!({"source":"observed bid OHLC, not traded-volume candles","closed_bars":self.closed_bars,"missing_buckets":self.missing_buckets,
            "last_closed":self.last_closed,"pending":self.bars.pending(),"atr_units":self.atr.value(),"trend_mean_units":self.mean.value(),
            "ready":!self.faulted && self.atr.value().is_some() && self.mean.value().is_some(),"exposure":self.exposure,"distance_units":self.distance_units(),
            "requested_target_lots":self.requested_target(),"faulted":self.faulted,"scope":"long-only paper signal and ATR distance sizing; no installed stop loss, exchange candle volume, future data, external venue route or alpha proof"});
        if let Some(execution) = self.execution.diagnostics() {
            diagnostics["execution"] = execution;
        }
        diagnostics
    }
    fn validate_state(&self, p: &Parameters) -> Result<(), &'static str> {
        p.validate()?;
        if &self.parameters != p
            || self.closed_bars > 1_000_000_000_000
            || self.closed_bars > self.bars.observations()
            || self.segment_bars > self.closed_bars
            || self.exposure && (self.atr.value().is_none() || self.mean.value().is_none())
        {
            return Err("bar strategy checkpoint identity/bounds conflict");
        }
        self.bars.validate_state(p.bar_interval_ns)?;
        self.atr.validate_state(p.atr_period)?;
        self.mean.validate_state(p.trend_window)?;
        self.execution.validate_state(&p.execution.composition())?;
        if self.atr.seeded() != self.segment_bars.min(p.atr_period as u64) as usize
            || self.mean.observations() != self.segment_bars.min(p.trend_window as u64) as usize
        {
            return Err("bar indicator observation counts conflict");
        }
        if let Some(b) = &self.last_closed {
            b.validate()?;
            if !b.start_ns.is_multiple_of(p.bar_interval_ns)
                || self.closed_bars == 0
                || self.atr.source() != Some((b.start_ns, b.close))
                || self.bars.pending().is_none_or(|current| {
                    b.start_ns.checked_add(p.bar_interval_ns) != Some(current.start_ns)
                })
            {
                return Err("closed bar is not preceding observed bucket");
            }
        } else if self.segment_bars != 0 || self.atr.seeded() != 0 || self.mean.observations() != 0
        {
            return Err("bar indicators lack source");
        }
        Ok(())
    }
}
impl Strategy for BarAtrStrategy {
    fn on_quote(&mut self, v: StrategyView<'_>) -> Action {
        if self.observe(v.quote).is_err() {
            self.faulted = true;
        }
        Action::None
    }
    fn on_quote_with_constraints(
        &mut self,
        v: StrategyView<'_>,
        c: Constraints,
        a: &mut ActionBuffer,
    ) {
        if self.observe(v.quote).is_err() {
            self.faulted = true;
        }
        let target = self.requested_target();
        let _ = a.push(self.execution.decide_target(v, c, target));
    }
    fn on_event(&mut self, e: Event) {
        self.execution.on_event(e);
    }
    fn on_submit_result(&mut self, r: OrderRequest, result: Result<OrderId, RejectReason>) {
        self.execution.on_submit_result(r, result);
    }
    fn on_risk_rejected(&mut self, r: OrderRequest, reason: crate::paper::RiskReject) {
        self.execution.on_risk_rejected(r, reason);
    }
    fn decision(&self) -> Option<&Decision> {
        self.execution.decision()
    }
    fn diagnostics(&self) -> Option<serde_json::Value> {
        Some(BarAtrStrategy::diagnostics(self))
    }
    fn validate_execution(&self, e: &Engine) -> Result<(), &'static str> {
        self.validate_state(&self.parameters)?;
        self.execution.validate_execution(e)?;
        if self.bars.last_input().is_some_and(|(s, t)| {
            e.last_quote()
                .is_none_or(|q| s > q.sequence || t > q.timestamp_ns)
        }) {
            return Err("bar strategy contains future quote");
        }
        Ok(())
    }
    fn custom_checkpoint(&self) -> Option<CustomCheckpoint> {
        Some(CustomCheckpoint {
            name: NAME.into(),
            version: 1,
            state: serde_json::to_value(self).expect("bounded integer strategy state"),
        })
    }
}
pub fn factory(
    parameters: &serde_json::Value,
    state: Option<&serde_json::Value>,
) -> Result<Box<dyn Strategy>, PaperError> {
    let p: Parameters = serde_json::from_value(parameters.clone())?;
    let s = if let Some(state) = state {
        let s: BarAtrStrategy = serde_json::from_value(state.clone())?;
        s.validate_state(&p)?;
        s
    } else {
        BarAtrStrategy::new(p)?
    };
    Ok(Box::new(s))
}
