# KazeQuant与成熟量化平台：借鉴与超越的验收路线

调研日期2026-10-06。核心结论：KazeQuant目前是有可复现证据的Rust回放/纸面平台和独立测试网订单网关，尚未在完整交易生态、市场覆盖、研究工具或实盘能力上超过vn.py、ABU或NautilusTrader。Rust并不自动保证更低延迟、无泄漏、正确账务或盈利。本路线选择可以量化验收的优势，同时补齐真正影响交易闭环的缺口。

本次直接阅读vn.py事件/网关、独立CTA模板/回测、TWAP，以及ABU因子、仓位、监督、并行研究与指标代码。所读七个仓库来源均固定上游提交与文件内容哈希；初次匿名API限流后通过只读连接器补齐，失败请求也保留。在线文档仅记录访问日期，latest链接仍可能变化。[来源清单](evidence/v06/platform-sources.json)保存SHA/文件哈希/失败请求。没有根据Star数、README自述或语言推导性能排名；也没有本轮安装运行这些平台做同条件计时。

## 它们各自在解决什么问题

| 平台 | 阅读确认的定位与强项 | KazeQuant应吸收什么 | 当前差距 |
|---|---|---|---|
| [vn.py / VeighNa](https://github.com/vnpy/vnpy) | Python事件驱动交易平台；独立gateway/app生态、CTA/组合/价差/执行算法及桌面工具 | 网关能力声明、策略生命周期/回报、目标持仓和执行算法、运维可见性 | 我们只有一个外部测试网、CLI和独立预算现货模型，没有国内期货/股票规则与GUI生态 |
| [ABU](https://github.com/bbfamily/abu) | Python量化研究体系；买卖因子、选股、仓位、监督过滤、并行择时与绩效分析 | 信号/仓位/过滤分离，研究实验编排、解释性报告 | 我们策略/统计/数据研究工具少，独立周数据转换仍失败，不能说研究更成熟 |
| [NautilusTrader](https://github.com/nautechsystems/nautilus_trader) | Rust核心加Python接口、事件驱动回测/实盘、执行与组合对账 | 同一策略核心、明确外部命令未知状态、启动/运行对账、类型化资产模型 | 已补受限Spot原币账本与有界私有流，仍缺组合估值/连续实盘节点；它是最接近的系统架构参照 |
| [LEAN](https://github.com/QuantConnect/Lean) | C#/Python多资产算法引擎；研究/回测/实盘与算法框架 | universe→alpha→portfolio→risk→execution职责拆分、交易日历与资产规则 | 我们不能把独立资产预算当成共享资金组合，也未支持企业行动/保证金 |
| [HftBacktest](https://github.com/nkaz001/hftbacktest) | 市场回放、部分成交/队列位置与行情/下单/回报延迟模型 | 真实L2+逐笔成交驱动的队列模型、实测延迟校准 | 我们的DeltaBudget只是保守L1预算，不能声称知道外部排队/冲击 |
| [Freqtrade](https://github.com/freqtrade/freqtrade) | 加密货币策略工具；提供lookahead/recursive等诊断 | 对已触发信号做切片对照、指标预热/递归稳定性检查 | 当前严格前缀接口减少直接未来读取，但可信策略仍可能使用外部信息，不能宣称消除所有偏差 |

这张表是能力映射，不是质量总排名。ABU源码研究流程也不能等同于已验证的低延迟实盘OMS；vn.py的广泛接口同样不能替代本项目的逐项故障实验。

## 从vn.py借鉴：网关、生命周期与执行

[EventEngine源码](https://github.com/vnpy/vnpy/blob/c6e231caf32b7fc97e6459817fff66458cf7e7c4/vnpy/event/engine.py)使用按事件类型分派的队列、处理线程和定时线程；[BaseGateway](https://github.com/vnpy/vnpy/blob/c6e231caf32b7fc97e6459817fff66458cf7e7c4/vnpy/trader/gateway.py)定义行情/订单/成交等推送及连接、订阅、下单、撤单、查询边界。[CTA模板](https://github.com/vnpy/vnpy_ctastrategy/blob/7a8768de9784dda35a7b261a7ade1dbfbff50919/vnpy_ctastrategy/template.py)提供初始化/启动/停止与tick/bar/order/trade回调，并有TargetPosTemplate。

KazeQuant已有Strategy回调/有界动作、PaperRuntime、SQLite候选状态和版本化工厂。下一步增加统一VenueCapabilities与ExecutionAdapter，显式声明订单类型、撤改单、价格数量过滤、私有流/历史查询和恢复能力；不支持的能力在启动时拒绝。规范化事件须保存venue/account/instrument/strategy/client-order身份、交易所/本地时间、原始来源；当前没有此完整统一接口。

策略生命周期的启动边界应是：配置与版本验证→检查点恢复→交易所核对→预热完成→Ready。断线/未知订单/余额不符进入暂停，不能通过重启自动交易。回测、纸面、测试网复用策略状态与决策核心，外部成交/撤单时机由各环境适配器产生；不能声称三个环境必然相同成交。

[TWAP源码](https://github.com/vnpy/vnpy_algotrading/blob/bee959dc464749f7cce66e766249ccdbb2d4869a/vnpy_algotrading/algos/twap_algo.py)按定时器间隔分批、检查限价与剩余成交量，是值得实现的第一种执行算法。我们的版本应将parent/child ID、计划时间、已成交/待确认/待撤单量一起持久化，整数lot分配余数，订单未终结/取消结果未知时不重复补量，策略停止后仍可核对遗留订单。**TWAP是如何执行目标仓位，不是盈利策略**。目前尚未实现；不能简单把定时回调接到Testnet POST。

vn.py这个核心EventEngine快照使用未设置maxsize的Queue；我们的实时入口已有4096帧/8KiB上限。可验证差异是指定入口的容量约束，不是整个vn.py都无风控或全系统内存无上限。我们保留有界类型化单写入者，不照搬字符串广播队列进入执行热路径。UI/研究放控制平面，不能阻塞账本写入。

## 从ABU/LEAN借鉴：可组合策略与研究

[ABU买入因子](https://github.com/bbfamily/abu/blob/d602d847677e4c2b77b0a122df30816ea68b5710/abupy/FactorBuyBu/ABuFactorBuyBase.py)组合仓位类、滑点类、选股和卖出因子，并通过监督模块过滤；[仓位基类](https://github.com/bbfamily/abu/blob/d602d847677e4c2b77b0a122df30816ea68b5710/abupy/BetaBu/ABuPositionBase.py)计算下单单位，[监督基类](https://github.com/bbfamily/abu/blob/d602d847677e4c2b77b0a122df30816ea68b5710/abupy/UmpBu/ABuUmpBase.py)承载训练/预测职责。这种职责拆分比不断新增大策略枚举更可扩展。计划采用：

```text
数据/因果特征 → 信号 → 目标仓位 → 风控 → 执行计划 → 订单/成交 → 账务/解释
```

每个模块带name/version/parameters/state，快照只保存有界状态；信号不能直接改账户，仓位计算不能绕过现金冻结，模型过滤不能放行硬风控拒单。先做确定性规则组合、目标仓位与可用现金/波动约束，再加入模型。模型与特征产物固定训练截止时间/哈希，交易时仅取已可用信息。现有Rust注册工厂是接入基础，不代表模块组合已实现。

[ABU多标的并行择时](https://github.com/bbfamily/abu/blob/d602d847677e4c2b77b0a122df30816ea68b5710/abupy/AlphaBu/ABuPickTimeMaster.py)先拆分标的任务、汇总行动后应用资金。[LEAN Algorithm Framework](https://www.quantconnect.com/docs/v2/writing-algorithms/algorithm-framework/overview)将Universe Selection、Alpha、Portfolio Construction、Risk Management与Execution分开。借鉴这些边界时，我们只并行独立实验/数据准备，共享现金组合的成交与资金预留仍按全局确定性顺序执行；不能并行“算完收益”后才发现资金重复使用。

[ABU指标模块](https://github.com/bbfamily/abu/blob/d602d847677e4c2b77b0a122df30816ea68b5710/abupy/MetricsBu/ABuMetricsBase.py)提供收益/基准/回撤等统计。我们的研究报告应扩展权益曲线、回撤、换手、费用/滑点分解、敞口、订单未成原因；Sharpe必须声明采样频率和年化假设，稀疏或无交易样本不能给漂亮排名。Python用于Notebook/实验组织/图表；Rust继续负责数据边界、策略执行、撮合与账务。Python接口先做版本化JSONL/进程接口，量出跨边界成本后再决定是否需要PyO3。

[Freqtrade lookahead-analysis](https://www.freqtrade.io/en/stable/lookahead-analysis/)通过基线与切片回测对比查信号差异，也明确未触发信号可能漏检。我们的测试要保存可用时间/输入前缀，比较全程与截断至t时刻的相同策略输出，并测试不同预热长度；未来机器学习标签另加时间隔离。它是一组检测，不是数学上证明任意代码无未来信息。

## 更可信的仿真与外部对账

[NautilusTrader实时契约](https://nautilustrader.io/docs/latest/concepts/live/)明确共享策略代码与外部执行差异、未解决提交、启动与运行对账。我们已有意图先持久化、同client ID仅一次POST、unknown只查询，以及两轮真实测试网基础核对；本阶段已补有界私有事件消费、三币种费用账本及订单/全资产核对；下一步补历史游标、连续高水位/缺口恢复和外部活动分类。事件重复/乱序/断流不得重复入账；历史不完整必须停止核算并标记未知，不自动创造余额调整。

[HftBacktest成交/队列文档](https://hftbacktest.readthedocs.io/en/latest/order_fill.html)明确回放不能改变市场，有些成交假设仍不现实；[延迟文档](https://hftbacktest.readthedocs.io/en/latest/latency_models.html)分开feed、entry、response。先录制合法Spot L2快照/增量+逐笔成交，验证衔接与重建，再实现保守队列/部分成交/撤单回报模型。使用测试网不能校准主网真实成交概率；L1更新ID不能冒充连续深度序号。

## 我们可以选择的工程差异

当前Testnet接口强制意图持久化成功后才外发；纸面事务则命令、回执与检查点原子提交后才确认。[Nautilus实时文档的Persistence before transport说明](https://nautilustrader.io/docs/latest/concepts/live/#backtest-and-live-differences)明确其内建cache/event capture不把持久提交作为dispatch门槛。这是具体默认路径的取舍差异：我们更强调已确认边界与事后追溯，付出同步存储成本；不能推导对方无法自定义持久策略，也不能声称我们的所有远端成交都已完成持久账务。

我们采用单写入者、整数经济事件、有界策略状态/动作、严格身份恢复，以及公开完整失败记录。它们可以形成更易检查的小内核，但规模/种类较少也使测试更容易，不能拿当前测试数量证明可靠性优于成熟平台。性能优化先根据真实profile定位候选状态复制、检查点序列化、JSON和SQLite代价，再做逐事件/故障对照；不预设无锁、SIMD或零分配必然更快。

## 可测量的“超越”目标

下面是预先声明的工程目标，**不是已有结果**。与别人的比较必须先实现并公开适配脚本/固定版本/共有输入域；语义不一致时分别报告，不能通过减少校验、关闭持久化或改变成交模型赚跑分。

| 方向 | 公平实验及目标 | 现在的状态 |
|---|---|---|
| CPU效率 | 固定1M真实报价/同策略/订单规模/成交规则，预热+至少7轮；CPU-only目标吞吐中位≥参照1.5倍且RSS更低，公开raw与变化范围 | 只有固定Barter组件对照；未运行vn.py/ABU/Nautilus完整对照 |
| 持久尾延迟 | 独立列出无持久化、同类durable ACK模式；固定输入速率/突发/热状态，目标新实现p99上界较自己的冻结基线改善≥20%，不增加丢失/重复 | 新全样本区间统计已实现；没有新旧同负载改善证据 |
| 恢复与安全 | 下单前/后崩溃、丢ACK、重复乱序成交、撤单未知、断流；已确认命令0丢失、0重复经济事件/重复POST，状态链核对 | 纸面恢复、受限Testnet原币账本/有界私有流已有；连续缺口恢复、完整组合和真实部分成交仍缺 |
| 仿真可信度 | 同L2/逐笔输入、不同队列/费用/延迟场景，报告成交概率/偏差，不以PnL更高当更好 | 当前仅L1限价/共享量/DeltaBudget/成本压力 |
| 研究可复现性 | manifest固定日期/哈希/代码/候选，训练选择后独立测试；失败保留，不根据测试收益重选 | 流程已有，六日筛选未通过、独立周转换失败，没有合格alpha |
| 可用性 | 新机器10分钟跑出demo、接策略、停止、恢复、读拒单原因；Mac/Linux同契约，文档与运行样例匹配 | CLI/教学样例已有，Python SDK/运营界面/生产Linux长测待做 |

报价执行基准优先对照vn.py tick适配与Nautilus/Barter相同输入域；ABU以bar/因子研究为主，应另做同样日线数据、因子和成本的研究基准，不能拿我们的L1处理速度和它的整套日线研究时间相除。

现有[验收证据](ACCEPTANCE.md)：固定Barter0.14 L1组件100000帧中位Kaze20.661ms/Barter23.721ms（约1.148倍），输入为未转义ASCII、双方契约类型不同，不能泛化为整套引擎超越。百万真实报价完整账本旧版本约37026–38790命令/秒；没有跨框架同条件对照。不得把CPU组件与SQLite/网络全路径放一张“谁快”榜单。

## 实现顺序与明确出口

1. **先闭合现在的三个门槛。** 保留旧24h实验及失败记录；本轮全程遥测/过期批次/收尾错误修复已完成，详情[TELEMETRY](TELEMETRY.md)。新版本另跑全天；测试网还需真实部分成交等未通过项；独立周数据要先给出异常分布与新的预声明数据规则，在产生策略收益前冻结，不用放宽到能盈利。24h观察本身不能替代成交与收益验收。
2. **外部执行闭环。** 统一身份/能力、私有回报+REST补洞、账户经济事件和余额核对，先单账户单Spot。出口：上述故障表逐项通过，恢复不重发、重复事件不重复入账；仍保持Testnet边界。
3. **策略能组合、订单能解释。** 目标仓位、信号/仓位/过滤、版本化生命周期、TWAP父子订单及教学模板。出口：手算小账本、部分成交/撤单未知、不同时刻强杀恢复、同输入回放结果一致。
4. **研究与仿真。** 数据质量报告、因果bar/特征、成本/风险报告、固定实验并行；Spot L2+逐笔和队列/延迟模型。出口：前缀/预热对照、训练测试隔离、共享预算守恒与实测偏差报告。
5. **达到明确范围后做对照与操作台。** 固定Linux机器/CPU绑定，公开完整CPU-only与durable基准；Python研究SDK、只读运行/风险/恢复面板。多市场先补资产/日历/结算语义再接网关，不能用“通用接口”冒充支持CTP、股票T+1或衍生品。

最有机会形成的特色是**小而可审计的Rust执行内核 + 可组合研究层 + 带数据与故障记录的验收工具**。这是一条路线；当前没有全面超越的证据，也没有真钱部署准入。

## 代码复用与许可来源

本轮没有复制任何上游源码进入执行内核。vn.py核心/所读CTA与Algo子项目快照为MIT；保留许可与署名后可评估局部复用或独立Python网关桥接，但具体依赖还要核对。ABU仓库LICENSE为GPLv3，本轮仅分析职责与行为并独立设计，不能把其Python代码机械翻译成Rust后直接当作MIT原创。NautilusTrader为LGPLv3、LEAN为Apache-2.0、所读HftBacktest LICENSE为MIT；未来实际引入模块时逐项记录版本、许可、修改与依赖。源码哈希清单用于调研追溯，不代表我们已将这些库集成。

## 实施记录：第一步外部执行基础

已引入账户/venue身份与能力声明、期初原币资产账本、私有执行回报与REST核对、乱序/重复/外部活动阻断和审计身份聚合。实现与实际通过范围见[EXTERNAL_LEDGER](EXTERNAL_LEDGER.md)。这是单Spot测试网有界验收，尚非持续策略节点；连续重连/私有历史高水位/真实部分成交仍缺。性能仅对自己的原审计扫描作同负载对照，没有新增跨框架性能胜出结论。
