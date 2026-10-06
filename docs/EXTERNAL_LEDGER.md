# 外部执行第一步：有身份、有账务、有恢复证据

本阶段借鉴 [vn.py gateway](https://github.com/vnpy/vnpy/blob/c6e231caf32b7fc97e6459817fff66458cf7e7c4/vnpy/trader/gateway.py) 的职责边界和 [Nautilus 运行对账](https://nautilustrader.io/docs/latest/concepts/live/) 的问题划分，独立实现 Rust 状态机。没有复制或翻译上游源码。目标是先让一笔外部订单能解释、能恢复，再扩展目标仓位、信号/仓位/过滤和 TWAP。

## 可运行入口

先按 [TESTNET](TESTNET.md) 在本机配置测试网密钥，保留同一可执行文件。以下只连接 Binance Spot Testnet 虚拟账户：

```sh
cargo build --locked --release --features network --bin kaze-testnet
mkdir -p reports
target/release/kaze-testnet reports/external-new.db ledger-init BTCUSDT
target/release/kaze-testnet reports/external-new.db stream-watch 10
target/release/kaze-testnet reports/external-new.db reconcile
target/release/kaze-testnet reports/external-new.db audit
```

`ledger-init` 要求全新 journal 和全账户无未结订单；从服务器 UID 派生带 venue 域的 SHA256 身份、绑定官方 base/quote 元数据和全账户期初余额。它不以 API key 作为账户身份，也不自动升级有历史成交的旧 journal。请保持测试账户专用于这个账本；期初余额与全账户未结订单读取并非交易所原子快照，账户隔离仍是必要前提。

每次打开账本都置为待核对，重新查询身份、已知订单、成交、全账户未结订单及全部资产后才允许新意图。旧未绑定 journal 仍沿用 v1 行为，`external_ledger=null`，不能当成已通过组合账务核对。

```sh
# 自动生成约15 USDT的被动买、穿价买、穿价卖，各只发送一次；全部为虚拟订单。
python3 scripts/external_acceptance.py \
  --binary target/release/kaze-testnet --directory reports/my-external-pilot

# 手工测试自己的 LIMIT/GTC 意图；上限仍为100 USDT，观察5..300秒。
target/release/kaze-testnet reports/external-new.db stream-submit intent.json 20 10
```

`stream-submit` 要求已有绑定，订阅认证成功后才进入订单验收。观察结束撤销该意图仍开放的剩余量，再消费短尾事件并进行 REST 完整核对。不自动卖掉已成交持仓；失败也不重发。故障后使用 `reconcile`/`cancel` 查询与处理原 ID，不能换新 ID 盲目重试。这个入口是有界人工验收，不是连续策略实盘节点。

## 核算与故障规则

价格、数量、资产余额以 1e-8 整数/i128 核算。买入增加 base、减少 quote；卖出反向；手续费直接减少交易所给出的币种。例：期初 1 BTC、1000 USDT、2 BNB，买入两笔各0.01 BTC/1 USDT，一笔收0.0001 BTC，另一笔收0.001 BNB，期末应有 **1.0199 BTC、998 USDT、1.999 BNB**。没有汇率时不换算第三币手续费、不声称策略 PnL。

资产标识保留原始 Unicode 和大小写，限制64 UTF-8字节、字母/数字或连字符/下划线；不能假设全账户币种都为全大写 ASCII。本轮真实测试网返回502种资产，其中有中文资产名称；初版绑定因此失败，未下单。修复后用大小写不同和中文标识加入回归测试，不删除零余额资产来掩盖问题。可交易的 symbol 仍受现有大写 USDT Spot 约束。

私有执行事件的订单/成交/资产变动在同一 SQLite WAL/FULL 事务写入。REST补查的订单证据与成交页分开写入，成交与资产变动仍同事务；只有整轮成功才恢复Ready。成交按 `(symbol,trade_id)` 去重；REST 与推送的小数写法先规范化，零费用的币种置空；同 ID 不同经济内容是错误。成交必须属于已绑定工具及已知订单，方向/限价/报价金额/累计量接受边界检查；迟到事件可补入未见成交，但不能复活终态或倒退当前订单。私有回报按 `(symbol,order_id,execution_id)` 去重，重复事件不重复扣费。

当前私有协议依据固定 [Binance 文档提交](https://github.com/binance/binance-spot-api-docs/tree/b45e91d9824b6d523e8c9656de4efd1083c261e7/testnet)，文件哈希见 [来源清单](evidence/v07/protocol-sources.json)。支持 HMAC `userDataStream.subscribe.signature`、指定 subscriptionId、executionReport、部分账户更新、余额变动和结束事件。单帧64KiB，处理Ping/Pong；主机固定，不跟随重定向，不保存认证请求，不输出签名或密钥。

部分账户推送只使状态需要 REST 核对，不能代替全账户快照。外部余额活动、陌生订单、账户身份冲突或矛盾私有事件会留下不可自动清除的阻断；不自动创造“余额调整”。缺失成交或资产总数不一致同样阻止新订单。核对 `free+locked`，因为冻结资产仍属于账户；本账本尚不推导逐项可用/冻结分配，也没有共享现金策略风险预算、衍生品和 FX 估值。

全审计从原始成交重算资产变动，与物化表比较，再按期初+变动得到应有资产；不能只信缓存余额。这个复算复用同一交易经济函数，手算测试另行检查公式；它不是异构实现证明或认证签名。若外部活动净额恰好为零且发生在未覆盖的私有流间隙，总资产匹配也不能证明活动不存在。历史交易游标、外部活动分类、持续重连及跨进程私有流高水位仍待实现。

每个账本上限：10000意图、100000订单观测/成交/账户快照/私有执行事件，64工具、4096资产，SQLite主文件约1GiB上限；磁盘历史未轮换，WAL还需要部署侧磁盘监控。容量耗尽拒绝继续，不能称无限运行。

## 测试与性能证据

新增18项核心测试覆盖原币手续费、卖出、整页回滚、重复/迟到/矛盾回报、累计超量、账户身份在查询前校验、陌生订单/转账阻断、重启、中文资产和物化表损坏。网络故障物理断包和交易所真实部分成交不能由合成事件测试冒充。

审计优化把逐订单全表交叉扫描改成有序身份聚合：O(订单×成交) 改为 O((订单+成交) log 订单)。可重跑：

```sh
cargo run --locked --release --example execution_audit_bench -- 2000
cargo run --locked --release --example execution_audit_bench -- 10000
```

计时包括十进制解析、聚合分配和诊断生成，排除输入生成、SQL、资产复算、网络和策略。合成乱序输入，每单10笔成交，并预设部分累计不符；一次预热、7轮交替顺序，完整诊断逐轮相等。参考路径只覆盖这个已知订单/买方向负载，不覆盖新增加的陌生成交等规则；那些规则由语义测试检查。因此性能倍数只适用于自己的审计组件，不表示整引擎提速或超越任何开源平台。原始计时、二进制哈希及环境见 [性能证据](evidence/v07/audit-performance.json)。

真实 Testnet 结果、失败尝试和准确版本验证会保留在本目录证据中；原始全账户观测与凭据只留本机，不公开。仍须完成真实部分成交、断流补洞和长时间新版本运行，再推进连续策略执行。此前24h旧二进制观察独立计入原版本，不能用作本阶段寿命证据。


本轮测量：Apple M5 / macOS26.5.2，Rust1.99.0，release thin-LTO/codegen-units=1，旧版24h观察在后台运行。2,000单/20,000成交的扫描/聚合中位为89.962/2.228ms（40.38倍），10,000单/100,000成交为2439.291/14.581ms（167.30倍）；不是逐事件延迟分位数或框架胜出排名。完整7轮见原始证据。

实际私有测试网：3次POST，1撤销、2成交，6条私有执行事件/2条成交回报。买入费用0.00000017 BTC，卖出费用0.01378932 USDT；所有币种资产变化与REST一致，新进程订单/成交/资产投影一致。另用[回报再投递样例](../examples/private_event_replay.rs)对已捕获6条事件全部重投，新增事件0、完整审计及资产不变。样例只做本机重投，不声称网络物理重复注入。[实际结果](evidence/v07/testnet-private.json)、[重投结果](evidence/v07/private-replay.json)、[失败尝试](evidence/v07/attempts.json)均保留。

```sh
cargo run --locked --release --example private_event_replay -- reports/my-external-pilot/execution.db
```

当前本机160项Rust测试在debug/release通过、15项原有Python数据/验收测试通过，fmt/严格clippy/rustdoc通过；新增有界Python验收脚本同时在真实测试网完整运行。精确代码SHA的Linux/macOS CI另外记录，不能拿旧版本CI证明本阶段。实际PARTIALLY_FILLED仍未观测，本阶段也没有完成新版本24h、物理断流、连续自动重连或策略盈利验收。
