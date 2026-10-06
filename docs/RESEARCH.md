# 调研与设计取舍

调研日期：2026-10-04 / 2026-10-06（用户所在时区）。引用的分支和 latest 文档可能随后变化；本文件记录的是本次读到的设计。没有把上游代码复制进本项目，也没有把上游作为运行时依赖。GitHub API 的匿名访问达到限额、Git 协议访问失败后，通过网页读取官方文档和下面两份 Rust 源文件，未取得可固定的上游 commit。

## NautilusTrader

来源：[官方架构](https://nautilustrader.io/docs/latest/concepts/architecture/)、[官方回测说明](https://nautilustrader.io/docs/latest/concepts/backtesting/)、[Rust Price 源码](https://github.com/nautechsystems/nautilus_trader/blob/develop/crates/model/src/types/price.rs)。

它把行情、执行、风控、账户/组合分为明确职责，并在不同运行环境共享核心组件。Price 使用固定精度的底层表示与构造校验。

本项目借鉴职责边界、类型化价格和确定性事件处理的思路。为了让初学者看清调用顺序，首版在一个 Engine 内组合职责，不引入消息总线、多资产缓存和网络运行时。我们的 Price 只支持正数、固定“分”精度；上游支持的精度/资产范围远大于我们。后续要先引入 Instrument 的 tick_size、lot_size、币种，才适合扩展资产范围。

## HftBacktest

来源：[官方成交模型说明](https://hftbacktest.readthedocs.io/en/latest/order_fill.html)、[Rust 深度实现](https://github.com/nkaz001/hftbacktest/blob/master/hftbacktest/src/depth/hashmapmarketdepth.rs)。

官方说明强调历史回放不能被模拟订单改变，成交仿真依赖流动性和排队假设。所读深度代码用整数价格档位索引、HashMap 保存深度/订单，并维护最优报价相关状态。

本项目借鉴“先写清成交假设”的方法，只消费当前报价里显式提供的数量，同一报价不会给多张订单重复分配流动性。后来增加了有界 L2 深度更新模块，但没有实现它的 L3 深度、外部排队位置、feed latency 或市场冲击，也不把报价数量当成真实可成交保证。

## Rust 所有权与内存

来源：[Rust Book：引用环可能泄漏](https://doc.rust-lang.org/book/ch15-06-reference-cycles.html)、[Rust 安装说明](https://rust-lang.org/tools/install/)。

Rust 不保证消除内存泄漏。这里由 Engine 单独拥有账户和 Vec<Order>，活跃索引只保存 usize，策略借用只读视图。核心没有引用环、自定义 unsafe 或全局可变状态。0.2 边界增加 serde/serde_json 与 sha2，不把这些第三方依赖的内部实现等同于本项目 forbid(unsafe_code) 的范围。预分配且限制订单历史容量，防止长回放无限增加内存。保留历史直到 Engine 被 drop 是有界的设计性存储，不是不可达内存泄漏。

## 我们自己的优化与实验

1. **活跃订单索引**：自己的朴素对照版本每条报价扫描全部订单；默认版本只扫描活跃订单。历史保留在连续 Vec 里供审计，终结订单从活跃索引稳定删除，维护提交顺序。首次测量显示所有记录都活跃时索引有小幅额外成本，因此增加 active.len()==orders.len() 时的连续扫描路径。
2. **环形滚动均值**：保存窗口与 u128 增量和，替换逐次全窗口求和。通过独立窗口重算核对每个输出。

3. **小深度簿的有序数组**：每侧预分配 Vec，二分查找档位，常数时间读取最优报价。对照 BTreeMap 在同一受校验更新流上核对和计时。64 档约快 1.8 倍，但 8192 档约慢 2.8 倍，说明该布局适合有限深度，不能作为大型订单簿的通用性能结论。

这些是为本项目独立实现、验证的优化，不声称算法在业界首次出现，也不声称性能超过上游项目。对照双方共用执行/核算，因此扫描对照只验证调度优化；手算账本与边界测试承担执行规则的独立验证。

性能数字来自自己的基准，见 [BENCHMARK.md](BENCHMARK.md)。所有订单都活跃时，索引并不减少扫描量，甚至可能有额外成本。下一步的优化必须由对应负载下的测量决定。


## 0.2 持久化与资产索引调研

2026-10-06 读取 [NautilusTrader Event Sourcing](https://nautilustrader.io/docs/latest/concepts/event_sourcing/) 与 [Barter 官方仓库](https://github.com/barter-rs/barter-rs)。上游 latest 页面与默认分支可能变化，以下为本次设计参考，不构成代码复用或性能比较。

Nautilus 将有序状态变更记录与缓存投影区分，使用运行 manifest 和高水位描述恢复依据。我们借鉴完整输入/运行身份/重建状态的边界，但实现方式不同：本项目将规范化报价也放入有界 WAL，同步写入作为内存处理的前置条件；没有异步队列、redb、二级索引或快照。同步成本通过自己的实验公开，不把上游异步捕获能力描述为本项目能力。

Barter 提供事件驱动交易框架、Strategy/RiskManager 扩展与索引组织的状态。本项目借鉴稳定整数 market ID 和状态访问边界，独立实现 Vec<Market> 路由与预算隔离；没有复用其引擎或连接器，也没有实现共享资产账户。

实现依据：[Rust File 锁与同步](https://doc.rust-lang.org/std/fs/struct.File.html)、[Serde 容器属性](https://serde.rs/container-attrs.html)、[sha2 API](https://docs.rs/sha2/latest/sha2/)。价格/数量使用 try_from 验证反序列化；JSON 无参数命令使用空结构变体，避免单元变体忽略额外字段；该边界有实际失败后修复的回归测试。

我们的特色是把小型 CPU 执行内核、有界策略/风险边界和可验证恢复明确分开。性能、持久性与教学可读性各自有可检查的代价，而不是把更多框架堆在核心热路径上。
