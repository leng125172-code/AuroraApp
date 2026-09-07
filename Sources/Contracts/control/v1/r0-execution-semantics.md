# R0 Execution Semantics v1

- 生命周期：Preview
- 版本：1.0
- 适用范围：普通 Linux x64、Rust `std`、Phase R0 Control Engine
- 关联决策：R-002、R-003、R-017、R-023、R-032～R-034、R-061、R-066、ADR-0004

本文是 R0 调度、任务状态、提交、快照、Fallback 和 Trace 语义的规范源。后续 Rust 类型、二进制布局和测试必须实现本文，不得从实现反向推导或改变语义。

## 1. 规范用语与边界

“必须”“不得”和“仅允许”为强制要求。除非字段明确说明，所有时间间隔和单调时间均为无符号整数纳秒；UTC 只用于观测，不参与 release、deadline、年龄或 Fault 判断。

R0 只定义单进程 Control Engine 的可移植核心和 Linux 时钟适配边界，不定义 Aurora ST、Cyclic Workflow、真实 I/O、共享内存 ABI、数据库、网络服务、RTOS、`no_std` 或硬实时保证。

容量和时间阈值必须来自已验证工程配置/Target Profile，并在启动前固定。平台不提供默认最小周期、默认容量或合格线。

## 2. 标识、计数与范围

| 概念 | 表示 | 有效范围与规则 |
| --- | --- | --- |
| `TaskHandle` | `u32` | `0..=u32::MAX-1`；`u32::MAX` 无效；同一静态计划内唯一 |
| `EngineEpoch` | `BootEpochId` | 每次 Control Engine 进程启动唯一；不得跨启动比较单调时间 |
| `TaskEpoch` | `u64` | `1..=u64::MAX`；每次成功重新初始化严格递增 |
| `ReleaseSequence` | `u64` | 每个 task epoch 从 `0` 开始；每个 scheduled release 消耗一个值，包括跳过项 |
| `CommitSequence` | `u64` | 声明初值为 `0`；每个成功周期递增一次，Fault/skip 不递增 |
| `EventSequence` | `u64` | 每个 engine epoch 从 `0` 开始；每个尝试发布的 Trace 事件递增，包括因满而丢弃的事件 |
| `Priority` | `i16` | 数值越大优先级越高；只用于同一周期执行线程的静态排序 |
| `PeriodNanos` | `u64` | `1..=u64::MAX` |
| `PhaseNanos` | `u64` | `0..period-1` |
| `RelativeDeadlineNanos` | `u64` | `1..=period` |
| `ExecutionBudgetNanos` | `u64` | `1..=hard_limit`；用于准入和预算观测 |
| `HardLimitNanos` | `u64` | `execution_budget..=relative_deadline` |
| `MissWindow` | `u32` | `1..=target_max_miss_window`；启动时预分配 |
| `MaxMisses` | `u32` | `0..=MissWindow`；窗口内 miss 数大于该值时 Fault |
| `ConsecutiveMisses` | `u32` | `1..=MissWindow`；连续 miss 数达到该值时 Fault |
| Ring/Trace 容量 | `u32` | `1..=target_declared_max`；零容量拒绝启动 |

任何转换超出上述范围、静态表出现重复 owner/handle，或容量乘法超过目标内存预算时，必须在启动前返回可穷举 validation error。不得截断、取模或使用默认值继续。

运行期计数器不得回绕。控制语义计数器将在下次递增会溢出时锁定对应任务并请求 Fallback；纯观测计数器饱和后停止产生该流的新记录，设置 sticky `CounterSaturated` 健康诊断，但不得因此伪造连续 Trace 或阻塞控制。

## 3. 静态任务与绝对调度

### 3.1 任务表

R0 使用一个周期执行线程和启动前冻结的任务表。任务实现、工作集、state/output bank、miss 窗口和通信容量均在启动前完成分配与预触页。周期路径不得发现任务、改变容量、增长集合、创建线程或加载插件。

任务表按以下稳定键排序：

1. `scheduled_release_nanos` 升序；
2. `Priority` 降序；
3. `TaskHandle` 升序。

同一输入、同一静态计划和同一手动时钟序列必须得到相同调用顺序。R0 不使用 OS 抢占优先级表达任务语义，也不并行执行两个周期任务。

### 3.2 Release 公式

`k` 是 `EngineEpoch` 内原始绝对时间网格的 ordinal，不是 task epoch 内的
`ReleaseSequence`。首次启动二者从 `0` 开始；reset 后的接续规则见第 4.3 节：

```text
scheduled_release = engine_start_elapsed + phase + k * period
absolute_deadline = scheduled_release + relative_deadline
```

`engine_start_elapsed` 是 `EngineEpoch` 作用域内的单调 `u64` 纳秒值。计算必须使用 checked arithmetic；中间值可以使用更宽整数，但写回单调 `u64` 前必须验证。无法表示下一个 release/deadline 时，任务进入 `FaultLocked(ScheduleTimeOverflow)`。

调度等待使用最近 `scheduled_release` 的绝对单调时间。不得使用 UTC，不得以“上次完成时间 + period”安排下一次执行。

### 3.3 迟到、跳过与不追赶

调度器每次观察 `now` 时，对每个任务只允许调用至多一次：

- 计算 `scheduled_release <= now` 的最大 `k`。
- 尚未处理且小于 `k` 的 release 全部标记 `SkippedRelease`，每项都计为 deadline miss 并消耗 `ReleaseSequence`。
- 批量记入跳过项后立即计算 miss 阈值；若任务已经进入 `FaultLocked`，不得再执行第 `k` 项。
- 若 `now > absolute_deadline(k)`，第 `k` 项也记为 `StartAfterDeadline` miss，不执行用户任务。
- 只有 `now <= absolute_deadline(k)` 时才允许开始第 `k` 项。

多个跳过项必须用至多 `MissWindow` 次更新或等价的有界批量算法处理；不得按任意大积压量循环补跑或逐项记日志。累计跳过数使用饱和观测计数，并在 Trace 中记录首尾 release sequence 和数量。

### 3.4 Deadline、预算与 HardLimit

- 周期执行耗时为 `finish_monotonic - start_monotonic`；未来时间或跨 `EngineEpoch` 时间是 `ClockContractViolation` Fault。
- `finish_monotonic > absolute_deadline` 为 deadline miss；等于 deadline 仍在边界内。
- 耗时 `> ExecutionBudgetNanos` 且未触发 Fault 时，周期可以提交，但任务进入/保持 `Degraded` 并产生 `ExecutionBudgetExceeded`；它不增加 deadline miss 计数。
- 耗时 `> HardLimitNanos` 时立即进入 Fault，不得提交。这里“立即”表示任务返回点或必经的有界执行检查点首次观察到超限；R0 不承诺以异步信号抢占任意 Rust 代码。
- deadline miss 的周期不得提交，即使未超过 `HardLimit`。

后续 AOT 代码和 Runtime 操作必须在静态可证明的循环/调用边界提供执行检查点。Control Engine 整体停止响应由 R3 Guardian 租约/心跳处理，不由 R0 伪装成可抢占保证。

### 3.5 停止与取消

停止请求只在 task/release 边界观察。已开始周期必须走向 commit 或 discard，不得留下“半停止”bank。正常停止不生成新的 control output；真实输出的 Fallback/维持策略属于 R3。

## 4. Miss 历史与任务状态机

### 4.1 Miss 定义

以下结果各计一个 miss：

- `SkippedRelease`；
- `StartAfterDeadline`；
- 已执行但 `finish_monotonic > absolute_deadline`。

执行预算超限但仍在 deadline/HardLimit 内不计 miss。执行 Fault 和 HardLimit 超限直接 Fault，不通过 miss 阈值间接转换。

`MissWindow = W` 表示最近 `W` 个 scheduled release 的布尔结果，包括跳过和未执行项。`MaxMisses = M` 表示窗口最多允许 `M` 个 miss；更新后 miss 数 `> M` 进入 Fault。`ConsecutiveMisses = C` 表示连续 miss 达到 `C` 时进入 Fault。成功且未 miss 的 release 将连续计数清零。

未达到 Fault 条件但窗口内仍有 miss，或最近成功周期超过执行预算时，任务为 `Degraded`。只有窗口内 miss 数为零且最近成功周期未超过预算时才恢复 `Running`。

### 4.2 状态

| 状态 | 是否可执行 | 是否可提交 | 说明 |
| --- | --- | --- | --- |
| `Reinitializing` | 仅初始化逻辑 | 仅声明初值 | 建立新 task epoch 和初始 bank |
| `Running` | 是 | 是 | miss 窗口为空且最近周期在预算内 |
| `Degraded` | 是 | 是，仅成功且未 miss 周期 | miss/预算异常仍在可容忍范围 |
| `FaultLocked` | 否 | 否 | staging 无效，Fallback 请求保持 pending |
| `Stopped` | 否 | 否 | 未启动或已完成受控停止 |

### 4.3 转移

```text
Stopped -> Reinitializing -> Running
Running -> Degraded
Running|Degraded -> FaultLocked
Degraded -> Running
FaultLocked -> Reinitializing -> Running
Reinitializing -> FaultLocked
Running|Degraded -> Stopped
```

- 任何 validation error 在进入状态机前拒绝启动。
- `HardLimit`、执行异常、时钟契约错误、计数溢出、miss 阈值或明确的 task Fault 都转入 `FaultLocked`。
- `FaultLocked` 不随时间自动恢复，不执行 task，也不重复提交旧输出。
- reset 必须携带匹配的 `EngineEpoch`、`TaskEpoch` 和 Fault generation，并通过外部授权/Fallback guard；旧请求必须拒绝。
- reset 丢弃 staging、清空 miss/预算历史、递增 `TaskEpoch`，从不可变声明初值建立 commit sequence `0`。初始化失败返回 `FaultLocked(ReinitializationFailed)`。
- `TaskEpoch` 无法递增时 reset 被拒绝，任务保持锁定；不得回绕。
- 成功 reset 不改变 `engine_start_elapsed`、phase 或 period。完成初始化后读取单调
  `reset_completed_at`，选择原始时间网格中严格大于该时刻的首个 release；恰好落在
  release 时刻也从下一项恢复。初始化耗时不得使用 reset 请求到达时刻代替。
- 新 task epoch 的首个恢复 release 使用 `ReleaseSequence = 0`，其后按原网格递增；
  Fault 锁定和重新初始化期间的历史 release 不补跑，也不计入新 epoch 的 miss。
  恢复后调度观察迟到时，仍按第 3.3 节统计新 epoch 内的 skip/miss。
- 下一恢复 release/deadline 或 ordinal 不可表示时，reset 失败并保持锁定，不发布
  新 epoch/初值。身份、授权或 Fallback guard 拒绝同样不得改变当前 committed descriptor。

上述 reset 网格规则经 ADR-0005 明确；它补齐尚无实现的 Preview 1.0 reset 边界，
不改变已有首次启动调度或序列化字段。不得把旧实现的 release counter 当作网格 ordinal。

## 5. 周期事务与 Fault 原子性

每个任务拥有两个固定容量的组合 bank；每个 bank 同时包含私有 state 和该任务拥有的全部 control output。单一 committed descriptor 标识当前完整 bank、`TaskEpoch` 和 `CommitSequence`。

周期生命周期固定为：

1. `begin`：锁存本周期输入/跨任务快照；把 committed state/output 复制到非活动 staging bank。
2. `execute`：只修改 staging；committed bank 对本周期不可变。
3. `validate`：检查显式 task error、预算、HardLimit、deadline、容量和输出所有权。
4. `commit`：仅在全部检查成功时，以一个新 `CommitSequence` 发布整个组合 bank。
5. `discard`：任一检查失败时使整个 staging 无效，committed bank 保留作诊断证据，但不得继续作为有效 control output 发布。

提交描述符必须以 Release 发布，reader 以 Acquire 锁存。state 与 output 不得使用两个独立可见的提交点。未写 output 延续上一 committed 值，因为 staging 在 begin 时完整复制；语言/工作流不得绕过 staging 直接修改 committed bank。

进入 Fault 后，任务本周期与之后的 output publication 都无效；Guardian Fallback 在 R3 中具有更高仲裁优先级。R0 测试替身必须验证该请求，不得把“保留上一 committed bank 用于诊断”解释为继续驱动物理输出。

## 6. 跨任务快照

### 6.1 所有权

- 每个快照源只有一个写入任务；零写者或多写者在构建/启动前拒绝。
- writer 拥有两个预分配 publication slot；每个 reader 拥有独立预分配的本周期锁存副本。
- reader 不得持有 publication slot 的可变引用，也不得让 writer 等待引用释放。
- reader 在周期 begin 锁存一次，本周期内只读该副本；本周期中途发布的新版本只对下次 begin 可见。

### 6.2 元数据

每个成功发布的快照至少携带：

- contract major/minor；
- `EngineEpoch` 与 source `TaskEpoch`；
- `CommitSequence`；
- source `ReleaseSequence`；
- publish monotonic nanoseconds；
- 可选 UTC timestamp 与对应 `TimeQuality`；
- `QualityCode`；
- payload length/schema identity。

UTC 缺失或质量变差不得改变调度结果。reader 使用同一 `EngineEpoch` 的单调时间计算 `age = now - publish_monotonic`；`age > configured_max_age` 时在 reader 本地增加 `STALE` flag。等于最大年龄仍可接受。Epoch 不匹配、时间来自未来、schema 不匹配或 payload 超长必须拒绝锁存，不得仅标记 Stale 后继续解释。

### 6.3 发布与锁存协议

writer 始终写非 published slot，并使用每槽 generation 防止 reader 接受撕裂内容：

1. 将目标槽 generation 变为奇数，表示写入中。
2. 写入固定容量 payload 和 metadata。
3. 以 Release 将槽 generation 发布为新的偶数。
4. 以 Release 发布包含 slot、task epoch 和 commit sequence 的 descriptor。

reader：

1. 以 Acquire 读取 descriptor 和偶数槽 generation。
2. 复制到 reader 自有预分配副本。
3. 再以 Acquire 读取槽 generation 和 descriptor；两次值必须完全一致且为偶数。
4. 验证失败时允许再尝试一次最新 descriptor；第二次失败返回 `SnapshotContended`，保留上一份 reader 副本并产生可观测计数，不得自旋或阻塞 writer。

payload 的并发存储必须使用安全原子固定宽度表示或经批准依赖的安全 API；Aurora 生产源码不允许为方便访问普通可变字节而绕过 `unsafe_code = "deny"`。具体槽字节布局在 R0-05/R0-07 版本化，不得使用 Rust ABI padding。

reader 观察到同一 epoch 中 `CommitSequence` 非递增时拒绝；递增超过 1 时接受最新完整快照，同时报告明确的 version gap。慢 reader 可以丢失中间版本，但不能阻塞 writer 或伪造连续性。

## 7. 有界 SPSC 与溢出

R0 进程内 SPSC 使用批准且精确锁定的 `rtrb = 0.4.0`，默认 `std` feature。Ring 只允许一个 producer 和一个 consumer，容量在初始化期分配并固定为正数；传入元素自身也必须是固定容量、移动/析构有界且不会在周期路径隐式分配的类型。

基础 `push` 在满时立即返回原值和 `Full`，不得等待 consumer。调用方策略必须静态固定：

| 通道 | 满时策略 | 结果 |
| --- | --- | --- |
| R0 Trace | `DropNewest` | 丢弃新记录；其 `EventSequence` 已消耗；增加 dropped count |
| 可拒绝控制/测试通道 | `RejectNewest` | 返回领域 `Full`；调用方在同一有界步骤内处理 |
| 最新跨任务快照 | 不使用 Ring | 使用第 6 节双槽，允许 reader 观察版本 gap |
| Fallback 请求 | 不使用 Ring | 使用第 8 节不可覆盖 mailbox |

R0 不在 producer 侧弹出 consumer 元素来模拟 `OverwriteOldest`。R5 Tag/Trend/Alarm 的共享内存覆盖、降采样和恢复语义必须另立跨进程布局和策略。

每个 Ring 必须暴露固定 capacity、当前可读/可写计数的观测快照、生命周期累计 push、pop、full/drop、abandoned 以及 high-water mark。观测计数可饱和但不得回绕；饱和必须设置 sticky 标志。consumer abandon 后 producer 不等待、不重连和不无限重试。

## 8. Fallback 请求 mailbox

每任务拥有一个固定槽 mailbox，状态为 `Empty`、`Pending` 或 `Acknowledged`。Fault 转移必须在锁定任务后发布 `Pending` 请求；请求至少包含：

- `EngineEpoch`、`TaskHandle`、`TaskEpoch`；
- Fault generation/request sequence；
- 可穷举 `FaultReason`；
- 该任务拥有的 output set identity；
- Fault release/commit sequence 与单调时间。

mailbox 不得覆盖未确认请求。重复读取同一请求是幂等的；ack 必须完全匹配 epoch、task epoch 和 request sequence，旧 ack 明确拒绝。R0 使用测试替身验证 guard；R3 才定义 Guardian 租约、真实输出应用和跨进程确认。Fallback 请求发布失败本身是 engine-level sticky Fault，不能返回伪成功或仅写日志。

## 9. Trace 与诊断语义

R0-00 冻结字段语义；R0-07 冻结固定宽度二进制 offset 和黄金字节。每个 Trace record 至少表达：

- layout major/minor、record kind/length；
- `EngineEpoch`、`TaskHandle`、`TaskEpoch`；
- `EventSequence`、`ReleaseSequence`、前后 `CommitSequence`；
- scheduled release、start、finish、absolute deadline 和 execution elapsed（单调纳秒）；
- 可选 UTC 与 `TimeQuality`；
- task state before/after、miss result、`FaultReason`、Fallback request sequence；
- snapshot input/output sequence 或 gap；
- Ring capacity、occupancy high-water、累计 dropped/full 计数及饱和标志。

`EventSequence` 在尝试入队前分配，因此满队列丢弃会使后续可见记录出现 gap。consumer 必须把 gap 报告为 `[expected, observed)`，不得补造事件。若 Ring 持续为满、没有后续记录可呈现 gap，独立健康快照中的累计 dropped count 仍必须变化。

Trace producer 只执行固定次数的编码与一次非阻塞 push；不得格式化文本、写文件、调用日志后端、重试或等待。Observe 只能读取，不得写值、Force、暂停任务或改变调度。

## 10. 错误分类

后续领域错误类型至少可穷举以下类别，不得只返回字符串：

- `InvalidTaskHandle`、`DuplicateTaskHandle`、`DuplicateWriter`；
- `InvalidPeriod`、`InvalidPhase`、`InvalidDeadline`、`InvalidBudget`、`InvalidHardLimit`；
- `InvalidMissWindow`、`InvalidMissThreshold`、`InvalidCapacity`、`ResourceBudgetExceeded`；
- `ScheduleTimeOverflow`、`CounterOverflow`、`ClockContractViolation`；
- `StartAfterDeadline`、`FinishAfterDeadline`、`ExecutionBudgetExceeded`、`HardLimitExceeded`；
- `TaskExecutionFault`、`ReinitializationFailed`、`FaultLocked`、`StaleResetRequest`；
- `SnapshotContended`、`SnapshotEpochMismatch`、`SnapshotFromFuture`、`SnapshotSchemaMismatch`、`SnapshotGap`；
- `QueueFull`、`ProducerAbandoned`、`ConsumerAbandoned`、`TraceCounterSaturated`；
- `FallbackAlreadyPending`、`FallbackAckMismatch`、`FallbackPublicationFault`。

配置错误拒绝启动；执行错误按本文进入 Degraded/Fault；观测错误不得静默，也不得无条件升级为停止全部健康任务。相互独立任务的 Fault 不能阻止调度器继续服务健康任务。

## 11. 兼容与后续边界

- 本规范为 Preview 1.0。破坏字段含义、状态转移、排序、阈值边界或原子发布语义必须增加 contract major，并提供迁移或显式拒绝路径。
- R0-01 定义 Rust 领域类型和 validation error，但不得加入 Linux、I/O、网络、存储或 UI 依赖。
- R0-05 定义快照/SPSC 的实现与压力验证；引入 `rtrb` 时执行 ADR-0004 的依赖门禁。
- R0-07 定义 Trace 二进制布局、版本拒绝和黄金字节。
- R1 只把 Aurora ST/AOT 绑定到本任务生命周期，不改变提交/Fault 语义。
- R3 定义 Guardian、共享内存、Fallback ack、I/O epoch 和物理输出窗口。
- R5 定义 Data Bridge、每消费者共享内存 Ring、完整快照/增量恢复和持久化隔离。

## 12. R0-00 验收映射

| Project 验收项 | 本规范证据 |
| --- | --- |
| 每个术语有显式单位、范围、错误和状态转移 | 第 2～4、10 节 |
| 并发所有权、原子序、阻塞/分配约束可测试 | 第 3、5～9 节 |
| 必要 ADR/契约评审完成，不存在未决实现选择 | ADR-0004 与本文；Accepted 后成立 |
| 明确 R0 非目标与 R1/R3/R5 占位边界 | 第 1、11 节 |
