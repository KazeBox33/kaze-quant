# 因果K线与ATR仓位策略 · v13

这轮新增能持续产生目标仓位并驱动纸面订单的策略，标准CLI注册名为`bar-atr`、版本1。链路是报价→已确认bid K线→收盘趋势→ATR距离预算→整数目标→已有TWAP/资金冻结/撮合。它与v12外部净目标不同：本策略已经在纸面运行；没有自动接到测试网或主网。

## 直接运行新策略

```sh
cargo build --locked --release --bin kaze-run
target/release/kaze-run --config configs/bar-atr-demo.json \
  --input data/bar-atr-demo.jsonl --db reports/my-bar-demo.db \
  --finish --verify-full --report reports/my-bar-demo.json
target/release/kaze-run --config configs/bar-atr-demo.json \
  --db reports/my-bar-demo.db --recover-only --verify-full \
  --report reports/my-bar-recovery.json
```

固定教学曲线32条报价，15根已确认bar，最后一根pending；产生买30@105、卖30@108两笔成交，按10bps逐笔向上取费用各4，最终持仓0、费用8、净变化82最小SIM货币单位。它是可手算工程示例，不是独立盈利证据。

公开行情配置`configs/btc-bar-atr-v1.json`冻结1秒bar、ATR14、收盘趋势窗口20、1bps趋势带、2倍ATR距离、最小距离100价格单位、距离预算1000000原始货币minor、目标上限200 lot和原有四片TWAP。参数在本轮性能/交易数据产生前固定，没有按利润调参。

```sh
target/release/kaze-run --config configs/btc-bar-atr-v1.json \
  --quotes reports/datasets/btc-quotes-1000000.csv \
  --db reports/my-btc-bar.db --quiet --verify-full --report reports/my-btc-bar.json
```

公开CSV生成方式沿用[行情说明](EVIDENCE.md)及固定SHA；不是把期货报价当真实Spot成交。复现完整新策略负载可用现有`scripts/target_bench.py --config configs/btc-bar-atr-v1.json`，新组件首个基线与旧配置回归分开报告，不能把两个不同策略的耗时叫算法提速。

## 因果边界

`QuoteBars`按输入时间固定网格聚合bid的open/high/low/close和报价观测数。下一桶首条报价到达才确认上一桶，其价格不能进入上一桶。EOF和Finish不把pending强行收盘，也不在关闭之后补发策略动作。这里没有成交量、成交价K线、交易日历或交易所K线修订；本地接收时间与归档UTC时间的解释由输入适配器决定。

输入sequence严格增加、timestamp不倒退；失败前验证，旧状态不改。没有观测的桶不填造K线。时间跳过整桶时，策略丢弃刚确认的缺口前bar作为指标输入，重置ATR与趋势预热，目标降为0；已有子单先撤销，撤单回报后才根据实际仓位退出。PaperRuntime的行情时效硬风控仍优先于策略；被它暂停时不会继续发退出委托，也不会自动恢复。

趋势仅根据已确认bar的close相对收盘滚动均值判定：超过正带进入多头、低于负带回到0、带内保留原信号。ATR衡量波动，不判方向。两个指标预热完成前目标为0，pending内价格变动不会更新ATR/趋势目标。

TR取本bar高低差、high与前close差的绝对值、low与前close差的绝对值的最大值；第一根没有前close，用高低差。前N个TR向上取整数均值作为ATR种子，之后用`ceil(((N−1)×ATR + TR)/N)`递推。与浮点RMA不同，种子及每次递推均向上量化，避免距离预算被截断低估。零波动由显式最小距离防止除零。

目标量是`floor(distance_budget_minor / max(min_distance_units, ceil(ATR×multiple_bps/10000)))`，再限制策略目标上限、市场持仓上限和数量网格。价格单位×lot=货币minor，用户须按InstrumentUnits填写距离预算。**该预算只是仓位计算用的距离预算，不是止损订单，也不保证实际最大亏损**；手续费、跳价、成交失败与持仓风险由现有核算/风控和后续策略规则处理。

## 接入与恢复

`StrategyRegistry::standard()`内置这一个工厂，默认PaperRuntime/SqliteSession使用标准表；`StrategyRegistry::default()`仍为空，显式自定义注册表保持原行为。配置采用已有Registered(name/version/parameters)，参数/状态仍受8KiB/64KiB限制。趋势窗口最多2048，ATR周期最多4096，聚合与ATR固定空间，趋势预分配窗口；不读取脚本或动态库。

指标、pending bar、趋势状态与已有父子执行状态一起检查点提交。恢复核对参数/时间/窗口和指标计数、上一收盘来源及执行器子单身份；全量审计重新播放原始报价/成交回执。版本/参数不同不能继续同一会话，完整历史损坏仍拒绝。

独立使用Bar/QuoteBars/WilderAtr的原始serde状态时，先调用`validate_state`再消费；注册工厂已在恢复时执行这些校验。

报告新增可选`strategy_diagnostics`，包含last_closed/pending、ATR、trend_mean、distance、requested_target、ready/faulted和缺桶统计，只在生成报告时计算。原策略无该字段，旧配置逐回执/状态对照继续验证。`requested_target`是信号/距离预算目标，实际决策中的`target_lots`还要经过市场上限、网格与价差过滤，成交也可能受资金或硬风控限制。

可信Rust扩展可复用`CompositionStrategy::decide_target()`提供整数目标，不能修改账户；当前入口保留预计仓位、单工作子单、变化先撤单、累计TWAP与完整回报握手。不绕过硬风控。

## 实测与范围

手算：TR2/4得到ATR3、2倍距离6、预算120→20 lot；下一根TR14得到向上ATR9、距离18→6 lot，未收盘期间目标不变。32报价买卖示例逐报价重启后原始回执/状态一致；SQLite每3条重开、未收盘bar/指标恢复和失败批次回滚通过。硬持仓9与网格3、历史容量1仍限制新策略；未来pending bar恢复拒绝。

完整功能、各项基线/旧版回归与未通过项见[数据表](FEATURE_SCORECARD.md)。新策略的行情/费用/滑点模型复用本内核，恢复相等不是独立交易所成交oracle。真实持久信号到测试网、实际止损、独立训练/测试alpha、新版24h和上游全平台对照仍待后续实现。

指标定义参考[TradingView官方ATR说明](https://www.tradingview.com/support/solutions/43000501823-average-true-range-atr/)及[RMA/TR函数说明](https://www.tradingview.com/charting-library-docs/latest/custom_studies/PineJS-Utility-Functions/)（2026-10-07访问），自行实现整数因果版本，没有复制上游代码。
