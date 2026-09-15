# SPEC-R2-001：Cyclic Workflow Preview 1.0

- 生命周期：Preview
- Schema version：`1.0`
- 作者格式：YAML 1.2
- 运行目标：普通 Linux x64、Rust `std`
- 依赖：R0 task/transaction/Trace 与 R1 Aurora ST Canonical IR

## 1. 范围与硬边界

Cyclic Workflow 是 Aurora 对 LD 的可视化替代控制语言，但不使用触点、线圈或梯级语法。
它是静态拓扑、固定容量、单线程顺序执行的 PLC 扫描状态机。Preview 1.0 只允许调用周期安全
的 Aurora ST POU、I/O 映像动作和 Cyclic 子工作流；具体 Action binding 在 R2-05 冻结。

周期路径禁止数据库、Redis、DNS、HTTP、网络、阻塞文件 I/O、阻塞日志、人工等待、运行期
插件发现、动态节点、动态容量和无界重试。Workflow、Trace 和 Fallback 不是功能安全系统，
不得绕过独立急停、人员防护或危险运动联锁。

Graph、诊断、Trace、排序、容量计算、序列化和摘要必须是 locale-neutral；系统语言、区域
数字/日期格式和翻译文本不得改变任何校验或控制结果。

## 2. YAML 输入、版本与规范表示

规范源文件后缀为 `.aurora-workflow.yaml`，顶层必须声明：

机器可读字段约束见
[Cyclic Workflow Graph Preview 1.0 JSON Schema](../../schema/aurora/cyclic-workflow/v1/cyclic-workflow.schema.json)。
JSON Schema 用于契约形状和黄金样本门禁；规范作者输入仍是 YAML，项目闭包和 Graph 语义必须由
有界验证器检查。

```yaml
kind: aurora.cyclic-workflow
schemaVersion:
  major: 1
  minor: 0
  lifecycle: preview
documentId: 018f0000-0000-7000-8000-000000000001
workflowId: 018f0000-0000-7000-8000-000000000002
canonicalName: conveyor_start
```

- 输入必须是无 BOM UTF-8，并按 YAML 1.2 Core Schema 解析。只接受 YAML 标准 tag；未知或
  自定义 tag 报 `WF0005`。
- 支持 YAML 1.2 锚点和别名，但解析必须在调用方给出的 source bytes、nesting depth、alias
  count、alias expansion nodes 和 decoded scalar bytes 上限内完成。循环别名报 `WF0006`。
- mapping key 必须是非空字符串；重复 key 报 `WF0004`，不得采用 first/last-wins。
- YAML 1.1 隐式日期/布尔规则和非标准 `<<` merge key 不属于本契约。`NaN`、`Infinity`、
  平台 native integer、locale number/date 和实现专用 tag 不得进入强类型模型。
- reader 只接受精确 Preview 1.0。未知字段、节点、枚举和更高 minor 均拒绝，不静默忽略。
- YAML 注释、缩进、引号风格、mapping key 顺序和锚点名称不进入语义模型。解析成功后按本
  规格建立强类型 Workflow IR，以 RFC 8785 JCS 序列化并计算
  `sha256:<lowercase-hex>` semantic digest。
- `sourceDigest` 是原始 UTF-8 source bytes 的 SHA-256，用于审计；`semanticDigest` 是强类型
  IR 的 JCS SHA-256；`planDigest` 在 R2-02 对静态执行计划计算。三者不得混用。
- 任何 Error 都使整份文档无效，不发布部分 IR、handle、计划或 Trace layout。

## 3. 身份、名称与文件关系

- document、Workflow、Node、Edge、variable、watch 和 layout 使用 RFC 9562 canonical
  lowercase UUIDv7。生成 UUID 所需时间和随机源由外层显式提供。
- 成功编译按 UUIDv7 的 16-byte 网络字节序分配连续 payload-local `u32` handle；
  `u32::MAX` 是所有 optional handle 的唯一 absent sentinel，永不分配。
- `canonicalName` 使用 ASCII lowercase `^[a-z_][a-z0-9_]{0,255}$`，在所属作用域唯一。
  人类文案、翻译和字体不进入本文件；展示层以后通过稳定 resource key 关联。
- 每个 Workflow 对象一个语义文件。Subworkflow 通过稳定 WorkflowId 引用其他文件；编译前
  完整引用图必须闭合。
- 画布数据放在独立 `.aurora-workflow-layout.yaml`，只通过稳定 ID 关联；布局缺失、损坏或
  改变不得改变 Graph 校验、静态计划、Runtime payload 或 semantic/plan digest。

## 4. Graph 结构

Preview 1.0 的节点目录固定如下，编号发布后不得复用：

| Value | Kind | 扫描行为 |
| ---: | --- | --- |
| 1 | `Entry` | 结构标记；初始化直接激活唯一后继，不进入 active set |
| 2 | `Action` | active 时每扫描执行一次周期安全 binding |
| 3 | `Decision` | 按显式 priority 选择首个 true 分支 |
| 4 | `Fork` | 在提交点激活全部有序分支 |
| 5 | `Join` | 显式 Merge、JoinAll 或 JoinAny 汇合 |
| 6 | `Wait` | 按计划 ReleaseSequence 等待条件或周期数 |
| 7 | `Subworkflow` | 执行一个编译期展开、状态隔离的调用实例 |
| 8 | `End` | 结构标记；满足完成条件时在提交点进入 Completed |

结构约束：

- 每个 Workflow 恰有一个 Entry，至少一个可达 End；Entry 无入边且恰有一条出边，End 无出边。
- 除 Entry 和 Join 外的节点恰有一个控制入边。所有汇合，即使来自 Decision 互斥分支，也必须显式使用
  Join；不存在隐式 OR/AND merge。
- Action、Wait、Subworkflow 和 Join 恰有一条正常出边。Decision 至少两条分支边；Fork 至少
  两条分支边。并行分支不得借普通节点交叉、合并或逃离所属 Fork/Join 区域。
- 每个可执行节点具有 `executionOrder`，在 Workflow 内是从 0 开始、无空洞、唯一的 `u32`
  序列。Entry/End 不分配 executionOrder。
- EdgeId 唯一；完全相同的 source/target/control kind 仍视为重复边并拒绝。
- 不可达节点、悬空边、没有可达 End 的非永久 Workflow 和删除回环边后仍存在的环均拒绝。

## 5. 扫描与事务语义

每个 R0 task release 按以下固定顺序执行：

1. 锁存输入映像、跨任务 snapshot、ReleaseSequence、当前 active set 和全部 Workflow committed
   state。跳过的 release 不执行 Workflow，但仍推进等待和 deadline 使用的计划序号。
2. 按 `executionOrder` 执行本周期开始时 active 的节点；每节点最多一次。Graph 在运行期不可
   发现、排序、增删或扩容。
3. 节点写入 task staging bank。顺序更后的 active 节点可读取本周期顺序更早的 staging 值；
   顺序更早的节点不能读取未来值。
4. guard/transition 只写 next active set。普通前向边、Fork 分支、Join 后继和 backedge 都不在
   本周期再次执行目标节点。
5. 成功周期统一提交 Workflow state、active set 和输出，并使 R0 CommitSequence 加一。
6. 任一节点 Fault、容量错误或 deadline Fault 均停止剩余节点，丢弃整个 task 的 staging
   state/output/active set，进入 R0 FaultLocked 并请求 Fallback。

同一目标不得由多个展开后节点写入。该单一静态写者规则适用于变量、FB/Program state、命令
slot 和输出映像；即使验证器能推断两条路径互斥也不放宽，不采用 last-writer-wins。

## 6. 节点语义

### 6.1 Entry、End 与 Action

- 初始化/reset 建立新的 TaskEpoch 和全部声明初值，并把 Entry 的唯一后继放入第一个 active
  set。Entry 自身不执行、不占执行预算。
- Action active 时调用一次 binding。Action 不返回隐藏的 Running 状态；唯一出边 guard 为
  true 时提交离开，false 时节点保留在 next active set。无 guard 表示恒 true，因此 Action
  恰执行一个扫描周期。
- R2-00 不定义 Action payload。R2-05 必须使用版本化、强类型、固定容量 binding，且不得通过
  opaque map、字符串命令或运行期插件绕过周期边界。
- 到达 End 产生 completion request。只有不存在 active、pending-cancel、KeepRunning 分支或
  未完成 Subworkflow 时，成功周期才进入 Completed。Completed 不自动重启且不执行节点，
  仅显式 reset/reinitialize 建立新运行。

### 6.2 Decision

- 每条 Decision 分支具有从 0 开始、无空洞且唯一的 `priority` 和一个 BOOL guard。
- 按 priority 升序求值，首个 true 分支获选；后续 guard 不求值。全部 false 时 Decision 保留
  active，下一扫描从 priority 0 重新求值。
- guard 的读取和 Fault 服从第 5 节执行顺序；不得依赖 mapping 顺序或本地化比较。

### 6.3 Fork 与 Join

- Fork 分支具有从 0 开始、无空洞且唯一的 `branchOrder`。Fork 完成时在 next active set 激活
  每个分支入口，不创建线程；分支仍按全局 executionOrder 执行。
- Join 必须显式声明 `Merge`、`JoinAll` 或 `JoinAny` mode。`Merge` 专用于 Decision 等非并行互斥
  分支的汇合：不得声明 ForkId 或 loser policy，且静态计划必须证明每次到达最多只有一个
  control token。不能证明互斥时报 `WF2005`，不得以文件顺序、NodeId 或 last-writer-wins
  选择到达。
- `JoinAll` 和 `JoinAny` 严格结构化并显式通过 ForkId 与唯一 Fork 配对。允许严格嵌套；
  禁止交叉区域、跨层 Join、重复 branch token 和从区域外伪造到达。
- JoinAll 收齐所属 Fork 的全部 branch token 后激活唯一后继；否则保持等待。
- JoinAny 收到首个或多个 token 后完成。同一扫描多个分支到达时，最小 branchOrder 获胜并
  写入 Trace。JoinAny 必须声明下列一种 loser policy：
  - `CancelOthers`：在成功周期提交点清除败方未来 active state；不追溯撤销此前已提交结果或
    本扫描已经合法执行的 staging 写入。
  - `KeepRunning`：获胜路径继续，败方保持执行；所有保留分支静止前顶层不能 Completed。
  - `WaitAtBoundary`：败方进入 pending-cancel，继续运行到显式 `cancellationBoundary` 节点
    成功完成；在该提交点停止且不激活其后继。
- 每条可能进入 WaitAtBoundary 的路径必须在 backedge/Wait 上限内证明能到达边界；否则报
  `WF2008`。取消不会吞掉分支 Fault，任何分支 Fault 都 Fault 整个 task。

### 6.4 Wait

Wait 必须选择一个互斥 mode：

- `cycles`：声明 `waitCycles >= 1`。节点在 release K 的提交点激活后，从 K+1 开始计数；
  `currentReleaseSequence - K >= waitCycles` 时成功。
- `condition`：每个 active scan 使用锁存输入和可见 staging 值求值 BOOL condition，并声明
  `timeoutCycles >= 1` 或 `permanent: true`，两者不得同时出现或同时缺失。
- condition 成功与 timeout 在同一 release 成立时，condition 优先；仅 condition 为 false 时
  timeout 才触发 `WFF0001`。永久等待仍受 task stop/reset/Fallback，不表示阻塞线程。
- wait arithmetic 使用 checked `u64`；不可表示或超过 Target Profile 时构建期拒绝。

### 6.5 Backedge

- 每条形成环的边必须显式标记 `backedge: true` 并声明 `maxTraversalsPerRun >= 1`。
- 计数作用域是每个展开后的 Workflow 实例、每次顶层运行；每条 backedge 独立计数，reset
  清零。离开循环区域或再次激活节点不重置。
- 第 `maxTraversalsPerRun + 1` 次尝试触发 `WFF0002`，不静默停止、截断或选择其他边。
- backedge 目标只进入 next active set。删除全部 backedge 后，剩余图必须是与
  executionOrder 一致的有向无环图。

### 6.6 Subworkflow

- 每个调用点在编译期展开为独立实例和固定 state；同一模板的不同调用点不得共享 active
  set、Wait、backedge、Fork/Join、POU 或 output state。
- 调用激活时复制一次全部输入。实例运行期间不重新绑定父级变量；成功完成时按声明顺序把
  全部输出一次性写入父级 staging bank，然后激活调用节点后继。
- Trace 同时保存展开实例 handle、模板 WorkflowId/NodeId 映射和稳定 instance path。
- 直接或间接递归拒绝。展开深度、实例数和展开后节点/边总量必须在 Target Profile 内。
- 子工作流 Fault 直接 Fault 顶层 task；Preview 1.0 没有 catch、compensation 或部分 reset。

## 7. Reset、Fault 与可观察性

- reset 只能作用于整个 R0 task；不能单独重置节点、分支或子工作流。它丢弃 Fault 周期的
  半更新私有状态，重建声明初值、Entry 后继 active set、Wait/backedge/Join/cancel 状态，
  并沿用 R0 绝对 release grid。
- 同一扫描多个可观察错误只选择 executionOrder 最早节点的最早 site 作为主 Fault；一旦
  Fault 确认，后续节点不执行。诊断和 Trace 不得因 HashMap 顺序或线程调度改变。
- Workflow Trace 使用 [独立固定布局](trace-layout.md) 并引用 R0 identity。consumer 停止时
  producer 只执行一次非阻塞 DropNewest；缺口必须由 sequence/counter 暴露，不能阻塞控制。
- 节点、transition、Fork/Join/cancel、completion、Fault 和 output provenance 始终是 Trace
  事件。I/O、变量和 FB state 值仅按编译期固定、排序且有界的 watch list 采样。
- 只有 `dropped_records = 0` 且 EventSequence 连续的 Trace 才能宣称完整解释每次输出变化；
  有缺口时工具必须报告 incomplete，不得补造事件或值。

## 8. Target Profile 资源字段

构建调用必须显式提供以下全部非零上限，不存在平台默认值：

- `max_workflows_per_task`、`max_source_nodes_per_workflow`、`max_source_edges_per_workflow`；
- `max_expanded_workflow_instances`、`max_expanded_nodes_per_task`、`max_expanded_edges_per_task`；
- `max_active_nodes_per_task`、`max_node_executions_per_release`；
- `max_fork_nesting_depth`、`max_branches_per_fork`、`max_pending_cancellations`；
- `max_subworkflow_expansion_depth`、`max_backedge_traversals_per_run`、`max_wait_cycles`；
- `max_workflow_state_bytes_per_task`、`max_workflow_staging_bytes_per_task`；
- `max_watch_handles_per_task`、`max_trace_events_per_release`、`workflow_trace_ring_capacity`；
- `max_yaml_source_bytes`、`max_yaml_nesting_depth`、`max_yaml_aliases`、
  `max_yaml_alias_expansion_nodes`、`max_yaml_decoded_scalar_bytes`。

所有计数、展开、size、offset、Trace fragment 和最坏路径运算使用 checked arithmetic。任何值
不可表示、为零、缺失或超过 Target Profile 都报 `WF3005`；构建不得按设备内存猜测默认值。
这些是准入容量，不是平台级性能保证。周期、抖动、deadline miss、队列水位和负载仍须在指定
硬件和工程上测量。

## 9. 稳定诊断目录

### 9.1 文档与 YAML

| Code | Name | 条件 |
| --- | --- | --- |
| `WF0001` | InvalidEncoding | 不是无 BOM UTF-8 |
| `WF0002` | UnsupportedSchemaVersion | version 缺失或不是精确 Preview 1.0 |
| `WF0003` | InvalidYaml | YAML 1.2 语法或 Core Schema 值无效 |
| `WF0004` | DuplicateMappingKey | mapping key 重复 |
| `WF0005` | UnsupportedYamlTag | 非标准或未知 tag/merge 扩展 |
| `WF0006` | AliasCycle | alias 直接或间接成环 |
| `WF0007` | SourceLimitExceeded | YAML bytes/depth/alias/scalar 预算失败 |
| `WF0008` | UnknownField | 当前版本未定义的字段 |
| `WF0009` | InvalidField | 字段类型、范围、组合或 required 规则无效 |
| `WF0010` | InvalidStableIdentity | ID 不是 canonical UUIDv7 |
| `WF0011` | DuplicateStableIdentity | 所属闭包内稳定 ID 重复 |
| `WF0012` | InvalidCanonicalName | machine name 非规范或重复 |

### 9.2 Graph、并行与生命周期

| Code | Name | 条件 |
| --- | --- | --- |
| `WF1001` | MissingEntry | 没有 Entry |
| `WF1002` | MultipleEntries | Entry 多于一个 |
| `WF1003` | InvalidEntryEdge | Entry 入边或出度无效 |
| `WF1004` | InvalidEndEdge | End 出边或入度无效 |
| `WF1005` | DanglingEdge | source/target 不存在 |
| `WF1006` | InvalidControlDegree | 节点控制入度/出度不符合目录 |
| `WF1007` | ImplicitMerge | 非 Join 节点存在多个控制入边 |
| `WF1008` | InvalidExecutionOrder | executionOrder 重复、空洞或用于结构节点 |
| `WF1009` | InvalidForwardDependency | 前向数据依赖逆于 executionOrder |
| `WF1010` | UnmarkedCycleEdge | 环边未显式声明 backedge |
| `WF1011` | UnboundedBackedge | backedge 缺少合法每次运行上限 |
| `WF1012` | UnreachableNode | 节点从 Entry 不可达 |
| `WF1013` | MissingCompletionPath | 非永久 Workflow 没有可达 End |
| `WF1014` | DuplicateEdge | 规范 source/target/control edge 重复 |
| `WF2001` | InvalidConditionType | guard/condition 不是 BOOL |
| `WF2002` | InvalidDecisionPriority | priority 重复、空洞或不可表示 |
| `WF2003` | InvalidBranchOrder | branchOrder 重复、空洞或不可表示 |
| `WF2004` | InvalidForkJoinPair | JoinAll/JoinAny 未绑定唯一 Fork，或 Merge 错误绑定 Fork |
| `WF2005` | CrossRegionJoin | 并行区域交叉、逃逸或跨层汇合 |
| `WF2006` | InvalidJoinMode | Join kind 或 JoinAny loser policy 无效 |
| `WF2007` | InvalidCancellationBoundary | boundary 位于不允许的节点/区域 |
| `WF2008` | UnboundedCancellationPath | WaitAtBoundary 路径不能证明有界到达 |
| `WF2009` | InvalidWaitPolicy | Wait mode/timeout/permanent 组合无效 |
| `WF2010` | InvalidWaitRange | wait/timeout 周期为零、溢出或超预算 |
| `WF2011` | RecursiveSubworkflow | Subworkflow 引用图直接或间接递归 |
| `WF2012` | MissingSubworkflow | 引用目标不存在或版本不兼容 |
| `WF2013` | InvalidSubworkflowBinding | 输入/输出数量、类型、方向或 ownership 无效 |
| `WF2014` | InvalidActionContract | Action envelope/version/binding 无效 |
| `WF2015` | UnsupportedActionKind | 当前构建未注册所需周期安全 Action kind |
| `WF2016` | InvalidCompletionPath | End 与活动/保留分支的完成规则不闭合 |

### 9.3 Ownership、容量与布局

| Code | Name | 条件 |
| --- | --- | --- |
| `WF3001` | WriteConflict | 展开后存在多个静态写者 |
| `WF3002` | ReadBeforeWrite | 本周期数据依赖要求读取未来 staging 值 |
| `WF3003` | TypeMismatch | port、guard、variable 或 watch 类型不兼容 |
| `WF3004` | DynamicCyclicStorage | 请求动态或运行期发现的容量/实例 |
| `WF3005` | ResourceBudgetExceeded | 资源缺失、不可表示或超过 Target Profile |
| `WF3006` | CanonicalizationFailure | 强类型 IR 不能产生规范 JCS 表示 |
| `WF3007` | TraceBudgetExceeded | 最坏 Trace event/fragment 超预算 |
| `WF3008` | InvalidWatchHandle | watch 目标不存在、不可观察或类型无效 |
| `WF3009` | DuplicateWatchHandle | watch list 重复引用同一 handle |
| `WF3010` | InvalidLayoutReference | layout 引用了其他或不存在的语义实体 |
| `WF3011` | LayoutLimitExceeded | layout 文档超过 host-only 显式预算 |

### 9.4 运行 Fault site

| Site code | Name | 条件 | R0 FaultReason |
| --- | --- | --- | --- |
| `WFF0001` | WaitTimeout | condition Wait 到达 timeout 且条件为 false | `TaskExecutionFault` |
| `WFF0002` | BackedgeLimitExceeded | 尝试超过每实例每次运行上限 | `CapacityExceeded` |
| `WFF0003` | ActionExecutionFault | Action/POU 返回已冻结 Fault | 由 binding 映射 |
| `WFF0004` | CancellationBoundaryExceeded | pending-cancel 未在静态上限内到达边界 | `TaskExecutionFault` |
| `WFF0005` | SubworkflowFault | 展开子实例产生运行 Fault | 保留原始 reason |

编号一旦发布不得复用。新增 code 需要升级 Workflow minor；改变已有条件、cardinality 或 Fault
映射需要升级 major。

## 10. 诊断排序与 cardinality

1. 诊断按规范 project-relative path UTF-8 bytes、YAML byte range start/end、code、相关实体
   UUID bytes 排序，不使用系统 locale。
2. span 是半开 UTF-8 byte range `[start,end)`；1-based Unicode scalar line/column 仅供显示。
3. 无法恢复的 YAML region 每个根因一个 primary；其无效 subtree 不进入 ID、拓扑或容量 pass。
4. 重复 key 在第二及后续 occurrence 各一项；重复 ID/name/order 在第二及后续规范输入项各一项。
5. 一条 edge、一个 node property、一个 binding 或一个预算项最多报告一个最具体 primary；
   不为同一根因级联产生悬空、类型和容量诊断。
6. layout 诊断不改变语义 Graph 的成功结果；语义 Graph 任一 Error 则不发布任何运行产物。

## 11. 规范场景

### 11.1 单周期与跨周期

- Entry 后继 Action 在第一次 task release 执行；无条件出边使其后继在第二次 release 执行。
- Action guard 为 false 时，同一 Action 在下一 release 再执行；本 release 不执行其后继。
- release K 激活 `waitCycles: 1` 的 Wait，K+1 成功并只在 K+2 执行其后继。
- backedge 在 release K 被选择，目标最早在 K+1 执行，即使其 executionOrder 更大。

### 11.2 同刻并行

- 两个 JoinAny branch token 在同一 release 到达时，最小 branchOrder 获胜，与 edge 文件顺序
  和 NodeId 无关。
- Decision 互斥分支通过显式 Merge 汇合；若静态计划不能证明单 token 到达，则拒绝 Graph。
- CancelOthers 败方如果已在该 release 按更早 executionOrder 执行，其合法 staging 写入仍随
  成功周期提交；取消只清除下一 active set。
- KeepRunning 败方未静止时，即使获胜路径已到 End，Workflow 仍是 Running。

### 11.3 拒绝边界

- 非 Join 节点两个入边只报 `WF1007`，不猜测 OR/AND。
- 同一输出的两个静态写者只报 `WF3001`，不因互斥 Decision 或取消策略放宽。
- condition 与 timeout 同一 release 成立时不产生 `WFF0001`；condition 成功。
- alias expansion、Subworkflow 展开、backedge 计数或 Trace fragments 的 checked arithmetic
  溢出均拒绝，不能通过截断形成部分计划。

## 12. 兼容与后续工作项

- R2-01 实现 YAML Graph/Layout Schema 与验证器；R2-02 实现 Canonical Workflow IR、静态计划
  和资源证明；R2-03 实现扫描与事务提交；R2-04 实现并行、取消、等待和子工作流。
- R2-05 冻结并实现 ST POU、I/O image action 和有类型命令 binding；R2-06 实现 Trace、离线
  仿真与回放；R2-07 以黄金 Graph/Input Trace 关闭阶段 Gate。
- Preview 1.0 不包含传统 LD、Hosted Workflow、真实物理 I/O、完整 Studio、Online Change、
  跨版本运行状态恢复、五国语言资源、字体、语言切换、RTOS、裸机或 `no_std`。
