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

value 采用 R1 canonical storage bytes。长度不超过 32 时一个 fragment；更长时按 32-byte
连续切分，index 从 0 开始且 count 非零。除最后项外 fragment bytes 必须为 32，最后项为
`1..=32`；同一值的 fragments 必须 EventSequence 连续、元数据一致且不得交错。每个 fragment
重复保存完整值 SHA-256。最大 fragment 数在构建期用 checked arithmetic 计算，必须可表示为
非零 `u16`（最多 65535 项）并计入 Trace 预算；否则报 `WF3007`，不得截断或回绕。

## 4. Event kind

| Value | Kind | 必需语义 |
| ---: | --- | --- |
| 1 | `WorkflowInitialized` | reset/reinitialize 后初始 active set 已建立 |
| 2 | `NodeExecuted` | active 节点按 ExecutionOrder 执行一次 |
| 3 | `TransitionTaken` | EdgeHandle 写入 next active set |
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

## 5. 规范事件顺序

同一 release 内按以下顺序发布：

1. 关联的 Force/Fallback/Deadline observation，按 EventKind 数值；
2. active 节点按 ExecutionOrder：`NodeExecuted`，随后该节点的 output/value handle 升序事件，
   transition 按 Decision priority 或 Fork BranchOrder；
3. Join/cancel/Subworkflow/completion 事件按触发节点 ExecutionOrder 和 EventKind；
4. watch values 按 ValueHandle、fragment index；
5. 恰好一个 `ScanCommitted` 或 `ScanDiscarded` 作为本 release 最后一项。

reset 产生 `WorkflowInitialized`，位于新 TaskEpoch 的第一个 release 其他事件之前。Fault 后不再
发布 NodeExecuted；`WorkflowFaulted` 后以 `ScanDiscarded` 结束。多个 Fault 候选只发布规格
选定的主 Fault。

## 6. Producer、drop 与离线工具

- producer 在初始化期预分配 `workflow_trace_ring_capacity` 个 192-byte slots 和单 release
  最坏事件 staging。周期内不分配、不锁等待、不调用文件或网络。
- 每个事件先消耗严格递增 EventSequence，再固定次数编码并只尝试一次非阻塞 DropNewest。
  full 时增加 dropped/full counter，不读取或覆盖 consumer slot。
- drop 可造成 EventSequence gap；file header 的 DroppedRecords 不得小于观察到的 gap。任何
  gap、fragment 缺失、顺序错误或非零 reserved 均使 replay/provenance 标为 incomplete。
- decoder 拒绝未知版本/flags/kind/detail、非规范 optional、截断、尾随字节、identity 不匹配、
  非连续完整值 fragment 和非法 commit transition，不补造缺失记录。
- R2-06 的 compare 工具必须先完整验证两个文件，再按 header 和 record bytes 找首个差异。

## 7. Locale 与安全边界

Trace 只承载稳定 enum/code、handle、UUID、序列、固定字节和有类型参数。它不承载翻译文本、
locale number/date 或权限决策。Trace 是 Observe 数据，不授予写值、Force、暂停、reset 或设备
控制能力；consumer 停止、Gateway 断开或本地化资源缺失不得影响周期控制。
