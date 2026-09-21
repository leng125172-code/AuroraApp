# SPEC-R3-002：I/O Target Profile 与证据模板 Preview 1.0

## 1. 目的

I/O Target Profile 为具体工程和目标硬件冻结 Guardian、共享映像、Driver Adapter 与现场协议的全部
容量、时间、身份、内核和恢复预算。平台没有默认最小周期、最大 jitter、允许 miss 或设备数量；缺失、
为零、不可表示、无法证明或超预算的必填值在设备打开/输出激活前拒绝。

首个测试机系列为 Dell Precision 7920 Tower。`最高配置` 不是可复现身份；正式报告必须填写实际
CPU SKU/socket/core/thread、BIOS、RAM、NIC/adapter PCI ID、firmware、Ubuntu image、kernel 和调优配置。

## 2. 文档身份与兼容

模板版本为 `major=1, minor=0, lifecycle=preview`，规范扩展键为 `aurora.io-guardian`。R3-01/R3-02
实现 Schema 时必须保持本规格字段和拒绝语义；R3-00 不手工伪造运行 Schema 或默认值。

每份 profile 包含：DocumentId、Target Profile digest、工程/Payload digest、Guardian Contract range、
Guardian build digest、backend/source digest、ConfigurationDigest、LayoutDigest、CapabilityDigest、创建时间
和审批身份。任何 digest 或 contract range 不匹配都不能复用报告。

## 3. 必填硬件与软件清单

| Section | 必填字段 |
| --- | --- |
| machine | manufacturer、model、service-class asset id（不得记录个人信息）、BIOS/UEFI version、Secure Boot state、TPM state |
| cpu | exact model、socket/core/thread count、microcode、NUMA topology、SMT、turbo、governor、C-state policy |
| memory | total bytes、DIMM topology、speed、NUMA placement、locked-memory limit、hugepage policy |
| os | Ubuntu release/image digest、Ubuntu Pro state、kernel release/build/config digest、security patch level、libc、systemd |
| boot | 完整 kernel cmdline、isolated CPU set、`nohz_full`/`rcu_nocbs`（使用时）、IOMMU、mitigation policy；不得只写“已调优” |
| scheduling | Guardian/Driver Host policy/priority、CPU affinity、cpuset/cgroup quota、IRQ affinity、memory/NUMA policy |
| NIC | interface stable identity、PCI path/vendor/device/subsystem ID、driver/version、firmware、MAC、MTU、queue/RSS、offload、IRQ mapping、link speed |
| serial | adapter stable identity、USB/PCI ID、driver/firmware、port electrical mode、udev rule、permission owner |
| CAN/LIN | adapter/controller/transceiver identity、driver/firmware、clock、termination、电气速率、LIN timing capability |
| EtherCAT | installed backends、preferred/approved fallback list、各自 source commit/digest/features、raw socket 或 IgH kernel capability、ESI/SII/topology/PDO digest |

未知、自动或继承值必须展开成实际值。Secret、私钥、认证 token、个人身份和可远程利用的设备凭据不得
写入 profile、报告、命令行或日志。

设备身份默认记录 `ExactIdentity`；只有布局和行为兼容证据已批准时才允许
`ApprovedRevisionRange`。现场替换设备使用 `CommissionedReplacement`，生成新签名配置与 generation，
不得依靠 `/dev` 名称、接口名或拓扑位置自动接受。每个 DriverInstance 还必须记录 static/isolated 执行
模式、服务身份、设备白名单、Linux capabilities、namespace/seccomp/systemd 限制和预期 peer identity；
沙箱无法完整应用时不允许激活。

## 4. 固定资源预算

以下字段均为非零整数并使用字段名所示单位；上限的上限由实际 Rust/ABI 表示和 checked layout 同时
约束。R3-01/R3-02 可以增加更低工程上限，不能猜测默认值。

### 4.1 Guardian 与共享映像

- `maximum_guardian_memory_bytes`、`maximum_driver_host_memory_bytes`；
- `input_payload_capacity_bytes`、`output_payload_capacity_bytes`、`maximum_values`；
- `maximum_input_groups`、`maximum_output_groups`、`maximum_drivers`、`maximum_devices`；
- `diagnostic_ring_capacity`、`fallback_image_bytes`、`maximum_fallback_versions=2`；
- `heartbeat_interval_ns`、`heartbeat_timeout_ns`、`maximum_input_age_ns`、`maximum_output_age_ns`；
- `maximum_latch_attempts=2`；Preview 1.0 不允许 profile 放大该值。
- UDS 控制消息最大 bytes/频率、每 lease 独立 `memfd` 大小、fd 数与 Driver Host instance 数；周期数据不得
  退化到 UDS，映射必须封闭 grow/shrink 并校验 `SO_PEERCRED`。

### 4.2 Update Group

每组必须给出 `period_ns`、`phase_ns`、`priority`、`input_sample_window_ns`、
`output_refresh_window_ns`、`maximum_operations_per_release`、`frame_or_request_capacity`、
`queue_capacity`、`timeout_ns`、`maximum_retries`、`retry_backoff_ns`、`stale_after_ns`、
`maximum_recovery_attempts` 和 `recovery_window_ns`。

每组必须声明绝对单调 release grid、miss/window/consecutive-miss 判定、batch/sub-batch metadata layout，
并为每个写操作声明 `IdempotentSet`、`NonIdempotent` 或 `PulseOrEdge`，读轮询声明 `ReadPoll`。只有具备
证明的 `IdempotentSet` 可以在当前预算内 retry；其他写在 timeout 后进入 `OutcomeUnknown`，且不跨
generation/lease/backend 重放。

### 4.3 EtherCAT

- interface、`preferred_backend=ethercrab`、有序 `approved_fallback_backends`、每后端 source
  commit/digest、feature set、安装/签名/隔离模式；
- maximum SubDevices/groups/PDI bytes/PDU data bytes/frames in flight/mailbox requests；
- expected WKC per datagram/group、maximum consecutive/rolling WKC misses；
- AL state/transition timeout、DC mode/reference/Sync0/Sync1 cycle and shift、maximum DC drift ns；
- Ethernet MTU、VLAN policy、ESI/SII/topology/PDO digest 和 mailbox allowlist；
- cable/device loss、backend process loss和恢复 deadline，以及 device watchdog/protection evidence；
- backend switch authorization、Fallback confirmation、quiesce/release deadline、candidate health window、
  maximum manual switches per maintenance window；Preview 1.0 不允许自动切换策略。
- 每个已安装 backend package 的 package/source digest、ABI、启用所需权限与内核模块；未选择 backend
  必须 disabled、无设备权限且不自动加载模块。Runtime 不允许在线下载，选择与切换只由 Target Agent
  执行签名 desired change。

### 4.4 Modbus/serial/CAN/LIN

- Modbus TCP maximum connections/in-flight requests/poll entries、connect/request timeout、retry/backoff、
  Unit ID/function/address allowlist；
- serial maximum ports/stations、baud/parity/data/stop/flow、RS-485 direction/turnaround/silent interval、
  Modbus RTU ADU capacity 与 CRC policy；
- CAN/CAN FD maximum interfaces/filters/RX/TX frames、ID/flag allowlist、bit/data rate、socket/application
  queue、bus-off restart count/window；公开 backend 固定为 Linux SocketCAN，首批硬件厂商范围只允许
  BUSMUST 与 TOSUN，实际 profile 仍记录精确型号、VID/PID、序列号、固件和 channel capability；
- LIN maximum schedules/slots、break/sync/PID/checksum/version、slot period、response timeout、publisher/
  subscriber、BUSMUST BMAPI 或 TOSUN libTSCAN/tsdev SDK ABI/digest/threading/blocking/reconnect 证据和
  approved adapter timing evidence。平台不锁定具体型号，但缺少 LIN/timestamp/schedule 能力即拒绝。

## 5. Fallback 与恢复模板

每个 output 必须记录 owner、normal writer、value/range、active/pending Fallback、保护等级、设备 watchdog
能力与 deadline、外部安全保护引用、恢复授权和人工确认要求。缺少第二层保护的危险 output 不允许依赖
Guardian crash 后继续维持值。

每个 output 还必须绑定构建期固定 `FallbackDomain` 与以下一种有界 action：`SetFixed`、
`HoldLastThenFixed` 或具备证据的 `DeviceWatchdogPreset`。跨协议联动输出必须同属一个 domain；脚本、ramp、
ST/Workflow 或任意状态机不得成为 Fallback。危险输出不得以 hold-last 绕过外部安全系统。

故障矩阵至少包含 Control/Guardian/Driver Host kill、lease/heartbeat timeout、stale output、queue full、
NIC link/cable loss、SubDevice power loss、WKC/AL/DC fault、TCP reset/half-open、RTU timeout/CRC/framing、
serial unplug/replug、CAN error-passive/bus-off、LIN timeout/checksum 和系统重启。每项定义检测上限、输出
结果、quality/gap、资源释放、重验条件、最大恢复次数和禁止自动恢复的危险状态。

普通 domain 可在固定 attempts/window/backoff 内自动恢复；耗尽后为 `RecoveryLocked`。危险 domain 只可
自动恢复通信与只读健康，恢复输出必须另有本地/签名授权和独立安全许可。backend、device、topology、
mapping/layout、group budget、retry/stale/recovery、Fallback/protection/source/capability 变化都必须创建
新 ConfigurationGeneration，禁止在 Running 中热改。

## 6. 测量计划与阶段责任

每次报告必须记录唯一 run id、源码 commit、Payload/配置/layout/依赖摘要、完整硬件软件清单、测试
拓扑、设备固件、周期计划、容量、warm-up、测量时长、样本数、时钟来源和 raw artifact digest。

R3 负责实现采集、指标、故障注入、报告 schema，并完成足以证明契约和驱动行为正确的短时开发/硬件
验证；R3 Gate 不以长时间整机调优结果冒充实现完成。最终核心阶段 I0-08 在 Studio、部署和实际工程
闭合后执行正式准入：baseline 与 tuned 各 3 次独立冷启动，每次同时满足不少于 30 分钟和 100 万周期；
tuned 正式配置另做不少于 8 小时 soak。可自动注入的故障每项至少 100 次，拔线、断电、设备替换等
人工硬件故障每项至少 10 次。上述数字限定证据量，不替代项目自己的 timing/miss/age/recovery 预算。

### 6.1 场景

1. baseline：发行版默认非 RT kernel，除运行所需权限外不隐藏调优；
2. tuned：记录每项 kernel cmdline、CPU/IRQ affinity、governor/C-state、NIC queue/offload、memory lock 和
   service isolation 变化；不得只写“启动参数优化”；
3. CPU、memory、storage 和 other-NIC 分别及组合压力；
4. diagnostics consumer stall、mailbox load、queue saturation 和最大批准拓扑；
5. 所有协议故障、Control/Guardian/Driver Host crash 和重复恢复；
6. 当前 security kernel、下一批准 security kernel 和旧 kernel rollback，Secure Boot 始终开启。

### 6.2 指标

- requested/actual group period、input age、output latency；
- jitter p50、p99.9、max（ns）和完整分位计算方法；
- deadline miss count/rate、连续/窗口最大值；
- EtherCAT WKC mismatch、AL/DC fault、timeout、DC drift；
- Modbus timeout/exception/late response、serial CRC/framing、CAN error/bus-off、LIN miss/checksum；
- Guardian/Driver Host CPU、RSS、page fault、context switch；
- 每个 queue/ring 当前值、high-water mark、drop/reject/sequence gap；
- Fallback detection/effective time 和恢复健康窗口。

报告同时给出预算、实测值和 pass/fail。平均值不能替代 p99.9/max；丢失样本、计数饱和或时间质量
非 Good 必须使对应结论 invalid/incomplete，而不是补造结果。任何数据撕裂、旧代输出重放、未观测
丢失或越权访问直接失败，不能由平均值或低 miss rate 抵消。

仓库只保存测试规范、报告 schema、小型黄金样本和可审阅摘要；周期级原始数据、长时间 trace 与 soak
日志进入构建/CI 制品存储。摘要必须记录原始制品 SHA-256、工具版本、Git commit、硬件身份、kernel/
cmdline、driver/SDK、Target Profile digest 和可访问位置。原始制品缺失或 digest 无法验证时，不得作为
正式 Gate 证据。

## 7. Precision 7920 与 EtherCAT 选型记录

用户提供的初步观测仅记录为下表，不进入 Gate：

| Backend | Preliminary observation | 证据状态 |
| --- | --- | --- |
| IgH | 最大约 `40 μs` | 未记录具体 CPU/NIC/kernel/topology/load/sample count；仅比较输入 |
| EtherCrab | 通常约 `50 μs`，多数 `50～80 μs`，最大约 `200 μs` | 同上；尾部需定位，不得以通常值替代 max |

R3 将 EtherCrab 作为 preferred backend，但这只是维护与集成决策，不是性能已经达标的声明。
Precision 7920 baseline/tuned 报告必须重新测量并记录准确单位与指标定义；若 max、miss、WKC/DC 或
恢复超过工程预算，必须拒绝激活或调整工程预算/配置，不能声称“非 RT 核调优后应该没问题”。IgH 是
approved fallback backend，必须在同一硬件、拓扑、工程和负载矩阵上独立通过门槛；两个后端不得共享
性能、健康或恢复合格证明。

## 8. 依赖与更新证据

- 每个 backend/driver 记录 source URL、commit、digest、许可证、feature、直接/传递依赖、SBOM、advisory、
  维护状态、替代方案和撤回条件。
- EtherCrab 跟随官方发布线；上游没有长期 `release` branch 时，使用官方 main 上经审查的 release/tag
  commit。经审查源码纳入 Aurora 仓库并记录上游 URL、准确 commit、SHA-256、许可证与同步日期；每个
  Aurora commit/Payload 都精确确定，`Cargo.lock` 继续固定完整 Rust 传递依赖图；禁止 build-time
  floating update 或 Runtime 在线拉取。
- 同步前后执行 API adapter compile、contract simulation、golden layout、fault injection、目标硬件和
  current/new/rollback kernel 矩阵。失败保持上一已批准 commit；不修改历史或静默降级。
- IgH 若安装，额外记录 kernel module/source、Application Interface、GPL/LGPL 审查、DKMS/MOK、module
  signing、Secure Boot enrollment、kernel ABI 和 uninstall/rollback。
- BMAPI、libTSCAN/tsdev 等厂商 SDK 是槽外系统依赖；Aurora 仓库只保存获批 FFI/Adapter、测试和许可证
  元数据，不保存无再分发许可的 `.so`。R4 系统维护流程验证 SDK version/ABI/SHA-256；未来获得书面再
  分发许可后也只能作为系统包交付，Runtime 仍不下载或更新。

## 9. Device Description 与 commissioning

- Studio 导入 ESI/DBC/LDF 后，把原始文件安装为版本化 Aurora Device Description；项目固定
  `Vendor + DeviceId + Version + SHA-256`，不同内容不得静默替换同版本。
- Studio、CLI 与 CI 调用同一个确定性 importer/builder，输出 normalized device/mapping/schedule、
  LayoutDigest 和签名 Payload。Target/Runtime 只消费构建产物，不解析原始 ESI/DBC/LDF。
- 描述包严格声明式，只能包含 schema、参数/通道/信号/帧/schedule、图标、文档、locale、capability、
  来源/许可证与固定 migration data；禁止代码、脚本、DLL/`.so`、插件、网络下载或私有 driver。
- 新 LDF 通常只要求重建并部署配置，不更新固件或 driver。只有新 SDK/ABI/backend 才走系统 driver
  更新；adapter firmware 仅在能力、缺陷或厂商要求有证据时更新。
- Studio 扫描只在 commissioning 授权下生成候选拓扑。EtherCAT 可读取只读身份；CAN/LIN 只使用厂商
  明确证明无危险动作的发现 API，禁止发送任意帧、试探输出或暴力枚举。候选必须经用户确认、构建、
  签名和新 generation 部署；生产 Runtime 对未声明设备只告警。

## 10. 激活拒绝条件

任一项成立即拒绝 Running：身份/拓扑/layout/config digest 不匹配；unknown required capability；容量或
checked layout 超限；非 lock-free/aligned atomics；Fallback 未 armed；watchdog/外部保护不足；过期 lease；
unsupported kernel/NIC/adapter/firmware；未批准 dependency/source digest；缺少当前阶段要求的验证或
更新回退证据；性能/恢复超过工程预算；报告样本缺失或质量无效。I0-08 正式准入完成前只能标记为
Engineering Preview，不得宣称已通过最终整机性能与稳定性验收。

后端二进制由独立签名的系统维护包安装/更新，普通 Runtime 包只选择已安装且已获 Target Profile
批准的 capability。选择或切换后端不改变 Control Engine API，但一定改变 GuardianEpoch、LeaseId、
backend source digest 和运行证据。

本模板不构成功能安全、硬实时、跨硬件性能、EtherCAT 一致性或商业认证声明。
