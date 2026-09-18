# ADR-0008：R2 Cyclic Workflow 扫描与 Trace 语义

- 状态：Accepted
- 日期：2026-09-15
- 决策人：Caymir
- 关联需求/问题：GitHub Project `R2-00`、SPEC-R2-001、R-006、R-007、R-035～R-042

## 背景

R0 已冻结 task release、整周期事务、FaultLocked、固定容量 SPSC 和 Trace identity；R1 已冻结
Aurora ST Canonical IR、POU 状态和 Linux x64 AOT。R2 在实现 Graph Schema、静态计划和运行
状态机前，必须固定节点、同刻顺序、回环、逻辑并行、取消、等待、子工作流、容量和节点级
Trace，否则 validator、Runtime 和回放工具可能对同一图产生不同 active set 或输出。

## 备选方案

### 作者格式与画布

- JSON 单文件：规范化直接，但大图人工编辑和注释体验有限。
- YAML 与 presentation 同文件：作者体验集中，但移动节点会污染控制载荷和摘要。
- 完整 YAML 1.2 语义文件加独立 Layout：保留可读性和锚点复用，同时以强类型 IR 隔离格式与
  控制语义；需要显式 alias 预算和 canonicalization。

### 执行与并行

- 节点内部返回 Running、任意图汇合和运行线程并行：表达力高，但生命周期、取消、写冲突和
  最坏执行量难以证明。
- PLC 周期步骤、显式 Decision/Fork/Join、单线程静态顺序：图稍显式，但每扫描行为、写入
  ownership 和资源上界可审查。

### Trace

- 扩展 R0 固定 record：会破坏已冻结布局，且一周期节点事件数不是常数。
- 独立固定宽度 Workflow event stream：保留 R0 契约，通过 identity/sequence 关联并独立暴露
  drop 与完整性。

## 决策

- 接受 [SPEC-R2-001](../../Sources/Contracts/workflow/v1/language.md)、独立
  [Layout](../../Sources/Contracts/workflow/v1/layout.md) 和
  [Workflow Trace](../../Sources/Contracts/workflow/v1/trace-layout.md) 作为 Preview 1.0 规范源。
- 作者格式使用 YAML 1.2 Core Schema 和标准 tag；支持受显式预算限制的 anchor/alias，拒绝
  重复 key、循环 alias、未知 tag、非标准 merge 和未知契约字段。解析后只以强类型 JCS IR
  决定语义与摘要。
- 节点目录固定为 Entry、Action、Decision、Fork、Join、Wait、Subworkflow 和 End。Entry/End
  是结构标记；Action 是 active 时每扫描执行一次的 PLC 周期步骤。
- 执行使用显式连续 executionOrder、next active set 和成功周期末统一提交。每个写目标只有
  一个展开后静态写者；Fault 丢弃整周期并沿用 R0 FaultLocked/Fallback。
- Decision 等非并行互斥分支使用不绑定 Fork 的显式 Merge，且必须静态证明单 token
  到达。Fork 只与 JoinAll/JoinAny 严格结构化配对且逻辑并行，不创建线程。JoinAny
  同刻按 BranchOrder 决胜，并显式使用 CancelOthers、KeepRunning 或 WaitAtBoundary；取消在提交点
  生效，不追溯回滚已执行工作。
- Wait 使用计划 ReleaseSequence，condition 与 timeout 同刻时 condition 优先。backedge 按
  展开实例、每次运行独立有界计数；Subworkflow 按调用点编译期展开、输入复制、状态隔离。
- 所有实际容量由 Target Profile 提供，缺失、不可表示、无法证明或超限均在部署前拒绝。
- Workflow Trace 是独立的固定 192-byte event stream，关联 R0 identity，使用预分配、非阻塞
  DropNewest 和显式 gap。值只按编译期固定 watch list 采样。
- Graph、诊断、排序、Trace 和 Hash 只使用 locale-neutral code、ID 和类型化参数。R2 不加载
  五国语言资源；具体 locale、fallback、字体和布局验证由 H0/I0 冻结。

## 影响

- R2-01～R2-07 必须以这三份规范为输入，不得由实现反向改变节点或扫描语义。
- YAML parser、Graph/Layout Schema、Canonical Workflow IR、静态计划、Runtime 和 Trace codec
  仍按 Project 工作项顺序实现；R2-00 不创建占位 crate 或运行伪实现。
- 完整 YAML 支持增加 host 解析复杂度，因此 R2-01 必须先验证 source/alias/scalar 预算，且需
  单独审批新的 parser 依赖；运行 Payload 不包含 YAML parser。
- Preview 破坏性修改仍需迁移和显式版本拒绝。布局变化不会触发 Runtime 语义变化，但语义
  YAML、Target Profile 或工具链变化会改变相应构建摘要。
- 本 ADR 不改变 R0 Trace 布局、Guardian I/O ownership、Hosted Workflow、A/B 更新、功能安全
  或首版普通 Linux x64/Rust `std` 边界。

## 2026-09-18 Preview 契约完整性修订

PR #4 复审确认：Static Workflow Plan 1.2 的 value/edge 目录不足以证明结构事件的精确语义，
成功 commit receipt 也必须覆盖完整 release identity，observer 退出后的发布损失必须进入 drop
证据。因此 traced writer 升级为 Plan 1.3，并将每个 root/child 展开实例的 Entry 目标
`initial_active`、节点类别、
Fork/Join/Wait/cancel、Runtime edge、Subworkflow 调用和 root instance 的 `trace_structure` 纳入 JCS 与 `plan_digest`；成功 receipt
私有保存完整 CycleIdentity，Trace publisher 饱和合并 ring-full 与 observer-loss 计数。

Reader 继续读取 1.1、1.2 和 1.3，但只有 1.3 在连续 EventSequence、零 drop、闭合 release 和
全部结构检查通过时可报告 `traceable`；1.1/1.2 只能报告 `unverified`。旧产物不原地迁移，可信
结构回放需要重新编译并重新采集。此修订只收紧 Preview 证据完整性，不修改 Graph Schema、
96/192-byte Trace Layout、PLC 扫描、Fork/Join、Fault、deadline 或周期事务语义，也不改变
R-001～R-067 的已接受架构边界。

同次复审还明确：结构事件必须闭合到本 release 的执行/完成生产者，同一 task epoch 的
ReleaseSequence 只能严格前进；Entry 直接连接 End 的空 root 沿用 Runtime 初始化即完成的既有语义，
不通过补造 `WorkflowCompleted` 改变执行历史。这些均属于 Reader 证据收紧，不改变 Trace 布局或
Runtime 状态机。

后续复审进一步要求 discarded Fault 保留到 fault node 为止的静态执行前缀，并禁止取消审计把整个
root 扩大成 allowed set。Plan 1.3 因此在节点目录内增加以 JoinStep/BranchOrder 作用域表达的完整
branch membership，replay 只删除已应用 loser cancellation 对应的未来活动节点及其 child instance。
该字段进入 JCS/plan digest，仍不修改 Graph Schema 或 96/192-byte Trace Layout。
