# Rust workspace

本 workspace 承载 Aurora 的 Runtime、Target、Gateway、host-only 构建工具和 Rust SDK。

## 当前范围

当前 workspace 包含：

- `aurora-types`：无 I/O、网络、存储和平台依赖的基础领域类型边界。
- `aurora-control-contracts`：版本化控制契约及生成类型的承载边界。
- `aurora-control-engine`：Control Engine 可移植核心；当前包含 R0-02 固定容量工作集、
  R0-03 静态绝对调度、R0-04 周期 state/output 事务，以及 R0-05 跨任务双槽快照和
  进程内有界 SPSC。
- `aurora-test-support`：仅供测试使用的仿真时钟、虚拟 I/O、故障计划和确定性回放工具。
- `aurora-build`：host-only 的跨平台验证、摘要与供应链产物入口。

`aurora-control-engine` 的调度器只读取可注入单调时钟并返回绝对 `WaitUntil`、release
或停止决策；具体 Linux 单调时钟/绝对等待适配、产品任务体、完整任务状态机和真实
I/O 仍按后续工作项分别交付。workspace 仍不包含 Aurora ST、工作流、设备驱动、
生产部署或 UI。

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

## R0-05 快照与 SPSC 边界

`SnapshotPublisher` 使用两个初始化期预分配的原子槽。唯一 writer 先把非活动槽的
generation 标为奇数，写完完整 staging 后以 Release 发布偶数 generation，再以奇偶
descriptor 发布 slot、TaskEpoch 和 CommitSequence。每个 reader 以 Acquire 读取并在
复制到自己的预分配 staging 后复核槽 generation 与完整 descriptor；首次争用只尝试
一次最新 descriptor，第二次失败返回 `Contended` 并保留 reader 上一完整副本。成功
锁存按 reader 独立记录 commit 进度、缺口、单调年龄和 `Stale` 质量；停止消费的
reader 不持有共享槽，也不会阻塞 writer。

进程内 `bounded_spsc` 精确使用 `rtrb = 0.4.0`，初始化先验证 Target 上限、依赖的双倍
位置空间和实际槽位分配布局，失败显式返回且容量初始化后不增长。producer 和
consumer 均只有一个所有者，调用立即返回。`RejectNewest` 在满队列时退回 item 且不消耗
sequence；`DropNewest` 丢弃新 item、累计 drop 并消耗 sequence，使 consumer 可观察
后续 gap。固定 capacity、当前 readable/writable、累计 push/pop/full/drop、high-water
mark、统计饱和和 endpoint abandoned 均可观测。R0 SPSC 不覆盖 consumer 槽；
latest-wins 使用上述快照双槽，R5 的共享内存 `OverwriteOldest` 和跨进程 ABI 未实现。

## R0-06 Deadline、Fault 与 Fallback 状态机

`TaskStateMachine` 复用调度器的 skipped range、带 release sequence 的事务提交证据和
锁存故障，不建立第二套 release、commit 或 Fault generation。每项结果必须按
`ReleaseSequence` 严格连续，重复或缺口证据在改变统计前拒绝。最近 `MissWindow` 个结果
保存在初始化期预分配的固定字节环中；超大 skipped 批次最多检查一个窗口以定位首个
阈值，余量以常数次折叠计数，不逐项追赶；批量历史触发 Fault 后同次已选择但不再执行的
当前 release 只追加一次未执行 miss。窗口 miss 未越阈值或最近成功周期超预算时为
`Degraded`；窗口清空且
最近成功周期回到预算内时恢复 `Running`，HardLimit/执行故障和阈值越界保持
`FaultLocked`。

每任务只有一个 `Empty -> Pending -> Acknowledged` Fallback mailbox。任务先锁定再发布，
重复同步同一故障不生成新 request sequence，错误 ack 不清除请求，未确认请求不得被
覆盖；publication 失败设置只随进程重启清除的 engine-level sticky 标志。成功
reinitialize 后才清空旧窗口、预算状态和已确认请求。R0 mailbox 是 Guardian 接入前的
进程内确定性边界，不实现 R3 Guardian 租约、真实输出应用或跨进程 ack。

## R0-07 Observe 与有界 Trace

Trace 1.0 使用固定 64-byte 文件 header 和 320-byte little-endian record，显式编码
epoch、task/release/commit sequence、单调/可选 UTC 时序、状态、miss/Fault/Fallback、
skipped range、input/output snapshot gap 与 ring overflow 统计。decoder 精确拒绝未知
版本/flags、非零 reserved、非规范 optional、截断、尾随字节和 header/record epoch 不同。

`bounded_trace_channel` 在启动期预分配，producer 每项只做固定编码和一次非阻塞
`DropNewest` push；重复/缺口 EventSequence 在改动 ring 前拒绝，full drop 消耗 sequence，
后续 Observe 明确报告 gap。`TraceObserver` 只暴露读取与统计，没有写值、Force、暂停或
调度 API。host-only `aurora-build trace-decode` 和 `trace-compare` 提供离线验证与逐项比较，
不连接 Runtime，不实现 R3/R5 共享内存、网络或持久化分发。

## R0-08 自动化验证套件

`aurora-control-engine/tests/r0_verification.rs` 是可移植核心的跨组件验收入口：固定 seed
`0x8a1359d724c6e0f1` 回放 256 个周期并逐项比较 state、output、commit 和 Trace bytes；
六个多周期/多相位任务运行至 50,000ns，按精确期望数量验证 release 顺序、horizon 包含边界
和首个 release 超界，并在 UTC 前后跳变时保持同一结果；snapshot 使用固定 32-byte payload、
4,096 次发布、两个经计数确认的活跃 reader 和一个 stalled reader，并以 10 秒整体固定超时
拒绝挂起。

同一套件还覆盖五种执行 Fault 边界的部分 bank discard、授权 reset、声明初值恢复，以及
容量 3 的 SPSC full/empty、4,096 次索引 wrap、DropNewest sequence gap 和 high-water。
模块内既有测试继续覆盖每个 state/output 索引的 Fault 注入、miss 窗口、reset 拒绝路径和
并发原子序。统一入口为 `cargo test --locked --manifest-path Sources/Rust/Cargo.toml
-p aurora-control-engine --test r0_verification -- --test-threads=1`；Linux x64 CI 是主运行门禁，
Windows 仅运行同一无平台 I/O 的可移植核心，不据此声明 Linux 性能或硬实时能力。

## R0-09 Linux x64 性能与退出门禁报告

host-only `aurora-build r0-report` 仅接受 Linux x64 Release 构建。命令先执行 Rust workspace
的 `fmt --check`、`clippy -D warnings` 和全部测试，任一失败均不生成报告；随后在固定 1ms
基准周期、四个静态任务、2KiB 工作集和 256-slot Trace 容量下测量 10,000 个周期。测量
包含固定 1,000-cycle consumer stall 和部分 bank 写入后的 Fault 探针，精确记录实际周期、
p50/p99.9/max jitter、deadline miss、CPU、内存、队列水位、drop 和 gap。

```bash
cargo run --locked --release --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- \
  r0-report --output Builds/r0-linux-x64-report.md
```

报告记录 OS、kernel、CPU、内存、commit、工作树状态、构建类型、全部固定容量和统计定义。
结果只适用于报告内的机器与工程负载；WSL2、开发机或任一目标机的结果均不得外推为平台
性能、实时或功能安全保证。目标部署仍须在其指定硬件和完整工程上重新运行并人工准入。

reset 契约补全见 [ADR-0005](../../Documents/ADR/0005-r0-reset-release-grid.md)；
R0 有界并发和 `rtrb` 审批见 [ADR-0004](../../Documents/ADR/0004-r0-execution-semantics.md)。
既有二进制契约未改；新增 Rust API 尚未发布，无持久化迁移或部署步骤。

## R1-00 Aurora ST 规格完整性门禁

R1-00 只冻结 [Aurora ST Preview 1.0](../Contracts/st/v1/language.md)、
[规范 EBNF](../Contracts/st/v1/aurora-st.ebnf) 和
[地址映射语义](../Contracts/st/v1/address-mapping.md)，不创建 Lexer、Parser、AST、IR、AOT、
Device Mapping Schema 或 Target 运行期编译器。host-only `aurora-build verify` 会检查 EBNF
规则恰好完整、没有未定义引用，并检查 Preview 1.0 的 65 个编译/运行/地址诊断无重复、
无遗漏和无悬空引用。门禁失败时不生成部署产物。

语义选择和不包含范围见
[ADR-0006](../../Documents/ADR/0006-r1-st-language-and-address-semantics.md)。后续 R1 编译器工作项
必须消费这些规范源，不能由实现反向扩展接受集或修改诊断 cardinality。

## R1-01 Aurora ST 词法、语法与 AST

`aurora-st-ir` 提供 host-only 的 Preview 1.0 Lexer/Parser 和版本化、保留 UTF-8 byte span 的
syntax AST。调用方必须显式提供单文件 byte、token、AST node 和嵌套深度上限；任一边界失败
只返回稳定诊断，不发布部分 AST。有效 AST 只以精确 schema `1.0` 编码为 RFC 8785
canonical JSON，未知 writer schema 不产生输出。本工作项不执行名称解析、类型检查、地址绑定、
Canonical IR 或 AOT，也不将解析器引入周期执行路径。

## R1-02 Aurora ST 名称与标量类型语义

`aurora-st-ir` 在完整项目的 parser AST 上按规范路径 bytewise 顺序收集顶层和 POU scope，绑定
类型、变量、Function、Function Block 与 enum member 引用，并检查 Preview 1.0 标量公共类型、
无损隐式扩宽、显式 `TO_*`、标准函数与静态调用图。失败分析只发布按 path/span/code 排序的
稳定诊断，不发布部分 semantic model；诊断也可编码为 RFC 8785 canonical JSON。R1-02 不计算
复合类型容量与布局、不生成 arithmetic/index Fault site、不绑定逻辑地址，也不创建 Canonical IR、
AOT、图形编辑器、Online Change 或跨版本状态迁移；这些边界仍由 R1-03 及后续工作项负责。

## R1-03 固定容量数据与静态 FB 实例

`aurora-st-ir::analyze_fixed` 在成功的 R1-02 semantic model 上计算规范 fixed layout。调用方必须
显式提供全部非零 Target Profile 上限；STRING/WSTRING、ARRAY、STRUCT、enum、named alias、
Program static storage 和 Function/FB/Program invocation frame 均使用 SPEC-R1-001 冻结的
little-endian size/alignment/padding 规则与 checked arithmetic。超容量、动态边界、递归图或预算
失败只发布稳定诊断，不发布部分 fixed model。

静态 FB 实例先计算每个 Program template 的完整实例数并验证预算，再按声明顺序和数组索引升序
分配 Program-local identity、规范路径和包含关系明确的 state offset；同级/根实例不重叠，嵌套 FB
只占用父实例唯一拥有的子区域。声明初值必须是无 variable/user-call 依赖的编译期表达式，并保留为
可复现初始化计划；global 的 fixed type/initializer 关联继续提供给 R1-05，
未使用 payload 和 padding 必须在后续 lowering/reset 中归零。此阶段不生成 arithmetic/index Fault
site、不绑定逻辑地址、不创建 Canonical IR/AOT，也不承担 R1-06 task plan 的实际 Program 实例总预算。

## R1-04 确定算术与 Fault site

`aurora-st-ir::analyze_faults` 在成功的 R1-03 fixed model 上验证整数溢出 mode、除零、非有限
float、显式转换、`LIMIT`、固定容量 `CONCAT` 与 ARRAY index。编译期可确定的失败只生成一个
稳定诊断且不发布 partial model；dynamic 风险操作按规范 source path/span/operation 顺序恰好生成一个
site identity。单个 site 保存非空、排序、去重且有界的 possible Fault outcomes，因此 signed
integer division 的 overflow/zero-divisor 不会复制执行项。已证明安全的 widening conversion、
常量合法 ARRAY index、identity checked arithmetic、已知非零且非 `-1` 的 integer divisor、
空串 identity `CONCAT`、saturating/wrapping arithmetic 和合法 constant `LIMIT` bounds 不生成
多余 site；已知为零的 integer divisor 直接产生一次 `ST4003`，不遗留运行 site。

本阶段只产生供后续 lowering 消费的 host-only semantic model，并提供无分配的固定宽度 integer
policy/ARRAY bounds 函数；不创建 Canonical IR、Source Map、AOT、参考执行器、逻辑地址绑定或周期
运行时。R1-06/07 必须共同消费这套 policy，并用相同输入 bit pattern 做差分验证。

## R1-06 Canonical ST IR（分步交付）

`aurora-st-ir::lower_canonical_ir` 的第一阶段在已接受的地址模型和静态工作量证明上生成结构化、
确定性的 Canonical ST IR。每个 Function、Function Block 和 Program 声明恰好对应一个 POU，
每个可执行 AST 节点按全局 preorder 恰好生成一个 dense `u32` node ID；`FOR` 只生成一个带精确
迭代次数的结构化节点，不按迭代次数复制 body。每个运行 Fault site 和循环证明都必须恰好消费
一次，Task 表必须与 Program-to-Task 绑定双向一致。任一模型不一致直接失败，节点/POU 容量失败
只发布一个 `ST3005` 且不返回 partial IR。完整 IR 可在显式 byte 上限内编码为 RFC 8785 JSON。

后续初始化阶段为每个 fixed global、实际 Task Program 实例和 POU invocation frame 生成独立的
规范 byte image。所有 image 在分配前以 checked `u64` 汇总并同时验证 per-task、task 总量、global
总量、frame 总量和全体初始化总量；恰好等于限制可接受，超过一个 byte 即只发布一个 `ST3005`。
每个 destination 先完整清零，再写 little-endian 标量、enum、UTF-8/UTF-16 payload 或递归
aggregate，因此 field/stride/tail padding 与未使用 string payload 保持为零。显式 initializer 同时
保留 source path 与 span，跨文件 named-type 默认值不会错取同 offset 的其他表达式。

Source Map 的 source-to-IR 阶段与 IR 原子发布：规范 path 顺序为每个输入文件分配 dense source
ID（包括不含可执行 POU 的文件），并为每个 semantic symbol、Canonical node 和 runtime Fault site
恰好生成一条映射。node entry 保留所属 POU 与半开 UTF-8 byte span；Fault entry 关联唯一 node、
source span 和 operation。各集合与 RFC 8785 JSON bytes 都有独立非零上限；等于限制可接受，超过
即只发布一个 `ST3005`，IR 与 Source Map 都不返回。

当前 Source Map 不包含虚构的 native range；真实 native address 必须由后续 AOT 生成后补入。
当前也不生成原生指令或 checkpoint，且不修改 F0 的外部 Canonical IR JSON Schema；该 Schema 的
R1 ST unit 形状尚未被接受，不能由实现自行发明。

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
