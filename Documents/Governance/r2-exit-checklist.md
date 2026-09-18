# Phase R2 状态与退出检查

- 状态：PR #4 整改验证中；完成 CI 与所需复审前不得关闭 R2 Gate
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
- [x] Static Workflow Plan 1.3 的 `trace_values`/`trace_structure` 对实例、节点类别、Runtime edge、分支、取消、子工作流和多 root 执行精确闭包审计；闭合 release 会拒绝结构事件缺失、复制或重新编号掩盖。
- [x] 结构化 Runtime 仅允许回边携带非零 traversal limit；前向边携带 limit、回边缺少 limit 和 `complete` edge 携带 limit 均在构造期拒绝。
- [x] 成功 commit receipt 绑定 EngineEpoch、TaskHandle、TaskEpoch、ReleaseSequence 与 CommitSequence；跨 task/epoch/release 回执全部拒绝。
- [x] ring-full 与 observer-loss 分别计数、饱和合并且不双计；EventSequence 继续单调消耗，后续周期不被 poison。
- [x] `aurora-build workflow-trace-replay` 覆盖 Plan 1.1/1.2 `unverified`、1.3 `traceable`、结构篡改拒绝、错误 digest、截断 Trace 与不同 locale 路径。
- [x] root completion 与 retained/complete/discard 生命周期闭合；finish-time deadline discard 不发布已暂存的 watch。
- [ ] R2 单线程黄金 Gate、全仓库 verify、Ubuntu full gate、Windows smoke、Rust coverage、依赖/许可证与 secret scanning 通过。
- [ ] PR #4 的七个最新审核问题均有修复位置和回归证据，所需复审通过后才恢复 R2 Gate 为关闭状态。

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

2026-09-18 本地证据：定向 Rust tests、严格 Clippy、R2 Gate 单线程黄金验证和仓库统一
`aurora-build verify` 均通过。Windows 下直接用 `cargo run ... -- verify` 会因父进程锁定自身 exe
而阻止 workspace 测试替换文件；使用同一已构建 verifier 的临时副本运行后完整通过，临时文件
已删除。远端 Ubuntu/Windows/coverage/供应链门禁及所需复审仍以 PR #4 结果为准。

## R2 明确不包含

不包含传统 LD 触点/线圈、Hosted Workflow、真实物理 I/O、完整 Studio UI、运行期插件发现、
五国语言资源、RTOS、裸机、`no_std`、Online Change 或功能安全能力。R2 Trace 是只读 Observe
证据，不授予 Force、暂停、reset、Fallback 应用或设备控制权限。
