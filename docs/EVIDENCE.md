# 0.3 真实数据、速度与可用性证据

2026-10-06，在 Apple M5 / 10 核 / 24GiB RAM / macOS 26.5.2 / Rust 1.99.0 上测量。release、thin LTO、codegen-units=1，单写入者、不绑核，接 AC 电源，本地文件系统。完整环境与实际可执行文件 SHA-256 在 [environment-mac.json](evidence/environment-mac.json)。测量的核心代码固定于 [a8171ae](https://github.com/KazeBox33/kaze-quant/commit/a8171ae11d341737482979afacb2dd08e5b50a65)。后续 CLI 将 CSV 摘要改为同一消费流计算；这里保留测量二进制身份，不把旧结果冒充新二进制结果。没有与其他交易框架做速度排名。

## 数据身份

来源为 Binance 官方 BTCUSDT USD-M futures bookTicker **2024-01-01** 日文件，下载压缩包 128,284,409 bytes，SHA-256 `f9d281b949ac10aa59af3a7b07f41b4e640a8ecc76210f97c4c25ae7c8173e7c`，与官方 `.CHECKSUM` 一致。

使用原档案连续前 1,000,000 行，从 00:00:00.011 到 01:45:38.913 UTC，共约 106 分钟。没有筛选价格或按未来时间重新排序。规范化文件 58,551,133 bytes，SHA-256 `4725608a261cbff4a2879bef7d7cff7984eb1f70248ba4e627b56ab0eeeafd31`。转换记录中的时钟修正、数量向下量化、连续重复 update_id 均为 0；不由此推导整天无缺口。

价格表示每 1e-5 BTC lot 的 1e-8 USDT minor 数；原始价格乘 1000，原始数量乘 100000。原始 event_time 为此固定 futures 文件的毫秒，转换为 ns；内部序号是档案行索引。详细来源、原始前缀哈希、单位和规则在 [数据 manifest](evidence/btc-prefix-1000000.manifest.json)。下载和转换不需要密钥，脚本固定 SHA，官方文件若变更会拒绝覆盖数据版本。

这些是期货报价，但执行账户仍是无杠杆只做多的纸面模型；没有期货保证金、资金费、真实排队或冲击。行情来源的真实性不等于模拟成交一定可在真实市场实现。

## 吞吐与确认成本

合成负载交替报价、单位买单、报价、单位卖单，锁价、零费用，验证每个版本最终账本相同。每组合重复 3 次。原始数据：[2,000 命令](evidence/store-2000-mac.csv)、[200,000 命令](evidence/store-200000-mac.csv)。计时包括处理、序列化和事务提交；不包括初始化、恢复和全量审计。运行顺序固定，未隔离其他系统活动。

| 2,000 命令模式 | 总时间中位数 | 吞吐约值 |
|---|---:|---:|
| 文件 WAL，每命令 sync_all | 7.999s | 250/s |
| SQLite FULL，每命令事务 | 8.259s | 242/s |
| SQLite FULL，每批 64 | 135.004ms | 14,814/s |
| SQLite FULL，每批 256 | 39.875ms | 50,157/s |

256 批量路径相对逐条文件同步，整批吞吐约 **约 200 倍**。这个提升来自合并同步次数，SQLite 单命令事务并未更快。两条路径的系统刷新方式也不同（SQLite 在 Mac 请求 fullfsync），不能用它们推导设备掉电保证相同。2,000/256 只有 8 次事务，不足以证明尾延迟稳定。

更大负载给出 200,000/256 = 782 个真实计时事务/轮：

| 200,000 命令模式 | 总时间中位数 | 每轮事务 p99 | 正常关闭后恢复 |
|---|---:|---:|---:|
| SQLite FULL，64/批 | 14.647s | 10.89–11.98ms | 12.52–14.84ms |
| SQLite FULL，256/批 | 4.802s | 17.90–18.03ms | 13.01–13.37ms |

事务 p99 是 `execute_batch` 的耗时，**不是逐事件延迟或含排队的持久确认 p99**。默认 CLI 另外最多等待首条入批后的 5ms 触发提交，还受队列/操作系统/stdout 调度影响。批次越大，吞吐越高，但单批计算和尾延迟也可能增长。200,000 命令全量重放审计约 353–365ms，普通恢复仅读取有限热检查点和尾索引。

## 百万真实报价端到端运行

包含 CSV 字节哈希、流式解析、有限通道、策略/风控、候选状态、每批事务、SHA 链和检查点写入，`--quiet` 排除逐条 stdout 传输。不包括打开数据库、全量审计和最后写报告。每种模式 3 次，最终状态/命令链重复相同，恢复后全部命令通过审计。

| 模式 | 端到端吞吐范围 | 100ms 采样峰值 RSS | 正常关闭后恢复 | 最终热检查点 |
|---|---:|---:|---:|---:|
| passive，百万报价，EOF 保留会话 | 44,833–46,739 命令/s | 6.48MiB | 13.69–14.63ms | 664 bytes |
| mean_reversion，百万报价，EOF 保留会话 | 42,681–43,423 命令/s | 7.52–7.69MiB | 11.91–14.22ms | 52,393 bytes |

mean_reversion 包含 4096 报价滚动窗口及有限终态订单；数据库逻辑大小约 279.7MiB，WAL 约 4.1MiB。磁盘历史随命令增长，RAM 不保留全量命令身份。RSS 是外部 `ps` 采样，可能遗漏瞬时峰值，也不是分配器探针或“绝不泄漏”的证明。恢复时间包含二进制哈希，测量在缓存较暖的本机环境；崩溃恢复还可能处理 SQLite WAL，不承诺固定毫秒 SLA。

原始状态、各轮时间/身份、RSS 和恢复报告在 [real-quotes-mac.json](evidence/real-quotes-mac.json) 及相邻 CSV/JSON。内存运行对照位于 [memory-reference.json](evidence/memory-reference.json)，与持久回放逐报价之后的最终状态相同；它共用执行内核，只证明存储投影一致性，执行规则另有手算测试。

## 千万条单会话扩容验证

另取同一官方档案连续前 10,000,000 行，覆盖约 **20.815 小时**，转换仍无时钟修正或数量量化。数据身份见 [千万条 manifest](evidence/btc-prefix-10000000.manifest.json)。使用同一策略与2bps费率，将存储页配额预先设为16GiB；没有通过清空历史重置会话。

单次运行处理 10,000,000 报价与1条finish，耗时 **222.097s，约45,025命令/s**，39,064个事务。采样峰值 RSS **7.55MiB**，热检查点 **52,545 bytes**，磁盘审计历史 **2,934,308,864 bytes**。正常关闭后热检查点恢复 **7.46ms**；10,000,001条命令两次全量审计通过，恢复前后状态与链一致。进程总耗时约247s包含第一次全量审计和输出，这与222s回放时间分开记录。

实际有2025张接受订单、2014次成交和30次撤销，包含部分成交/IOC路径。finish撤销剩余委托而不自动平仓，最后仍有0.01 BTC，现金9344.04643764 USDT，按最后bid估值的权益9782.72643764 USDT，净损益-217.27356236 USDT，已付费用170.81396236 USDT。不能把剩余持仓当作零风险或已实现损益。

完整结果在 [soak-10000000-mac.json](evidence/soak-10000000-mac.json)，RSS序列在 [相邻CSV](evidence/soak-10000000.rss.csv)。这是一轮加速历史压力实验，未重复三次，也没有20.8小时墙钟运行；它验证超过旧百万命令上限后，磁盘历史持续增长而热状态仍受限制。不能由7ms暖缓存恢复推导掉电后的固定恢复SLA。

```sh
python3 scripts/market_data.py --rows 10000000
python3 scripts/real_soak.py --output reports/my-10m-soak
```

## “有用”的具体证据

固定策略参数 window=4096、entry=1bps、exit=0bps、quantity=1000 lot（0.01 BTC），假设 1ms 执行延迟；使用下一条符合延迟的报价共享实际报价数量。选择这些参数用于覆盖成交/恢复路径，没有做盈利参数搜索、训练测试拆分或统计优势证明。

2bps/成交费率下，发生 **184 次成交、92 次往返**，最终零持仓、零冻结、无风险拒单。初始 10,000 USDT，最终 9,980.38345540 USDT，手续费 15.65354460 USDT，净亏损 19.61654460 USDT。所有费用实际发生后才计入，未把委托当成持仓。

成本敏感度使用相同信号和数据，仅改变假设费率，三组均有 184 次成交：

| 假设每成交费率 | 手续费 USDT | 净损益 USDT |
|---|---:|---:|
| 0bps | 0 | -3.96300000 |
| 2bps | 15.65354460 | -19.61654460 |
| 5bps | 39.13386150 | -43.09686150 |

见 [cost-study-mac.json](evidence/cost-study-mac.json)。这些费率是实验假设，不代表交易所当前收费。平台的作用是让交易假设、执行成本和亏损可核对，不从“模拟器能成交”推导“策略能赚钱”。

## 故障与验收

固定随机种子 `0x4B415A45`，20 次在真实报价处理过程中向实际 CLI 发送 SIGKILL，重开并重投完整输入，每次执行完整审计。恢复命令数从 3,840 到 19,968，均 >= 当时已观察确认高水位；有数轮发现已提交但尚未收到回执的额外批次，重投正确去重。之后回放至百万报价并 finish，与全程不中断参考的最终状态和链完全相同。

见 [crash-soak-mac.json](evidence/crash-soak-mac.json)。这验证进程崩溃，不验证电源/控制器缓存丢失。其他实际测试包含 SQL 写入故障注入、真实 SQLite 1MiB 页配额耗尽、读者固定旧快照造成 WAL 反压、备份恢复、全部输入切分点的策略窗口/订单恢复、恶意数值检查点。102 项 Rust 测试在 debug/release 均通过，3 项转换测试通过。

本次可验收范围是本地、有限资源、可恢复的确定性回放与纸面执行。没有实盘下单、共享保证金、外部对账、就地扩容/轮换、跨二进制资金会话迁移、24小时连续运行或硬实时验收。已运行数百万条重复真实报价和20次进程故障，但不将短时压力实验命名为全天 soak。

## 复现

```sh
cargo build --locked --release --bins --examples
python3 scripts/test_market_data.py
python3 scripts/market_data.py --rows 1000000
cargo run --locked --release --example store_bench -- 2000
cargo run --locked --release --example store_bench -- 200000
python3 scripts/real_bench.py --output reports/my-real-bench
python3 scripts/cost_study.py --output reports/my-cost-study.json
python3 scripts/crash_soak.py --output reports/my-crash-soak --cycles 20
```

输出路径须全新；保留可执行文件（manifest 绑定 SHA）。CSV/ZIP/数据库不放进 Git，固定脚本、manifest 与结果足够核对。迁移 Linux 后重跑，不从 Mac 数值推导 Linux 的性能。CI 会上传 Linux/macOS 的同负载存储比较及恢复报告，虚拟机结果应与本机测量分开解读。

CSV摘要修复后的新CLI另复跑百万报价：单次耗时22.334s（约44,775命令/s），采样峰值RSS约7.61MiB，184次成交账本与原内存参考相同，全部命令审计通过。只重复一次，原始记录在 [post-hash-fix-mac.json](evidence/post-hash-fix-mac.json)，不与旧二进制三轮数据混算分位数。

## Linux / macOS CI 对照

代码提交18fe54c的 [GitHub Actions](https://github.com/KazeBox33/kaze-quant/actions/runs/37437055462) 已完成，两项job均success，含102项debug/release测试、严格静态/文档检查、备份恢复及存储比较。

相同2000命令负载，Linux托管机逐条文件WAL中位825.090ms、256批量8.673ms；macOS托管机分别1690.205ms、32.301ms。原始三轮CSV见 [Linux](evidence/store-2000-ci-linux.csv)、[macOS](evidence/store-2000-ci-macos.csv)，身份见 [CI记录](evidence/ci-20261006.json)。不同硬件/虚拟化/文件系统同步差异很大，Linux的一些单次样本存在百毫秒级抖动；256模式每轮仅8个事务，不能推导稳定尾延迟。此处证明跨平台同语义运行和可复现实验，不用托管机结果保证未来Linux生产机SLA。
