# 有界外部父子执行计划（v10）

2026-10-07。当前可以通过独立 CLI tick 驱动一个固定限价、固定总毛成交量的 Binance Spot Testnet 分片计划。父计划不直接解释 SMA 信号，不等于连续目标净仓位策略节点。先补齐安全的外部执行生命周期，再接组合策略；纸面 composition 仍按 [TARGET_EXECUTION](TARGET_EXECUTION.md) 运行。

## 每次发送必须重新获得授权

计划只能创建在没有订单意图、已经绑定账户/工具/期初资产、开启历史恢复的新排他账本。每次 tick 先完整 REST 核对身份、历史订单/成交与全部账户原币资产；失败或超出核对时间预算，不产生下单许可。启动打开账本会要求重新核对，缓存 Ready 不能用于重启后发送。

父计划时钟、子单链接和 unknown 订单意图在同一 SQLite WAL/FULL 事务提交。仅提交成功的当前调用栈可以发一次 POST；这个许可不能持久化或从数据库重新获得。POST 超时、错误、应用层丢成功 ACK，均按原 client ID 查询；查询 not-found 也不能自动重发。因此可能留下实际上未发送、但无法证明未发送的 unknown，当前必须停止等待核查，不提供自动清除或重新建 ID 绕过机制。这里保证保守不重发，不宣称交易所与本地数据库之间有 exactly-once 事务。

同一计划最多一个未终结子单。限时或暂停后先持久化 cancel_unknown，再调用一次 DELETE；未知撤单后仍只查询，活跃订单回报不会抹去 cancel_unknown。终态订单和迟到成交经原币账本核对后，下一 tick 才能计算替代数量。`plan-pause` 只持久化暂停策略；需要后续 `plan-tick` 且核对成功才能请求撤单，网络不可用时不能保证立即撤掉交易所挂单。`plan-resume` 显式重新核对，保留原时钟和身份，未知提交/撤单阻断恢复。

受管理账本在初始化同事务升级为 `binance-testnet-execution-v2-plan`，旧 v1 二进制拒绝打开，手工 prepare 不能插入父计划外的订单。[保存的旧编译二进制实测](evidence/v10/legacy-binary-guard.json)在复制的 mock 账本上因 revision mismatch 拒绝打开，未访问交易所。旧手工账本仍使用 v1，不静默升级经济语义。父配置不可变；SHA-256 检查点用于检测损坏，不是对恶意数据库编辑的认证。

## 整数数量与费用

累计授权量为 `floor(total_steps × due_slices / slices) × step`，每个子单再限制最大量；按已确认毛成交量扣减。最后一片是释放时间，不是成交截止，不自动追价。未成交订单冻结在当前子单，未知撤单不能补量。部分成交后剩余不足最小子单或网格时暂停为 dust，绝不向上取整扩大敞口。

手续费以交易所实际原币成交为准。例如手算 fixture 买入毛量 0.10 BTC、BTC 手续费 0.0001，净增 BTC 为 0.0999；父计划毛量已完成不表示净仓位达到 0.10。这个外部账本不能直接伪装成纸面账户反馈。绑定账户必须独占，外部活动/资产差异阻断。

库内父计划固定限价总名义金额最多 100 USDT（不含手续费），1..1024 片/子单、最多 10000 tick、释放跨度最多一天；核对预算最多 60 秒，预检查后和落盘后发送前再次检查。容量耗尽持久暂停。当前没有共享策略组合现金、资产换算估值、账户回撤风控、自动信号到外部净目标路由或主网主机。全 REST 核对及 FULL 同步适合保守、低频分片，不是高频订单热路径。

## 运行与复现

```sh
cargo test --locked --all-features --test external_plan
cargo build --locked --release --example external_plan_demo
target/release/examples/external_plan_demo demo reports/my-plan-demo
python3 scripts/plan_crash.py --output reports/my-plan-crash
cargo build --locked --release --features network --bin kaze-testnet
```

手工流程是 `kaze-testnet JOURNAL ledger-init BTCUSDT` → `JOURNAL plan-init PLAN.json` → 多次 `JOURNAL plan-tick`；配置字段/合法网格见 `PlanConfig` 和 fixture 示例。真实工具网格及最小名义量必须由交易所预检查，不可直接拿 mock 的 100 USDT 限价交易 BTC。每个计划用新的空账本；已有 unknown 必须核查原账本，不能换账本重试。

已经配置测试网密钥并明确允许虚拟买入时，可以运行：

```sh
python3 scripts/plan_acceptance.py --binary target/release/kaze-testnet \
  --output reports/my-testnet-plan --symbol BTCUSDT
```

脚本按当前行情/过滤规则生成三片小额虚拟买单，父计划上限 60 USDT；第一片只丢应用 ACK，然后由新进程查询恢复，检查每片、重复 tick 与最终原币资产。失败停止，挂单按原计划暂停/核查，不自动重发；不是物理断网或真实部分成交验收。凭证仍只存本机忽略文件，详见 [TESTNET](TESTNET.md)。

## 本轮证据

- 15 项专项测试：毛量/基础币费用、提交未知、落盘后未发送、部分成交撤单替代、未知撤单跨重启、暂停/恢复、账户冲突/外来订单/缺成交/时钟回退、核对过期与慢预检查、SQL 子事务失败、配置/阶段/检查点/版本冲突、子单容量、配置边界和 dust。完整 debug/release 各 210 项 Rust、15 项 Python，fmt、严格 clippy/rustdoc 通过；[验证身份](evidence/v10/validation-mac.json)。部分成交场景是确定性 fixture。
- [真实 SIGKILL](evidence/v10/before-post-crash.json)：提交意图/父子链接各一条后，mock 尚未 POST，强杀所拥有的子进程；新进程 not-found 查询后仍 SubmitUnknown，POST 为零。验证本地进程/事务边界，不是物理磁盘断电或实际交易所未发送证明。
- [七轮 mock](evidence/v10/mock-summary.json)：每轮 1004 个完整计划 tick、三个子单、第一 ACK 丢弃后重启恢复；中位 13.088 秒，最终毛量 0.10、USDT 费用账本一致。6030 仅是 fixture 内记录的源操作数，不是完整实际 HTTP 请求数；时间包含每 tick 核对与 FULL 写盘。原始每轮 JSON 公开。
- [固定公开报价负载](evidence/v10/paper-summary.json)：100 万内存路径七轮中位 1,076,287 报价/秒，10 万持久确认三轮中位 39,731 报价/秒。内存时间包括 CSV/核心/回执哈希/保留；持久路径包括 WAL/FULL、256 批次和回执；独立全审计在计时外。数据/配置/二进制哈希与所有轮次公开，不是逐报价延迟或外部下单速度。
- [同机旧新版交替七对](evidence/v10/paired-regression.json)：同一 10 万连续前缀/配置、每轮新数据库；旧版中位 2.479989 秒，新版 2.535915 秒，耗时增加 2.255%，如实保留。全部原始回执 SHA 与最终状态相同，各自独立进程全量恢复相同。只与我们自身 v09 比较，没有 vn.py/ABU/Nautilus/Barter 同条件整平台排名；不把跨时段 I/O 差异当成已证明的原因。
- [测试网连接失败](evidence/v10/testnet-connection-failure.json)：本机 public `/api/v3/time` 多次 HTTP 502，官方测试网页面显示维护；真实三片脚本第一步 ledger-init 失败，持久意图零、POST 零。真实外部分片验收尚未通过。本轮没有新虚拟成交，不把旧人工验收移作该能力的实测。

机器为 Apple M5/macOS 26.5.2 arm64、Rust 1.99.0/LLVM 23.1.1、release thin LTO/单 codegen unit；二进制、负载、日志摘要见上述身份与 manifest。历史策略收益门槛失败、真实部分成交未观察、新版本 24h 未通过，状态继续保留。

## 借鉴与当前特点

本轮参考 [Binance REST 官方未知执行语义](https://github.com/binance/binance-spot-api-docs/blob/master/rest-api.md) 与 [NautilusTrader 启动核对/持久化文档](https://nautilustrader.io/docs/latest/concepts/live/)（访问 2026-10-07，在线文档可能变化），自行实现，没有复制上游源码。我们的可验收特点是固定边界的原币核对、父子同事务及提交前耐久屏障，并保留强杀后查询而不重发的证据。Nautilus 文档描述的原生 cache/event-store 持久化不作为执行发送的耐久屏障；这只是具体配置语义差异，不说明它无法自定义此屏障，也不支持整体质量/速度更优的结论。

下一阶段先恢复测试网连接、通过真实三片闭环，再让持久策略目标适配原币净仓位与挂单资源；之后才扩展多计划/组合估值、仿真校准与同条件上游完整负载对照。生产目标仍按逐项门槛验收。


本轮代码 `c4af51e85d55f7129007dedb300ee2fe5db8611c` 的 [CI37587473980](https://github.com/KazeBox33/kaze-quant/actions/runs/37587473980) 已在 Ubuntu/macOS 全部成功：[完整步骤与提交身份](evidence/v10/ci.json)、[两个 ZIP 摘要与原始恢复结果](evidence/v10/ci-artifacts.json)。两端提交前强杀均保持一条 unknown 与父子链接、新进程零 POST；1004 tick mock 均三子单完成、账本无问题，已有纸面五个强杀恢复点也通过。托管 runner 证据不等于目标生产 Linux 机器、真实交易所或全天门槛。该补充提交只更新文档/证据，执行源代码保持上述已验证提交。


后续v11增加[只读原币净目标编译](NET_TARGET.md)，可以生成本节固定毛量计划；当前执行器仍不自动维护净目标或费用预留，不能将该预览误认为连续策略适配已经完成。
