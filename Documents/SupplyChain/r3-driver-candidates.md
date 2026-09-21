# R3 Driver 与协议依赖候选评审

- 状态：R3-00 reviewed；未批准加入产品
- 评审日期：2026-09-20
- 范围：EtherCAT backend、ESI parser、Modbus、serial、SocketCAN 与 LIN 实现来源

本记录只关闭 R3-00 的候选与边界选择，不修改 `Cargo.toml`/`Cargo.lock`、`deny.toml`、内核模块或系统
镜像。每项进入实现前仍须在对应工作项获得依赖、`unsafe`、FFI、权限和许可证批准。

## EtherCAT

| Candidate | 2026-09-20 审查快照 | License/分发 | 优点 | 风险与准入 |
| --- | --- | --- | --- | --- |
| EtherCrab official `main` | `ethercrab-rs/ethercrab`；审查时 main `ea2860c…`；crates.io 0.7.1；活跃维护 | MIT OR Apache-2.0 | 纯 Rust 用户态 MainDevice；固定 PDU storage；SII、CoE/SDO、DC、io_uring；无需 out-of-tree kernel module | `0.x` API；ESI XML 不完整；DC/复杂拓扑开放问题；raw socket/io_uring/unsafe 与尾延迟须审计。R3 首选，R3-06 门禁后才能批准 |
| IgH stable-1.6 | `etherlab.org/ethercat`；审查时 stable-1.6 `61cc654…` / 1.6.13；活跃维护 | kernel/master GPL-2.0；用户态/headers 含 LGPL-2.1，最终以分发文件审计为准 | 成熟 Application Interface、domain/WKC/DC/watchdog、明确 cyclic RT-safe API | C FFI/unsafe、kernel module、DKMS/MOK、NIC binding、Secure Boot/更新回退和混合许可证。作为获批替代 backend，默认隔离 Driver Host |

上游 EtherCrab 当前没有名为 `release` 的长期分支。Aurora 所称“跟随 release”是跟随官方发布线：优先
选择官方 release/tag；没有对应长期分支时，使用官方 `main` 上经审查的发布 commit。经审查源码在
R3-06 依赖审批后纳入仓库，记录 upstream URL、准确 commit、SHA-256、许可证与同步日期；每次前移是
独立 Aurora 提交。构建时浮动拉取、未审查自动更新和把 source commit 排除出 SBOM/Target Profile 均禁止。

EtherCrab 与 IgH 可以同时由系统维护包装入 Target，但普通 Runtime 包不能安装它们。同一 NIC 任意
时刻只允许一个 backend owner；切换遵守 SPEC-R3-001，不构成两者自动 failover 或证据互认。

## ESI 与配置审计

| Candidate | Snapshot | License | Decision |
| --- | --- | --- | --- |
| `quick-xml` | crates.io 0.42.0，Rust 1.86，默认 feature 为空 | MIT | host-only ESI streaming parser 首选候选；必须增加输入 bytes/depth/element/attribute/text 上限、禁用外部实体/网络解析，并在 R3-06 单独批准 |

Aurora 必须生成自己的规范 topology/PDO/configuration digest。EtherCrab 的 SII 自动配置或 IgH 的 backend
配置结果只能作为被审计输入，不能取代签名 ESI/Device Mapping。若 parser 未批准或 ESI 超预算，拒绝
构建/激活，不退化为运行期网络发现。

## 其他协议

- Modbus TCP/RTU：R3 使用第一方固定 buffer codec 和状态机，不引入通用 Modbus runtime。原因是角色、
  retry、危险写、connection generation 和质量语义必须精确符合 Guardian Contract；网络/serial syscall
  适配仍在 R3-07/R3-08 单独审批。
- Serial：使用 Linux termios/RS-485 ABI 的窄 platform adapter；不引入脚本协议框架或动态 port discovery。
- CAN/CAN FD：公开 backend 固定为 Linux SocketCAN，首批硬件厂商只允许 BUSMUST 与 TOSUN；不为厂商
  CAN API 增加第二公开 backend，也不引入 CANopen/J1939/UDS/ISO-TP runtime。syscall binding 与 unsafe
  边界在 R3-09 单独审批；实际报告仍记录精确型号、VID/PID、序列号、固件与通道能力。
- LIN：只允许 BUSMUST BMAPI 或 TOSUN libTSCAN/tsdev，经隔离 Driver Host 接入。准入验证 SDK ABI、错误、
  threading/blocking、reconnect、version/digest，而非建立平台型号 allowlist；具体 Target Profile 仍必须
  以真实设备 identity/capability 拒绝不具备 LIN timing/schedule 的型号。没有获批硬件就不发布真实 LIN
  capability，但 codec、schedule、simulator 和拒绝路径可关闭 R3 条件能力门禁。

厂商 SDK 是槽外系统依赖。Aurora 仓库保存 FFI/Adapter、测试、来源与许可证元数据，不保存没有书面
再分发许可的二进制 `.so`；R4 系统维护包验证安装版本、ABI 与 SHA-256。Runtime 不在线下载或自动更新。
将来取得再分发许可也只改变系统包来源，不改变隔离、摘要或 Target Agent 准入要求。

## Device Description 供应链

- Studio/CLI/CI 共用确定性 importer，把 ESI/DBC/LDF 转成版本化、严格声明式 Aurora Device Description
  和 normalized mapping；项目固定 `Vendor + DeviceId + Version + SHA-256`，不静默选择 latest。
- 原始描述保留来源、许可证和 digest。组织仓库中的描述必须签名；开发期可本地导入，但进入生产签名
  前必须完成来源与审批。Device Description 与 Runtime Driver Package 独立版本化。
- 描述包可包含 schema、参数/通道/信号/帧/schedule、图标、文档、locale、capability 与固定 migration
  data；禁止 executable、DLL/`.so`、脚本、build plugin、网络下载和私有 driver。图片、文本和 parser
  资源均有固定上限。

## 同步与撤回条件

- 依赖同步必须记录 old/new commit、上游 changelog、许可证/feature/传递依赖差异、SBOM、advisory、
  API adapter compile、仿真/故障注入和目标硬件报告。
- 任一来源归档、许可证不兼容、未修复高危 advisory、行为/ABI 变化、内核/Secure Boot 不兼容、目标
  硬件预算失败或无法复现时，保持上一批准 source 或撤回 capability；不得静默更新或伪造成功。
- Aurora Adapter 是唯一后端依赖边界。Control Engine、Guardian Contract、I/O image 和工程语义不得
  直接引用 EtherCrab/IgH/quick-xml 类型。
- 系统可并存多个已批准 Driver Package，但未选择 package 不启动、不加载模块且无设备权限；Target Agent
  是安装验证与切换的唯一入口。同一模式只有在实现通过相同 Adapter、黄金测试与显式 allowlist 时才可
  扩展到其他驱动，不能演变为运行期动态插件。
