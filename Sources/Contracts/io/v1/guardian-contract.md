# SPEC-R3-001：I/O Guardian Contract 与 Driver Adapter Preview 1.0

## 1. 范围与基础

本规格冻结普通 Ubuntu Linux x64、Rust `std` 基线上的槽外 Guardian、Control Engine 共享 I/O
映像和协议无关 Driver Adapter。它不定义真实驱动实现、远程控制 API、OPC/MQTT Connector、
跨平台 Runtime、硬实时或功能安全能力。

除非字段另有说明，整数均为 little-endian，无符号时间间隔和单调时间均为纳秒。共享内存中的
原子字段使用目标原生 little-endian `AtomicU64`，启动前必须验证 8-byte alignment 和 lock-free；
不满足时拒绝映射。UTC 只用于观测，不参与租约、年龄、deadline 或恢复判断。

Preview 版本为 `major=1, minor=0, lifecycle=preview`，兼容窗口为 N/N-1。不兼容 major 必须拒绝；同 major
的双方在创建映射前交换支持的 minor、layout 和 capability，选择最高共同 minor。Guardian N 必须支持
Control N/N-1，Control N 也必须能连接 Guardian N/N-1；任一工程 required capability 在协商结果中不可用
时拒绝，不得静默降级。新功能默认 optional，Writer 不得向旧 reader 写入未知必选 capability。部署验证
必须同时覆盖当前与回滚 Runtime；Preview 破坏性变化仍须提供迁移说明和黄金布局。

## 2. 所有权与进程边界

| 角色 | 唯一权限 | 禁止事项 |
| --- | --- | --- |
| I/O Guardian | 创建 epoch/lease/共享区；拥有 Fallback；排他管理设备和 Driver Host | 执行 ST/Workflow；把设备句柄交给 Control Engine |
| Control Engine | 锁存完整 input image；向有效 lease 提交完整 output image | 打开 NIC/socket/serial/CAN/设备文件；调用后端原生 API |
| 静态 Driver | 在 Guardian 内执行构建 allowlist 中已审计的第一方安全 Rust 低延迟 bounded work | 运行期发现/装载；扩容；把后端指针写入共享区 |
| 隔离 Driver Host | 每个固定 DriverInstance 只访问一个物理接口及其有界设备集、固定共享槽和本地控制通道 | 跨无关接口复用 Host；访问 Control Engine 私有内存；加载第三方动态插件 |
| Target Agent | 预装 pending Fallback 并编排租约交接 | 直接执行周期 exchange 或管理槽内业务进程 |

每个物理 interface 同时只有一个 owner。重复 claim、身份不匹配、权限不足或已有 owner 时，激活在
设备打开前失败。Guardian 终止后只能由 systemd/Target Agent 按 Target Profile 恢复；新进程创建新
GuardianEpoch，不继承旧 lease 或危险输出。

Driver 的执行模式由构建 allowlist 与 Target Profile 共同固定，现场不能切换。EtherCrab 等通过审计的
第一方安全 Rust 实现可以静态链接；IgH、C FFI、内核耦合、厂商 SDK 或可能阻塞的实现必须进入隔离
Driver Host。两种模式消费同一 Adapter 契约和黄金测试。每个 Host 使用独立非 root 服务身份；systemd
必须启用 `NoNewPrivileges`、精确 DeviceAllow/capability、只读文件系统、网络/namespace 与 syscall
限制。沙箱、peer identity 或最小权限不能完整建立时拒绝启动并保持 Fallback。

Control/Driver Host 的本地控制面使用 Unix domain socket，并校验 `SO_PEERCRED`、预期 UID/GID、systemd
service/cgroup 身份、contract、epoch、capability 与 configuration digest；PID 只用于本次连接关联，不能
单独作为身份依据。Guardian 创建固定尺寸 `memfd`，传递 fd 前至少设置
`F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`；活动 writer 映射不要求 `F_SEAL_WRITE`。周期数据只走
共享内存，UDS 只承载有界非周期控制消息。每次新 lease 创建全新映射，旧映射在双方停写并关闭后作废，
永不复用。

## 3. 标识、版本和 capability

- `GuardianEpoch`、`LeaseSequence`、`ImageSequence`、`ConfigurationGeneration` 为非零 `u64`，
  checked 增长；不可表示时进入 Fallback 并要求重新初始化，禁止回绕。
- `LeaseId` 为 16-byte opaque identity，仅用于相等比较；不得从时间或 PID 推断。
- `ConfigurationDigest`、`LayoutDigest`、`CapabilityDigest` 和后端 source digest 均为原始 32-byte
  SHA-256，进入 Payload/Target Profile 证据。
- capability 使用 `aurora.io.<name>@<major>`；构建期列表排序、唯一且固定。未知 required capability
  拒绝；未知 optional capability 可记录为 unavailable，但不能静默启用。
- 下表是 Preview 1.0 唯一已知 capability 目录，不表示每个 Target 都能发布全部条目。实际 offered set
  必须是按表序编码、无重复的精确子集；只有构建期已包含且 Target Profile 已批准的能力才可出现。
  required/optional 均不得包含目录外值，未安装 backend、无获批硬件的 LIN、OPC UA/MQTT 均不得被多生成。

### 3.1 Known capability catalog

| Capability | 发布条件 |
| --- | --- |
| `aurora.io.guardian@1` | Guardian Contract 实现存在 |
| `aurora.io.image@1` | 对应 image layout 已实现并批准 |
| `aurora.io.driver-sdk@1` | Driver Adapter ABI 已实现并批准 |
| `aurora.io.ethercat-main-device@1` | 至少一个获批且已安装 EtherCAT backend |
| `aurora.io.modbus-tcp-client@1` | Modbus TCP Client 已构建并批准 |
| `aurora.io.modbus-rtu-master@1` | Modbus RTU Master 已构建并批准 |
| `aurora.io.serial@1` | serial transport 已构建并批准 |
| `aurora.io.socketcan@1` | SocketCAN backend 与目标接口已批准 |
| `aurora.io.lin-controller@1` | LIN Driver Host、SDK 与实际硬件能力均已批准 |

目录必须恰好包含上述九项；新增、删除或重命名 capability 是显式契约变更，必须同步规范、实现、黄金
样本和 N/N-1 测试。OPC UA/MQTT 不得出现在 Guardian capability 中。

## 4. Guardian 与租约状态机

```text
Cold → Fallback → LeasePending → Running
          ↑           |            |
          +-----------+------------+
              reject / timeout / fault / stop

Fallback → Reinitializing → LeasePending
```

- `Cold` 不驱动普通输出；设备打开、identity/topology/configuration/layout 校验完成后进入 Fallback。
- `Fallback` 只允许 active Fallback image 和设备 watchdog；pending 不能成为输出，直到完整健康窗口通过。
- `LeasePending` 校验 contract range、capability、epoch、configuration/layout digest、Control identity、
  heartbeat interval 和 timeout。成功创建新 LeaseId/LeaseSequence，ImageSequence 从 1 开始。
- `Running` 只接受当前 epoch/lease 且严格递增的完整 output image。重复、回退、跨 lease、未知 layout、
  过期或超容量输出在改变设备前拒绝。
- Control heartbeat 通过非周期 UDS 独立发送；每个 output group 另有独立 freshness deadline。两者均使用
  Guardian 单调时钟，heartbeat 不能延长输出有效期，发布输出也不能替代 heartbeat。
  heartbeat 和 group image 仅在 `now < deadline` 时有效，`now == deadline` 已过期，不允许边界时间重放。
  `heartbeat_timeout_ns >= heartbeat_interval_ns > 0`。任一全局 heartbeat/Control 故障撤销 lease 并使
  全部 FallbackDomain 进入 Fallback；单组过龄或局部设备 fault 只影响其构建期绑定的 domain。
- `Reinitializing` 关闭旧 driver session、清空 pending output、重新验证设备/topology/configuration，
  建立新 epoch 或 generation；只有新 lease 和健康窗口可以回到 Running。
- 恢复不得自动重放最后写操作、未确认 mailbox 命令或旧 output image。普通 FallbackDomain 可按
  Target Profile 的固定次数、窗口和 backoff 自动恢复，但每次都创建新 generation、重验身份/配置/layout、
  通过健康窗口、建立新 lease 并等待 fresh output；耗尽后进入 `RecoveryLocked`。危险 domain 只能自动
  恢复通信与只读健康，重新驱动危险输出还需要本地或签名授权以及独立安全系统许可。

激活后的 I/O 配置不可变。backend、device、topology、mapping/layout、group timing/capacity、timeout、
retry、stale、recovery、Fallback、protection、source 或 capability 任一变化都产生新的
ConfigurationGeneration：签名 pending config 通过静态校验后，受影响 domain 先进入 Fallback，再撤销
旧 lease、停止 driver、创建新 mapping/generation、完成健康窗口并取得新 lease。禁止 old/new 混用和
Online Change；纯 Studio 布局或不进入签名 Runtime 语义的标签可以独立变化。

## 5. 共享内存 ABI

### 5.1 Region header

Region header 固定为 256 bytes、64-byte aligned。总 region 顺序为 header、两个 input slots、两个
output slots；每个 slot stride 向上对齐 64 bytes，所有 offset/size 使用 checked arithmetic。

| Offset | Size | Field | 规则 |
| ---: | ---: | --- | --- |
| 0 | 8 | Magic | ASCII `AURIO001` |
| 8 | 2 | LayoutMajor | `1` |
| 10 | 2 | LayoutMinor | `0` |
| 12 | 4 | HeaderBytes | `256` |
| 16 | 8 | TotalBytes | 等于映射长度，非零且 64-byte aligned |
| 24 | 8 | GuardianEpoch | 非零；映射生命周期内不可变 |
| 32 | 32 | ConfigurationDigest | 签名配置 SHA-256 |
| 64 | 8 | InputOffset | 64-byte aligned，`>=256` |
| 72 | 4 | InputStrideBytes | 两个同尺寸 slot 的 stride |
| 76 | 4 | InputPayloadCapacityBytes | 固定 input payload 容量 |
| 80 | 8 | OutputOffset | 位于两个 input slots 之后并对齐 |
| 88 | 4 | OutputStrideBytes | 两个同尺寸 slot 的 stride |
| 92 | 4 | OutputPayloadCapacityBytes | 固定 output payload 容量 |
| 96 | 4 | ValueCount | 固定 local handle 数量 |
| 100 | 2 | InputGroupCount | 固定 input Update Group 数量 |
| 102 | 2 | OutputGroupCount | 固定 output Update Group 数量 |
| 104 | 8 | Flags | 未定义 bit 必须为 0 |
| 112 | 32 | CapabilityDigest | 排序 capability 目录摘要 |
| 144 | 16 | LeaseId | 非零；创建映射前分配，映射生命周期内不可变 |
| 160 | 8 | InputPublishToken | Guardian 单写 `AtomicU64` |
| 168 | 8 | OutputPublishToken | Control 单写 `AtomicU64` |
| 176 | 8 | InputDropCount | Guardian 饱和计数 `AtomicU64` |
| 184 | 8 | OutputRejectCount | Guardian 饱和计数 `AtomicU64` |
| 192 | 64 | Reserved | Writer 写 0；reader 要求为 0 |

### 5.2 Image slot header

每个 slot header 固定为 128 bytes、64-byte aligned；之后是由 LayoutDigest 固定的 value bytes、
value metadata 和 group diagnostics。未使用 padding 必须为 0。

| Offset | Size | Field | 规则 |
| ---: | ---: | --- | --- |
| 0 | 8 | Generation | 每 slot 独立 `AtomicU64`；odd=writing，even=stable |
| 8 | 8 | GuardianEpoch | 必须匹配 region |
| 16 | 8 | ImageSequence | 当前 lease 内严格递增，范围 `1..=2^63-1` |
| 24 | 8 | SourceMonotonicNs | input 采样或 output commit 时间 |
| 32 | 8 | PublishMonotonicNs | producer 完成发布时间，`>=SourceMonotonicNs` |
| 40 | 8 | UtcSeconds | signed Unix seconds；Unknown 时为 0 |
| 48 | 4 | UtcNanoseconds | `0..=999999999`；Unknown 时为 0 |
| 52 | 1 | TimeQuality | `0 Unknown, 1 Synchronizing, 2 Good, 3 Holdover, 4 Degraded, 5 Invalid` |
| 53 | 1 | AggregateQuality | `0 Good, 1 Uncertain, 2 Bad, 3 Stale` |
| 54 | 2 | StatusFlags | 未定义 bit 为 0 |
| 56 | 4 | ValueCount | 必须匹配 region |
| 60 | 4 | PayloadBytes | `<=` 对应 payload capacity |
| 64 | 32 | LayoutDigest | handle、offset、type、byte/bit order 和 metadata layout 摘要 |
| 96 | 8 | DroppedBefore | producer 饱和累计丢失/跳过数 |
| 104 | 4 | DiagnosticsOffset | slot-relative、64-byte aligned |
| 108 | 4 | DiagnosticsBytes | 固定且在 slot 内 |
| 112 | 16 | Reserved | 必须为 0 |

PublishToken 编码为 `(ImageSequence << 1) | SlotIndex`，SlotIndex 仅为 0/1，token 0 表示尚未发布。
writer 只能写当前 published slot 的另一槽：以 Release 把 Generation 变为下一 odd，写完整 header/payload，
以 Release 写下一 even Generation，再以 Release 写 PublishToken。任何失败都不发布 token。

reader 以 Acquire 读取 token 和对应 even Generation，复制到自己的预分配 staging，再以 Acquire 重读
Generation 与 token。四者完全相同才接受；首次失败只允许针对最新 token 再尝试一次，第二次失败返回
`Contended` 并保留上一完整 image。reader 不持有共享槽引用，不等待 writer，也不发布部分数据。

producer crash 留下 odd Generation 时 reader 拒绝该槽。sequence gap、drop/reject counter 饱和、slot
争用和 reader stale 都必须可观测。Atomics 之外的共享字段不得并发原地更新；lease 变化必须先进入
Fallback、撤销旧 lease、完成双方停写并关闭旧映射，再创建、清零、校验并传递含新 LeaseId 的映射。

## 6. I/O value 与质量语义

- local handle 为连续 `u32`，构建期绑定到唯一 TagId、direction、type、byte offset、bit offset、
  byte order、bit order、source identity、Update Group 和 protection level。
- 物理地址字符串、ESI path、Modbus 地址或 CAN/LIN signal 名称不得进入周期映像；它们只存在于
  构建期 Device Mapping 和签名配置。
- value metadata 由 LayoutDigest 固定。构建期固定的 batch/sub-batch 携带 source identity、sequence、
  source/publish monotonic timestamp、UTC/TimeQuality 和 group statistics；每个 value 仍携带紧凑
  `Quality`、`GapReason` 与 update marker。保留旧值时必须显式标为 Stale/Bad，不得仅靠 batch Good
  掩盖未更新 value。
- `Quality` 固定为 `Good/Uncertain/Bad/Stale`。缺失、过龄、CRC/WKC/error frame、断连、bus-off、
  queue overflow 或未知设备状态不得报告 Good。
- `GapReason` 固定为 `None/NotSampled/Timeout/Checksum/WorkingCounter/LinkDown/DeviceFault/QueueFull/
  SequenceGap/ConfigurationChanged/BackendUnavailable`。
- output 命令必须携带 expected lease/configuration/layout 和有效期。过期或状态不确定的危险写拒绝，
  不以 retry 猜测设备是否执行。
- 构建期为每个操作分类：`IdempotentSet`、`NonIdempotent`、`PulseOrEdge` 或 `ReadPoll`。只有有证明的
  absolute idempotent set 可以在当前周期预算内重试；非幂等与 edge 写不得自动重试，超时返回
  `OutcomeUnknown`。重连、新 generation、新 lease 或 backend 切换后禁止重放任何旧写。

## 7. I/O Update Group

每个 group 在激活前固定 `GroupHandle`、period、phase、input sample window、output refresh window、
maximum operations、frame/request capacity、queue capacity、timeout/retry budget 和 stale threshold。

- 所有时间来自 Guardian monotonic clock；UTC 跳变不改变计划。
- release 使用绝对单调时间网格；错过 release 时记录 miss 并跳到下一合法 release，禁止 catch-up burst。
  retry 不得跨出当前 group budget；旧 generation 的迟到响应丢弃，历史 output 不补发。
- 同一 interface 上 group 以构建期 `phase, priority, GroupHandle` 稳定排序；同一输入计划和 clock trace
  产生相同发布顺序。
- 慢 group、mailbox、TCP reconnect、RTU turnaround、CAN bus-off recovery 或 LIN schedule recovery
  不得阻塞其他健康 group 或 Control Engine。
- 超预算、miss、timeout、queue full 和 stale 使用饱和计数与 high-water mark；没有无限 retry、无界
  backoff 或静默覆盖。
- input latest-image 可以 DropNewest 并记录 gap；output command queue 满时必须 RejectNewest，不能覆盖
  尚未确认的危险输出。

## 8. Driver Adapter Preview 1.0

Adapter 是 Aurora 语义，不是 EtherCrab/IgH API 的联合类型。后端原生 handle、pointer、future、C struct
或错误码不得进入公开契约、共享内存和 Control Engine。

| 操作 | 上下文 | 契约 |
| --- | --- | --- |
| `validate_configuration` | host/init，可分配 | 验证 identity/topology/config/layout/capability 和全部容量，不打开输出 |
| `claim` | init，可阻塞且有 deadline | 排他取得批准设备；返回确切 backend/source digest |
| `initialize` | init，可阻塞且有 deadline | 建立设备状态、映射和 Fallback；失败保持非 Running |
| `activate` | init | 只在完整健康窗口、watchdog/protection 和 active Fallback 就绪后进入 cyclic |
| `exchange` | cyclic | 一次固定 group 的有界收发；无发现、扩容、文件、DNS、日志阻塞或 mailbox 等待 |
| `mailbox_step` | non-cyclic | 每次最多处理 profile 声明数量，显式 deadline/cancel/retry |
| `enter_fallback` | Guardian-owned | 幂等切换安全输出并报告确认状态；失败升级保护状态 |
| `recover` | non-cyclic | 释放旧 session、重验身份/拓扑并要求新 lease；不重放危险写 |
| `quiesce_for_switch` | Guardian-owned | 停止提交、确认 Fallback、释放后端资源并证明 interface 无 owner |
| `release` | shutdown | 有界停止、保持 Fallback、释放设备；失败仍撤销 Control lease |

Adapter capability 至少声明 backend identity、transport、supported AL states、PDO、CoE/SDO、DC mode、
mailbox types、maximum frame/PDI/subdevice/group counts、watchdog、redundancy、ESI/SII source 和隔离模式。
配置要求与 capability 不匹配时在设备输出激活前拒绝。

设备身份默认使用 `ExactIdentity`。只有布局与行为兼容证据完整时才允许
`ApprovedRevisionRange`；`CommissionedReplacement` 必须生成新的签名配置、ConfigurationGeneration、
GuardianEpoch 与 lease。Linux 设备名、接口名或拓扑位置本身均不足以证明身份，任何不匹配保持 Fallback。

### 8.1 EtherCAT backend 规则

- `EtherCrabBackend` 是首选实现；滚动跟随官方发布线，上游没有长期 `release` branch 时使用官方 main
  上经审查的 release/tag commit。经审查源码纳入仓库并固定 upstream commit/digest，`Cargo.lock` 固定
  Rust 传递依赖；每次同步是独立审计提交，不允许运行构建自动前移。
- `IghBackend` 是受控替代实现。默认通过隔离 Driver Host 调用经批准的 Application Interface；不得
  直接使用未安装的私有 ioctl header。
- 两个后端必须通过同一 normalized configuration、ESI/topology/PDO audit 和 Adapter contract tests。
  后端差异通过 capability/diagnostic extension 表达，不改变 Guardian 状态机或 image layout。
- 系统可以同时安装多个已签名后端。工程配置声明一个 `preferred_backend` 和有序、无重复的
  `approved_fallback_backends`；每项都必须在 Target Profile 有独立证据。普通应用包不安装后端。
- 未选中的 backend 不启动、不加载内核模块且不取得设备权限。Target Agent 是选择/切换的唯一执行入口，
  只验证已安装 package 的版本、ABI 与 digest；Runtime 不联网下载或更新 backend。该共存模型可用于
  其他协议，但仅限通过相同 Adapter、黄金测试与显式 allowlist 的第一方实现，不形成动态插件系统。
- 每个 interface 同时恰有一个 backend owner；不支持同 NIC 双主站。切换使用第 8.2 节状态机，不是
  进程内 hot swap，也不能在旧 owner 未释放时尝试候选。

### 8.2 Backend 受控切换

```text
Requested → FallbackArmed → LeaseRevoked → OldBackendQuiesced
          → DeviceReleased → CandidateValidated → CandidateHealthChecking
          → NewEpochLeasePending → Running
```

- `Requested` 只接受人工确认或包含 operator/policy identity、reason、目标 backend、interface、到期时间
  和 nonce 的签名策略。重复 nonce、过期或目标不在 allowlist 时拒绝。
- `FallbackArmed` 必须在旧后端仍健康时确认 active Fallback 已实际写入并满足 protection；无法确认则
  保持更保守保护并拒绝切换。
- `LeaseRevoked` 后旧 output/image/heartbeat 全部失效；Control Engine 不能持有跨切换运行许可。
- `OldBackendQuiesced/DeviceReleased` 必须证明进程/线程停止、fd/master handle 关闭、NIC driver binding
  符合候选要求且不存在 owner。超时保持 Fallback，不并行启动候选。
- `CandidateValidated` 重新校验签名/source digest、kernel/Secure Boot、capability、identity/topology、
  ESI/SII/PDO、capacity 和 Fallback；不得沿用旧后端的性能或健康证明。
- `CandidateHealthChecking` 只允许 Fallback 输出，覆盖 Target Profile 固定周期数与 WKC/AL/DC 门槛。
- 成功创建新 GuardianEpoch 和 LeaseId，Control/driver/image sequence 从新代开始。失败停止候选、释放
  设备并保持 Fallback；回到旧后端也是一次新的完整切换，不是恢复旧 lease。
- Preview 1.0 禁止由 jitter、单次 timeout 或 backend fault 自动选择下一后端。以后若引入自动策略，
  必须新增 ADR、滞回/频率上限和危险输出授权，不能作为实现细节开启。

## 9. Fallback、watchdog 与保护等级

每个 output 精确声明一个最低保护等级：`GuardianProtected`、`DeviceWatchdogProtected` 或
`ExternalSafetyProtected`。独立安全系统始终具有最高优先级，Aurora 不得绕过它。

- active/pending Fallback 各有版本和 digest。pending 只在设备能力、映射、输出范围、watchdog 和健康
  窗口通过后原子成为 active；失败保留上一 active 或保持更保守输出。
- 每个 output 构建期恰好绑定一个 `FallbackDomain`；同一 domain 的输出原子切换。局部设备故障只影响
  相关 domain，全局 lease/Control 丢失影响全部 domain；具有跨协议依赖的输出必须置于同一 domain。
  FallbackDomain 是运行故障隔离单位，不是功能安全分区。
- Fallback action 只允许类型化且有界的 `SetFixed`、`HoldLastThenFixed` 和有设备证据的
  `DeviceWatchdogPreset`。禁止脚本、循环、任意状态机、ramp、ST 或 Workflow；危险输出不得用
  HoldLast 绕过外部保护。
- Control Fault/timeout 由 Guardian 应用 active Fallback。Guardian crash 只有声明并验证设备 watchdog
  或外部保护的 output 才允许激活；无第二层保护的危险组合在部署前拒绝。
- `Fallback` 是运行保护，不是 SIL/PL 保证。FSoE、PROFIsafe、CIP Safety、OPC UA Safety 均不支持。

## 10. 错误目录

| Code | 含义 | 必要结果 |
| --- | --- | --- |
| IO0001 | UnsupportedContractVersion | 激活前拒绝 |
| IO0002 | UnknownRequiredCapability | 激活前拒绝 |
| IO0003 | InvalidRegionLayout | 拒绝映射 |
| IO0004 | AtomicRequirementUnavailable | 拒绝映射 |
| IO0005 | IdentityOrTopologyMismatch | 保持 Fallback |
| IO0006 | ConfigurationDigestMismatch | 保持 Fallback |
| IO0007 | CapacityOrBudgetExceeded | 激活前拒绝 |
| IO0008 | DeviceAlreadyOwned | 不抢占既有 owner |
| IO1001 | StaleOrForeignLease | 拒绝 output，计数 |
| IO1002 | ImageSequenceViolation | 拒绝 output，计数 |
| IO1003 | ImageContended | 保留上一完整 image |
| IO1004 | ImageExpired | 进入声明的 Fallback/quality |
| IO2001 | UpdateGroupMiss | group Degraded，按阈值升级 |
| IO2002 | ProtocolTimeout | quality 非 Good，执行有界恢复 |
| IO2003 | ProtocolIntegrityFailure | 拒绝 frame/image，记录原因 |
| IO2004 | LinkOrDeviceLost | Fallback 并要求重新验证 |
| IO2005 | QueueFull | 按方向 DropNewest/RejectNewest |
| IO2006 | BackendUnavailable | 保持 Fallback，不伪造成功 |
| IO3001 | FallbackNotArmed | 禁止 Running |
| IO3002 | WatchdogProtectionUnavailable | 禁止相关 output 激活 |
| IO3003 | RecoveryAuthorizationRequired | 保持 Fallback |

错误携带 code、GuardianEpoch、可用时的 LeaseId/GroupHandle/device/backend identity、monotonic timestamp
和饱和计数。周期路径只写固定记录，不格式化字符串或阻塞日志。

## 11. 安全、测试与不包含范围

- 外部配置、ESI/SII、帧、长度、offset、count、timeout 和算术全部在使用前校验；解析预算来自
  Target Profile。畸形输入不能触发扩容、无限 retry 或部分激活。
- 新依赖、`unsafe`、C FFI、raw socket capability、io_uring/XDP、内核模块或设备权限在实现前单独
  审批。本规格不构成批准。
- R3-00 不添加 EtherCrab/IgH 依赖，不创建产品 driver crate；依赖与 backend 实现在后续编号工作项
  按各自审批和验证门槛交付。
- R3-02 必须提供 region/slot 的黄金字节、offset/alignment、unknown version/flag、overflow、odd
  generation、writer crash、reader contention 和 sequence gap 测试。
- R3-05 必须用固定 seed/virtual clock 对所有 Adapter 操作和 failure mapping 建立 backend-neutral tests。
- ESI/DBC/LDF 等原始描述在 Studio/CLI/CI 侧作为不可信输入按固定 bytes/depth/count/time 预算解析，构建
  declarative Aurora Device Description、normalized mapping 与 LayoutDigest，再进入签名 Payload；Target
  只消费构建产物，不解析原始描述。描述包只允许 schema、信号/帧/schedule、参数、图标、文档、locale、
  capability、来源/许可证与固定 migration data，禁止 DLL/`.so`、可执行文件、脚本、build plugin、网络
  下载或私有驱动。项目固定 `Vendor + DeviceId + Version + SHA-256`，内容变化必须产生新版本/digest。
- 设备扫描只允许 Studio 在明确 commissioning 授权下发起，结果只是候选配置；CAN/LIN 不得发送任意帧
  或试探输出做发现。用户确认后仍须构建、签名和部署新 generation；生产 Runtime 对未声明设备只告警，
  不自动接纳。
- 不实现第三方动态驱动、运行期协议发现、商业授权现场栈、OPC/MQTT 服务、跨平台 Runtime、
  PREEMPT_RT、RTOS、裸机、`no_std`、在线变更或功能安全认证。
