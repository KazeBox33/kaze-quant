# Binance Spot 测试网与实时纸面运行

## 公开实时行情，不需要密钥

```sh
mkdir -p reports
cargo build --locked --release --features network --bins
cp target/release/kaze-live-paper reports/live-paper-binary
reports/live-paper-binary configs/binance-live-paper.json reports/live.db BTCUSDT 600 reports/live.json
reports/live-paper-binary configs/binance-live-paper.json reports/live.db BTCUSDT audit reports/live-recovered.json
```

网络功能是可选的；核心 crate 默认不加载 HTTP/WebSocket 依赖。公共行情主机固定为 `data-stream.binance.vision:443`；全部订单在本地仿真。4096帧通道（原始payload容量<=32MiB）、8KiB帧上限、5ms/256条批次。网络读取/写入有超时，队列满、断线、非法数据或有效报价过期会持久停止。重复ID不会延长报价新鲜度。行情没有交易所时间字段，报告明确只度量 socket read 到持久确认，不能当作交易所到订单延迟。

使用新会话建立每次连接，避免把中断时漏掉的行情当作连续数据。结束后取消未成交委托，持仓保留。`audit` 必须使用原二进制、相同配置/选项；更新版本前保留二进制及完整数据。报告记录实际配置、二进制、已消费原帧摘要和完整审计结果。schema 2使用全程固定内存直方图，包含最后部分批次；分位数返回桶区间，分开记录排队/等待/事务/确认与最慢分钟，详见[延迟统计](TELEMETRY.md)。历史schema 1仅前100000条且遗漏最后部分批次。

运行24小时可将600改为86400；Mac需保持唤醒和网络稳定。`success=true`只代表该次纸面运行完成，不代表订单真实性或策略盈利。首轮24小时实验在27.8分钟后断线/过载停止，失败会话恢复审计通过；第二轮在53.4分钟队列满停止并恢复通过；尚没有全天或多日验收记录。故障诊断的具体原因进入error字段，主线程优先停止而不继续消费剩余队列。

## 外部测试网订单

在 [Binance Spot Testnet](https://testnet.binance.vision/) 创建 HMAC 测试网密钥，使用测试网虚拟资产。不要使用主网密钥，也不要将密钥发送到聊天或提交到Git。

在仓库中创建 `configs/testnet.credentials.env`，权限600，内容为两个普通赋值：

```text
KAZE_BINANCE_TESTNET_KEY=YOUR_TESTNET_KEY
KAZE_BINANCE_TESTNET_SECRET=YOUR_TESTNET_SECRET
```

```sh
chmod 600 configs/testnet.credentials.env
```

也支持同名本地环境变量。文件路径已加入 `.gitignore`，程序不打印密钥、签名或请求 URL，禁止重定向。报价/数量使用8位精确十进制，不静默舍入下单值。外部 CLI 只接受固定测试网主机，不能切换到主网。

复制 [意图模板](../configs/testnet-intent.example.json)，修改唯一的 `kaze-` 前缀ID和当前有效限价/数量；模板旧价仅展示格式，可能被交易所价格过滤拒绝。核对实际交易对 `PRICE_FILTER` / `LOT_SIZE` / `MIN_NOTIONAL` 或 `NOTIONAL` 后运行：

```sh
target/release/kaze-testnet reports/testnet.db submit configs/my-testnet-intent.json 20
target/release/kaze-testnet reports/testnet.db reconcile
target/release/kaze-testnet reports/testnet.db cancel kaze-YOUR-UNIQUE-ID
target/release/kaze-testnet reports/testnet.db reconcile
target/release/kaze-testnet reports/testnet.db audit
```

可复现基础验收（会发送最多4张测试网订单，每单<=20 USDT；建议独立测试网账户，期间不要并发交易）：

```sh
python3 scripts/testnet_acceptance.py --output reports/my-testnet-acceptance
```

每步在独立进程中执行；真实买卖后根据LOT_SIZE可能留有少量虚拟资产尘埃。`submit-drop-ack`显式丢弃成功提交回报，用于验证unknown恢复；不是实际断网。汇总只报告观测事实，零手续费或没有部分成交都不能假装已测。失败只尝试查询/撤销本地身份，不重发未知订单或擅自撤销外部订单。

最后数字是该单USDT风险上限，不能超过100。实时交易所过滤和可用余额也会检查；动态价格带、账户级过滤由交易所最终检查。预检失败不创建意图；POST错误/超时后的意图保持未知，重复提交相同ID只查询。即使查询返回不存在，也不自动重发或删除未知意图，防止最终一致查询导致重复订单。需要人工查明后在新隔离测试会话中继续。

撤单先按已知的交易所orderId查询（尚未知时使用client ID）、记录取消未知状态，再发送DELETE，最后按稳定交易所orderId查询原订单证明结果。删除响应的新取消ID保留为远端身份，不能覆盖原意图ID。单次成交查询满1000条会停止而不是假装完整；此版本需要小规模独立测试账户。存在其他未跟踪订单会阻止对账（在已跟踪/目标交易对范围），不会擅自撤销用户外部订单。

测试网账本与纸面账本分开，禁止把模拟成交当作真实交易所成交。当前提供可调用的执行接口和操作CLI，新增受限Spot原币账本与有界私有回报；仍未将任意策略自动路由至测试网，也没有连续节点或完整跨资产组合估值。缺少测试网密钥时，只能完成模拟故障测试，不能宣称真实订单验收通过。

## 私有回报与原币资产核对

新增`ledger-init`、`stream-watch`和`stream-submit`。全新账本绑定全账户期初余额，私有回报按执行身份去重并原子记录原币手续费；最终REST补查核对每币种free+locked。完整运行命令、故障策略、公开证据与局限见[EXTERNAL_LEDGER](EXTERNAL_LEDGER.md)。旧账本不自动补期初资产，有界人工验收不等于连续实盘节点。


## 历史补洞与连续观察后续增量

新期初 REST 历史游标、整轮原子补查、持久连接代次与有界只读重连观察已经实现。验收、完整本地恢复计时、SQL索引修复和旧版24h失败见[HISTORY_RECOVERY](HISTORY_RECOVERY.md)。本增量替代本文之前“历史游标/连续重连尚未实现”的状态；不表示自动策略路由、全天运行、主网、真实部分成交或完整上游平台性能排名通过。下一顺序仍是目标仓位与可组合策略，再进入持久TWAP和更严谨的研究/仿真。
