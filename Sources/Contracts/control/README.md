# Control layouts

保存共享内存和周期控制二进制布局的规范源文件。布局必须固定宽度、显式字节序/位序/对齐、版本化且可做跨语言黄金样本测试；禁止使用语言 ABI 或未定义 padding 作为契约。

## Preview v1

- [R0 Execution Semantics v1](v1/r0-execution-semantics.md)：任务调度、miss/Fault、事务提交、跨任务快照、Fallback 和 Trace 的规范语义。
- [Control Layout Header v1](v1/control-layout.md)：共享内存布局的固定 Header。
