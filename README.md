# KazeQuant · 风量

[![Rust checks](https://github.com/KazeBox33/kaze-quant/actions/workflows/ci.yml/badge.svg)](https://github.com/KazeBox33/kaze-quant/actions/workflows/ci.yml)

用 Rust 构建的确定性量化回放与纸面交易平台。执行内核与持久化边界分离，价格和账务使用整数，项目代码禁止 `unsafe`。当前版本 **0.8.0** 增加多策略生命周期、订单/条件所有权与可核对的子账，成员共享一个撮合核心和总账户。已有本地条件/OCO引擎、可恢复突破退出策略、版本化策略注册、SMA交叉、walk-forward研究、公开实时纸面交易、滑点/流动性压力模型，以及独立的 Binance Spot Testnet 订单网关。核心继续支持多资产独立预算、事务确认、检查点、有限热历史、去重、全量审计和备份。

这是可运行、有故障验证的回放/纸面平台，测试网接口只能使用虚拟资产。新增单账户 Testnet 原币资产账本、私有执行回报与 REST 核对，见 [外部账本](docs/EXTERNAL_LEDGER.md)。新账本支持持久 REST 历史游标、原子漏收成交恢复和只读私有流重连，见 [历史恢复](docs/HISTORY_RECOVERY.md)。当前没有真实资金端点、自动实盘策略路由、完整组合估值或通过盈利准入的策略。三个门槛的逐项状态见 [验收记录](docs/ACCEPTANCE.md)，不能把代码完成或组件跑分快误读为真钱上线许可。

新增 [目标仓位与可组合纸面TWAP](docs/TARGET_EXECUTION.md)：信号/仓位/过滤/执行分层、整数累计分片、父子计划与账本同事务、逐决策回执及真实进程强杀恢复。新增[有界外部执行计划](docs/EXTERNAL_PLAN.md)：原币账户核对后分片、父子意图同事务、未知提交/撤单只查询和重启保护；固定毛量父计划已可驱动测试网网关，连续策略净目标路由仍在后续阶段。

新增[原币净目标编译](docs/NET_TARGET.md)：只读生成父计划、费用预留/净量区间与hold/资金/dust诊断；每项能力对应原始数据和前后变化，见[逐功能数据](docs/FEATURE_SCORECARD.md)。连续策略到外部净目标的自动协调仍待实现。

新增[持久净目标执行](docs/NET_EXECUTION.md)：一份不可变目标与父计划同事务提交，每片依据原币手续费/净量/free资源准入，重启不重置预算，毛量完成与净目标满足独立报告。支持有界测试网接口和本地故障验收，连续信号更新/共享账户策略服务仍待实现。

新增[订单热路径重构](docs/ENGINE_V14.md)：可复用槽位、ID索引、提交顺序链与终态索引，保持原经济检查点；Rust订单视图API迁移和下一阶段底层路线见设计文档。每项性能收益、内存成本和公开数据回归继续记录在[逐功能数据](docs/FEATURE_SCORECARD.md)。

新增[本地条件引擎与突破退出策略](docs/CONDITIONAL_ORDERS.md)：价格索引、到期、等待阶段OCO、触发后重风控、条件/订单/策略同事务恢复。已完成能力与下一阶段开发顺序见[vn.py对齐清单](docs/VNPY_PARITY.md)。

新增[统一算法执行](docs/ALGORITHMIC_EXECUTION.md)：Iceberg显示量/补单、BestLimit本方价跟随/撤挂、composition v2父成交进度、价格带预检查与事务恢复。两政策也可直接配置给bar-ATR；原v1配置保持回执兼容。

新增[多策略管理](docs/MANAGED_STRATEGIES.md)：初始化/启动/独立停止、私有现金/持仓/费用子账、条件OCO命名空间、停止后的显式资本转移与原始回执/事务恢复。限单市场纸面执行，多市场组合资金仍在后续范围。

## 五分钟运行

新增[因果K线与ATR仓位策略](docs/BAR_ATR_STRATEGY.md)：已收盘信号、波动自适应目标、缺桶重新预热、复用TWAP和可恢复纸面订单。运行`kaze-run --config configs/bar-atr-demo.json --input data/bar-atr-demo.jsonl`并指定新的db/report，可看到实际买卖和指标解释；公开行情配置为`configs/btc-bar-atr-v1.json`。

安装 Rust，克隆仓库；`rust-toolchain.toml` 固定工具链。其他机器直接使用 `cargo`；本机的 `scripts/cargo.sh` 兼容已有的独立 Rust 安装。

```sh
git clone https://github.com/KazeBox33/kaze-quant.git
cd kaze-quant
cargo test --locked --all-features --all-targets
cargo run --locked --release --bin kaze-run -- \
  --config configs/paper.json --db reports/demo/session.db \
  --input data/paper.jsonl --finish --verify-full --report reports/demo/result.json
```

两个虚构资产，共 40 条报价命令。stdout 输出每条命令的 JSON 回执，报告保留配置、二进制、实际输入字节和日志链的 SHA-256。金额在纸面协议/报告中使用十进制字符串，避免 JSON 客户端丢失精度。行情报价和数量仍是有界整数。

重复投递整份输入不会重复成交：

```sh
cargo run --locked --release --bin kaze-run -- \
  --db reports/demo/session.db --input data/paper.jsonl --finish \
  --report reports/demo/retry.json
```

恢复使用**相同二进制、规范化配置和存储选项**。保留生成该会话的可执行文件；重新编译、变更配置或执行语义后，不可继续写旧日志。`--finish` 撤销剩余委托并封闭会话，保留持仓估值；未加它时 EOF 只结束输入，会话继续可恢复。

仅恢复并检查已有状态：

```sh
target/release/kaze-run --db reports/demo/session.db \
  --recover-only --report reports/demo/recovered.json
```

也可持续从 stdin 接收规范化 JSONL。生产者每次收到持久回执再发送下一条，输入自然形成背压；行情静默期间应发送 `advance` 驱动 watchdog。完整操作和协议见 [运行手册](docs/OPERATIONS.md)。

## 已实现

| 组件 | 行为 |
|---|---|
| 执行与账务 | GTC/IOC、部分成交、延迟/限价内滑点、资金/持仓冻结、提交顺序、共享流动性/保守增量预算、逐成交手续费 |
| 多资产运行 | 直接整数索引路由，最多 64 个资产，各自明确预算、tick/数量网格和持仓上限 |
| 策略 | passive、threshold、momentum、mean_reversion、sma_cross、composition（目标仓位/即时/TWAP/Iceberg/BestLimit）、bar-atr、breakout-bracket；带版本的Rust策略注册与持久状态；`Strategy` 回调和每报价最多 64 个动作的有界批量接口 |
| 策略管理 | managed v1最多8成员、64总工作意图，显式生命周期、订单/条件归属、自己的子账预算、停止转账、逐owner决策和恢复核对 |
| 条件意图 | bid/ask阈值、停止限价模板、到期、等待阶段OCO、条件ID到普通子单映射、重风控/恢复 |
| 风控 | 单笔金额、价格偏离、持仓/资金/订单容量、行情超时、绝对回撤、人工停止；暂停后撤单 |
| 持久化 | SQLite WAL/FULL、事务保存命令/回执/检查点、提交后发布候选状态、SHA-256 链、磁盘去重、排他锁 |
| 恢复 | 校验热检查点后恢复账户/委托/风控/窗口；可逐事件全量审计；在线备份与容量反压 |
| 行情簿 | 有界 L2、整帧原子更新、快照恢复、缺口锁定；不暴露缺口后的旧报价 |
| 网络/测试网 | 可选network功能；公开实时L1、过期/断线/溢出停止；固定测试网主机、意图先持久化、未知结果查询、绑定账户原币账本、有界私有回报与REST核对 |
| 研究 | 数据/配置哈希、训练选参、独立测试账户、费用/延迟/滑点/流动性情景、失败筛选报告 |
| 运维 | JSON 回执、状态报告、运行身份与输入摘要、不可覆盖报告、Linux/macOS CI、依赖更新配置 |

金额采用货币 minor，数量采用整数 lot；`units.money_scale` 与 `units.quantity_scale` 明确换算。旧示例默认为分/整数资产，BTC 示例为 1e-8 USDT 与 1e-5 BTC。`Price × Quantity` 始终是金额 minor，独立预算不等同于共享现金投资组合。

## 真实数据与工程证据

公开数据下载、官方校验和核对、连续前缀转换都可重跑：

```sh
python3 scripts/market_data.py --rows 1000000
cargo build --locked --release --bins --examples
python3 scripts/real_bench.py --output reports/my-real-bench
python3 scripts/crash_soak.py --output reports/my-crash-soak --cycles 20
```

使用 futures bookTicker 作为报价负载，纸面账户采用无杠杆只做多成本模型；没有期货保证金/资金费/结算。策略示例用于检查账本和成本，不作为盈利推荐。百万报价重复实验与千万条单会话压力实验均保留原始结果；数据身份、测量条件和适用范围见 [EVIDENCE](docs/EVIDENCE.md)。

默认 256 条/批或首条等待 5ms 后提交；有限输入通道提供背压。每次事务提交后才输出回执，检查点与回执原子保存；终结订单回收不重用 ID。热状态容量与磁盘历史容量分别限制，达到配额有明确错误，运行手册包含监控与备份步骤。


## 实时纸面与策略研究

```sh
mkdir -p reports
cargo build --locked --release --features network --bins
cargo run --locked --release --example durable_strategy -- reports/pulse.db
target/release/kaze-live-paper configs/binance-live-active.json reports/live.db BTCUSDT 300 reports/live.json
python3 scripts/research_data.py --sample-ms 1000 --plan reports/research-plan.json
target/release/kaze-research reports/research-plan.json reports/research-result.json
```

网络与凭据说明见 [TESTNET](docs/TESTNET.md)，策略工厂/检查点见 [STRATEGIES](docs/STRATEGIES.md)。公开实时运行不需要密钥；外部测试网订单需要本地密钥，不能将纸面成交直接当作交易所成交。

## 用项目学习

先看一张订单的所有权和成交账务，再看如何组合平台：

```sh
cargo run --locked --example first_trade
cargo run --locked --example book_to_engine
cargo run --locked --example custom_strategy
cargo run --locked --release --bin kaze-quant -- \
  --input data/demo.csv --output reports/quick-replay
```

`first_trade` 两次成交后现金 7017 分、持仓 3、费用 3 分。`custom_strategy` 用两张 IOC 分享下一报价的 3 单位流动性，展示批量动作和回报回调。自定义策略先用 `PaperRuntime::with_strategies` 内存运行；持久恢复使用 `StrategyRegistry` 与 `SqliteSession::open_with_registry`，模板见 [策略接入](docs/STRATEGIES.md)。

轻量 CSV 回放仍保留为 `kaze-quant`，用于实验与学习，**不提供会话恢复**；`events.csv` 是观察日志。它与 `kaze-run`、保留作逐条同步对照的 `kaze-paper` 共用同一个执行内核。

| 人工数据场景 | 最终现金（分） | 持仓 | 权益盈亏（分） | 手续费（分） |
|---|---:|---:|---:|---:|
| threshold，零执行延迟 | 1014065 | 0 | 14065 | 415 |
| threshold，2ms 延迟 | 943357 | 6 | 3897 | 143 |
| momentum，零执行延迟 | 997296 | 0 | -2704 | 404 |

示例用于验证工程和手算账本，不表示策略具有统计优势。

## 验证与特色

142 个 Rust 测试与 15 个 Python 数据/研究/验收测试覆盖手算守恒、边界、逐事件对照、多资产隔离、风控、恢复、重复投递、损坏日志和强制终止进程。命令如下，测试证据及适用边界见 [VALIDATION](docs/VALIDATION.md)：

```sh
cargo fmt --check
cargo clippy --locked --all-features --all-targets -- -D warnings
cargo test --locked --all-features --all-targets
cargo test --locked --all-features --release
RUSTDOCFLAGS='-D warnings' cargo doc --locked --all-features --no-deps
```

自己的优化包括稳定活跃订单索引、全活跃时的连续扫描、增量环形均值，以及有限深度的连续价位存储。原始测量和负载定义见 [性能报告](docs/BENCHMARK.md)。0.3 增加批量持久化对照、百万/千万真实报价、RSS 采样和连续强杀恢复实验，公开可靠性带来的成本，详见 [真实数据与生产验收证据](docs/EVIDENCE.md)。

```sh
cargo run --locked --release --example bench -- 7
cargo run --locked --release --example book_bench -- 7
cargo run --locked --release --example store_bench -- 2000
```

## 文档入口

- [架构与扩展](docs/ARCHITECTURE.md)：模块边界、策略接入、确定性要求。
- [运行契约](docs/CONTRACT.md)：时间、整数、冻结、成交与审计规则。
- [运行手册](docs/OPERATIONS.md)：协议、恢复、停止、容量、部署与排障。
- [学习路线](docs/LEARNING.md)：从 Rust 所有权到执行与性能。
- [平台对比与路线](docs/PLATFORM_COMPARISON.md)：vn.py、ABU及其他引擎的可借鉴能力、当前差距与验收目标。
- [全程延迟统计](docs/TELEMETRY.md)：固定内存、误差区间、时效检查和故障收尾。
- [调研与取舍](docs/RESEARCH.md)：NautilusTrader、Barter、HftBacktest 与官方资料。
- [版本变化](CHANGELOG.md)。

MIT License。
