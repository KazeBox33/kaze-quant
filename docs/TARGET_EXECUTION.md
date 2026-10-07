# 目标仓位、可组合规则与持久纸面 TWAP

2026-10-07：新增 `composition` v1，将信号、仓位计算、过滤和执行配置分开。目标是让策略表达“想持有多少”，由执行器结合真实已成交持仓和未成交数量规划订单。当前进入 **PaperRuntime / kaze-run / kaze-live-paper / research::evaluate** 的纸面路径；没有自动测试网策略路由。

## 五分钟验证

```sh
cargo build --locked --release --bin kaze-run --example target_reference
target/release/kaze-run --config configs/target-twap-demo.json \
  --db reports/target-demo/run.db --input data/target-twap.jsonl \
  --finish --verify-full --report reports/target-demo/result.json
python3 scripts/target_acceptance.py --output reports/target-crash
```

教学输入20条报价，bid=100、ask=101，每侧每次可成交1 lot。目标10 lot，三片累计授权3/6/10，子单为3/3/4。第一条只提交，后续10次各成交1 lot；逐成交费用向上取整为1 minor，总费用10，初始现金10000→8980，持仓10，按bid估值权益9980、盈亏−20。`finish` 只撤单并封闭会话，不清仓。重跑使用新的报告目录；同一数据库必须使用相同可执行文件、配置和存储选项。

## 一个概念：目标不等于新增委托量

```text
预计仓位 = 已成交持仓 + 待买入数量 − 已冻结卖出数量
信号 → 目标仓位 → 价差/冷却过滤 → 单子单执行 → 硬风控 → 撮合/费用
```

例如已买入3、剩余买单7、目标10，预计仓位已经是10，重复信号不能再买7。目标降为0时，先请求撤销买单；收到撤单回报并释放冻结现金后，下一条报价才提交卖出实际持有的3。待撤状态会保存在策略检查点；没有撤单回报时保留等待状态。纸面 Engine 的撤单即时生效，这个状态机测试不等于已经实现外部异步撤单适配。

每个市场只允许组合执行器拥有一个工作子单。发现额外人工/其他策略活动订单就输出 `foreign_working`，不采用或撤销它们。信号与执行只读账户，通过 Action 请求由平台逐笔重新风控。策略收到核心接受/拒绝、平台拒绝、成交和撤单反馈；临时提交提案未完成握手时禁止持久快照。

## v1 配置的边界

| 层 | 选项与行为 |
|---|---|
| 信号 | `constant` 曝光bps、`threshold` 双阈值滞回、`sma_cross` 双有界环形均线/滞回带；SMA未预热完成不交易 |
| 仓位 | `fixed_lots` 或 `cash_budget`；现金预算包含ask上的保守逐单位费用；目标夹紧到持仓上限并向下对齐数量网格 |
| 过滤 | 宽价差阻止增仓/撤销待买，允许减仓；买入冷却不撤销工作单，也不阻止卖出；最小调仓量避免尘埃订单 |
| 执行 | `immediate` 或整数累计 `twap`；单子单数量、单笔金额、可用现金/持仓同时夹紧；工作超时先撤、下一报价再规划 |
| 诊断 | 每条运行中的组合报价保存 `strategy_decision` 回执：输入序号/时间、目标/已成交/预计/授权量、父ID/子ID、动作与原因 |

信号/仓位/执行是严格类型化配置，不是任意 Python 插件、模型过滤或完整共享现金组合。现有 Rust `Registered` 工厂仍可独立接策略，但不能直接把任意注册因子塞进 v1 composition 枚举。`CompositionStrategy::on_quote` 只预热信号；有市场约束与生命周期反馈的 `on_quote_with_constraints` 才执行。旧轻量 CSV `replay()` 不提供完整反馈，因此不能用于执行这个组合；使用 `kaze-run --quotes CSV`。

## TWAP 的释放与恢复

父单固定创建时间、起点持仓、目标、方向和总调整量。第 k 片释放：

```text
累计授权 lot = floor((总量 / quantity_step) × min(1 + elapsed / interval, slices) / slices) × quantity_step
本次待执行量 = 累计授权 − 已成交调整量
```

只有没有工作子单时才规划新子单，错过多个时间片不会循环爆发补单。余数进入最后一片，小于网格的早期片可能为0；最多1024片、24小时授权跨度。最后一片表示数量全部获准执行，**不是成交截止时间**；余量可以在之后的报价继续执行。TWAP由行情时间驱动，静默不会凭空成交，`advance` 仍驱动已有平台行情超时保护。现金预算会随ask变化重算目标，目标变化可能重建父单；稳定分片教学使用 fixed_lots。

父/子单、窗口、冷却与最近决策同账户/订单/原始回执进入 SQLite WAL/FULL 事务。恢复验证冻结配置、窗口、父子边界、核心活跃订单ID/原请求/剩余量与时钟，随后可全量重放核对。SHA摘要不提供身份认证；快速恢复不替代全历史审计。失败事务候选中的部分成交、费用和子单剩余量一起回滚。

## 已验证的结果

- 16个专项测试：重复目标、部分成交、反向撤单、价差/冷却、非整除TWAP、跳时/子单上限、超时撤挂、缺失撤单回报、含费用预算、容量拒绝、人工单隔离、损坏恢复、事务失败/重复重投、所有前缀与逐检查点一致、研究预热与时间/预算边界。
- [五次真实进程 SIGKILL](evidence/v09/target-crash.json)：已确认前缀1/2/4/7/10后强杀自有纸面进程，新进程重投20条报价，21条命令/原始回执/链与连续运行逐项相同。只证明确认后恢复；不宣称命中事务提交瞬间或交易所部分成交。
- [固定公开数据重复负载](evidence/v09/target-real-bench.json)：Apple M5 / macOS26.5.2 arm64，Rust1.99.0，release thin-LTO/单codegen unit。百万报价内存路径7轮中位904.853ms（110.5万条/秒），含CSV解码、策略/撮合、决策回执序列化/摘要和回收；最终审计与输入文件预哈希在计时外。
- 同配置固定前10万条，SQLite FULL、256条/批，3轮中位2.179秒（4.59万条/秒）；含有界队列读取、候选复制、所有决策回执与检查点持久提交，不含退出后全量审计。采样RSS约7.37–7.44MB，每100ms的ps采样可能漏过瞬时峰值；逻辑数据库58,699,776字节。每轮10万条原始回执摘要/最终状态与共享内核内存对照相同，新进程全量审计通过。事务耗时不代表逐报价尾延迟。

数据为2024-01-01 Binance UM futures bookTicker连续百万前缀，身份见[manifest](evidence/v09/dataset-manifest.json)。纸面只做多无杠杆，没有期货资金费/保证金。配置在看结果前固定，百万条产生212个接受子单、211次成交、1次撤单，费用0.88365653 USDT、盈亏−0.53549753 USDT。10万条的未清仓估值为+0.06617326 USDT，不能挑这个短前缀解释为盈利。两种行数用于不同工程计量，没有独立留出集alpha结论，也不是性能优化前后或上游同条件排名。

复现：

```sh
python3 scripts/market_data.py --rows 1000000
cargo build --locked --release --bin kaze-run --example target_reference
python3 scripts/target_bench.py --output reports/target-benchmark
```

Linux/macOS [CI 37579229668](https://github.com/KazeBox33/kaze-quant/actions/runs/37579229668) 在精确代码提交 `be19d172539f6d2d9ebb30367d73bae5570a221e` 上全部成功。两平台各五个强杀恢复点已核对原始附件及ZIP哈希：[任务身份](evidence/v09/ci.json)、[Linux回执恢复](evidence/v09/ci-target-linux.json)、[macOS回执恢复](evidence/v09/ci-target-macos.json)。后续证据提交只补文档，没有替换被验证的源代码；托管runner不代表目标生产机或24h连续运行。

## 借鉴与下一步

参考固定源码：[vn.py TargetPosTemplate](https://github.com/vnpy/vnpy_ctastrategy/blob/7a8768de9784dda35a7b261a7ade1dbfbff50919/vnpy_ctastrategy/template.py)、[vn.py TWAP](https://github.com/vnpy/vnpy_algotrading/blob/bee959dc464749f7cce66e766249ccdbb2d4869a/vnpy_algotrading/algos/twap_algo.py)、[ABU因子组合](https://github.com/bbfamily/abu/blob/d602d847677e4c2b77b0a122df30816ea68b5710/abupy/FactorBuyBu/ABuFactorBuyBase.py)。实现独立编写，没有复制上游源码。我们本阶段的特点是整数累计释放、冻结账本与计划同事务、逐决策解释及强杀恢复证据；没有运行上游同条件TWAP对照，不能据此宣称胜过它们。

后续先把策略Ready/暂停/未知提交/待撤/重启核对与外部执行连接，再验证测试网父子路由。独立数据留出、Spot L2/逐笔与延迟/队列仿真、同条件上游比较仍未完成；旧24h观察失败状态保持，真实部分成交和真钱准入没有通过。


后续 v10 已完成有界固定毛量外部父子执行生命周期，见[EXTERNAL_PLAN](EXTERNAL_PLAN.md)。原币手续费/净仓位与本节纸面目标语义不同，尚未把composition直接自动路由到外部账户。


后续v16将执行器扩展为四政策并提供composition v2统一进度/报告，原v1即时/TWAP保持兼容；Iceberg/BestLimit操作、边界与故障证据见[ALGORITHMIC_EXECUTION](ALGORITHMIC_EXECUTION.md)。
