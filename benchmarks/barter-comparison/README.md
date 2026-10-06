# 固定版本 Barter 组件对照

```sh
python3 scripts/market_data.py --rows 1000000
python3 scripts/compare_barter.py --archive reports/datasets/BTCUSDT-bookTicker-2024-01-01.zip --output reports/comparison.json
```

从仓库根目录执行。脚本获取/验证干净的上游提交 `9770b27a83f844472b93b593b08063affc974b0d`；可用 `--source PATH` 指向该提交的干净本地克隆，避免网络重复下载。依赖源码放在忽略的 `benchmarks/barter-source`，Cargo.lock固定其余依赖；上游源码摘要、比较二进制、Kaze源文件摘要、输入摘要进入报告。

固定官方BTCUSDT 2024-01-01归档校验和，转换最前100000条为JSON，预加载后计时。每次先逐条核对4个输出数值，一次预热，7次交替顺序测量；3次独立调用的所有样本公开。时钟/订阅元数据等输出差异不隐藏，计时没有网络、策略、撮合、风险或持久化。这不是完整引擎benchmark，也不证明系统可承受这些速率的实盘行情。

两个故障属性调用真实上游 `Orders` 组件，分别输入较新时间但较少累计成交、终态后重新打开。Kaze使用保留意图/终态的严格网关，上游组件允许不同快照更新/外部订单采用政策。实验只说明这两项默认政策差异，不将其描述为整个Barter系统存在漏洞。
