# Rust workspace

本 workspace 承载 Aurora 的 Runtime、Target、Gateway、host-only 构建工具和 Rust SDK。

## 当前范围

当前 workspace 包含：

- `aurora-types`：无 I/O、网络、存储和平台依赖的基础领域类型边界。
- `aurora-control-contracts`：版本化控制契约及生成类型的承载边界。
- `aurora-control-engine`：Control Engine 可移植核心；当前包含 R0-02 固定容量工作集和
  R0-03 静态绝对调度决策。
- `aurora-test-support`：仅供测试使用的仿真时钟、虚拟 I/O、故障计划和确定性回放工具。
- `aurora-build`：host-only 的跨平台验证、摘要与供应链产物入口。

`aurora-control-engine` 的调度器只读取可注入单调时钟并返回绝对 `WaitUntil`、release
或停止决策；具体 Linux 单调时钟/绝对等待适配、任务体执行、事务提交、完整任务状态机、
快照、SPSC 和真实 I/O 仍按 R0 后续工作项分别交付。workspace 仍不包含 Aurora ST、
工作流、设备驱动、生产部署或 UI。

## R0-03 调用与修复迁移

R0-03 的 Rust API 尚未发布。本次修复保持 Preview 1.0 执行语义及已有序列化契约，
将原有 `ReleaseDecision::readiness()` 调用迁移为以下顺序：

1. `plan.observe(clock, control)` 返回 release 选择，记录 scheduled release/deadline 和跳过范围。
2. 调用方先有界批量处理 skip/miss 并检查任务准入（R0-06）；不准入时丢弃选择结果。
3. 紧邻任务调用执行 `selected.begin(clock, control)`，重新读钟判断 `StartAfterDeadline` 或停止。
4. `Execute(window)` 内在有界检查点及返回点调用 `window.checkpoint(clock)`，
   将最终结果交给 R0-04/R0-06 的 commit/discard 与 Fault 逻辑。

`ScheduleAction`、`ReleaseDecision`、`ReleaseReadiness`、`ExecutionWindow` 带计划借用生命周期，
不再支持 Clone/Copy；元数据访问改用借用，检查窗口使用 `&mut self`。窗口结束后共享的
单调时间高水位仍保留，回退/跨 epoch 错误锁存，不能通过重新观察伪造恢复。
Stop 在计划中锁存；没有无条件 Continue 恢复或 reset API。

任务时间/序列溢出首次显式返回错误并保存在固定槽，后续观察跳过该任务；
`task_schedule_error(index)` 可持续读取，R0-06 负责接到 Fault/Fallback 流程。
所有任务均失去调度资格时返回 `NoSchedulableTask`。内部时间网格 ordinal 与 task epoch
内 `ReleaseSequence` 分开计算；具体 reset 后的 phase/epoch 接续策略仍在 R0-04 接入时
按已接受契约确认，本次不提供重新初始化能力。

Linux 层实现 `MonotonicWait::wait_until_once`，使用相同单调时钟原点的绝对等待。
`plan.wait_once` 每次最多等待到一个显式非零的停止检查间隔边界，前后读取 `StopSignal`。
`Interrupted`（含 EINTR）交还外层检查，不内部重试；平台错误和提前误报到期显式返回。
原始 release 不随中断或分段等待变化。此边界允许 OS 等待，但不适用于任务执行中；
停止响应仍受普通 Linux 调度延迟影响，尚未实现或验证真实 Linux syscall 适配器。

## 后续 crate 名称

达到对应路线图阶段后，只能按架构基线使用以下名称：

- Runtime：`aurora-control-engine`、`aurora-io-guardian`、`aurora-st-ir`、`aurora-workflow-cyclic`、`aurora-workflow-hosted`、`aurora-runtime-supervisor`、`aurora-data-bridge`、`aurora-storage-service`
- 平台与管理：`aurora-platform-linux`、`aurora-target-agent`、`aurora-gateway`
- Host-only：`aurora-build`、`aurora-cli`

新增 crate 前必须先确认所属阶段、职责、允许依赖和验收测试。可执行程序放入 `apps/`，不得把进程入口与领域实现混在同一 crate。

## 验证

在本目录运行：

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --target x86_64-unknown-linux-gnu
```

也可从仓库根目录执行 `cargo run --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- verify` 运行完整跨语言门禁。
