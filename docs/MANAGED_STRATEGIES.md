# 多策略生命周期、所有权与子账（v17 / 0.8）

`managed` v1是标准注册策略管理器，位于现有PaperRuntime/Engine上方。同一市场的成员共享**一个**核心订单表、撮合顺序、显示流动性和总账户；每个成员拥有自己的整数现金/冻结/持仓/费用/成交名义额子账。没有为每个策略重复创建行情撮合Engine。

## 直接运行

```sh
cargo build --locked --release --bin kaze-run --example managed_reference --example target_reference
mkdir -p reports/managed-demo
target/release/kaze-run --config configs/managed-demo.json --input data/managed-demo.jsonl \
  --db reports/managed-demo/session.db --finish --verify-full --report reports/managed-demo/report.json
target/release/kaze-run --config configs/managed-demo.json --db reports/managed-demo/session.db \
  --recover-only --verify-full --report reports/managed-demo/recovered.json
python3 scripts/managed_crash.py --output reports/my-managed-crash
# 两份相同突破策略，独立条件/OCO/子账；单份报价总数量6。
target/release/kaze-run --config configs/managed-bracket-demo.json --input data/managed-bracket-demo.jsonl \
  --db reports/managed-demo/bracket.db --finish --verify-full --report reports/managed-demo/bracket.json
```

## 生命周期与命令

配置中的`members`数组顺序就是稳定owner索引；名称在创建时查重，热路径使用整数索引。1..8个成员，普通活跃单与等待条件合计<=max_working（1..64）。管理器只接受内置策略和标准bar-atr/breakout-bracket v1，禁止递归管理器；composition要求v2自己的成交进度。它是可信编译Rust代码，不是任意插件安全沙箱。

- Created → `init` → Ready：参数/市场/初始子账在创建时已经验证；init不下载历史或注入资金。
- Ready → `start` → Running：Ready期间报价更新内置指标/bar，不执行返回动作，不产生冻结或订单；初始化完成不等于指标预热完成。
- Running → `stop` → Stopped：取消本成员普通工作单及等待条件；保留已经成交的仓位、费用、指标和父计划。其他成员继续工作。
- Stopped → `start` → Running：在保留的进度继续；Stopped不更新指标。下一报价正常检查bar缺口/市场风控，不强制平仓或重置父计划。
- 无效转换在命令修改前拒绝。市场StaleFeed/Drawdown/Operator/Finish仍暂停整个市场；不能通过成员start解除市场暂停。全市场停止同时撤单并清除未执行的动作握手。

每行外层仍是`{"seq":连续编号,"command":...}`：

```json
{"type":"strategy_control","market":0,"owner":0,"operation":"init"}
{"type":"strategy_control","market":0,"owner":0,"operation":"start"}
{"type":"strategy_action","market":0,"owner":0,"action":{"Submit":{"side":"Buy","limit":101,"quantity":2,"time_in_force":"GoodTilCancelled"}}}
{"type":"strategy_action","market":0,"owner":0,"action":{"Cancel":1}}
{"type":"strategy_control","market":0,"owner":0,"operation":"stop"}
{"type":"strategy_transfer","market":0,"from":0,"to":1,"amount":"300"}
```

StrategyAction是可信操作方显式指定的子账操作，不是远程身份认证。普通不带owner的Submit/Cancel/条件操作在managed市场被Ownership拒绝；对其他成员的ID操作同样拒绝。手工操作可能改变该成员策略自己的计划，建议passive成员承接人工指令；composition v2自己的父进度不自动采用额外人工成交。

## 账务和准入

所有成员initial_cash之和必须**等于**市场Engine初始现金，不能重复分配同一资金。每个成员max_position<=市场上限；成员上限合计可以更高，但核心仍按真实总持仓+总待买量拒绝超限。

成员买单只能使用自己的available_cash，按限价+保守逐lot费用冻结；卖单只能使用自己的未冻结持仓。不把别人的现金/待卖意图当自己的购买能力。平台价格网格/价格带/金额/时效/历史容量和核心风控随后再次检查。条件等待不冻结，实际激活携带ConditionalId重新核对**同一成员**的真实资源，不按重复的请求内容猜测归属。

StrategyTransfer只允许已初始化的Ready/Stopped成员，双方都不能Running。金额为正、来源有足够未冻结现金和剩余capital；同时改双方cash/capital，核心总现金不变，费用/成交名义额/持仓不变。不从未成交卖单预支现金；不从其他市场转账，不自动汇集利润或重新分配预算。

恢复独立重算每个成员的冻结现金、待买、冻结卖出和活跃/等待数量，并核对：

- `cash = capital - net_buy_notional - fees_paid`，现金覆盖冻结、成员仓位/待买/冻结卖出不超限。
- 所有子账字段（含费用、成交名义额和冻结）合计精确等于核心Account，capital合计等于市场初始现金。
- 每个核心活跃订单有唯一owner，原请求/剩余量/冻结单价一致；子策略工作ID只能属于自己。
- 每个等待条件有唯一owner，原请求一致；子策略只能在过滤并还原局部分组号的只读条件投影中验证自己的ID。
- 非Running成员无工作订单/等待条件；只保留已成交仓位。没有未执行动作或尚未派发的激活身份可以进入检查点。

订单/成交反馈按订单ID归属路由，先更新自己的实际子账，再通知子策略；不向其他成员广播成交。条件局部OCO组通过`local_group*8+owner`映射，最大局部组为floor((10^12-7)/8)。两份相同突破策略的局部group=1分别映射8与9，不能互相撤销保护。普通单与条件ID仍是各自命名空间，生命周期单调编号不重用；终态owner热表删除，完整归属与事件保留在原回执。

`owned_action.gate_rejection`只表示管理层准入结果；实际平台/核心接受、拒绝和成交仍以随后原始事件为准。多成员逐报价决策通过owned_decision回执记录owner；报告的members[i].decision/diagnostics提供当前解释，普通订单动作的owner也留在回执；单成员继续提供原StrategyDecision回执，方便精确对照。

## 手算验收

两成员各5000，目标各5：报价数量3先分给先提交的alpha，再下一条数量3填alpha剩2和beta的1；这是自己的FIFO而不是交易所外部排队。三次成交后alpha现金4493/持仓5、beta现金4898/持仓1。alpha尝试撤beta单和不带owner撤单均拒绝；停止alpha不撤beta剩4。随后beta成交4，总现金8986/持仓10/费用4。双方停止后alpha转300给beta：cash4193/4793、capital4700/5300，总现金仍8986。Finish不卖出这10lot。

双突破示例各买3@101、卖3@111，各两次费用1；每人cash5028/仓位0，总cash10056/fee4。条件/OCO/成交所有权均随每命令检查点恢复；利润只是虚构手算，不能当alpha。

## 数据和性能复现

```sh
python3 scripts/market_data.py --rows 100000
python3 scripts/managed_bench.py --quotes reports/datasets/btc-quotes-100000.csv --output reports/my-managed-bench
```

同一最终构建direct bar-ATR与1成员managed交替7对，内存计时含CSV/真实核心/规划/回执摘要/回收；仅基准将初始2个控制命令排除、回执seq恢复报价序号并去掉OwnedAction，核对剩余原始回执字节和全部经济字段。数据库保存的回执没有删字段；每种1/2/8成员持久测试3轮，核对完整状态、规范化回执及新进程原始链/100000+2N命令全审计。2/8成员预算和策略订单不同，只作首次工程基线，不和其他负载做收益/吞吐优越排名。时间不是逐事件延迟；所有轮次和资源成本见[FEATURE_SCORECARD](FEATURE_SCORECARD.md)。

参考固定[vn.py CTA引擎](https://github.com/vnpy/vnpy_ctastrategy/blob/7a8768de9784dda35a7b261a7ade1dbfbff50919/vnpy_ctastrategy/engine.py)的初始化/启动/停止、订单到策略映射和成交分发，独立编写Rust管理器。这里没有运行vn.py全框架或测该模块的上游速度，不以功能名称相同宣称整个平台超越。

当前仍是**单市场、同报价币种、只做多纸面模型**，多市场Engine仍独立预算。没有跨市场共享现金/组合NAV、自动组合分配、收益归因、账户迁移、外部连续策略路由、任意用户插件隔离或真实venue子账户。内置策略本身的保护/队列/alpha限制仍适用；八成员和64总工作容量明确有界，管理器合计指标窗口额度<=1024，在构造子策略前检查（RollingMean按实际槽位，ATR周期也计入保守额度，WilderATR自身为常数状态）；参数8KiB、注册状态64KiB，不能假装无限策略。

包0.8.0、执行`kaze-paper-v4-managed`、SQL`kaze-sql-v4-managed`，旧会话保留旧二进制，新版本新路径运行。不修改manifest绕过绑定。
