# Workflow Trace Binary Layout Preview 1.0

- 生命周期：Preview
- Layout：`1.0`
- 字节序：little-endian
- 文件 Header：96 bytes
- Record：192 bytes

本布局定义 R2 节点级事件流，独立于 R0 的 320-byte task Trace。两者通过 EngineEpoch、
TaskEpoch、TaskHandle、ReleaseSequence 和 CommitSequence 关联。不得修改 R0 record 或把
可变长节点数组嵌入 R0 Trace。

## 1. 文件 Header

| Offset | Size | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 8 | Magic | ASCII `AURWFT01` |
| 8 | 2 | Layout major | `1` |
| 10 | 2 | Layout minor | `0`；reader 精确接受 |
| 12 | 2 | Header size | `96` |
| 14 | 2 | Record size | `192` |
| 16 | 4 | Flags | 当前必须为 `0` |
| 20 | 4 | Reserved | `0` |
| 24 | 16 | EngineEpoch | RFC 9562 UUIDv7 网络字节序 |
| 40 | 32 | PlanDigest | 静态 Workflow plan 原始 SHA-256 |
| 72 | 8 | RecordCount | 文件内实际 record 数 |
| 80 | 8 | DroppedRecords | producer DropNewest 累计值 |
| 88 | 8 | Reserved | `0` |

总长度必须恰好为 `96 + RecordCount * 192`，checked 乘加不可表示时拒绝。Header EngineEpoch
必须与每条 record 相同；PlanDigest 必须解析到与所有 local handle 对应的静态计划。

R2-06 producer 使用 Static Workflow Plan 1.3。计划中的 `trace_values` 是全局稠密目录：每个
writable Action port 和每个计划 watch 恰有一个 descriptor，固定 task、展开实例、source、
TypeHandle、image area/offset、canonical byte width 与 fragment count。producer 和 replay 都必须
逐项核对该目录；不得把 port index、task-local watch 次序或运行期发现结果当作 ValueHandle。
计划中每条源自可执行节点的 edge 还必须携带 `source_step`；replay 将 task-local EdgeHandle
映射回该 step 的 ExecutionOrder，拒绝从同实例其他合法节点伪造的转移。

Plan 1.3 另包含只服务于 Trace 审计的 `trace_structure`：

- `initial_active` 按全局 StepHandle 排列，固定每个展开实例（root 与 Subworkflow child）由 Entry
  选择的首个可执行 step；Entry 直接连接 End 的空实例不产生条目；
- `nodes` 按全局 StepHandle 稠密排列，固定 step 的精确节点类别、JoinAny loser policy、合法
  BranchOrder、WaitCondition timeout 形状、Subworkflow task-local CallHandle/child instance 及有序
  input/output state-copy 表，cancellation boundary，以及以配对 JoinStep/BranchOrder 为作用域的
  完整分支成员；
- `edges` 按 task、task-local Runtime EdgeHandle 排列，固定 owning task、expanded edge、
  source step、目标 step/`complete` 和可选 BranchOrder；
- `root_instances` 精确列出所有顶层展开实例，一个 task 可以有多个 root。

编译器对四张表执行 no-missing、no-extra、稠密顺序、task/instance ownership 与引用闭包审计，
并把它们写入 JCS bytes 和 `plan_digest`。replay 必须据此证明 Fork edge/order、Join mode/winner、
Wait subtype/timeout、JoinAny cancellation policy、声明取消边界、Subworkflow parent/child/call、
completion-capable edge、root completion 与 Fault 的 instance/node/source/execution order 完全一致；
仅通过 optional 字段形状相似不得视为有效结构事件。每个闭合 release 还必须按已执行节点审核
结构事件基数：Fork 的全部 branch transition/activation 不得缺失或重复，JoinSatisfied 必须与唯一
transition 结对，Wait 必须恰有一次与结果相符的 observation，JoinAny loser cancellation、Subworkflow
activation/completion 和 CompletionRequested 也必须形成计划允许的闭包；修补 EventSequence 不能掩盖
结构事件被删除或复制。
replay 必须按 task epoch 跨 committed release 跟踪每个 live Subworkflow call：
`SubworkflowCompleted` 只能关闭此前已激活且尚未关闭/取消的同一 parent node、child instance 与
CallHandle，并且该 child tree 在本 release 后不得再有 active node 或 live nested call。完成时由
parent call 产生、但本 release 没有对应 `NodeExecuted` 的独立 `TransitionTaken`，必须在同一 release
由同一 execution order 的 `SubworkflowCompleted` 闭合；缺少该闭合时通用 layout reader 即拒绝。
`WorkflowFaulted` 必须由同一 release 的 `NodeExecuted` 产生；`CancelApplied(AtDeclaredBoundary)`
必须由同一 release 的 `NodeExecuted`，或该 Subworkflow call 的 `SubworkflowCompleted` 路径产生。
计划中存在但本 release 未执行的合法节点不能充当 Fault 或声明边界取消的生产者。
Runtime bridge 必须只从已签名的 Subworkflow node 派生 owned call/state-copy 表，并逐项校验 call range、
copy range 与 state image 边界；调用方不能用未签名的 copy 表改变输入/输出映射。
同一 owned plan 还必须保留已审计的 structured node/edge 表，原始 loader 输入在审计后被修改不得改变
Runtime 使用的节点类别、edge target 或 plan identity。签名 watch 表不得作为公开可变 `Vec` 暴露；
recorder 由 traced binding bundle 使用计划内事件上限、task image 尺寸和私有 watch 表直接构造。
节点执行期间任何会锁定周期事务的扫描失败（包括非法 outcome 与不属于该节点的 edge）必须以当前
节点和映射后的 `FaultReason` 产生唯一 `WorkflowFaulted`；节点外入口/收尾校验、deadline receipt 与
Trace recorder 生命周期失败不得借用先前节点伪造 Workflow Fault。
`WorkflowCompleted` 还必须与同一 root tree 的 release 生命周期闭合：保留中的节点不得同时报告完成，
无歧义的最后一个 complete transition 不得漏掉完成事件；discard release 不得发布 root completion。
Entry 直接连接 End 的空 root 在 Runtime 初始化时已经完成，不要求、也不得伪造一次
`WorkflowCompleted`；其首个 release 只需正常记录初始化和 terminal。
`WorkflowInitialized` 必须从签入 `plan_digest` 的 root `initial_active` 建立首个 required/allowed active set；
`SubworkflowActivated` 则必须从同一目录选取该 child instance 的精确 Entry 目标作为下一周期 required/allowed
节点。因此 root 首周期和 child 首周期都不能从同一实例的任意后继节点开始。每个 committed release 还会形成下一 release 的 active-set
证明；后继 `NodeExecuted` 必须来自上一提交的
transition target 或明确保留节点。删除 transition 后重编号 EventSequence 不能把不可达节点伪装成合法执行；
discard 保持上一已提交 active set；Subworkflow 首次激活使用计划签入的 child `initial_active`，
取消使用 scoped branch membership，二者均不得扩大成有界允许集。
`JoinAny(KeepRunning)` 已经获胜后，迟到败方到达配对 Join 的边仍必须发布
`TransitionTaken(ResolvedJoinConsumed)`；该事件证明静态边被消费，但不得再次把 Join 写入 next active set。
reader 只有在同一 task epoch 之前已经验证该 Join 获胜、source 属于相同 branch，且中间没有配对 Fork
重新激活时才接受该 detail。最后活动败方以此事件形成 root completion candidate；把普通转移伪装成
已解决消费、删除该标记或交换 branch 都会拒绝。
同一 TaskHandle 的 TaskEpoch 不得回退；同一 `(TaskHandle, TaskEpoch)` 的 ReleaseSequence 必须按
观察顺序严格递增。允许调度语义产生 release 跳号，但 epoch/release 重复或回退都会使完整 Trace
失去可验证的生命周期并被拒绝。
Fault discard 的 `NodeExecuted` 必须是 prior committed active set 按 ExecutionOrder 排列、直到 faulting
node（含）的精确前缀；不能删除更早的活动节点。取消提交则按签入 `plan_digest` 的 scoped branch
membership 从下一 active set 精确移除 loser branch 及其 child instance，不得把整个 root 降级成允许集。
其他非 deadline、非 Fault 的普通 discard 发生在完整扫描之后，因此 `NodeExecuted` 必须精确覆盖
prior committed active set；删除任一较早或较晚节点都不能继续报告 `traceable`。

`WaitAtBoundary` 的有界性证明不能只证明控制流到达 `cancellationBoundary`，还必须证明该节点能在
静态上界内进入 Runtime 的 boundary-take 或 Fault 路径。Preview 只接受无 guard Action、Merge、
WaitCycles 和有限 WaitCondition；guarded Action、Decision、Fork、JoinAll/JoinAny、Subworkflow 与永久
WaitCondition 都不能充当该证明。编译器必须以 `UnboundedCancellationPath` 拒绝，否则 guard 永远为
false 等合法输入会让 pending loser 永久 retain。

Runtime binding plan 必须同时独占签名 resource proof 中的 TaskHandle、`active_nodes`、
`node_executions_per_release`、`pending_cancellations` 与 task image 尺寸，并通过 plan-bound runtime
构造入口使用它们。loader 不得在审核 node/edge 后另行向 `StructuredWorkflowDefinition` 注入更小容量；
没有 pending cancellation 的计划允许证明值为零，但非空 executable plan 的 active/execution 容量不得为零。

| Static Workflow Plan | Reader | 结构证明 | 完整 Trace 的 `traceability` |
| --- | --- | --- | --- |
| 1.1 | 支持 | 无 `trace_values`/`trace_structure` | `unverified` |
| 1.2 | 支持 | 有 value 目录、无结构目录 | `unverified` |
| 1.3 | 支持 | value 与结构目录均逐项验证 | `traceable` |
| 未知版本 | 拒绝 | 不猜测 | 不适用 |

Plan 1.3 也只有在 EventSequence 连续、header drop 为零、每个 release 闭合且全部结构检查通过时
才能报告 `traceable`；任何 gap/drop 仍报告 `incomplete`。旧计划不原地迁移；需要可信结构回放时
必须重新编译工程并采集与新 `plan_digest` 匹配的 Trace。

## 2. Record flags 与 sentinel

| Bit | Meaning |
| ---: | --- |
| 0 | NodeHandle 存在 |
| 1 | EdgeHandle 存在 |
| 2 | SourceHandle（Action/POU/Fault site）存在 |
| 3 | ValueHandle 与 TypeHandle 存在 |
| 4 | BranchOrder 存在 |
| 5 | ExecutionOrder 存在 |
| 6 | R0 FaultReason 存在 |
| 7 | ValueDigest 存在 |
| 8 | value fragment 存在 |

bits 9..15 必须为零。optional flag 未置位时相应 handle/order/type 使用 `u32::MAX`，计数和
bytes 全零；flag 置位时不得使用 sentinel。ValueDigest 缺失时 32 bytes 全零；reserved 全零。

## 3. 固定 Record

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | Magic `AURWFR01` |
| 8 | 2 | Layout major `1` |
| 10 | 2 | Layout minor `0` |
| 12 | 2 | Record length `192` |
| 14 | 2 | Presence flags |
| 16 | 2 | WorkflowTraceEventKind |
| 18 | 2 | EventDetail |
| 20 | 4 | TaskHandle |
| 24 | 4 | WorkflowInstanceHandle |
| 28 | 4 | optional NodeHandle |
| 32 | 4 | optional EdgeHandle |
| 36 | 4 | optional SourceHandle |
| 40 | 4 | optional ValueHandle |
| 44 | 4 | optional BranchOrder |
| 48 | 4 | optional ExecutionOrder |
| 52 | 4 | optional TypeHandle |
| 56 | 2 | fragment index |
| 58 | 2 | fragment count |
| 60 | 2 | fragment bytes (`0..=32`) |
| 62 | 2 | optional R0 FaultReason |
| 64 | 16 | EngineEpoch |
| 80 | 8 | TaskEpoch |
| 88 | 8 | EventSequence |
| 96 | 8 | ReleaseSequence |
| 104 | 8 | CommitSequence before |
| 112 | 8 | CommitSequence after |
| 120 | 32 | optional complete-value SHA-256 |
| 152 | 32 | optional canonical storage fragment |
| 184 | 8 | Reserved `0` |

TaskHandle、WorkflowInstanceHandle、TaskEpoch、EventSequence 和 ReleaseSequence 始终存在。
CommitSequence after 只能等于 before 或加一。`ScanCommitted` 必须为 before+1；
`ScanDiscarded` 和所有其他事件必须为 after=before。读取方以 release 的最后一项确定本周期
是否提交，不得从较早事件推测最终结果。

成功提交只能由 `CycleTransaction::finish_observed` 返回的 receipt 证明。receipt 私有保存完整
`CycleIdentity`（EngineEpoch、TaskHandle、TaskEpoch、ReleaseSequence）、CommitVersion 和最终
checkpoint；Trace recorder 必须同时比较完整 identity、commit sequence 与 deadline 状态，不能
只凭 TaskEpoch/ReleaseSequence/CommitSequence 接受另一 task 的成功回执。调用方只能通过
`identity()`、`version()`、`checkpoint()` 和 `release_sequence()` 读取，不存在公开构造器。

value 采用 R1 canonical storage bytes。长度不超过 32 时一个 fragment；更长时按 32-byte
连续切分，index 从 0 开始且 count 非零。除最后项外 fragment bytes 必须为 32，最后项为
`1..=32`；同一值的 fragments 必须 EventSequence 连续、元数据一致且不得交错。每个 fragment
重复保存完整值 SHA-256。最大 fragment 数在构建期用 checked arithmetic 计算，必须可表示为
非零 `u16`（最多 65535 项）并计入 Trace 预算；否则报 `WF3007`，不得截断或回绕。
reader 必须按顺序拼接每个 fragment 的有效 bytes 并重算完整值 SHA-256；只比较各 fragment
重复携带的 digest 字段不足以证明内容完整性。

## 4. Event kind

| Value | Kind | 必需语义 |
| ---: | --- | --- |
| 1 | `WorkflowInitialized` | reset/reinitialize 后初始 active set 已建立 |
| 2 | `NodeExecuted` | active 节点按 ExecutionOrder 执行一次 |
| 3 | `TransitionTaken` | 静态 EdgeHandle 被采用；普通转移写入 next active set，已解决 KeepRunning Join 的迟到败方只消费边 |
| 4 | `ForkActivated` | 按 BranchOrder 记录分支 token |
| 5 | `JoinSatisfied` | Merge 到达、JoinAll 完整或 JoinAny 获胜 |
| 6 | `WaitObserved` | remaining/condition/timeout 结果 |
| 7 | `CancelRequested` | loser 进入提交点取消或 pending-cancel |
| 8 | `CancelApplied` | loser future active state 被清除 |
| 9 | `SubworkflowActivated` | 展开实例输入已复制 |
| 10 | `SubworkflowCompleted` | 展开实例输出已写入 staging |
| 11 | `OutputStaged` | 输出变化及 Node/Source provenance |
| 12 | `WatchedValue` | 编译期 watch value fragment |
| 13 | `CompletionRequested` | End 到达但可仍有保留分支 |
| 14 | `WorkflowCompleted` | 顶层提交 Completed |
| 15 | `WorkflowFaulted` | 主 Fault site 和 R0 reason |
| 16 | `ForceObserved` | 本 release 关联的显式 Force 状态 |
| 17 | `FallbackObserved` | 本 release 关联的 Fallback 状态 |
| 18 | `DeadlineObserved` | 本 release 关联的 deadline outcome |
| 19 | `ScanCommitted` | 本 release Workflow transaction 提交 |
| 20 | `ScanDiscarded` | 本 release Workflow transaction 丢弃 |

### 4.1 EventDetail

EventDetail 是按 event kind 解释的冻结 `u16` 枚举：

| Event kind | Detail values |
| --- | --- |
| `TransitionTaken` | `0=TargetActivated`、`1=ResolvedJoinConsumed` |
| `JoinSatisfied` | `1=JoinAll`、`2=JoinAny`、`3=Merge` |
| `WaitObserved` | `1=WaitingCycles`、`2=CyclesSatisfied`、`3=ConditionFalse`、`4=ConditionSatisfied`、`5=TimedOut`、`6=PermanentWaiting` |
| `CancelRequested` | `1=CancelOthers`、`2=WaitAtBoundary` |
| `CancelApplied` | `1=AtCommitBoundary`、`2=AtDeclaredBoundary` |
| `OutputStaged` | `1=ValueChanged`、`2=ValueUnchanged` |
| `ForceObserved` | `1=Applied`、`2=Active`、`3=Released` |
| `FallbackObserved` | `1=Requested`、`2=Active`、`3=Cleared` |
| `DeadlineObserved` | `1=OnTime`、`2=SkippedRelease`、`3=StartAfterDeadline`、`4=FinishAfterDeadline`，与 R0 `MissOutcome` 数值一致 |

其他 Preview 1.0 event kind 的 EventDetail 必须为 0。未知 detail 拒绝；未来增加 kind/detail 需要
升级 minor，旧 reader 不猜测。R2 尚无真实 Force/Fallback producer 时只冻结事件值，不得以
模拟实现冒充 R3/R4 能力。

`OutputStaged(ValueChanged)` 必须携带完整值 digest 和 fragment；
`OutputStaged(ValueUnchanged)` 必须不携带 fragment。reader 对多生成或漏生成的两种形状都拒绝。

## 5. 规范事件顺序

同一 release 内按以下顺序发布：

1. 关联的 Force/Fallback/Deadline observation，按 EventKind 数值；
2. active 节点按 ExecutionOrder：`NodeExecuted`，随后该节点的 output/value handle 升序事件，
   transition 按 Decision priority 或 Fork BranchOrder；
3. Join/cancel/Subworkflow/completion 事件按触发节点 ExecutionOrder 和 EventKind；
4. watch values 按 ValueHandle、fragment index；
5. 恰好一个 `ScanCommitted` 或 `ScanDiscarded` 作为本 release 最后一项。

Subworkflow child 在较后的 ExecutionOrder 完成时，parent call 的完成转移按 parent ExecutionOrder
出现在第 2 层，允许没有同 release 的 parent `NodeExecuted`；reader 会暂存该 completion-driven
transition，并要求第 3 层存在同 execution order 的 `SubworkflowCompleted`。这只是既有
`TransitionTaken(TargetActivated)` 的严格闭合规则，不增加 EventKind、EventDetail 或 record 字段。

每个闭合 release 的 deadline 证据必须与 terminal 一一对应：`ScanCommitted` 恰有一个 `OnTime`；
由 deadline 丢弃的 `ScanDiscarded` 恰有一个 `FinishAfterDeadline`；Fault 或其他非 deadline discard
不得携带 deadline observation。缺失、重复、detail 改写或 terminal/outcome 不匹配都会拒绝。

所有 `ScanDiscarded` 路径都在提交点丢弃 commit-dependent 的 `CancelApplied`、
`WorkflowCompleted` 与此前暂存的 `WatchedValue`，并且不得把回滚后的取消或 staging watch
伪装成已提交证据；`JoinSatisfied`、`CancelRequested` 和 `CompletionRequested` 仍可保留为本次扫描
曾经到达的结构事实。完整 release 审计因此只对 `ScanCommitted` 要求完整 watch catalog，并只在提交时
按 JoinAny policy 要求 cancellation application；discard 必须对两者都要求为空。
如果超时只在所有 active node 执行完成后的 finish checkpoint 观察到，`NodeExecuted` 等于完整 prior
committed active set；如果节点内 checkpoint 已跨过 deadline，执行会在当前节点后停止，因此证据必须是
prior active set 按 ExecutionOrder 的精确前缀。prior active set 非空时该前缀必须非空；空 root 或已完成
workflow 没有 active node，finish checkpoint 超时时合法前缀为空。两种路径都不能删除已执行的更早 active node，
也不能执行越过首次超时 checkpoint 的更晚节点。deadline observation 在规范排序中位于节点事件之前，
但它仍由最终不可伪造的 transaction receipt 决定，不表示扫描在节点执行前已知最终 outcome。

reset 产生 `WorkflowInitialized`，位于新 TaskEpoch 的第一个 release 其他事件之前。Fault 后不再
发布 NodeExecuted；`WorkflowFaulted` 后以 `ScanDiscarded` 结束。多个 Fault 候选只发布规格
选定的主 Fault。

## 6. Producer、drop 与离线工具

- producer 在初始化期预分配 `workflow_trace_ring_capacity` 个 192-byte slots 和单 release
  最坏事件 staging。周期内不分配、不锁等待、不调用文件或网络。
- 每个事件先消耗严格递增 EventSequence，再固定次数编码并只尝试一次非阻塞 DropNewest。
  full 时增加 dropped/full counter，不读取或覆盖 consumer slot。
- observer 已退出时，producer 同样只消耗 EventSequence，并在 producer-local counter 中逐次
  计入 dropped-newest；统计值与 ring-full drop 饱和相加且不得双计。不得使 recorder 失效，
  更不得把 Observe consumer 的生命周期反向传播成周期事务 Fault。
- drop 可造成 EventSequence gap；file header 的 DroppedRecords 不得小于观察到的 gap。任何
  gap、fragment 缺失、顺序错误或非零 reserved 均使 replay/provenance 标为 incomplete。
- decoder 拒绝未知版本/flags/kind/detail、非规范 optional、截断、尾随字节、identity 不匹配、
  非连续完整值 fragment 和非法 commit transition，不补造缺失记录。
- R2-06 的 compare 工具必须先完整验证两个文件，再按 header 和 record bytes 找首个差异。

## 7. Locale 与安全边界

Trace 只承载稳定 enum/code、handle、UUID、序列、固定字节和有类型参数。它不承载翻译文本、
locale number/date 或权限决策。Trace 是 Observe 数据，不授予写值、Force、暂停、reset 或设备
控制能力；consumer 停止、Gateway 断开或本地化资源缺失不得影响周期控制。
