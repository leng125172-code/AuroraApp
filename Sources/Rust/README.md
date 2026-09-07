# Rust workspace

本 workspace 承载 Aurora 的 Runtime、Target、Gateway、host-only 构建工具和 Rust SDK。

## 当前范围

当前 workspace 包含：

- `aurora-types`：无 I/O、网络、存储和平台依赖的基础领域类型边界。
- `aurora-control-contracts`：版本化控制契约及生成类型的承载边界。
- `aurora-control-engine`：Control Engine 可移植核心；当前包含 R0-02 固定容量工作集和
  R0-03 静态绝对调度决策和 R0-04 周期 state/output 事务。
- `aurora-test-support`：仅供测试使用的仿真时钟、虚拟 I/O、故障计划和确定性回放工具。
- `aurora-build`：host-only 的跨平台验证、摘要与供应链产物入口。

`aurora-control-engine` 的调度器只读取可注入单调时钟并返回绝对 `WaitUntil`、release
或停止决策；具体 Linux 单调时钟/绝对等待适配、产品任务体、完整任务状态机、
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
   R0-04 调用方改由 `TaskTransaction::begin(selected, clock, control)` 完成 staging
   复制和开始检查，通过返回的 `CycleTransaction` 执行、检查并 finish/discard。

`ScheduleAction`、`ReleaseDecision`、`ReleaseReadiness`、`ExecutionWindow` 带计划借用生命周期，
不再支持 Clone/Copy；元数据访问改用借用，检查窗口使用 `&mut self`。窗口结束后共享的
单调时间高水位仍保留，回退/跨 epoch 错误锁存，不能通过重新观察伪造恢复。
Stop 在计划中锁存；没有无条件 Continue 恢复或全局 Stop reset API。

任务时间/序列溢出首次显式返回错误并保存在固定槽，后续观察跳过该任务；
`task_schedule_error(index)` 可持续读取，R0-06 负责接到 Fault/Fallback 流程。
所有任务均失去调度资格时返回 `NoSchedulableTask`。内部时间网格 ordinal 与 task epoch
内 `ReleaseSequence` 分开计算；R0-04 的 reset 按 ADR-0005 保持原网格，
不清除 engine 时钟故障或全局 Stop。

Linux 层实现 `MonotonicWait::wait_until_once`，使用相同单调时钟原点的绝对等待。
`plan.wait_once` 每次最多等待到一个显式非零的停止检查间隔边界，前后读取 `StopSignal`。
`Interrupted`（含 EINTR）交还外层检查，不内部重试；平台错误和提前误报到期显式返回。
原始 release 不随中断或分段等待变化。此边界允许 OS 等待，但不适用于任务执行中；
停止响应仍受普通 Linux 调度延迟影响，尚未实现或验证真实 Linux syscall 适配器。

## R0-04 事务与恢复边界

`TaskTransaction` 在启动前接收已验证 TaskSpec、EngineEpoch、state/output 声明初值和
总容量/内存预算。一份固定字节工作集保存不可变初值和两个组合 bank，不引入泛型
Clone、堆对象、语言状态模型或共享内存布局。单个区域可为空，总容量必须为正。
首次初始化为 TaskEpoch 1、CommitSequence 0；初值不直接作为 control output 发布。

调用顺序是：调度选择 → skip/miss 准入 → begin → 有界 execute/read/write/checkpoint
→ finish 或 discard。begin 整体复制 state/output 后重新读钟；任务只有 staging
字节访问权。任何读写越界或显式执行 Fault 都锁存，忽略返回值不能恢复执行/提交。
finish 强制最终时间检查；预算超限但仍在 HardLimit/deadline 内可提交，并返回预算
观测。deadline miss 丢弃但不直接锁 Fault，R0-06 负责累计阈值。HardLimit、时钟错误
或 commit counter 溢出立即锁定。未结束句柄 Drop 也锁定；forget 后再次 begin 拒绝。
deadline miss 后的检查点和 finish 仍读取单调时钟，后续 HardLimit/时钟错误不能被
早期 miss 掩盖。任务步骤或时钟回调 unwind 时先锁定事务再传播异常；宿主捕获也不能
继续执行/提交。这里不吞掉 panic，也不承诺从进程 abort 恢复。

state/output 和版本只有一个 Release 提交点；只读视图以 Acquire 锁存，并受 Rust
借用约束，不允许 writer 与普通 bank reader 并发修改。`diagnostic()` 保留上一完整
bank；`publishable()` 在初值、Fault、discard 或活动事务时返回 None。staging 没有
可发布视图。复制出去的历史诊断数据不构成继续驱动输出的许可。跨任务并发 publication
slot 属于 R0-05，不要从这里持有 bank 引用跨周期读取。

reset 必须精确匹配 EngineEpoch、TaskHandle、TaskEpoch、Fault generation，且通过
外部 `ResetGuard` 的授权与 Fallback 前置条件检查。仅在测试中提供受控 guard 替身，
没有无条件通过的生产实现。reset 从不可变初值建立 staging，经只读初始化验证后读钟，
从严格晚于完成时刻的首个原网格 release 恢复，新 epoch 内 ReleaseSequence 从 0
开始；锁定期间不补跑、不计入新 epoch 的 miss。成功一次性提交新 epoch/commit 0，
首次正常成功周期才恢复发布资格。初始化失败保持旧 committed，锁存新故障代际，
旧 reset 请求失效；授权/身份拒绝或时间溢出不能发布新 epoch。所有控制计数禁止回绕。
初始化验证回调 unwind 与显式失败一样锁存新初始化故障并使旧请求失效，异常继续
向宿主传播；公开 API、提交版本和 ADR-0005 的 reset 网格规则不变。

R0-04 不包含完整 Running/Degraded/miss 历史、Fallback mailbox/ack、Guardian、
语言执行器或物理输出。调度器目前仍可能返回故障任务的 release，事务层拒绝执行；
R0-06 接入完整任务准入和 Fallback 请求，健康任务可以继续。调用方仍需在初始化及
任务步骤中遵守固定容量与有界检查点要求；这里不承诺抢占任意 Rust 回调。

契约补全见 [ADR-0005](../../Documents/ADR/0005-r0-reset-release-grid.md)。既有
二进制契约和依赖未改；新增 Rust API 尚未发布，无持久化迁移或部署步骤。

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
