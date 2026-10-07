# 统一父子执行：Immediate、TWAP、Iceberg与BestLimit（v16 / 0.7）

四种策略复用CompositionStrategy的信号/目标、单工作子单、撤单反馈、数量网格、资源预算、硬风控与事务恢复。composition v2给四种调度统一增加按自己成交累计的父计划进度和报告；v1的Immediate/TWAP继续保持原始行为与检查点形状。v1中新增Iceberg/BestLimit也使用新进度契约，兼容现有bar-ATR注册参数。

## 直接运行

```sh
cargo build --locked --release --bin kaze-run
mkdir -p reports/algo-demo
# 使用新的db/report路径，各自形成独立纸面会话。
target/release/kaze-run --config configs/iceberg-demo.json --input data/iceberg-demo.jsonl \
  --db reports/algo-demo/iceberg.db --finish --verify-full --report reports/algo-demo/iceberg.json
target/release/kaze-run --config configs/best-limit-demo.json --input data/best-limit-demo.jsonl \
  --db reports/algo-demo/best-limit.db --finish --verify-full --report reports/algo-demo/best-limit.json
python3 scripts/algo_crash.py --output reports/my-algo-crash
# 原有TWAP示例的v2统一进度版本（输入仍是同一份）
target/release/kaze-run --config configs/target-twap-v2-demo.json --input data/target-twap.jsonl \
  --db reports/algo-demo/twap.db --finish --verify-full --report reports/algo-demo/twap.json
```

## 执行政策

| 调度配置 | 行为 | 重要边界 |
|---|---|---|
| `{"type":"immediate"}` | 未完成目标量授权，逐资源预算夹紧单子单 | 仍要下一报价成交，不是市场单 |
| `{"type":"twap","interval_ns":1000,"slices":3}` | 整数累计释放、尾片吸收网格余数 | 时间释放不保证成交截止，跳时不会连续爆发下单 |
| `{"type":"iceberg","limit":102,"display_lots":3,"replenish_interval_ns":5}` | 固定限价，工作量<=min(display,max_child)，没有工作单且间隔满足才补下一张 | 间隔从上次接受子单开始计算，适用于两方向；不是成交后睡眠或交易所原生隐藏单 |
| `{"type":"best_limit","limit_guard":120,"min_reprice_ns":50}` | 买跟bid、卖跟ask；本方价格改变且接受后最短间隔满足，请求撤销 | 撤单回报前不替换，同报价最多一个动作；不保证maker身份，不提供PostOnly |

BestLimit买价不得高于guard、卖价不得低于guard。最新本方价格越界时先撤自己的工作单，之后PriceGuard等待；不把旧价格钳到guard冒充最优价。guard可能阻止减仓成交，是明确的执行价格限制，不是自动风险清仓。

价位、显示量、订单量必须匹配市场整数网格。新进度政策先检查价格带和实际限价的保守逐lot手续费，避免已知无效提案；平台仍逐笔重做硬风控。旧v1即时/TWAP仍保持原提案/拒绝语义。固定Iceberg限价会随市场偏离而输出PriceBand，不能为了提高成交率绕开价格带。

## 父子守恒与反馈

父计划保存ID、创建时间、起点持仓、目标、方向与总调整量。v2及新算法附加progress：自己的`filled_lots`、`submitted_children`、`cancelled_children`、`reprice_requests`。工作子单保存原请求、真实remaining、接受时间与cancel_pending。子单接受才冻结账户资源，等待下一片不冻结整份父量。

1. 原工作单先在新报价上撮合；实际Fill更新父成交和剩余量，再运行策略规划。
2. 发现目标变化、超时、guard越界或需要跟价时，仅请求撤旧单。
3. 未收到Cancelled之前保持cancel_pending，包括序列化/重启之后；没有回报时不能假设旧单消失。
4. 纸面撤单即时回报。即使已收到，同报价也不会接着发替换单；下一报价重新读账户与市场限制。
5. 部分成交后重挂只执行父剩余量；反向目标新父计划从已经成交的当前仓位开始。
6. 额外工作单输出ForeignWorking，不采用或撤销其他人的单；人工成交改变父预期仓位后输出ForeignPosition，必要时撤自己的子单，不能把它算作执行进度。不自动吸收外部仓位，需要操作方停止/核对或新会话。多策略子账仍在后续范围。

父filled<=total，filled+当前子单remaining<=total，取消/跟价计数有界。恢复核对子单原请求/remaining/ID、限价/显示量/guard、父进度和时钟；与账务及原回执同事务提交。候选中成交、费用、撤单和父进度一起回滚。SHA不是认证签名，不防有权限者改写完整数据库与摘要。

## 手算结果

- Iceberg目标10，显示3，子单3/3/3/1；第一张拆为三次1，其后3/3/1。按101成交，6次手续费各1：cash10000→8984、pos10、fee6。第一报价工作量3、冻结309，而不是把全部10 lot冻结。
- BestLimit目标5：首单5@100，先成交2，再撤剩3；3@99先成交1，再撤剩2；2@98成交2。实际成本200+99+196、三次手续费各1：cash9502、pos5、fee3。两次跟价/撤单和三个子单均可核对。
- v2 TWAP相同20报价与v1逐原回执一致：子单3/3/4、10次1lot成交、cash8980、pos10、fee10，新增父进度报告而不改变这组经济行为。

Finish/Halt撤工作单并释放冻结，保留真实仓位。等待、部分成交、补单、跟价撤单与完成阶段的进程SIGKILL验收见逐功能证据；不宣称注入了事务提交瞬间、物理断电或真实交易所撤单延迟。

## 上游共同测试域

参考[固定vn.py Iceberg](https://github.com/vnpy/vnpy_algotrading/blob/bee959dc464749f7cce66e766249ccdbb2d4869a/vnpy_algotrading/algos/iceberg_algo.py)和[BestLimit](https://github.com/vnpy/vnpy_algotrading/blob/bee959dc464749f7cce66e766249ccdbb2d4869a/vnpy_algotrading/algos/best_limit_algo.py)。独立编写Rust，没有复制上游实现进执行代码。

上游BestLimit的精确AST类在stub接口运行：单资产只买、固定min=max5、无成交数量、立即终态撤单反馈；测试相同价格变化下接受的下单/确认撤单原始二进制摘要。它包含源码中的随机数量函数调用，但返回量固定。Kaze侧包含CSV读取、完整纸面风控/冻结/核心/回收与相同摘要；双方计时成本不同，上游没有完整框架、事件发布、账户或网关。不是全平台排名、成交收益比较或Iceberg上游速度比较。

```sh
cargo build --locked --release --example algo_reference --example target_reference
python3 scripts/algo_compare.py --binary target/release/examples/algo_reference \
  --source docs/evidence/v16/upstream-best-limit.py.txt --output reports/my-best-limit-compare
python3 scripts/target_bench.py --config configs/btc-bar-atr-iceberg-v1.json --output reports/my-iceberg \
  --binary target/release/kaze-run --reference target/release/examples/target_reference
```

公开策略配置测量前冻结，用同一因果ATR信号接两种执行政策；这些策略经济行为不同，不能以吞吐/PnL互相排名。没有外部排队、真实显示量重置优先级、市场冲击、不同maker/taker费率、原生隐藏单、自动外部路由或独立alpha证明。数据/性能/成本见[FEATURE_SCORECARD](FEATURE_SCORECARD.md)。

## 版本与API

Rust公共Schedule新增枚举变体；Constraints新增`price_tick/price_collar_bps`，建议用`Constraints::from_market()`。composition v2选择统一进度/隔离/预检查政策；v1旧配置经济行为保留。包升0.7.0，执行版本`kaze-paper-v3-algorithms`、SQL版本`kaze-sql-v3-algorithms`。旧会话用原冻结可执行文件审计，新版本新建会话，不修改manifest绕过绑定。
