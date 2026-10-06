# 贡献与修改

修改执行规则前阅读 docs/CONTRACT.md，更新契约并提供手算/边界测试。不要只用两个共用核心实现的对照来证明成交规则。

修改持久处理、策略或风控时，增加中途恢复与连续运行对照；改变语义时升级 EXECUTION_REVISION。保持核心整数构造边界、有界容量、无自定义 unsafe，不修改旧日志兼容条件来让测试通过。

执行 cargo fmt --check、cargo clippy --locked --all-targets -- -D warnings、cargo test --locked --all-targets、cargo test --locked --release。性能结论附 release 原始数据、环境、负载和重复次数，明确内存执行与磁盘持久化的差别。
