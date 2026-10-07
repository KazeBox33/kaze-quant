# 0.7纸面算法增量（v16）

统一Immediate/TWAP/Iceberg/BestLimit执行完成；10个已确认阶段强杀恢复，285项debug/release Rust及15项Python通过。公开数据与固定vn.py源方法对照的范围及成本见[逐功能数据](FEATURE_SCORECARD.md)、[运行/接口](ALGORITHMIC_EXECUTION.md)。本轮没有测试网/主网下单，也没有补齐真实部分成交、非零真实费用反馈、新版24h或独立alpha门槛。远程CI待确认。

以下保留历史验收记录；较新的逐功能记录优先，不能将历史测试网费用结论合并成新算法已验收。

---

# 0.4 验收与可复现实验

最新策略功能（v13）：因果观测bid K线、收盘趋势和整数ATR距离仓位已接入标准注册表/纸面TWAP。13专项测试、本机debug/release各247 Rust、15 Python通过；百万报价741单/734成交，费用后净损益−1.83412116USDT且有未平仓。操作与首次性能基线见[BAR_ATR_STRATEGY](BAR_ATR_STRATEGY.md)、[FEATURE_SCORECARD](FEATURE_SCORECARD.md)。这不新增外部自动路由、实际止损或alpha准入；本轮未发布/未跑远程CI。

最新增量（2026-10-07）：本机debug/release各234 Rust、15 Python通过；本轮毛量与不可变净目标各三片真实测试网验收通过（各3 POST/3成交，首片丢ACK后新进程恢复，全资产核对），六笔手续费全零。真实非零费用反馈、部分成交、新版24h和alpha仍未通过；本轮未运行远程CI。最新[逐功能数据](FEATURE_SCORECARD.md)、[净目标设计](NET_EXECUTION.md)与[evidence/v12](evidence/v12/validation.json)。下面保留历史阶段的结果与失败，不能把后续增量之外的缺口当已完成。

2026-10-06，Mac 上的本地验证。该版本增加可恢复策略接入、真实实时纸面链路、Binance Spot Testnet 执行网关与成本研究。**三个生产准入门槛尚未全部通过，当前不能据此投入真实资金。** 没有主网订单端点。

## 三个门槛

| 门槛 | 已完成证据 | 尚未满足 | 结论 |
|---|---|---|---|
| 交易所执行可靠性 | 意图先持久化、一次发送、超时/不存在不重发、重启、部分成交累计量、终态冲突、撤单未知结果、成交去重和余额观测测试 | 真实测试网两轮各4单、成交/撤单、应用丢弃ACK后的独立进程恢复与真实非零手续费/全资产余额变化核对已通过；仍缺真实部分成交、线路故障/私有回报流、完整组合账本或策略自动外部路由 | 未通过 |
| 持续纸面运行与公平比较 | 10分钟行情运行及恢复；5分钟真实行情运行产生27张纸面订单、13次成交及独立进程全量审计；百万真实报价3轮；固定上游Barter组件比较 | 24小时首轮27.8分钟、第二轮53.4分钟失败；第二轮明确队列满且恢复通过。新增4096帧缓冲/积压监控/消费前时效检查，新会话重测中；多日运行及目标Linux硬件仍缺证据 | 部分完成 |
| 独立成本策略验证 | 六个整日BTC/ETH数据集，训练选参/测试冻结，3种成本情景，输入/参数/结果哈希 | 筛选失败；成交不足、费用后亏损和风控拒单。独立周协议已冻结，前三轮转换失败记录保留；第4版离线事件时间重建因84.722秒发布延迟超过预先声明的60秒上限失败。没有通过筛选、统计优势或未来稳定收益证据 | 未通过 |

“实现验收框架”和“通过准入”是两件事。下面保留失败结果，不能通过删除亏损数据、放松筛选或重调已看过的测试集将它变成独立验证。

## 与 Barter 的实测

调用实际上游源码，固定 [Barter 0.14 提交](https://github.com/barter-rs/barter-rs/tree/9770b27a83f844472b93b593b08063affc974b0d)。官方BTCUSDT futures bookTicker 2024-01-01归档连续前100000条转成JSON；同一输入逐条核对bid/ask及数量四个整数输出，再预热一次，每次7轮交替运行，共3次调用。全部21个样本/实现均公开。

| 测量范围 | Kaze | Barter | 可支持的结论 |
|---|---:|---:|---|
| 100000条L1 JSON规范化，21样本中位数 | 20.661ms | 23.721ms | Kaze吞吐约为1.148倍，处理耗时低约12.9% |
| 三次调用各自中位数之比 | — | — | 吞吐优势12.5%–16.7% |
| 更新累计成交量倒退/终态后重开两项政策 | 拒绝 | 上游Orders组件采用更新 | 我们选择保留意图/终态的严格政策；不证明整个Barter系统存在漏洞 |

优化是借用JSON字符串字段，移除每帧5次临时String分配；输出仍逐条校验。优化前一次基线Kaze23.764ms、Barter23.496ms，Kaze当时略慢。[优化前原始记录](evidence/v04/barter-comparison-before-borrow.json)、最终三次原始记录：[1](evidence/v04/barter-final-v04-1.json)、[2](evidence/v04/barter-final-v04-2.json)、[3](evidence/v04/barter-final-v04-3.json)。

两者输出类型/契约不同：Kaze借用未转义ASCII字段（含反斜杠转义的合法JSON字符串会拒绝），实际官方输入均属于此域；Kaze提供整数、符号/更新ID/数值检查和本地接收时间；Barter构造Decimal、UTC时间及订阅元数据。输入预加载，计时不含网络、撮合、策略、风控或持久化，不能据此宣称整套交易引擎胜过Barter。两项订单属性也不是完整框架故障对照。实际Barter优势仍包括适配器生态、异步实时框架和成熟组件覆盖；Kaze当前突出方向是本地确定性审计、事务恢复、自定义策略状态与可核对的成本实验。

本机未绑核，后台另有公开实时纸面行情持久化运行，未控制温度/系统活动；不挑最大速度样本作标题。脚本保留上游源码、比较二进制、Kaze相关源文件和输入摘要。复现见 [比较说明](../benchmarks/barter-comparison/README.md)。

## 完整执行链路与实时运行

[0.4百万报价三轮记录](evidence/v04/real-quotes-v04-mac.json)：相同官方归档前缀、256条事务、4096窗口均值回归，包含解析、输入摘要、策略/风控、候选状态、SQL FULL确认和检查点。耗时25.780–27.008秒，约**37026–38790命令/秒**；100ms采样峰值RSS **7.77–7.80MiB**；正常关闭后恢复13.87–14.08ms。每轮1000000条命令在新进程完整审计，最终状态与链重复相同，184次成交、净损益−19.61654460USDT（2bps假设费用），零持仓/冻结。计时不含初始化/恢复/审计；RSS不证明永不泄漏。二进制SHA在报告中。

旧0.3约4.3万/秒的数据仍保留于EVIDENCE，不能冒充0.4结果。新测量与旧测量的后台任务、二进制功能和语义不同，尚未做控制变量的版本回归归因；新功能有成本，不宣称全部指标提升。之后迁移Linux须在目标机器重测。

[5分钟实时记录](evidence/v04/live-paper-active-v04.json)及[新进程恢复](evidence/v04/live-paper-active-v04-recovery.json)：40002条BTCUSDT Spot公开报价，27张接受订单、13次纸面成交、14次撤销、0次风控拒单。总40003条命令审计通过，状态与链完全一致。净损益−0.09829677USDT，费用0.11189877USDT，仍持有0.0001BTC；finish撤单而不自动平仓。

socket read→持久确认p50=11.006ms、p95=16.345ms、p99=21.872ms，含队列/入批等待，不含交易所/网络/内核读取之前时间。最多采100000帧，尾部不足批次不进分位数；不是下单延迟或硬实时SLA。

另一份[10分钟记录](evidence/v04/live-paper-final-v04.json)与[恢复](evidence/v04/live-paper-final-v04-recovery.json)处理46863报价、没有策略订单，证明行情/持久化恢复而不是成交能力。它使用单独保留的候选二进制，不与5分钟结果合并计算延迟。

24小时首轮实际1666.001秒（27.8分钟）后失败，处理177130条报价，报告“feed disconnect/overload: fail closed”，全部177131条命令经新进程恢复审计，状态/链一致、账户仍暂停且无委托。[失败原始记录](evidence/v04/live-paper-24h-v04.json)、[恢复审计](evidence/v04/live-paper-24h-v04-failed-recovery.json)。旧记录没有区分网络错误和队列满，无法回溯精确原因；新增有界故障原因通道，将网络关闭、IO、TLS、协议、帧容量和队列过载分别记录。

随后使用新二进制、新数据库和相同passive配置重新启动24小时实验，当前仍未通过。每次须success、完整审计和新进程恢复后才能验收。断线/过期/溢出即持久停止，不能把多个新会话累加成同一次连续成功。

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

本地131项Rust测试在debug/release通过，6项Python转换测试通过；fmt、all-features/all-targets clippy -D warnings及严格rustdoc通过。测试包含可信策略工厂的每个切分点恢复、非法/非规范状态回滚、未知版本拒绝，及订单未知结果不重发。机器/结果身份见 [验证环境](evidence/v04/validation-mac.json)。核心提交dac7309的 [Linux/macOS Rust CI](https://github.com/KazeBox33/kaze-quant/actions/runs/37448290078) 已全部success；包含完整检查、策略重投、网络程序构建检查和备份恢复，身份见[CI记录](evidence/v04/ci.json)。诊断修复64ff845的[最终Linux/macOS CI](https://github.com/KazeBox33/kaze-quant/actions/runs/37449934255)也全部success，详见[最终CI记录](evidence/v04/diagnostics-ci.json)。托管runner不等于目标Linux生产机器。

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

## 本版本跨平台存储实验

同一2000命令合成负载、3轮、release，Linux托管runner逐条文件WAL中位471.156ms，SQLite FULL 256/批中位9.626ms；macOS托管runner对应1454.047ms、24.039ms。原始stdout CSV：[Linux](evidence/v04/store-2000-ci-linux.csv)、[macOS](evidence/v04/store-2000-ci-macos.csv)，任务步骤见[ci-jobs.json](evidence/v04/ci-jobs.json)。这是同步批量化与跨平台同语义运行的证据；硬件/虚拟化/文件系统不同，每轮256模式仅8次事务，不能推导稳定尾延迟或未来生产机器SLA，也不是Barter比较。

测试网公开API连通与当前BTCUSDT过滤条件已经实测：[公共连通记录](evidence/v04/testnet-public-connectivity.json)。这不需要密钥，也不证明账户权限、订单接受、成交或撤单。诊断新二进制另外完成[30秒实时冒烟](evidence/v04/live-paper-diagnostics-smoke.json)与[独立进程恢复](evidence/v04/live-paper-diagnostics-smoke-recovery.json)，2803条报价、状态与链一致；这段时间不计入新24小时会话。


## 2026-10-06 真实测试网与突发行情补充

首轮零费用：[汇总](evidence/v05/testnet-zero-fee-summary.json)、[订单/余额/调用证据](evidence/v05/testnet-zero-fee-orders.json)。随后启用非零测试网手续费：[汇总](evidence/v05/testnet-fees-summary.json)、[订单/余额/调用证据](evidence/v05/testnet-fees-orders.json)。每轮有2个被动买单最终撤销、1个穿价买单和1个穿价卖单最终成交；每单<=20 USDT，仅测试网虚拟资产。四次相同意图重投均在新进程中submit_calls=0；应用显式丢弃交易所成功ACK后，SQLite保留unknown且没有观测值，新进程按原ID查询恢复，再撤销。该故障是应用回报抑制，不是物理网络断包；计数是适配器submit调用数，不是交易所全账户POST审计。

非零轮实际2次成交，买入手续费0.00000017 BTC，卖出手续费0.01379904 USDT。成交原币种费用、订单累计量、去重交易、新进程状态均核对通过；交易前后全部账户资产的free+locked变化与成交/费用逐项一致。净变化BTC +0.00000983、USDT -0.87624074：卖出数量向下对齐LOT_SIZE后留有BTC尘埃，因此USDT差值不能解释成全部亏损或策略PnL。未观测到真实PARTIALLY_FILLED状态，仍保留该项未通过；不能用已有模拟部分成交测试替代。

第二次24小时尝试在3201.921秒后因feed queue capacity exceeded失败，275611条报价、275612条命令全部新进程恢复，状态和链一致，持久暂停无订单。[运行](evidence/v05/live-paper-24h-v04-rerun.json)、[恢复](evidence/v05/live-paper-24h-v04-rerun-recovery.json)、[未通过结论](evidence/v05/live-paper-24h-v04-rerun-validation.json)。本次新增4096帧有界FIFO，每帧上限8KiB，原始payload容量上限32MiB（不含容器/分配器/线程等开销）。pending计数包括最多一个发送中的帧，high_water是成功发送的上界估计；队列满仍立即停止，消费前超过max_quote_age_ns也立即停止，绝不以扩大缓冲授权过期策略动作。

新增[30秒冒烟](evidence/v05/live-paper-v05-smoke.json)和[恢复](evidence/v05/live-paper-v05-smoke-recovery.json)，随后冻结该二进制、新数据库启动全天实验；不能将旧会话分钟数叠加。此缓冲变化没有重新测Barter或完整吞吐，不沿用旧组件数据声称新版本更快。

独立周日期为2024-01-02..07训练、01-08..14测试，BTC/ETH、固定8候选和3成本，训练最大ask决定100 USDT名义单量，测试价格不参与单量。v1严格原顺序转换失败，v2异常隔离失败，v3事件日边界检查过严失败，均无策略回报产生：[v1](evidence/v05/holdout-v1-failure.json)、[v2](evidence/v05/holdout-v2-failure.json)、[v3](evidence/v05/holdout-v3-failure.json)。源码归档交错多个时间段，且末日交易的毫秒发布延迟可跨日；并非可以直接当线上接收顺序回放。

v4独立声明离线重建：每个UTC秒选最早的交易所事件，平局按event_time/update_id/source行号；所有原始行完整计数/哈希。归档以交易时间划日，event必须在交易后0..60秒；发布越过UTC日末的行保留计数/哈希、排除本日网格，避免训练测试重叠。未按价格或回报挑行，未改日期、策略候选、成本和筛选规则。该数据假设提供交易所事件时间排序的离线研究，不能代表在线接收因果、测量真实网络延迟或校准成交概率；期货报价用于无杠杆现货式模型的限制继续保留。v4转换完1月2日后，在1月3日第7283469行发现84.722秒的事件发布延迟，超过预先声明的60秒上限而失败；未放松阈值，也未生成策略收益。[v4失败与时间戳](evidence/v05/holdout-v4-failure.json)、[已完成1月2日转换manifest](evidence/v05/BTCUSDT-2024-01-02-utc-second-first-event.manifest.json)。这些失败说明数据质量与假设仍需解决，不支持收益门槛通过。

本地本轮133项Rust debug/release通过，15项Python通过；fmt、all-features/all-targets clippy和严格rustdoc通过，[本轮身份](evidence/v05/validation-mac.json)。新增队列FIFO/溢出不覆盖、故障注入只丢成功ACK、三币种手续费手算、独立训练单量/哈希/时间边界以及离线事件重建测试。代码提交8c16421的[本轮Linux/macOS CI](https://github.com/KazeBox33/kaze-quant/actions/runs/37457351772)已全部通过；[运行身份](evidence/v05/ci.json)、[完整步骤](evidence/v05/ci-jobs.json)公开，历史CI没有被当作本轮替代。

## 全程遥测与退出边界补充（2026-10-06）

新增固定内存全样本统计与最慢分钟摘要，修复末批遗漏、提交前最老报价检查和退出阶段错误吞没；旧全天二进制继续独立运行，不能替代新版本全天验收。[设计与实测](TELEMETRY.md)。新120秒公开行情/纸面负载12151报价、六次模拟成交，12152命令独立进程恢复全部一致；receive至durable ACK p99区间25.165824–25.690111ms；统计器82240字节。费用/净损益如实为0.00516810/−0.00509270 USDT，没有收益门槛通过。[运行](evidence/v06/live-paper-load.json)、[恢复](evidence/v06/live-paper-load-recovery.json)。

本地142项Rust debug/release、15项Python、fmt/clippy/严格rustdoc通过。本轮代码提交b29dc7c的[Linux/macOS CI](https://github.com/KazeBox33/kaze-quant/actions/runs/37464111653)均全部通过；[身份](evidence/v06/ci.json)与[完整步骤](evidence/v06/ci-jobs.json)已公开，托管runner不等于目标生产机器或全天验收。[本地来源身份](evidence/v06/validation-mac.json)。与vn.py、ABU、NautilusTrader、LEAN、HftBacktest等的能力取舍与可测量目标见[平台比较](PLATFORM_COMPARISON.md)，尚无vn.py/ABU同条件性能对照。

## 外部执行与资产基础增量

新增18项Rust测试，当前本机debug/release合计各160项、原15项Python检查通过。真实私有Testnet3次POST（1撤销、2成交），6私有执行事件、2成交，原币费用和全部资产变化匹配，独立进程恢复与6次已捕获事件再投递一致。详情、失败记录和限定范围见[EXTERNAL_LEDGER](EXTERNAL_LEDGER.md)。这是有界手工测试网能力；真实部分成交、物理断流补洞、持续策略节点、新版本24h与alpha仍未通过。自身审计组件有合成同负载改善证据，不是与vn.py/ABU/Nautilus完整比较。

本阶段代码`b6e338199ec33e199e3dba09e5d29665811f4e2f`的[CI37473198292](https://github.com/KazeBox33/kaze-quant/actions/runs/37473198292)Ubuntu/macOS全部成功；[SIGKILL恢复证据](evidence/v07/private-process-kill.json)补充验证私有认证连接后进程崩溃，不声称间隙成交/物理断流证明。


## 历史补洞与连续观察后续增量

新期初 REST 历史游标、整轮原子补查、持久连接代次与有界只读重连观察已经实现。验收、完整本地恢复计时、SQL索引修复和旧版24h失败见[HISTORY_RECOVERY](HISTORY_RECOVERY.md)。本增量替代本文之前“历史游标/连续重连尚未实现”的状态；不表示自动策略路由、全天运行、主网、真实部分成交或完整上游平台性能排名通过。下一顺序仍是目标仓位与可组合策略，再进入持久TWAP和更严谨的研究/仿真。

本阶段代码`115b727`的[CI37501360696](https://github.com/KazeBox33/kaze-quant/actions/runs/37501360696)Ubuntu/macOS全部成功；本地179项Rust debug/release、15项Python通过。相同最终二进制的实际漏收成交补回与91.642秒只读自动重连通过，完整恢复路径自身对照及两端原始轮次公开。真实部分成交、新版本24h、alpha与主网仍未通过；[完整边界和证据](HISTORY_RECOVERY.md)。

## 2026-10-07 目标仓位与纸面TWAP增量

组合策略/父子执行计划已进入同一纸面事务核心，专项语义、真实进程强杀与固定公开报价数据见[TARGET_EXECUTION](TARGET_EXECUTION.md)。这补齐可运行执行算法和解释证据；外部测试网策略路由、真实部分成交、新版本24h与独立盈利留出门槛仍未通过，不能代替原有真钱准入。


## 2026-10-07 有界外部计划增量

固定限价毛量父计划现在能够通过已有测试网网关分片执行：每次完整原币核对、父子意图同事务、一次发送许可、未知提交/撤单查询与暂停/恢复。管理账本版本阻止旧二进制和手工prepare绕过父计划；细节、限制、操作和原始证据见[EXTERNAL_PLAN](EXTERNAL_PLAN.md)。这是一个排他账户/账本/父计划的有界能力；连续composition信号到净目标、组合估值/回撤、真实部分成交、新版本24h、alpha与主网仍未通过。真实三片验收在ledger-init前置请求因502失败，0意图/0 POST；官方测试网页面显示维护，不沿用旧人工测试网实测作为此计划的验收。


## 2026-10-07 原币净目标编译增量

新增只读净目标→毛量计划编译：核对后的原币资产、free资源、网格/容量与显式整计划费用预留；输出预测净量区间和不可执行原因，不写意图、不下单。CLI预览前完整REST核对会更新账户/历史证据；库调用者需自行保证新核对和时效。本增量不持久保存/自动强制外部净目标，费用预留为调用方假设；连续策略协调、真实三片/部分成交、新版24h、alpha/主网仍未通过。[设计与数据](NET_TARGET.md)、[每功能变化](FEATURE_SCORECARD.md)。


## 2026-10-07 持久净目标执行增量

在v11只读编译之上，v12支持一份不可变净目标与固定毛量父计划同事务保存。逐子单完整REST核对后强制累计原币费用、净量方向与free资源边界；超预算暂停，重启/重复初始化/resume不能重置预算。毛量完成、净目标满足与残量分开报告，不自动补单。版本升级阻止旧二进制绕过；[设计、操作和证据](NET_EXECUTION.md)、[逐功能数据](FEATURE_SCORECARD.md)。这替代上节“目标不能持久保存/强制”的历史状态，仅限单目标单排他父计划；连续composition信号更新、多计划轮换、共享账户/估值、真实净目标非零费用/部分成交、新版24h、alpha及主网仍未完成。本轮公开time前置先502后200，真实毛量与净目标各一轮三片验收已通过（各3 POST/3成交，首片丢ACK后新进程恢复，全资产核对）；当前实际手续费均为零，非零费用另待验收，不把旧人工实测当本轮验收。


## v15 / 0.6 条件与CTA纸面能力

18项新增专项测试，debug/release各271项、15项Python检查；等待/激活/成交/OCO/退出五个已确认阶段SIGKILL后逐原始命令/回执/链和完整状态一致。4096同时激活产生820017字节回执，通过全审计4098命令和重复投递。条件失败/过期/风控停止、部分入场/退出、跳空限价未成交均有边界测试。公开百万固定策略与十万持久原回执/恢复对照是首次工程基线，不是alpha、真实条件单、真实部分成交、新24h或主网验收。完整逐功能数据见[FEATURE_SCORECARD](FEATURE_SCORECARD.md)。
