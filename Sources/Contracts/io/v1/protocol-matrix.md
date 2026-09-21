# R3 现场协议矩阵 Preview 1.0

## 1. 共同规则

本矩阵固定 R3 第一方现场协议角色。每个实例必须在 Target Profile 中声明 owner、backend、设备身份、
固定容量、帧上限、period/phase、timeout/retry、sequence、timestamp、Quality/Stale、overflow、Fallback
和 recovery。未列出的角色与上层协议不因底层 transport 存在而获得支持。

所有协议经 Driver Adapter 写入统一 I/O 映像；Control Engine 不解析帧、厂商地址、ESI、寄存器或信号
名称。网络/串口恢复运行在 Guardian 非周期路径或隔离 Driver Host，不进入 Control Engine 周期。

所有发送操作在构建期分类为 `IdempotentSet`、`NonIdempotent`、`PulseOrEdge` 或 `ReadPoll`。只有具备
协议与设备证明的 absolute `IdempotentSet` 可在本次预算内 retry；其余写 timeout 后为
`OutcomeUnknown`，不得在 reconnect、新 generation/lease 或 backend 切换后重放。

## 2. 固定角色和帧边界

| Protocol | R3 role | Frame/data boundary | Overflow/retry | 明确排除 |
| --- | --- | --- | --- | --- |
| EtherCAT | MainDevice | Ethernet MTU、PDU/frame 数、PDI bytes、SubDevice/group 数均由 profile 固定；不批准 jumbo 就只接受实测标准 MTU | cyclic 不无限 retry；WKC/AL/DC mismatch 可见并按阈值 Fallback | SubDevice、FSoE、未验证 FoE/SoE/EoE、商业主站 |
| Modbus TCP | Client | MBAP 7 bytes；PDU `<=253` bytes；ADU `<=260` bytes；固定 connection/in-flight/request table | Transaction ID + connection generation；有界 retry/backoff；不重放结果不确定的危险写 | Server、Modbus Security/TLS gateway、互联网暴露、云连接器 |
| RS-485/RS-232 | 排他有界 transport | baud/parity/data bits/stop bits/flow control、direction、turnaround 和 buffer 固定；R3 application payload 仅 Modbus RTU | short read/write、framing/overrun/break 穷举；拔插后重验稳定设备身份 | 动态发现、任意脚本协议、把物理层当应用协议 |
| Modbus RTU | Master | ADU `<=256` bytes，address/function/length/CRC 严格校验，帧间隔与 turnaround 使用 profile 时间 | 每 station 有界 timeout/retry；迟到响应不得匹配新 request；危险写不盲目重放 | Slave、Modbus ASCII、广播写除非 profile 明确批准 |
| CAN 2.0 | SocketCAN raw | standard/extended ID；classic payload `0..=8` bytes；filter 与 RX/TX queue 固定 | error frame、error-active/passive、bus-off 和 queue full 可观测；恢复次数/时间有上限 | CANopen、J1939、DeviceNet、UDS、ISO-TP |
| CAN FD | SocketCAN raw | payload `0..=64` bytes；DLC、BRS/ESI flag、nominal/data bitrate 固定 | 与 CAN 2.0 相同，额外验证 controller/transceiver FD capability | 未声明的 non-ISO mode、任何上层协议 |
| LIN | Controller + fixed schedule | 6-bit frame identifier/PID、`0..=8` data bytes、classic/enhanced checksum、publisher/subscriber 和 slot 固定 | 无响应/checksum/schedule miss 可见；仅通过获批 BUSMUST BMAPI 或 TOSUN libTSCAN/tsdev Driver Host | LIN SubDevice 仿真、用普通 UART 猜测 break/schedule、动态 schedule |

Modbus 长度来自 Modbus Application Protocol V1.1b3/Serial Line V1.02 的协议上限；CAN/CAN FD 帧大小
来自 Linux SocketCAN ABI。协议标准允许不等于项目允许，功能码、地址、ID、flag 和写方向仍须 allowlist。

## 3. EtherCAT MainDevice

### 3.1 Aurora normalized configuration

后端共同消费以下已签名事实，不从运行网络猜测工程意图：

- interface identity：ifindex 仅作本次启动引用；稳定身份使用 MAC、PCI path、vendor/device ID、driver、
  firmware 和配置 digest；任何变化重新准入。
- 有序 topology：alias/position、VendorId、ProductCode、Revision、Serial（工程要求时）、SII digest、
  ESI digest、port/topology constraints。
- PDO layout：SyncManager、direction、index/subindex、bit length、logical offset、byte/bit order、local handle
  与完整 LayoutDigest。
- state policy：INIT/PREOP/SAFEOP/OP 允许转移、目标 state、AL status allowlist、transition timeout。
- cyclic plan：group、period/phase、expected WKC、frame/PDU/PDI capacity、DC policy 和 mailbox budget。

发现数量、顺序、identity、PDO、ESI/SII 或 capability 不一致时不得进入 OP。ESI 是 host-side 工程输入；
即使后端能从 SII 自动配置，也必须与签名 ESI/映射审计结果一致。

### 3.2 EtherCrab backend

- 当前选择 EtherCrab，原因是纯 Rust 用户态 MainDevice、固定 PDU storage、无需 out-of-tree kernel module，
  更适合普通 Ubuntu/Secure Boot 持续更新路线。它不是功能安全或 EtherCAT 产品认证声明。
- Aurora 不硬编码其 `0.x` 公共类型。Adapter 负责 state、PDI、WKC/DC、错误和 identity 转换；业务模块
  不得直接依赖 EtherCrab。
- 上游策略为滚动跟随官方发布线；上游没有长期 `release` branch 时，以官方 main 上经审查的 release/tag
  commit 为同步点。经审查源码进入 Aurora 仓库并固定准确 commit/digest；官方变更不会自动进入同一
  Aurora commit，显式同步必须重新生成 SBOM 并执行仿真/硬件回归。
- EtherCrab 的 SII、CoE/SDO、DC 和 io_uring/XDP capability 分别协商。ESI XML、复杂拓扑、32-bit DC、
  non-DC 混合拓扑和尾延迟不能凭库说明认定，必须由 R3-06 测试。
- raw socket capability、NIC 独占、io_uring opcode/kernel 支持和 memory locking 在激活前检查。XDP 默认
  不启用；只有独立批准和同等故障/更新验证后才能成为 Target Profile 选项。

### 3.3 IgH backend boundary

- Adapter 兼容 IgH stable Application Interface 的 configuration/activate/cyclic/domain/DC/watchdog 语义。
- 初始化期阻塞配置与激活后 cyclic RT-safe API 必须分离。normalized exchange 顺序映射为 receive、
  domain process/state、读取输入、写输出、domain queue、send；DC application time 位于固定周期点。
- IgH C pointer/ioctl/errno 不穿过 Adapter。默认隔离 Driver Host 拥有 master device；Guardian 只接收
  normalized image/diagnostics 和 host heartbeat。
- 启用前单独审批 C FFI/`unsafe`、GPL-2.0/LGPL-2.1 组件边界、DKMS/MOK、kernel/NIC module、Secure Boot、
  当前/更新/回退内核矩阵。不得直接使用非公开 ioctl 结构模拟 Application Interface。
- EtherCrab 与 IgH 的性能、DC、WKC 和恢复证据互不继承。可以按 Guardian Contract 受控切换，但
  同一 interface 任意时刻只有一个 owner，禁止自动热切换和双主站。

## 4. Modbus

### 4.1 TCP Client

- connection identity 为目标地址、port、Unit ID、配置 generation 和 allowlisted function set；DNS 不进入
  周期路径，生产 profile 应使用隔离 OT 网络和预解析/静态 endpoint。
- request identity 为 connection generation + Transaction ID。重连增加 generation；旧连接迟到响应、
  重复 ID、错误 Unit ID/function/length 或异常响应不能完成新 request。
- TCP 分片和粘连按 MBAP length 解析；在完整 ADU 前不发布。length 小于合法 PDU 或大于 260 立即拒绝，
  不继续累积无界 buffer。
- read/poll 可按固定策略 retry；写在发送后断连属于 outcome unknown，除非命令具备协议外幂等证明，
  否则不自动重放并将 output 标记 Uncertain/Bad。

### 4.2 Serial 与 RTU Master

- Linux 设备使用稳定 udev identity 与排他打开；`/dev/ttyUSBn` 名称本身不是身份。配置变化、拔插或
  driver generation 变化要求重新初始化。
- RS-485 direction control、pre/post delay、turnaround 和 silent interval 由 profile 用明确纳秒给出，
  不依赖隐式库默认。RS-232 不伪造半双工 direction。
- RTU 只接受 allowlisted station/function/address/quantity。CRC、长度、异常响应和 timing 任一失败均不
  发布 Good；迟到帧在 request generation 改变后丢弃并计数。
- 一台慢/坏 station 的 retry budget 用尽后让该 station Degraded/Bad，释放总线给固定计划的其他 station。

## 5. CAN/CAN FD 与 LIN

- CAN/CAN FD 的公开 backend 固定为 Linux SocketCAN，不为 BUSMUST/TOSUN 另建厂商 CAN API。首批硬件
  厂商范围只允许 BUSMUST 与 TOSUN；平台验证 BMAPI、libTSCAN/tsdev 的 SDK 契约而不锁定具体型号，
  但每份实际 Target Profile/报告仍必须记录精确型号、VID/PID、序列号、固件和 channel capability。
- SocketCAN interface identity 包含 stable device path、driver/firmware、controller clock、bit timing、
  transceiver capability 和 termination evidence；interface up 不等于工程健康。硬件不能提供经过验证的
  SocketCAN 时，不得以厂商 CAN API 绕过本版边界。
- kernel filter、RX/TX socket buffer 和应用 queue 都有明确容量。kernel drop counter、error frame、bus-off
  和 restart 必须映射到 diagnostics；自动 restart 只有次数和时间都在 profile 内时允许。
- CAN RTR 默认拒绝；仅在 profile/function allowlist 明确批准时接受。CAN FD non-ISO 默认拒绝。
- LIN 使用隔离 Driver Host 调用 BUSMUST BMAPI 或 TOSUN libTSCAN/tsdev；厂商类型、handle、线程与错误码
  不越过 Adapter。SDK 必须验证 ABI、blocking/threading、错误、reconnect 和 version/digest；组合设备若
  不能安全地由 SocketCAN 与厂商 LIN SDK 同时拥有，必须按 Target Profile 选择单一 owner 并拒绝冲突。
- LIN adapter 必须原生提供可测 break/sync/PID/checksum/schedule 能力。普通 UART 加 sleep 的实现不能
  通过 capability。schedule 只能来自签名静态表，运行期不能插入任意 frame。R3 可在 codec、schedule、
  simulator 和拒绝路径完成后关闭条件能力门禁；没有获批硬件时 Target 不发布真实 LIN capability，后续
  启用必须补齐 R3-09/R3-11 对应硬件证据。

## 6. Device Description 与配置生成

- ESI/DBC/LDF 是 Studio/CLI/CI 侧的不可信工程输入。统一 importer 以固定资源预算生成严格声明式 Aurora
  Device Description、normalized mapping/schedule 与 LayoutDigest；Runtime 不解析原始描述。
- 项目固定 `Vendor + DeviceId + Version + SHA-256`。原始文件保留来源证据，新内容必须新 version/digest；
  Device Description 与 Runtime Driver Package 独立版本化。
- Studio 的标准流程是导入/安装描述、在 Device Tree 添加设备或 LIN cluster、配置 mapping/schedule、
  调用与 CLI/CI 相同 builder、签名并部署。新 LDF 通常不更新 firmware/driver，只产生新配置 generation。
- 描述包不得携带 DLL/`.so`、可执行文件、脚本、build plugin、网络下载或私有 driver；厂商运行代码只能
  进入独立 Driver Package 与 R4 系统维护流程。
- commissioning 扫描只返回候选拓扑，不改变 Running 配置。用户确认后必须重新构建、签名并部署；
  CAN/LIN 禁止通过任意发帧、试探输出或暴力枚举发现设备。

## 7. 信息互联与安全边界

- R3 image 为后续 R5/E0 提供稳定 TagId、source identity、sequence、source/server timestamp、
  TimeQuality、Quality/Stale 和 gap。它不运行 OPC UA、MQTT 5 与 Sparkplug B。
- OPC 只指未来 OPC UA，不支持 OPC Classic/DCOM；任何 subscriber/command ingress 只能调用后续受审计
  command service，不能直写 Guardian image。
- PROFINET、EtherNet/IP/CIP、CC-Link、PROFIBUS 及需要商业运行时授权或强制产品认证的协议栈不进入
  R3。FSoE、PROFIsafe、CIP Safety、OPC UA Safety 和全部功能安全声明明确排除。
