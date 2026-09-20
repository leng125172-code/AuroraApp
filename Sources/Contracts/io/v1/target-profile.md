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

### 4.2 Update Group

每组必须给出 `period_ns`、`phase_ns`、`priority`、`input_sample_window_ns`、
`output_refresh_window_ns`、`maximum_operations_per_release`、`frame_or_request_capacity`、
`queue_capacity`、`timeout_ns`、`maximum_retries`、`retry_backoff_ns`、`stale_after_ns`、
`maximum_recovery_attempts` 和 `recovery_window_ns`。

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

### 4.4 Modbus/serial/CAN/LIN

- Modbus TCP maximum connections/in-flight requests/poll entries、connect/request timeout、retry/backoff、
  Unit ID/function/address allowlist；
- serial maximum ports/stations、baud/parity/data/stop/flow、RS-485 direction/turnaround/silent interval、
  Modbus RTU ADU capacity 与 CRC policy；
- CAN/CAN FD maximum interfaces/filters/RX/TX frames、ID/flag allowlist、bit/data rate、socket/application
  queue、bus-off restart count/window；
- LIN maximum schedules/slots、break/sync/PID/checksum/version、slot period、response timeout、publisher/
  subscriber 和 approved adapter timing evidence。

## 5. Fallback 与恢复模板

每个 output 必须记录 owner、normal writer、value/range、active/pending Fallback、保护等级、设备 watchdog
能力与 deadline、外部安全保护引用、恢复授权和人工确认要求。缺少第二层保护的危险 output 不允许依赖
Guardian crash 后继续维持值。

故障矩阵至少包含 Control/Guardian/Driver Host kill、lease/heartbeat timeout、stale output、queue full、
NIC link/cable loss、SubDevice power loss、WKC/AL/DC fault、TCP reset/half-open、RTU timeout/CRC/framing、
serial unplug/replug、CAN error-passive/bus-off、LIN timeout/checksum 和系统重启。每项定义检测上限、输出
结果、quality/gap、资源释放、重验条件、最大恢复次数和禁止自动恢复的危险状态。

## 6. 测量计划

每次报告必须记录唯一 run id、源码 commit、Payload/配置/layout/依赖摘要、完整硬件软件清单、测试
拓扑、设备固件、周期计划、容量、warm-up、测量时长、样本数、时钟来源和 raw artifact digest。

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
非 Good 必须使对应结论 invalid/incomplete，而不是补造结果。

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
- EtherCrab manifest 可跟踪官方 `main`，但每个可构建 Aurora commit 必须由 `Cargo.lock` 固定 source
  commit。同步使用独立提交，禁止 build-time floating update。
- 同步前后执行 API adapter compile、contract simulation、golden layout、fault injection、目标硬件和
  current/new/rollback kernel 矩阵。失败保持上一已批准 commit；不修改历史或静默降级。
- IgH 若安装，额外记录 kernel module/source、Application Interface、GPL/LGPL 审查、DKMS/MOK、module
  signing、Secure Boot enrollment、kernel ABI 和 uninstall/rollback。

## 9. 激活拒绝条件

任一项成立即拒绝 Running：身份/拓扑/layout/config digest 不匹配；unknown required capability；容量或
checked layout 超限；非 lock-free/aligned atomics；Fallback 未 armed；watchdog/外部保护不足；过期 lease；
unsupported kernel/NIC/adapter/firmware；未批准 dependency/source digest；缺少 baseline/tuned 或更新回退
证据；性能/恢复超过工程预算；报告样本缺失或质量无效。

后端二进制由独立签名的系统维护包安装/更新，普通 Runtime 包只选择已安装且已获 Target Profile
批准的 capability。选择或切换后端不改变 Control Engine API，但一定改变 GuardianEpoch、LeaseId、
backend source digest 和运行证据。

本模板不构成功能安全、硬实时、跨硬件性能、EtherCAT 一致性或商业认证声明。
