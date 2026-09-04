# ADR-0004：R0 确定性执行语义与有界并发基础

- 状态：Accepted
- 日期：2026-09-04
- 决策人：Caymir
- 关联需求/问题：GitHub Project `R0-00`、R-002、R-003、R-017、R-023、R-032～R-034、R-061、R-066

## 背景

R0 必须在生产实现开始前冻结任务释放、deadline miss、Fault 原子性、跨任务快照和 Trace 的可测试语义。现有架构已经限定普通 Linux x64、Rust `std`、固定容量、周期线程不阻塞、单调时钟调度和 Fault 后 Fallback，但尚未定义同刻任务顺序、积压周期处理、阈值边界、发布内存序和队列满时行为。

R0 不提供硬实时保证。规范的目标是让相同静态计划和输入 Trace 可复现，并让超时、丢失、容量不足和降级显式可见。

## 备选方案

### 任务执行

- 每任务一个 OS 线程并使用抢占优先级：可以并行，但普通 Linux 上的抢占与完成顺序不可复现，跨任务提交和 Fault 隔离更复杂。
- 单一周期执行线程按静态顺序运行全部到期任务：没有任务级并行，吞吐上限更低，但顺序、所有权和最坏工作量可以静态分析。
- 对迟到任务补跑全部积压周期：保留每次状态转移，但积压量无界并会放大过载。
- 跳过已经失去执行窗口的 release：不会形成无界追赶，代价是必须把每个跳过显式计为 miss。

### 有界并发

- 只用安全 `std` 自研通用 SPSC：标准库没有满足固定容量、wait-free 和泛型所有权转移的现成原语；自行组合会扩大并发正确性证明范围。
- 局部 `unsafe` 自研 Ring：依赖最少且可定制，但 Aurora 必须自行承担别名、初始化、析构和原子序的 soundness 证明。
- 使用 `rtrb`：提供安全 API、初始化期固定容量分配和 wait-free SPSC；代价是引入包含内部 `unsafe` 的第三方生产依赖和供应链审查责任。

## 决策

- R0 使用一个周期执行线程串行执行静态任务表。任务不会在 R0 内创建并行执行线程；同一 release 时刻按优先级降序、再按 `TaskHandle` 升序执行。
- release 在 `EngineEpoch` 作用域内使用单调时钟绝对公式 `engine_start_elapsed + phase + k × period`。调度器不按上次完成时间累加，也不补跑已经失去执行窗口的 release；跳过项以有界批量更新记入 miss 历史。
- deadline、执行预算和 `HardLimit` 都相对 scheduled release/实际 start 使用纳秒整数表示，具体边界和状态转移以 `Sources/Contracts/control/v1/r0-execution-semantics.md` 为规范源。
- 任务状态与输出共用 committed/staging 两个预分配 bank 和一个提交版本。成功周期整体提交；任何 Fault 丢弃整个 staging，锁定任务并发布持久的 Fallback 请求。
- 跨任务快照由单写者发布到两个预分配槽，reader 锁存到自己的预分配副本。发布描述符使用 Release，reader 使用 Acquire；reader 争用或停止不能阻塞 writer，失败与版本缺口必须可见。
- R0 的进程内 SPSC 采用精确锁定的 `rtrb = 0.4.0`，启用默认 `std` feature。Aurora 生产源码继续保持 `unsafe_code = "deny"`，不复制或包装暴露该库内部指针，也不据其可选 `no_std` 能力声明 Aurora 支持 RTOS、裸机或 `no_std`。
- `rtrb` 只承担单生产者/单消费者的所有权转移；最新快照使用双槽发布，Fallback 请求使用不可覆盖的任务 mailbox。R0 Trace Ring 满时丢弃新记录并计数，不在 producer 中读取或覆盖 consumer 槽。
- R5 面向 HMI/Storage/Gateway 的共享内存 Ring、Tag 覆盖最旧值和跨进程二进制布局不由 `rtrb` 定义，仍需独立版本化规格。

## 依赖审批记录

- 用途：R0 进程内固定容量 SPSC；不用于调度、共享内存 ABI、网络或持久化。
- 批准版本：`0.4.0`，后续实现必须使用精确版本并提交锁文件。
- 许可证：`MIT OR Apache-2.0`。
- 维护状态：截至 2026-09-04，官方仓库未归档，`0.4.0` 于 2026-08-17 发布，仓库于 2026-09-01 仍有提交活动。
- 供应链风险：库内部使用 `unsafe` 管理预分配槽；升级可能改变内存序、析构或 MSRV。引入时必须通过 `cargo deny`、RustSec、许可证和来源门禁，并用固定容量、满/空、wrap-around、drop、线程退出和压力测试验证使用边界。
- 替代方案：安全原子固定记录、局部 `unsafe` Ring、`crossbeam` 有界队列。前者扩大自研并发代码，后两者分别增加 soundness 责任或超出 SPSC 所需能力。
- 升级策略：任何版本变化均作为显式依赖变更评审，不使用宽松 SemVer 范围；发现 advisory 或 soundness 问题时可回退到最后通过验证的精确版本，或以相同契约替换实现。

## 后果

单线程静态执行牺牲任务并行吞吐，但避免普通 Linux 调度顺序成为控制语义。迟到时不追赶可以保证每次调度工作的上界，但程序必须通过 miss、Degraded、Fault 和 Fallback 处理缺失周期。

双槽快照允许 reader 停顿而不阻塞 writer；reader 可能跳过中间版本，因此 sequence gap 和 `Stale` 是正常且必须处理的契约结果。Trace 过载不会反压周期线程，但诊断历史可能不完整，必须由 sequence 和累计丢弃计数暴露。

采用 `rtrb` 不改变 Aurora 的平台边界，也不构成硬实时声明。R0-05 引入依赖时仍需完成实际代码、锁文件和自动化验证，本 ADR 不提前实现该功能。

## 验证

- 手动单调时钟验证 phase、同刻 tie-break、UTC 跳变、迟到 release、跳过批量和时间溢出。
- 固定 seed 验证相同计划与输入 Trace 产生相同执行顺序、提交版本、状态和诊断。
- 在每个执行边界注入 Fault，证明 committed state/output 不变，staging 不可继续执行，reset 从声明初值和新 task epoch 启动。
- 并发压力验证快照无撕裂，reader 停顿不阻塞 writer，争用、版本缺口和 `Stale` 可见。
- SPSC 验证容量 1、满/空、wrap-around、producer/consumer 退出、丢弃计数和 high-water mark；周期 producer 不等待 consumer。
- 仅在指定 Linux x64 硬件与工程负载上报告周期、p50/p99.9/max 抖动、deadline miss、CPU 和队列水位，不外推平台保证。
