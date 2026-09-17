# Phase R2 状态与退出检查

- 状态：Implemented，等待远端 CI 复验
- 基线日期：2026-09-17
- 产品版本：0.1.0

## 交付映射

| 路线图交付物 | 实现与证据 |
| --- | --- |
| Graph、Schema 与静态计划 | `aurora-workflow-graph` 的版本化 YAML、Graph/Layout 校验、Canonical IR、静态顺序与精确资源证明 |
| PLC 扫描与原子提交 | `aurora-workflow-cyclic` 的 next-active 扫描、每节点每周期一次和 R0 `CycleTransaction` 统一提交 |
| Fork/Join、取消与 Fault | 单线程逻辑并行、Join All/Any、三种 loser policy、Wait、回边和子工作流分项测试 |
| Action 与 Trace | 强类型 ST POU/I/O image/typed command binding、固定 192-byte Workflow Trace 和 output provenance |
| R2 Gate | `aurora-build/tests/r2_gate.rs` 复用已登记 `join-any` 黄金 Graph，按固定 Input Trace 比对逐周期证据并调用真实 CLI replay |

## 退出门槛

- [x] 图拓扑、写冲突、循环上限和最坏周期资源均有静态正反门禁。
- [x] 黄金 Input Trace 的活动节点、转移、取消、输出和 commit/discard 与固定证据逐周期一致；重复运行的 Trace bytes 相同。
- [x] Fork 分支在调用线程按静态顺序执行，不创建运行线程；较后分支 Fault 不提交较早分支的 staging 输出。
- [x] 每个输入恰好对应一个 release，每个 release 恰好一个 terminal；计划实例、step、edge、资源和 Trace value 基数固定。
- [x] `aurora-build workflow-trace-replay` 的正确 plan、错误 digest、截断 Trace 与不同 locale 环境路径通过。
- [x] R2 Graph、Runtime、binding、Trace、CLI replay、格式和静态检查门禁通过。

## 复现入口

从仓库根目录执行 R2 Gate：

```text
cargo test --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build --test r2_gate -- --test-threads=1
```

R2 分项与全仓库门禁：

```text
cargo fmt --manifest-path Sources/Rust/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-workflow-graph -p aurora-workflow-cyclic -p aurora-build --all-targets -- -D warnings
cargo test --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-workflow-graph -p aurora-workflow-cyclic -p aurora-build
cargo run --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- verify
```

Linux x64 是 Runtime 主门禁；Windows 仅验证相同的可移植核心。R2 不据此声明硬实时或目标硬件性能。

## R2 明确不包含

不包含传统 LD 触点/线圈、Hosted Workflow、真实物理 I/O、完整 Studio UI、运行期插件发现、
五国语言资源、RTOS、裸机、`no_std`、Online Change 或功能安全能力。R2 Trace 是只读 Observe
证据，不授予 Force、暂停、reset、Fallback 应用或设备控制权限。
