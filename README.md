# KazeQuant · 风量

[![Rust checks](https://github.com/KazeBox33/kaze-quant/actions/workflows/ci.yml/badge.svg)](https://github.com/KazeBox33/kaze-quant/actions/workflows/ci.yml)

用 Rust 构建的确定性量化回放与纸面交易平台。执行内核与持久化边界分离，价格和账务使用整数，项目代码禁止 `unsafe`。当前版本 **0.4.0** 增加版本化策略注册、SMA交叉、walk-forward研究、公开实时纸面交易、滑点/流动性压力模型，以及独立的 Binance Spot Testnet 订单网关。核心继续支持多资产独立预算、事务确认、检查点、有限热历史、去重、全量审计和备份。

这是可运行、有故障验证的回放/纸面平台，测试网接口只能使用虚拟资产。当前没有真实资金端点、自动实盘策略路由、完整组合对账或通过盈利准入的策略。三个门槛的逐项状态见 [验收记录](docs/ACCEPTANCE.md)，不能把代码完成或组件跑分快误读为真钱上线许可。

## 五分钟运行

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
| 策略 | passive、threshold、momentum、mean_reversion、sma_cross；带版本的Rust策略注册与持久状态；`Strategy` 回调和每报价最多 64 个动作的有界批量接口 |
| 风控 | 单笔金额、价格偏离、持仓/资金/订单容量、行情超时、绝对回撤、人工停止；暂停后撤单 |
| 持久化 | SQLite WAL/FULL、事务保存命令/回执/检查点、提交后发布候选状态、SHA-256 链、磁盘去重、排他锁 |
| 恢复 | 校验热检查点后恢复账户/委托/风控/窗口；可逐事件全量审计；在线备份与容量反压 |
| 行情簿 | 有界 L2、整帧原子更新、快照恢复、缺口锁定；不暴露缺口后的旧报价 |
| 网络/测试网 | 可选network功能；公开实时L1、过期/断线/溢出停止；固定测试网主机、意图先持久化、未知结果查询、成交去重/核对 |
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
