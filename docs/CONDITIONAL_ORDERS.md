# 本地条件引擎与突破退出策略（v15 / 0.6）

## 使用

```sh
cargo build --locked --release --bin kaze-run
mkdir -p reports/bracket
# 使用全新db/report；以下都在纸面引擎内，不发网络订单。
target/release/kaze-run --config configs/breakout-bracket-demo.json \
  --input data/breakout-bracket-demo.jsonl --db reports/bracket/session.db \
  --finish --verify-full --report reports/bracket/result.json
target/release/kaze-run --config configs/breakout-bracket-demo.json \
  --db reports/bracket/session.db --recover-only --verify-full \
  --report reports/bracket/recovered.json
python3 scripts/conditional_crash.py --output reports/my-conditional-crash
```

标准注册策略`breakout-bracket`版本1：第一报价提交ask突破入场条件；下一报价可以激活Buy GTC；实际买满后生成bid止损/止盈OCO；退出成交后Complete。demo手算买3@101、卖3@111、两次手续费各1minor：10000→10028、持仓0。这个虚构盈利是账本测试。

真实数据配置`configs/btc-breakout-bracket-v1.json`冻结于测量前，使用公开首报价附近的绝对价格、0.002BTC单次循环。它不是训练后盈利策略。单位同现有BTC配置，不直接用100或101示例价格交易BTC。

## 意图与普通订单分开

```json
{"seq":2,"command":{"type":"submit_conditional","market":0,"request":{"reference":"ask","direction":"above_or_equal","trigger":101,"order":{"side":"Buy","limit":102,"quantity":3,"time_in_force":"GoodTilCancelled"},"expires_at_ns":1000,"oco_group":null}}}
```

先用该会话配置输入seq1报价，随后顺序递增；字段枚举大小写遵循已有OrderRequest协议。取消命令为`{"seq":3,"command":{"type":"cancel_conditional","market":0,"conditional_id":1}}`。策略也可发`Action::SubmitConditional/CancelConditional`；`on_conditional_event`收到条件回执，`on_event`仍收到普通订单成交。

条件ID与普通订单ID分别单调，`Triggered`记录映射及触发quote sequence。Accepted只说明意图进入等待表；Triggered说明普通子订单已经接受；Fill才说明成交。等待中的意图不冻结资金或持仓，触发时重新走原有资金、持仓、价格带、历史和工作单上限。

## 因果与失败规则

- 四种组合：bid/ask × >=/<=。等于阈值算触发，仅报价序号严格晚于提交序号才有资格。
- Paper先推进全局时间并检查所有资产watchdog，过期条件（expiry<=now）先撤销，再处理既有工作订单成交/回撤停止，再触发条件，最后运行报价策略。激活的零延迟子单也不能在触发报价成交。
- 同报价多个条件按ID顺序激活。触发失败一次即终结为ActivationRejected，不自动反复尝试。新意图须新身份；OCO同组其余等待意图保留，可以继续独立接受风控。
- **OCO在一个子订单被接受时取消同市场同组的其他等待意图。** 不是等待它成交；不撤销已经工作的其他子单，不是交易所原生原子OCO。组ID由调用者管理，重用已清空组不表示新的独立全局身份。
- Halt/Finish/watchdog撤销全部等待意图及工作单，释放工作单冻结；持仓不自动平掉。Advance也推进所有条件到期，不需要价格触发。
- stop-limit会遇到跳空不成交：bid跌到80、Sell limit88即使止损阈值90触发，也仍工作等待。没有收益/最大亏损保证。
- breakout-bracket只跟踪自己的成交，完整入场后才建立退出组；**部分入场尚无退出保护**。部分成交后人工撤单或halt会Paused，保留已成交仓位；不自动补单/重启/移动止损。单次循环、绝对价位、只做多，尚无动态ATR/trailingstop。

## 存储与复杂度

每资产等待容量=min(max_active_orders,4096)，独立于核心工作单占用；生命周期ID<=10^12。等待表BTreeMap，四组价格BTreeSet、到期BTreeSet、OCO组索引；价格选择O(log N+K)，恢复从规范化等待记录重建全部索引。纯索引跨组候选按ID排序，有K log K成本；Adaptive以完全跨阈值集合的容量估计密度下界，超过等待数1/4则切换ID顺序扫描，密集全触发不再逐ID查树/排序；部分跨阈值未达到下界时仍走索引；动态树/Vec分配，不能宣称零分配或硬实时。

条件、策略阶段、普通子单和账户在相同检查点/原始回执事务提交。失败批次不发布候选；重投相同命令去重，恢复后不会再次提交已激活子单。新增快照字段仅使用功能时出现；既有策略未使用条件功能的回执/报告经济状态仍可逐字节对照。

单条SQLite回执上限从64KiB提高到8MiB，整批编码回执总量限制16MiB；检查点仍64MiB。4096同报价激活专项验收覆盖原上限以上回执；过大多资产突发会返回容量错误并整批回滚，调用方须按确认记录重试，不能丢命令。复制/树索引/编码和更大单条额度增加资源成本，磁盘/WAL原有配额仍生效。

Rust公共Action枚举新增变体，版本升0.6.0；匹配语句要处理新变体。轻量CSV replay不执行条件动作，使用PaperRuntime/kaze-run。执行版本`kaze-paper-v2-conditional`、SQLite manifest版本`kaze-sql-v2-conditional`阻止旧二进制续写新会话；保留生成旧数据库的原二进制，不就地绕过版本绑定迁移。

## 上游参考与差别

参考[固定CTA停止单源码](https://github.com/vnpy/vnpy_ctastrategy/blob/7a8768de9784dda35a7b261a7ade1dbfbff50919/vnpy_ctastrategy/engine.py)。上游使用latest trade price，停止单触发后选择涨跌停价/五档价发送普通限价单；发送成功才移除停止单。本实现使用显式bid/ask、固定限价模板和一次激活失败终结，不能视为所有经济语义完全一致。

Rust本轮没有复制上游代码；测量脚本提取MIT源码中的精确方法，仅测无触发分支。输入整数100与阈值90..119可精确表示，Rust bid与上游last取相同参考值。速度范围、dense退步与完整公开路径数据见[逐功能数据](FEATURE_SCORECARD.md)，能力与后续顺序见[VNPY_PARITY](VNPY_PARITY.md)。
