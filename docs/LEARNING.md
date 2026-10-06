# 小步学习路径

一次只理解一个机制。先读一小段代码和一个测试，再自己修改例子；每一节的完成标准是能解释行为、手算结果并通过对应验证。当前代码提供可以运行的终点，你无需一次读完。

## 第 1 课：数值类型和所有权

阅读 src/types.rs 与 src/account.rs。关注 Price(u64)、Quantity(u64)、私有字段、Result、Copy、借用与 i128。

价格 99.10 在程序中保存为 9910。new 校验后，Price 值无法由普通调用方改成零。资金和成交额没有浮点误差。Engine 拥有 Account 和订单，策略只能得到 &Account。你熟悉的“引用不是复制底层存储”在 Rust 里依然成立：&Account 借用同一账户；active 的 usize 只是 orders 的位置，不会复制订单。

思考：Price 和 Quantity 都包含 u64，为什么不直接用 u64？为什么 Account 可以 Clone，但策略不需要 Clone 账户？

练习：在测试里尝试构造零价格、数量 0、边界最大值，解释为什么校验应该在输入边界完成。

```sh
./scripts/cargo.sh test validated_price_and_quantity_boundaries
```

## 第 2 课：一笔买入的全过程

阅读 Engine::submit、Engine::on_quote 与 Account::apply。暂时忽略 ScanPolicy，按默认 Active 顺序跟踪。

先运行不到六十行的 examples/first_trade.rs，只观察一张订单：

```sh
./scripts/cargo.sh run --example first_trade
```

该例有手续费并分两次成交，最终 cash=7017、position=3。下面先去掉费用来简化手算。

已有现金 10000 分，提交数量 3、limit 100 分、零手续费。submit 后 cash 仍是 10000，reserved_cash 为 300，position 为 0。下一条报价 ask 为 99，成交后 cash 为 9703，position 为 3，冻结现金归零。

这里把“交易意图”“资源承诺”“真实成交”分开表达。你可以在纸上列三列：cash、reserved_cash、position，逐事件填入数值。

练习：把下一条 ask 改成 101，预测账户与订单状态；再加一条 ask=99，检查自己的预测。

```sh
./scripts/cargo.sh test zero_latency_still_requires_a_later_quote
./scripts/cargo.sh test non_crossing_limit_does_not_fill
```

## 第 3 课：部分成交与资源约束

阅读 OrderStatus、try_execute 与 cancel_index。学习 match、可变借用、生命周期状态与账户不变量。

同一条报价 ask_quantity=5，两个买单各买 4：第一个成交 4，第二个只能成交 1。把可用量分别传给每个订单而不递减，会制造不存在的流动性。

练习：先提交三个同价订单，取消第一个，再给只够填满一个订单的流动性。为什么不能用 swap_remove 删除活跃索引？观察 compaction_keeps_submission_priority。

再检查手续费：100 分的资产，1 bps，拆成三次每次一单位成交。每次收费向上取整都是 1 分，整单 300 分的费用却只需 1 分。如果只冻结整单费用，最后一笔可能没有足够现金。我们的逐单位冻结保守地覆盖这个问题。

```sh
./scripts/cargo.sh test partial_fills
./scripts/cargo.sh test fifo_and_shared_liquidity
./scripts/cargo.sh test conservative_fee_reserve
```

## 第 4 课：模拟时钟和因果顺序

阅读 Quote、on_quote 的输入校验、eligible_at_ns 与 src/replay.rs。模拟时间来自输入，不来自系统时间；系统时间仅用于默认报告目录名。

订单提交时的 sequence 必须小于执行报价的 sequence。latency=200ns、提交时刻 100ns：200ns 的报价不能执行，300ns 的报价可以。即使延迟为零，也要等后续事件。这是明确的教学仿真假设。

练习：给两条报价相同 timestamp、不同递增 sequence，解释为何有效；交换两条 sequence，观察错误以及账户不变。

```sh
./scripts/cargo.sh test latency
./scripts/cargo.sh test malformed_quote_is_atomic
```

## 第 5 课：把策略与执行分开

阅读 StrategyView、Strategy、ThresholdStrategy，最后阅读 MomentumStrategy。学习 trait、泛型静态分发和 enum 动作。

ThresholdStrategy 只决定什么时候提交订单，Engine 决定是否接受、什么时候执行、能成交多少。RollingMean 在当前事件更新指标，不读取未来报价。MomentumStrategy 只是工程练习，信号是否有效要用真实且清洗过的数据研究。

先运行示例并手算两轮买卖：

```sh
./scripts/cargo.sh run -- --output reports/lesson-5
```

费用 415 分，最终现金 1014065 分。读 events.csv，把每笔成交重新累计。max_drawdown 使用每条报价上的 bid 权益，含价差损失与已支付费用。

练习：复制 demo.csv 到自己的文件，只改一条后续报价，解释哪些已发生事件不会变化，哪些尚未成交订单可能变化。不要把练习数据上的盈利当成策略验证。

## 第 6 课：CPU 性能与算法工作量

阅读 ScanPolicy 两个分支、active.retain、RollingMean，再看 examples/bench.rs 与 BENCHMARK.md。

历史扫描的总工作量随累计订单增长；活跃索引的工作量跟当前活跃订单数走。优化不改执行规则，而是减少触碰的数据。环形窗口避免移动窗口元素，也避免重复求和。这里只研究单线程 CPU 执行，尚不需要原子操作或无锁队列。

```sh
mkdir -p reports
./scripts/cargo.sh run --release --example bench -- 7 > reports/my-benchmark.csv
```

先看 orders_examined，再看耗时。所有订单都活跃的对照为什么可能慢一点？小窗口下增量均值未必有优势，测到什么就解释什么。batch_ns 是一整批操作的时间，不能把 median_batch_ns 当作逐事件 p50，也不能声称测到了实盘 p99。

## 第 7 课：补充量化研究能力

### 深度簿：数据布局的收益和代价

先阅读 src/book.rs 和 examples/book_to_engine.rs。两侧数组均按价格升序，best bid 在末尾，best ask 在开头。已有档位更新通过二分查找定位；插入/删除需要移动元素。有界容量允许预分配，不代表插入删除变成 O(1)。

```sh
./scripts/cargo.sh run --example book_to_engine
./scripts/cargo.sh test --test book
./scripts/cargo.sh run --release --example book_bench -- 7
```

观察 64 档和 8192 档的结果。小簿连续存储的优势，何时会被大簿插入/删除的移动成本抵消？在本次负载中，8192 档数组比 BTreeMap 慢约 2.8 倍。练习：删除最优报价档位，预测新 best；再人为制造序号缺口，确认没有修改簿和时钟。

随着项目推进学习：收益率、价差/手续费、回撤、换手率、概率与期望、方差、相关性、线性回归、时间序列训练/验证划分。你需要能解释“净收益为什么变化”，也需要理解多次尝试策略参数会造成选择偏差。

0.2 已加入整数网格元数据、BookFeed缺口/快照、纸面运行和持久恢复。后续研究先补真实历史数据与样本外实验，再按市场需求增加网络协议、队列/流动性校准与独立执行回报/撤单延迟。根据实际测量，再决定多线程、SIMD或Linux专项profiling。

项目更新沿用小步方式：改一个机制，解释所有权与交易含义，用手算或不变量验证，必要时测量，再做范围明确的提交。


## 第 8 课：多资产和批量策略

阅读 config.rs、paper.rs 和 examples/custom_strategy.rs。跟踪 `(market, order_id)` 为什么比单独的订单号更完整，以及不同资产怎样拥有各自的账户预算。比较泛型 Strategy 的静态调用与平台 Box<dyn Strategy> 的动态回调。

运行 custom_strategy，手算两笔成交的300分金额和两次各1分费用；再把流动性改为2，预测哪张IOC被取消。运行 tests/paper.rs，解释动作缓冲溢出为什么不能执行前64个动作。

## 第 9 课：从日志恢复，而不是复制内存

阅读 journal.rs。顺序是校验输入 → 写入命令和哈希 → sync_all → 修改内存 → 回执。分别在“同步前”和“同步后但未确认”中断，预测重投同一编号会发生什么。

```sh
./scripts/cargo.sh test --test journal
./scripts/cargo.sh test --test paper_cli killed_process_recovers_durable_ack_and_retries_safely
```

练习：对比观察events.csv与完整输入WAL，解释为何accepted事件不足以还原限价委托。区分残缺尾帧、完整帧校验失败，以及整个有效尾部被删除；不要把SHA链当成认证签名。恢复测试共用运行逻辑，所以也要保留前几课的独立手算。

## 第 10 课：运行风控和可靠性成本

阅读 expire_feeds、halt_market 与 OPERATIONS.md。解释新鲜报价到达前为何先撤销超时订单；解释暂停后持仓还会随行情变动。

```sh
./scripts/cargo.sh test --test paper
./scripts/cargo.sh run --release --example paper_bench -- 2000
```

观察内存处理与每命令同步落盘的差别。下一次优化应提出明确需求：降低模拟CPU时间、提高持久吞吐，还是缩短恢复？这些目标需要不同负载和验证，不能用一个漂亮倍数互相代替。
