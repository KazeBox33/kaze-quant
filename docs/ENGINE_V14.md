# v14 / 0.5：订单存储与生命周期热路径重构

这轮改变 Engine 的订单存储方式。v13 已有活跃订单扫描，本轮解决的是 ID 查找、单笔撤单、终态保留与槽位回收。策略、报价因果性、资源冻结、IOC/GTC 与共享流动性规则不变。

## 结构和复杂度

每个物理 Slot 拥有一份可选 Order 和两套 prev/next 整数索引。历史链包含所有保留记录，活跃链只包含工作单，两条链都严格按提交 ID 增加；物理下标不代表优先级。Vec 扩容可以移动 Slot，但整数索引仍有效，不保存裸地址，不引入 unsafe 或新依赖。

HashMap 将生命周期唯一 OrderId 映射到当前槽位。归档先删除映射再释放槽位，下一笔可以复用槽位而不能复用 ID，因此旧 ID 无法误操作新订单。订单 ID 不向外暴露槽位编号，无需把物理槽位代次当作交易身份。

终态记录通过 BTreeSet<(id,slot)> 排序。必须按提交 ID 回收，而不是按终结时刻排队：老 GTC 可能比新订单更晚终结。归档只摘除超出保留预算的终态节点，存活订单不搬移、活跃链不重建。

| 操作 | v13 | v14 | 代价/边界 |
|---|---|---|---|
| 按 ID 查询 | 有序 Vec 二分 O(log R) | HashMap 平均 O(1) | 哈希表内存；不保证最坏时间常数 |
| 撤单索引维护 | 二分及 O(A) retain | 平均 O(1) 查找/O(1) 活跃摘链 | 终态索引插入 O(log T)；整笔撤单并非严格 O(1) |
| 回收 k 笔终态 | O(R) retain + 活跃重建，k=0 也扫描 | k=0 O(1)，否则 O(k log T) | BTreeSet 分配与更新成本 |
| 每报价工作单扫描 | O(A)，之后再次 retain | O(A)，终结时摘链 | 依旧检查未穿价/未到期工作单，没有价格队列或到期索引 |
| 快照 | 克隆有序 Vec O(R) | 遍历历史链打包 O(R) | 检查点不保存空槽、哈希种子、链接，不是增量检查点 |

R=保留记录，A=活跃订单，T=终态数。Slot 数不超过曾经同时保留订单的高水位，最大受 max_orders 控制；Vec/HashMap 可保留分配容量，不能把内存复杂度说成零开销。新表按需扩容，首次提交/扩容/哈希迁移、终态索引可能分配，本轮不承诺无分配热路径或硬实时延迟。重启重建索引只依赖保留记录，不依赖生命周期 ID 总数。

## Rust API 迁移

`Engine::orders()` 从 `&[Order]` 改为 `Orders<'_>` 借用有序视图。`len()`、`is_empty()`、`iter()`、`to_vec()`、Debug/相等比较仍可用。`orders()[i]` 沿链查第 i 个保留记录，成本 O(i)；业务定位应使用 `engine.order(id)`。需要连续切片时：

```rust
let owned = engine.orders().to_vec(); // 显式 O(R) 克隆
let slice: &[kaze_quant::types::Order] = &owned;
```

不隐式物化历史数组来维持旧切片接口，避免风控只读 len() 也产生分配。composition 子订单恢复核对改为直接 ID 查询。API 有破坏性变化，因此包版本升为 0.5.0。

EngineSnapshot 仍保存按 ID 增加的 Vec<Order>，经济字段与序列化形状保持一致。恢复先验证外部 ID 的唯一性/顺序，再重建所有索引并独立审计；重复 ID 返回错误，不触发内部 insert assert。SQLite/WAL manifest 仍绑定原二进制、规范化配置和存储选项，不能用新二进制直接续写旧会话；需要原二进制恢复审计或新建会话。

## 验证与重现

新增六项测试：重复回收的槽位高水位、晚终结老订单回收、索引/链损坏拒绝、槽位复用后 FIFO 与陈旧 ID、相邻 IOC 摘链与 finish 顺序、重复快照 ID 拒绝。保留所有既有账户、逐事件、事务回滚、策略和恢复测试。

`examples/order_store_replay.rs` 分别链接冻结 v13 与 v14，两个流动性模型各 20,000 步，包含费用/滑点/延迟、买卖、IOC/GTC、拒单、旧 ID 撤单、不同保留预算与每 17 步恢复。摘要覆盖每步结果、完整事件和完整 EngineSnapshot；不是把 ScanPolicy 两个入口共用新容器当成独立存储对照。双方仍遵循相同经济算法，不是独立交易所仿真器。

```sh
scripts/cargo.sh test --all-features --all-targets
scripts/cargo.sh test --release --all-features --all-targets
scripts/cargo.sh build --release --example order_store_bench --example order_store_replay
# OLD 是检出 f7a9e9d 后，复制相同 example 源码并用同一锁文件/编译器编译的程序。
python3 scripts/order_store_bench.py --old /absolute/OLD --new /absolute/NEW --output reports/engine-comparison --repeats 7
```

固定 128/4096/32768 工作单，分别计量整批撤单、10,000 次新增/撤单/归档、百万 ID 查询、1,000 条不穿价报价、100 次完整快照 JSON。准备与审计不计入组件时间，macOS time -l 的峰值 RSS 包含整个进程；所有七轮和状态身份保留。公开 BTC 纸面策略另做冻结 v13/v14、相同配置/原回执/各自新进程全审计的端到端交替对照，避免用组件提速代替实际平台数据。

最终数据见 [每功能数据](FEATURE_SCORECARD.md) 和 [v14证据](evidence/v14/manifest.json)。本轮不下单，没有新 24h/真实费用/真实部分成交/alpha验收。

## 后续底层实施顺序

1. **本轮完成**：订单槽位、直接 ID 查询、双顺序链、终态索引；以逐事件兼容和七轮性能/内存数据验收。
2. **下一轮价格与时间候选索引**：减少未穿价或未到期订单检查，保持本平台自己的提交顺序共享流动性。用混合密集/稀疏订单规模与实际策略负载验收；不把内部 FIFO 当交易所外部排队。
3. **增量事务投影与检查点**：解决每批全状态复制/序列化的 R 规模成本；需要新的存储版本、失败原子性、全审计等价与实际 SIGKILL 验收后才能切换，不能只把同步改异步。
4. **持续策略外部协调**：将已实现纸面策略连接到可轮换的多父计划会话；保留一次发送许可、原币费用/净量预算与恢复核对。先测试网，以非零费用、部分成交和全天运行作为验收，不扩大到主网。
5. **Linux 与成熟平台统一基准**：冻结输入/订单语义、版本/硬件/编译参数，分别列出共同功能和各自额外功能成本；完成之前不宣布全平台超越。

参考了 [NautilusTrader Cache](https://nautilustrader.io/docs/latest/concepts/cache/) 的状态索引/有界缓存思路与 [slotmap](https://docs.rs/slotmap/latest/slotmap/) 的可复用槽位/陈旧键隔离思路。这里保留本项目生命周期单调 ID、提交 FIFO 和有序经济检查点，不复制上游代码或引入它们的运行框架。这些参考不能作为胜过它们的跑分证据。
