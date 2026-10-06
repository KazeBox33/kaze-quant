# 全程延迟统计与安全收尾

实时纸面运行报告现在使用 schema_version=2。以前只保存最先100000条延迟且遗漏最终部分批次；现在每条已持久确认的报价都计入固定大小直方图，持续运行多久都不增加采集器容量。历史v04/v05报告保留原有采样语义，不能用新说明重新解释旧数值。

## 一个报价经过的四个时刻

```text
socket.read完成 → 主线程出队 → execute_batch开始 → SQLite持久确认
       received       dequeued         begin               ack
```

- queue_wait：dequeued − received，每条已提交报价一次。
- batch_wait：begin − dequeued，包括等待凑批、规范化和提交前工作，每条已提交报价一次。
- receive_to_durable_ack：ack − received，每条已提交报价一次。
- commit_per_transaction：ack − begin，每个报价事务一次；不是每条报价的确认延迟。

计时使用同一连接的单调时钟。没有交易所时间戳、网络传输或内核读取前等待，不表示下单至成交延迟。停止时尚未消费/未提交的帧不会假装得到确认；报告另列帧数、已提交报价和故障原因。采集器仅在提交后记录，失败事务不计成功样本。

## 固定内存与误差

每个2的幂区间划分32个子桶，使用2049个u64计数。一次record只定位桶和更新计数/极值/u128总和，不分配堆内存；导出JSON才分配。最近秩分位数返回lower_ns/upper_ns，真实精确分位数处于该区间。区间宽度至多约该数量级下界的3.125%；小整数受离散边界影响。min/max和sum为精确整数，mean_floor_ns向下取整。

单直方图本机大小16432字节；LiveLatency含四个全程直方图、当前分钟直方图和摘要，总共82240字节（约80.3KiB）。这是采集器本体，不包括帧队列、SQLite、线程、报告JSON或整个进程RSS。

schema 2消费者应读取receive_to_durable_ack.p99.lower_ns/upper_ns，旧p50_ns/p95_ns/p99_ns点值不再提供。JSON中的sum_ns是十进制字符串，避免客户端大整数精度丢失。

worst_minute按连接开始后的60秒窗口、ACK时刻归属，选p99上界最大的一分钟；相同上界时保留样本更多的窗口，末尾未满一分钟也纳入。必须同时看样本数：单样本窗口不能当作稳定p99。没有伪造空闲样本，也没有coordinated omission校正；它只描述实际已提交输入。

## 过期批次与退出错误

消费时检查新鲜度之外，提交前再次检查整批每条报价，最老报价超过max_quote_age_ns则不触碰SQLite并持久Halt。边界等于阈值允许。这是提交开始时的检查，不承诺慢事务结束仍处于时限。

结束采集时关闭接收者并join网络线程，再决定持久Finish或Halt。只忽略主动关闭接收队列产生的consumer closed；队列满、网络故障、线程panic即使发生在收尾阶段也不能变成成功。最后部分批次触发风控暂停同样不能被Finish覆盖。没有确认任何报价的运行不能success。observation_duration_seconds只计观察过程；duration_seconds还包括退出与完整审计。

## 复现实验

```sh
cargo test --locked --all-features --test telemetry
cargo run --locked --release --example telemetry_bench
cargo build --locked --release --features network --bin kaze-live-paper
mkdir -p reports
cp target/release/kaze-live-paper reports/telemetry-binary
reports/telemetry-binary configs/live-telemetry-load.json reports/telemetry.db BTCUSDT 120 reports/telemetry.json
reports/telemetry-binary configs/live-telemetry-load.json reports/telemetry.db BTCUSDT audit reports/telemetry-recovery.json
```

需要全新DB/报告路径；保留原二进制用于恢复。配置为固定小单均值回归工程负载，只用本地模拟账户；没有向交易所发订单，也不能保证每次两分钟市场都会触发交易。

2026-10-06本机120秒：12151条真实Spot报价、2323个报价事务、六张模拟订单全部成交、最终空仓。全部报价纳入三个逐报价统计；新进程审计12152条命令，状态/链/二进制一致。socket read至持久确认p99区间25.165824–25.690111ms，最慢一分钟p99区间33.030144–33.554431ms（6677样本）。费用0.00516810 USDT，净损益−0.00509270 USDT；这是工程闭环而非盈利验证。[运行](evidence/v06/live-paper-load.json)、[恢复](evidence/v06/live-paper-load-recovery.json)、[核对](evidence/v06/live-paper-load-validation.json)。

单直方图记录组件在Apple M5/macOS26.5.2/rustc1.99 release下，100万次、一次预热、七轮中位4.113458ns/record；输入预载、导出不计时、没有CPU绑定，后台同时运行旧全天实验及短纸面实验。[原始七轮与来源身份](evidence/v06/telemetry-bench.json)。该结果排除了时钟读取、其余直方图、策略、网络与持久化，不能描述成整个引擎4ns、HFT优势或比其他平台快。

借鉴[HdrHistogram](https://docs.rs/hdrhistogram/latest/hdrhistogram/)的固定内存/桶误差思想，当前不是该库实现，也没有照搬源码或完整特性。故障收尾参考[NautilusTrader实时运行契约](https://nautilustrader.io/docs/latest/concepts/live/)。本轮本地142项Rust debug/release、15项Python及严格静态检查通过，[环境与源码身份](evidence/v06/validation-mac.json)。代码提交b29dc7c的[Linux/macOS CI](https://github.com/KazeBox33/kaze-quant/actions/runs/37464111653)全部通过，[CI身份](evidence/v06/ci.json)与[完整步骤](evidence/v06/ci-jobs.json)保留；不是24h通过证据。

旧24小时进程继续使用冻结的v05二进制，不具备本轮新统计/收尾检查。它的结果只能验收旧二进制；升级后需全新会话重新跑完整全天，不能混合版本或累计断开的分钟数。
