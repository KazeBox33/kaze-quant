# 持久净目标与原币费用反馈 · v12

v11只读编译之后，v12增加**一份不可变净目标、一份固定限价毛量父计划**的持久执行。`init_net_target()`先完整REST核对，再把目标请求、初始预览来源、父计划与管理版本放进同一个SQLite WAL/FULL事务。它不下单；`tick_plan()`或CLI `target-tick`才有每子单一次发送许可。这是有界执行接口，连续策略信号更新、多目标轮换和多策略共享账户仍待实现。

## 一个目标如何完成

1. 新的排他账户/历史绑定账本；任何已有意图或父计划都不能重新编译目标。
2. 核对和编译共同受请求的时间预算约束。`hold`、资源不足和dust不创建目标或父计划，调用方先读取预览解释。
3. 初始预览保留账户身份、基础/报价币、净量、free资源和毛量。目标正文最多64KiB，SHA检查损坏；打开时重新核对请求、账户/工具身份与父计划的一致性。SHA不是认证，不防有权限者重写数据库/摘要。
4. 每次tick完整REST补查并核对原币账本。实际手续费按原币累计，已消耗预算不能在重启、重复初始化或人工resume后恢复。
5. 新子单前检查剩余费用预算、净量方向边界、最新全账户总资产一致性和free资源。基础币费用与报价币费用分别处理，locked资产计入净量但不提供可用资金。
6. 实际手续费超过预留、正数第三币种费用或方向边界越界会暂停未来动作；已有活动子单走原撤单/未知结果查询流程。这不能撤销已经发生的成交或费用，也不能保证网络发送后交易所的费率/账户不再变化。
7. 毛量父计划结束后，净量在容差内且已核对才报告`satisfied`；否则`completed_with_residual`。尾差不自动生成补单，不能用新ID/重设目标绕开原意图。`observed_within_tolerance`只是一项数值观测，`confirmed_satisfied`才是完成状态判断。

同请求再次初始化只核对并返回现状；不同请求拒绝，不改变时间表、费用或子单。新`v3-target`账本同时防止老二进制及手工prepare绕过管理目标；新二进制原`plan-tick`也执行净目标检查。旧v1/v2账本仍按原语义打开，不能直接给已有毛量计划追加净目标。

手续费预留仍是调用方声明的**整份计划**最大原币费用，须包含部分成交和舍入。预留不足时只能停止后续订单，不能倒推已成交不会越界。新子单资源检查会为剩余整份费用保留空间，因此可能比仅检查当前子单更保守。固定目标与网格/毛量上限也可能留下残量；容差内满足不等于清仓。

## 可复现运行

```sh
cargo test --locked --test net_execution
cargo build --locked --release --example net_execution_demo
target/release/examples/net_execution_demo target reports/my-net-execution
target/release/examples/net_execution_demo budget reports/my-net-budget
target/release/examples/net_execution_demo residual reports/my-net-residual
python3 scripts/net_execution_crash.py --output reports/my-net-crash
python3 scripts/net_execution_bench.py --output reports/my-net-policy-cost
```

三个demo均使用固定价格100、0.01数量网格和0.1%基础币费用的本地fixture。正常三片毛量0.10、净量0.0999；预算不足只发第一片；容差为零时保留0.0001残量。每个demo含1004个完整持久tick，报告初始化时间、执行时间、订单、原币手续费及完整账本。

测试网CLI接口：

```sh
cargo build --locked --release --features network --bin kaze-testnet
target/release/kaze-testnet JOURNAL ledger-init BTCUSDT
target/release/kaze-testnet JOURNAL target-preview TARGET.json
target/release/kaze-testnet JOURNAL target-init TARGET.json
target/release/kaze-testnet JOURNAL target-tick
target/release/kaze-testnet JOURNAL reconcile
target/release/kaze-testnet JOURNAL audit
```

`configs/net-target-preview.json`是手算fixture，实际价格/数量/过滤与预算需按当前市场填写。`target-init`为零POST，tick不自动循环，调用方需提供调度；CLI含`target-tick-drop-ack`应用层故障模式，不是线路断包。真实有界验收可用 `python3 scripts/plan_acceptance.py --binary target/release/kaze-testnet --net-target --output reports/my-live-net`；这会下测试网虚拟订单，按现时过滤与期初净量冻结三片/整计划1%两币预留，不代表真实费率估计。每次打开仍要求新REST核对；audit本身不把旧观测升级成已满足。现有暂停/恢复接口仍为`plan-pause`/`plan-resume`。不要复用demo数据库绑定真实测试网。

## 功能、数据和限制

14项专项测试覆盖：目标/父计划事务回滚、核对过期零初始化、丢ACK恢复、每片重启、预算超出/禁止resume、部分成交后暂停撤单、撤单未知后的晚到成交、free不足、正文/版本损坏、重复初始化、不可执行目标、卖出残量和第三币种费用。实际SIGKILL发生在子单/父链接提交之后、mock POST之前；新进程0 POST并保留原目标和未知身份。它验证进程边界，不能当作物理断电或真实交易所未收到订单的证明。

完整基准和逐功能变化见[数据表](FEATURE_SCORECARD.md)及[evidence/v12](evidence/v12/validation.json)。同二进制gross/target交替对照隔离新增净目标策略的成本；旧v11/新v12交替另检验原毛量计划兼容性。两类负载不能互相当成提速数据。预算累计当前扫描该有界账本的原成交，诊断核对父子计划；成本随成交/子单历史增加，1004 tick/3成交实验不代表大历史或网络延迟。

本轮公开测试网time前置先502、后恢复200；真实毛量和净目标各一轮三片验收已完成，各3 POST/3成交、首片丢ACK后新进程恢复、全资产核对通过；净目标实际残量0并报告satisfied，但六笔实际手续费均为零。非零费用状态见逐功能数据，不沿用过去人工成交作为本轮结果。真实非零费用反馈、部分成交、新版24h、独立alpha、目标Linux硬件和上游全平台同条件比较仍未通过。

参考[Binance官方REST执行未知状态说明](https://github.com/binance/binance-spot-api-docs/blob/master/rest-api.md)（2026-10-07访问）：超时/5xx之后先查询原订单；本项目进一步保留首次发送前崩溃的未知身份，查不到也不给重发许可。没有复制上游代码，仍沿用本项目MIT许可和原有原币账本。
