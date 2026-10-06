# 0.4 验收与可复现实验

2026-10-06，Mac 上的本地验证。该版本增加可恢复策略接入、真实实时纸面链路、Binance Spot Testnet 执行网关与成本研究。**三个生产准入门槛尚未全部通过，当前不能据此投入真实资金。** 没有主网订单端点。

## 三个门槛

| 门槛 | 已完成证据 | 尚未满足 | 结论 |
|---|---|---|---|
| 交易所执行可靠性 | 意图先持久化、一次发送、超时/不存在不重发、重启、部分成交累计量、终态冲突、撤单未知结果、成交去重和余额观测测试 | 无密钥实际订单记录；尚需测试网真实成交/撤单、网络故障后的恢复核对、费用与余额变化；无私有回报流、完整组合账本或策略自动外部路由 | 未通过 |
| 持续纸面运行与公平比较 | 10分钟行情运行及恢复；5分钟真实行情运行产生27张纸面订单、13次成交及独立进程全量审计；百万真实报价3轮；固定上游Barter组件比较 | 24小时墙钟连续运行刚启动，尚无完成报告；多日运行、网络故障演练、目标Linux硬件验证仍需补充 | 部分完成 |
| 独立成本策略验证 | 六个整日BTC/ETH数据集，训练选参/测试冻结，3种成本情景，输入/参数/结果哈希 | 筛选失败；成交不足、费用后亏损和风控拒单。没有独立于开发选择的新留出集、统计优势或未来稳定收益证据 | 未通过 |

“实现验收框架”和“通过准入”是两件事。下面保留失败结果，不能通过删除亏损数据、放松筛选或重调已看过的测试集将它变成独立验证。

## 与 Barter 的实测

调用实际上游源码，固定 [Barter 0.14 提交](https://github.com/barter-rs/barter-rs/tree/9770b27a83f844472b93b593b08063affc974b0d)。官方BTCUSDT futures bookTicker 2024-01-01归档连续前100000条转成JSON；同一输入逐条核对bid/ask及数量四个整数输出，再预热一次，每次7轮交替运行，共3次调用。全部21个样本/实现均公开。

| 测量范围 | Kaze | Barter | 可支持的结论 |
|---|---:|---:|---|
| 100000条L1 JSON规范化，21样本中位数 | 20.661ms | 23.721ms | Kaze吞吐约为1.148倍，处理耗时低约12.9% |
| 三次调用各自中位数之比 | — | — | 吞吐优势12.5%–16.7% |
| 更新累计成交量倒退/终态后重开两项政策 | 拒绝 | 上游Orders组件采用更新 | 我们选择保留意图/终态的严格政策；不证明整个Barter系统存在漏洞 |

优化是借用JSON字符串字段，移除每帧5次临时String分配；输出仍逐条校验。优化前一次基线Kaze23.764ms、Barter23.496ms，Kaze当时略慢。[优化前原始记录](evidence/v04/barter-comparison-before-borrow.json)、最终三次原始记录：[1](evidence/v04/barter-final-v04-1.json)、[2](evidence/v04/barter-final-v04-2.json)、[3](evidence/v04/barter-final-v04-3.json)。

两者输出类型/契约不同：Kaze提供整数、符号/更新ID/数值检查和本地接收时间；Barter构造Decimal、UTC时间及订阅元数据。输入预加载，计时不含网络、撮合、策略、风控或持久化，不能据此宣称整套交易引擎胜过Barter。两项订单属性也不是完整框架故障对照。实际Barter优势仍包括适配器生态、异步实时框架和成熟组件覆盖；Kaze当前突出方向是本地确定性审计、事务恢复、自定义策略状态与可核对的成本实验。

本机未绑核，后台另有公开实时纸面行情持久化运行，未控制温度/系统活动；不挑最大速度样本作标题。脚本保留上游源码、比较二进制、Kaze相关源文件和输入摘要。复现见 [比较说明](../benchmarks/barter-comparison/README.md)。

## 完整执行链路与实时运行

[0.4百万报价三轮记录](evidence/v04/real-quotes-v04-mac.json)：相同官方归档前缀、256条事务、4096窗口均值回归，包含解析、输入摘要、策略/风控、候选状态、SQL FULL确认和检查点。耗时25.780–27.008秒，约**37026–38790命令/秒**；100ms采样峰值RSS **7.77–7.80MiB**；正常关闭后恢复13.87–14.08ms。每轮1000000条命令在新进程完整审计，最终状态与链重复相同，184次成交、净损益−19.61654460USDT（2bps假设费用），零持仓/冻结。计时不含初始化/恢复/审计；RSS不证明永不泄漏。二进制SHA在报告中。

旧0.3约4.3万/秒的数据仍保留于EVIDENCE，不能冒充0.4结果。新测量与旧测量的后台任务、二进制功能和语义不同，尚未做控制变量的版本回归归因；新功能有成本，不宣称全部指标提升。之后迁移Linux须在目标机器重测。

[5分钟实时记录](evidence/v04/live-paper-active-v04.json)及[新进程恢复](evidence/v04/live-paper-active-v04-recovery.json)：40002条BTCUSDT Spot公开报价，27张接受订单、13次纸面成交、14次撤销、0次风控拒单。总40003条命令审计通过，状态与链完全一致。净损益−0.09829677USDT，费用0.11189877USDT，仍持有0.0001BTC；finish撤单而不自动平仓。

socket read→持久确认p50=11.006ms、p95=16.345ms、p99=21.872ms，含队列/入批等待，不含交易所/网络/内核读取之前时间。最多采100000帧，尾部不足批次不进分位数；不是下单延迟或硬实时SLA。

另一份[10分钟记录](evidence/v04/live-paper-final-v04.json)与[恢复](evidence/v04/live-paper-final-v04-recovery.json)处理46863报价、没有策略订单，证明行情/持久化恢复而不是成交能力。它使用单独保留的候选二进制，不与5分钟结果合并计算延迟。

24小时任务使用独立passive配置、冻结二进制和新数据库，保持Mac唤醒；当前仅标记进行中，最终须success、完整审计和新进程恢复后才能验收。若断线/过期/溢出即持久停止，不能把重新连接的新会话累加成同一次连续成功。

## 整日成本研究：筛选失败

[完整报告](evidence/v04/walk-forward-sampled-final-v04.json)、[计划](evidence/v04/research-sampled-plan.json)及相邻六份manifest记录来源与哈希。声明2024年1/2/3/4月首日BTCUSDT/ETHUSDT，4月两份官方归档404如实记录，不补选日期。六个可用整日共消费**151352660条原始行**，按因果规则每至少1秒保留下一条观察报价，共**501871条**；没有按未来时间排序或按收益过滤。

每个整日独立前半训练/后半测试，六个固定SMA/均值回归候选只按训练结果选择；预热指标不复制训练持仓。测试从独立现金/空仓开始，选择固定后测10bps/10ms、20bps/100ms、10bps/10ms加1bps滑点及增量流动性预算。期货报价仍用于无杠杆只做多纸面模型，未覆盖资金费/保证金。

| 测试日 | 10bps净损益USDT（含估计退出费用） | 测试成交数 | 20bps压力结果USDT |
|---|---:|---:|---:|
| BTC 1月1日 | +1.39350900 | 19 | −7.25598200 |
| BTC 2月1日 | −2.46700800 | 14 | −8.43801600 |
| BTC 3月1日 | 0 | 0；16273次金额风控拒绝 | 0 |
| ETH 1月1日 | −0.35428900 | 21 | −0.86677800 |
| ETH 2月1日 | −0.16211910 | 16 | −0.52833820 |
| ETH 3月1日 | −0.45757800 | 9 | −0.79975600 |

BTC固定0.01BTC单量在3月超过500USDT风险上限，导致交易被阻止；这是配置/策略不适配的实际失败，不是盈利稳定。各资产名义金额不同，不用损益绝对值排名策略。退出费用是估计，未实际卖出；没有外部排队/冲击校准。当前筛选要求每个测试情景正损益、至少20次成交、无风险拒单，只是必要工程筛选，不是充分盈利证明。报告明确screen_passed=false。

## 工程验证与来源

本地131项Rust测试在debug/release通过，6项Python转换测试通过；fmt、all-features/all-targets clippy -D warnings及严格rustdoc通过。测试包含可信策略工厂的每个切分点恢复、非法/非规范状态回滚、未知版本拒绝，及订单未知结果不重发。机器/结果身份见 [验证环境](evidence/v04/validation-mac.json)。Linux/macOS新CI的结果以对应提交Actions为准，不将旧版本CI当作本版本验证。

借鉴并注明来源：[NautilusTrader对账](https://nautilustrader.io/docs/latest/concepts/live/)、[HftBacktest成交与队列边界](https://hftbacktest.readthedocs.io/en/latest/order_fill.html)、[Freqtrade未来数据检查](https://www.freqtrade.io/en/stable/lookahead-analysis/)、[Binance官方REST订单/撤单协议](https://github.com/binance/binance-spot-api-docs/blob/master/rest-api.md)。当前未复制它们的完整队列模型、回报流或框架。

```sh
cargo build --locked --release --all-features --bins --examples
cargo test --locked --all-features --all-targets
cargo test --locked --all-features --release
python3 scripts/test_market_data.py
python3 scripts/test_research_data.py
python3 scripts/market_data.py --rows 1000000
python3 scripts/real_bench.py --output reports/new-bench --modes mean-reversion --repeats 3
python3 scripts/compare_barter.py --archive reports/datasets/BTCUSDT-bookTicker-2024-01-01.zip --output reports/new-comparison.json
python3 scripts/research_data.py --sample-ms 1000 --plan reports/new-plan.json
target/release/kaze-research reports/new-plan.json reports/new-research.json
```

完整策略模板见[STRATEGIES](STRATEGIES.md)，公开实时/测试网使用及密钥本地配置见[TESTNET](TESTNET.md)。
