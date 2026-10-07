# 策略接入与研究

内置 `passive`、`threshold`、`momentum`、`mean_reversion`、`sma_cross` 通过 JSON 配置；SMA 使用两个预分配环形窗口和整数增量和。`fast < slow`，`band_bps` 抑制微小交叉。它们是工程和研究起点，没有通过盈利准入。

自定义 Rust 策略实现 `Strategy`：只读借用 `StrategyView`，返回 `Action` 或填充有界 `ActionBuffer`；执行回报通过 `on_event` 消费。账户和冻结只能由执行内核修改。需要恢复时实现 `custom_checkpoint`，并提供显式版本化工厂。

可运行模板在 [examples/support/pulse.rs](../examples/support/pulse.rs)。工厂反序列化参数和状态，验证 period、数量、计数上限及配置一致性；`StrategyRegistry::register("pulse",1,factory)` 完成注册。配置如下：

```json
{"type":"registered","name":"pulse","version":1,"parameters":{"period":4,"quantity":1}}
```

```sh
mkdir -p reports
cargo run --locked --release --example durable_strategy -- reports/pulse.db
cargo run --locked --release --example durable_strategy -- reports/pulse.db
```

第二次运行只重投相同输入，策略计数器、成交和余额不重复推进。`SqliteSession::open_with_registry` 将工厂传给启动、事务候选恢复和全量审计。注册函数编译进自己的运行二进制；公共 CLI 不会动态读取用户的 Rust 文件。缺少同版本工厂、配置不一致或损坏状态会失败。

策略不能读取未来行情、墙钟、随机全局状态、网络或隐藏文件并仍声称可确定恢复；完整审计会检查回执和最终状态，不能证明任意第三方代码安全。工厂应在自己的单元测试中覆盖所有检查点切分、边界和参数版本变更。改变行为时升级策略版本，并使用新二进制/会话。

## Walk-forward

```sh
python3 scripts/research_data.py --sample-ms 1000 --plan reports/research-plan.json
cargo run --locked --release --bin kaze-research -- reports/research-plan.json reports/research-result.json
```

先声明每月第一日、BTC/ETH、6个候选；404记录为不可用，没有替换日期。采样保留距离上一保留事件至少1秒的首个实际事件，无插值、不看下一个报价、不排序时钟。这用于秒级研究，不能验证采样间的成交或 HFT。

每个可用日期按时间前后半日分训练/测试。训练只用于排序候选与预热指标；测试从平仓、独立现金账户开始，第一费用情景选参，另外情景只评估冻结的选择。每次真正读取的整个CSV与计划SHA-256核对，源校验和及配置写入报告。数据跨度不足、交易不足、风控拒单和负收益必须保留。

`screen_passed` 要求每折、每成本情景净盈亏为正、至少20次成交、无风控拒单。它只是探索性必要条件，不是统计显著性、样本外盈利保证或真钱许可。短前缀与已看过的样本也不构成独立保留集；当前研究仍缺更多日期、资产、组合约束和实际成交误差校准。


## v13 因果K线与ATR纸面策略

标准注册表新增bar-atr v1：只用已确认观测bid OHLC，Wilder ATR整数向上舍入、收盘趋势与距离预算生成持续目标，复用同一TWAP/资源/硬风控状态机。缺桶不补造bar，重置预热并目标归零；EOF不强行收盘。pending bar/指标/父子状态同事务，报告可查看指标解释。操作、边界和手算买卖见[BAR_ATR_STRATEGY](BAR_ATR_STRATEGY.md)，性能/回归见[FEATURE_SCORECARD](FEATURE_SCORECARD.md)。该能力已驱动纸面策略订单，尚无持续策略到测试网、实际止损或独立alpha证明。


## breakout-bracket v1与条件回调

新增`Action::SubmitConditional/CancelConditional`、`on_conditional_event`与恢复时`validate_conditionals`默认钩子；自定义策略需要跟踪自己的条件ID与子单映射。标准策略参数/单次阶段/恢复/未保护部分入场范围见[CONDITIONAL_ORDERS](CONDITIONAL_ORDERS.md)。使用PaperRuntime/kaze-run；轻量CSV replay不支持执行条件动作。
