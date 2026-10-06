# 0.2 交付验证

日期：2026-10-06。Apple M5 / arm64 / macOS 26.5.2，Rust 1.99.0。旧0.1的47个测试保留，新增34个测试，共81个。

| 检查 | 实际本地结果 |
|---|---|
| cargo fmt --check | 通过 |
| cargo clippy --all-targets -- -D warnings | 通过 |
| cargo test --all-targets | 81通过 |
| cargo test --release | 81通过 |
| RUSTDOCFLAGS=-D warnings cargo doc --locked --no-deps | 通过 |
| custom_strategy example | 两张IOC共享3单位，cash=999698，position=3，fees=2 |
| kaze-paper demo + finish | 40报价命令+finish；DEMO-A cash=1014065、fees=415；DEMO-B cash=501278、fees=82；持仓均0 |
| 重启分段重放 | 每段7条命令重新打开，逐回执与连续PaperRuntime一致，最终账户/委托/策略一致 |
| 强杀实际CLI子进程 | 8个持久回执后kill，整份输入重投，前8条去重；最终state与chain hash和连续运行相同 |
| 不完整WAL尾帧 | 7种长度/载荷/哈希截断，备份尾部，只保留完整前缀，重投后文件与完整参考一致 |
| 完整帧checksum损坏 | 拒绝打开，原文件字节不变 |
| 写入故障注入 | 只读句柄注入：不改状态、对象poisoned、禁止后续命令 |
| 配置/版本/容量/双写入者 | 拒绝冲突配置、部分manifest、非法长度、第二写入者及超限命令 |
| release可靠性成本实验 | 三轮2000命令，每轮内存/同步WAL/恢复状态一致，原始CSV公开 |

新增测试按模块：book_feed 5、paper 15、journal 9、paper_cli 4、日志故障单元测试1。原有模块：book 8、engine 25、replay 9、cli 5。

独立验证仍保留手算费用/资金守恒、预冻结资源、流动性共享、IOC/延迟边界、最大数值；12种子3000报价对照Active/History调度，8种子16000更新对照BTreeMap深度。双方共用执行实现的恢复/扫描对照不能独自证明模型正确；手算与边界测试提供额外证据。

帧级行情测试覆盖同帧中间交叉但最终合法、满容量删除后插入、重复档位、非法数量、缺口锁定与快照恢复；运行风控测试覆盖价格/金额/网格、无行情、先超时撤单后安装行情、回撤触发、跨资产watchdog、封闭会话与策略动作溢出。

## 验证范围

代码提交 `4e30f30030171f508126adb337dc0eed8ec610ea` 的 [GitHub Actions](https://github.com/KazeBox33/kaze-quant/actions/runs/37425102399) 已在2026-10-06完成：Linux和macOS两项job均success，包含工具链安装、锁依赖、静态检查、debug/release全部测试、严格文档检查和纸面恢复/重试冒烟。后续提交的结果仍以其对应Actions为准。

没有实盘订单/成交对账、真实历史数据队列/冲击校准、网络恢复、断电/存储控制器故障、长时间压力或RSS/分配探针。文件锁/硬链接依赖本地文件系统语义；不覆盖不遵守锁协议的外部写入。

当前可用范围为有界、多资产独立预算的确定性回放与纸面会话。没有日志轮换、账户迁移、快照加速、跨币种保证金或无限连续运行保证。完整有效尾帧的删除需要外部确认高水位才能识别。安全Rust也不能证明永不泄漏。

观察CSV不是恢复源；恢复只使用完整输入WAL和相同代码/配置。具体适用条件见CONTRACT与OPERATIONS。
