# v15原始证据

`components.json`是最终同输入组件七轮对照；上游精确方法仅无触发分支、浮点mock，Rust是整数价格。`components-before-adaptive.json`保留首轮索引退步；`components-integer-mock.json`保留中间整数mock，不作为最终上游结论。整段时间不是逐事件延迟。

`bracket.json`含公开百万内存七轮/十万持久三轮、所有原回执摘要/状态、各自恢复与RSS；首次新策略基线，大部分报价发生在单次交易结束后，不能与持续ATR策略的吞吐直接排名。`legacy-pairs.json`固定同ATR配置七对，回执/状态一致但新版本慢3.56%。首轮优化前数据也保留。

`crash.json`是五个已确认阶段SIGKILL恢复，逐payload/receipt/chain一致；非中途事务时序注入。`capacity.json`与压缩输入/回执覆盖4096同时触发、820017字节回执、4098全审计和重复去重。SQL数据库留在本机ignored reports，需要原冻结二进制恢复；代码版本不同不可绕过manifest。

上游完整MIT源码/许可与精确摘取方法用于可复验测量，未复制进Rust实现。验证日志包含失败尝试、最终通过计数、硬件、编译器、源码/二进制/输入身份。manifest覆盖本目录全部其他文件；不含凭据或真实资金订单。
