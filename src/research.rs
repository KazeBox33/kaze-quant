//! 预先声明训练/测试边界；每折仅按训练分数选参数，测试期间参数冻结。
use crate::config::{PaperConfig, StrategyConfig};
use crate::engine::Engine;
use crate::paper::{Command, Envelope, PaperError, PaperRuntime};
use crate::strategy::{Strategy, StrategyView};
use crate::types::Quote;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fold {
    pub train_start_ns: u64,
    pub train_end_ns: u64,
    pub test_start_ns: u64,
    pub test_end_ns: u64,
}
impl Fold {
    pub fn validate(&self) -> Result<(), PaperError> {
        if self.train_start_ns > self.train_end_ns
            || self.train_end_ns >= self.test_start_ns
            || self.test_start_ns > self.test_end_ns
        {
            return Err("training must strictly precede nonempty test interval".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostScenario {
    pub name: String,
    pub fee_bps: u32,
    pub latency_ns: u64,
    #[serde(default)]
    pub slippage_bps: u32,
    #[serde(default)]
    pub liquidity_model: crate::engine::LiquidityModel,
}
#[derive(Clone, Debug, Serialize)]
pub struct Evaluation {
    pub quote_count: u64,
    pub first_ns: Option<u64>,
    pub last_ns: Option<u64>,
    pub state: crate::paper::MarketReport,
    #[serde(with = "crate::config::money")]
    pub exit_fee_estimate_minor: i128,
    #[serde(with = "crate::config::money")]
    pub liquidation_adjusted_pnl_minor: i128,
}
/// 数据源在回调外负责哈希与源版本固定；训练和测试使用同一整数报价契约。
pub fn evaluate<I>(
    config: &PaperConfig,
    strategy: &StrategyConfig,
    fold: &Fold,
    test: bool,
    cost: &CostScenario,
    quotes: I,
) -> Result<Evaluation, PaperError>
where
    I: IntoIterator<Item = Result<Quote, PaperError>>,
{
    fold.validate()?;
    if config.markets.len() != 1 || cost.fee_bps > 10000 {
        return Err("research requires one market and valid fee".into());
    }
    let mut cfg = config.clone();
    cfg.markets[0].strategy = strategy.clone();
    cfg.markets[0].engine.fee_bps = cost.fee_bps;
    cfg.markets[0].engine.latency_ns = cost.latency_ns;
    cfg.markets[0].engine.slippage_bps = cost.slippage_bps;
    cfg.markets[0].engine.liquidity_model = cost.liquidity_model;
    cfg.validate()?;
    let mut warm = strategy.build()?;
    let dummy = Engine::new(cfg.markets[0].engine.clone())?;
    let mut runtime: Option<PaperRuntime> = None;
    let mut count = 0;
    let mut first = None;
    let mut last = None;
    let mut previous: Option<Quote> = None;
    for quote in quotes {
        let q = quote?;
        q.validate()?;
        if previous.is_some_and(|p| q.sequence <= p.sequence || q.timestamp_ns < p.timestamp_ns) {
            return Err("research input sequence/clock regression".into());
        }
        previous = Some(q);
        if test && q.timestamp_ns >= fold.train_start_ns && q.timestamp_ns <= fold.train_end_ns {
            // 训练数据只预热指标，账户从零持仓独立预算开始；丢弃训练订单动作。
            warm.on_quote(StrategyView {
                quote: q,
                account: dummy.account(),
                active_orders: 0,
            });
        }
        let (start, end) = if test {
            (fold.test_start_ns, fold.test_end_ns)
        } else {
            (fold.train_start_ns, fold.train_end_ns)
        };
        if q.timestamp_ns < start || q.timestamp_ns > end {
            continue;
        }
        if runtime.is_none() {
            let mut r = if test {
                PaperRuntime::with_strategies(cfg.clone(), vec![Box::new(warm.clone())])?
            } else {
                PaperRuntime::new(cfg.clone())?
            };
            r.configure_retention(64)?;
            runtime = Some(r);
        }
        let r = runtime.as_mut().unwrap();
        r.process(&Envelope {
            seq: r.processed() + 1,
            command: Command::Quote {
                market: 0,
                quote: q,
            },
        })?;
        count += 1;
        first.get_or_insert(q.timestamp_ns);
        last = Some(q.timestamp_ns);
    }
    let mut r = runtime.ok_or("evaluation interval contains no quotes")?;
    r.process(&Envelope {
        seq: r.processed() + 1,
        command: Command::Finish {},
    })?;
    r.check_invariants()?;
    let state = r.report().markets.remove(0);
    let engine = r.engine(0).ok_or("missing market")?;
    let notional = i128::from(state.position_units)
        * i128::from(engine.last_quote().ok_or("no quote")?.bid.units());
    let exit_fee = (notional * i128::from(cost.fee_bps) + 9999) / 10000;
    Ok(Evaluation {
        quote_count: count,
        first_ns: first,
        last_ns: last,
        liquidation_adjusted_pnl_minor: state.pnl_minor - exit_fee,
        exit_fee_estimate_minor: exit_fee,
        state,
    })
}
/// 并列按预先声明候选顺序确定；不查看测试分数。
pub fn choose_training(results: &[Evaluation]) -> Result<usize, PaperError> {
    results
        .iter()
        .enumerate()
        .max_by(|(ai, a), (bi, b)| {
            a.liquidation_adjusted_pnl_minor
                .cmp(&b.liquidation_adjusted_pnl_minor)
                .then_with(|| bi.cmp(ai))
        })
        .map(|(i, _)| i)
        .ok_or_else(|| "empty candidate grid".into())
}
