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
- [x] Static Workflow Plan 1.3 的 `trace_values`/`trace_structure` 对实例、节点类别、Runtime edge、分支、取消、子工作流、有序 state-copy 表和多 root 执行精确闭包审计；闭合 release 会拒绝结构事件缺失、复制或重新编号掩盖。
- [x] Plan 1.3 签入 Action guard、Wait 的精确周期/timeout 与 backedge traversal limit；replay 跨 committed release 审核 Wait activation threshold、per-task-epoch 回边累计和无 guard Action 的唯一 transition。
- [x] replay 跨 committed release 保存配对 Join 的 branch arrival token；JoinAll 缺分支、JoinAny 未到达 winner、discard arrival 污染后续周期均会拒绝。
- [x] 结构化 Runtime 仅允许回边携带非零 traversal limit；前向边携带 limit、回边缺少 limit 和 `complete` edge 携带 limit 均在构造期拒绝。
- [x] 成功 commit receipt 绑定 EngineEpoch、TaskHandle、TaskEpoch、ReleaseSequence 与 CommitSequence；跨 task/epoch/release 回执全部拒绝。
- [x] ring-full 与 observer-loss 分别计数、饱和合并且不双计；EventSequence 继续单调消耗，后续周期不被 poison。
- [x] 每个 TaskEpoch 锁定 traced 或 untraced 扫描模式；双向切换在执行/提交前拒绝，reset 新 epoch 可重新选择。
- [x] `aurora-build workflow-trace-replay` 覆盖 Plan 1.1/1.2 `unverified`、1.3 `traceable`、结构篡改拒绝、错误 digest、截断 Trace 与不同 locale 路径。
- [x] root completion 与 retained/complete/discard 生命周期闭合；finish-time deadline discard 不发布已暂存的 watch。
- [x] Runtime structured node/edge 与签名 Canonical IR/Static Plan 的类别、参数、target、branch role 和 traversal bound 逐项一致。
- [x] Runtime owned plan 与 replay 从签名 Entry 目标 `initial_active` 建立 root/child 初始活动集合；
  Subworkflow call 只能使用按 call handle 绑定的精确 child 入口，任意后继节点不能伪装成首个执行节点。
- [x] replay 跨 committed release 携带 active set；删除 transition 并重编号后仍会因不可达的后继 `NodeExecuted` 被拒绝。
- [x] Fault 与声明边界取消闭合到同 release 的执行/完成生产者；合法但未执行的节点不能伪造结构事件。
- [x] 每个 task epoch 的 ReleaseSequence 严格递增；`0,2,1` 回退证据会被 replay 拒绝。
- [x] Entry 直接连接 End 的空 root 以初始化即完成的真实 producer Trace 回放，不要求伪造 `WorkflowCompleted`。
- [x] Fault discard 的 `NodeExecuted` 是 prior active set 到 fault node 的精确静态前缀，不能删除更早活动节点。
- [x] JoinAny 取消按签名 JoinStep/BranchOrder membership 精确移除 loser 与 child future state，不扩大到整个 root。
- [x] 同一 task 的 TaskEpoch 不得回退；`FinishAfterDeadline` discard 必须包含 prior active set 到首次超时 checkpoint 为止的精确执行前缀，active set 非空时前缀必须非空，空 root/已完成 workflow 的 finish-only 超时允许空前缀。
- [x] runtime bridge 精确核对 canonical task-local Fork/BranchHandle；交换 Fork 分支不能复用签名 plan identity。
- [x] runtime bridge 的 Subworkflow call/state-copy 表只从 Plan 1.3 签名结构派生，copy range 缺失、替换、越界或交叉调用点均拒绝。
- [x] committed、deadline-discard 与其他 discard 分别要求唯一 `OnTime`、唯一 `FinishAfterDeadline` 与零 deadline observation；缺失、重复或终态不匹配均拒绝。
- [x] 所有 discard 的 staging watch、`CancelApplied` 与 `WorkflowCompleted` 均不发布；replay 只在 commit 中要求 watch/cancellation application，且拒绝回滚后伪造的提交证据。
- [x] `WaitAtBoundary` boundary 自身必须有界进入 take/Fault；永久 Wait、guarded Action、Decision 等可 retain 节点不能作为终止证明；普通非 Fault、非 deadline discard 必须覆盖完整 prior active set。
- [x] 已审计 structured node/edge 由 owned plan 保留，签名 watch 私有并通过 plan-bound recorder 入口使用；审计后替换原始表不能改变执行或采样语义。
- [x] 节点内会锁定 transaction 的非法 outcome、跨节点 edge 等扫描错误产生当前节点唯一 `WorkflowFaulted`；节点外、deadline 与 Trace 生命周期失败不伪造节点 Fault。
- [ ] R2 单线程黄金 Gate、全仓库 verify、Ubuntu full gate、Windows smoke、Rust coverage、依赖/许可证与 secret scanning 通过。
- [ ] PR #4 当前已识别的审核问题均有修复位置和回归证据，所需复审通过后才恢复 R2 Gate 为关闭状态。

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

2026-09-19 整改复验：补充签名 Subworkflow state-copy、deadline 精确执行前缀和 terminal/outcome
一一闭合后，`aurora-control-engine`、`aurora-workflow-graph`、`aurora-workflow-cyclic`、
`aurora-build` 定向测试、严格 Clippy、单线程 R2 Gate 与仓库统一 `aurora-build verify` 全部通过；
临时 verifier 副本已删除。R2 Gate 仍保持整改验证中，等待当前 PR head 的远端 CI 与所需复审。

同日后续复验：永久 `WaitCondition` cancellation boundary 与普通 discard 缺失 prior active node 的
两条回归均在修复前指向证据缺口、修复后通过；四个目标 crate 完整测试、严格 Clippy、单线程
R2 Gate 和仓库统一 `aurora-build verify` 再次通过，一次性 verifier 副本已删除。远端 CI 与复审
仍以推送后的最新 PR head 为准。

同日 boundary-progress 复验：guarded Action 与全 false Decision 均会合法 `Retain`，不能因标记
`cancellationBoundary` 就被当作 pending loser 的有界停止点；编译器以 `WF2008` 拒绝两种图，同时
保留无 guard Action、Merge、WaitCycles 与有限 WaitCondition 的既有有界边界。该修订不改变 Runtime
状态机、Graph/Trace 布局或周期事务语义。

同日 empty-active deadline 复验：空 root 或已完成 workflow 的 prior active set 为空时，finish
checkpoint 超时产生的精确执行前缀也为空；replay 接受该真实 producer 形状，但 active set 非空时仍
拒绝空前缀。该修订不改变 deadline、terminal 或事务语义。

同日 loader 封闭性复验：owned plan 在原始 node/edge 输入被改写后仍保留审计值，traced bundle
使用私有 watch、计划资源上限和 task image 尺寸构造 recorder。四个目标 crate 完整测试、严格
Clippy、单线程 R2 Gate 与仓库统一 `aurora-build verify` 再次通过，.NET 9/9 通过，一次性 verifier
副本已删除；远端门禁和复审仍以推送后的最新 PR head 为准。

同日最新复审整改：KeepRunning 迟到败方现在发布已解决 Join 的边消费证据，回放跨 release 验证
先前获胜与 Fork 重新激活状态，并据此闭合最后败方触发的 root completion；runtime binding plan
同时拥有签名 task identity、active/execution/pending-cancellation 容量，并通过 plan-bound 入口
构造 Runtime。producer、replay、伪造拒绝和 Fork 容量回归、五个相关 crate 完整测试、严格 Clippy、
单线程 R2 Gate 与仓库统一 `aurora-build verify`（含 .NET 9/9）均已通过，一次性 verifier 副本已删除；
远端 CI 与复审仍以推送后的最新 PR head 为准，R2 Gate 保持整改验证中。

同日下一轮复审整改：普通显式 discard 与 Fault/deadline discard 统一视为 watchless，并要求
`CancelApplied` 为空；JoinSatisfied/CancelRequested 仍作为本次扫描事实保留。真实 recorder 显式 discard、
普通 watchless replay、CancelOthers 回滚接受及伪造 application 拒绝回归已补充；远端 CI 与复审仍以
下一次推送后的最新 PR head 为准，R2 Gate 保持整改验证中。

同日当前复审整改：replay 新增跨 committed release 的 live Subworkflow call 状态，只有已激活且 child
tree 已无 active node/live nested call 时才允许 completion 推进 parent transition；dormant completion
与仍在运行 child 的伪造路径均有拒绝回归。通用 Trace reader 同时补齐非空 child 跨周期完成所需的
completion-driven transition 闭包，并拒绝缺少同 release `SubworkflowCompleted` 的 orphan transition；
真实两周期 producer 现可严格 file roundtrip。五个相关 crate 完整测试、严格 Clippy、单线程 R2 Gate
与仓库统一 `aurora-build verify`（含 .NET 9/9）均通过，一次性 verifier 副本已删除；远端 CI 与复审
仍以本次推送后的最新 PR head 为准，R2 Gate 保持整改验证中。

同日 temporal-bound 复验：Plan 1.3 的 Action guard、Wait 精确 duration/timeout 与 edge backedge limit
均进入 JCS/plan digest；replay 的 committed state 会拒绝过早满足、过期仍等待、超限回边和无 guard
Action retain，discard/Fault 不推进计数。R2 黄金输入已移除无 guard Action 的伪 Retain，同时保留
三 release JoinAny 完成证据。五个相关 crate、50 个 replay 单测、严格 Clippy、单线程 R2 Gate 与
仓库统一 `aurora-build verify`（含 .NET 9/9）均通过，一次性 verifier 副本已删除。远端 CI 与所需
复审仍以本次推送后的最新 PR head 为准，R2 Gate 保持整改验证中。

同日 trace-mode 复验：新增 traced→untraced、untraced→traced 双向拒绝与 reset 后新 TaskEpoch
重新选择回归；模式不匹配在 recorder begin、节点执行和 commit 之前 poison 当前 transaction，不能
再借 ReleaseSequence 跳号隐藏未记录的已提交 Workflow release。定向 cyclic 全量测试与严格 Clippy
已通过；五个相关 crate、R2 单线程 Gate 与仓库统一 `aurora-build verify`（含 .NET 9/9）也已复验。
远端 CI 和复审仍以最新 PR head 为准，R2 Gate 保持整改验证中。

同日 join-arrival 复验：replay 新增 per-task-epoch committed branch token，保持扫描开始锁存语义；
提前 `JoinAll`、合法但未到达的 `JoinAny` winner、重复/错误 membership arrival 均拒绝，普通 discard
中的 arrival 不会进入后续 release。五个相关 crate、52 个 `aurora-build` 单测、严格 Clippy、单线程
R2 Gate 与仓库统一 `aurora-build verify`（含 .NET 9/9）均通过，临时 verifier 副本已删除；远端 CI 和
复审仍以最新 PR head 为准，R2 Gate 保持整改验证中。

## R2 明确不包含

不包含传统 LD 触点/线圈、Hosted Workflow、真实物理 I/O、完整 Studio UI、运行期插件发现、
五国语言资源、RTOS、裸机、`no_std`、Online Change 或功能安全能力。R2 Trace 是只读 Observe
证据，不授予 Force、暂停、reset、Fallback 应用或设备控制权限。
