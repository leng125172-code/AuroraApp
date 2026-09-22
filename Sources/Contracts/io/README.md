# Guardian and I/O contracts

本目录保存槽外 I/O Guardian、共享映像、Driver Adapter、现场协议角色和 Target Profile 的规范源。
Control Engine、Guardian、Driver Host、构建工具和设备报告必须消费同一版本，不得各自扩展语义。

## Preview v1

- [SPEC-R3-001：Guardian Contract 与 Driver Adapter](v1/guardian-contract.md)：所有权、租约、
  N/N-1、共享内存、原子序、Fallback、Adapter 生命周期及错误目录。
- [R3 协议矩阵](v1/protocol-matrix.md)：EtherCAT、Modbus、serial、CAN/CAN FD、LIN 的固定角色、
  帧边界、恢复和明确排除范围。
- [SPEC-R3-002：I/O Target Profile](v1/target-profile.md)：容量、时间、硬件、内核、调优、负载和
  性能报告模板。

R3-00 只冻结规范和 host-only 完整性门禁，不创建产品驱动，不引入 EtherCrab/IgH 依赖，也不批准
`unsafe`、C FFI、内核模块或功能安全能力。R3-01 在 `aurora-io-guardian-contracts` 中实现
platform-neutral 的精确 capability/error 目录、双向 N/N-1 协商、epoch/configuration/lease identity、
heartbeat 与 output group freshness 状态机，以及 UDS peer/sealed-memfd 描述校验。它不打开 socket/fd，
不映射共享内存，不访问设备，也不实现 Driver Adapter 或协议后端；这些能力仍按 R3-02～R3-11 的
Project 顺序实现。

R3-02 在 `aurora-io-guardian` 中实现 platform-neutral 的固定 region/slot/value metadata/group
diagnostics 字节布局、精确 source/group/value mapping 闭包和安全 Rust SPSC 原子双缓冲核心。mapping
同时携带签名配置的 LayoutDigest 与 capability digest；尺寸相同但目录语义不同的 mapping 在 region
创建、导入或 image 校验前拒绝。Control
只能获得 input consumer 与 output producer，Guardian 只能获得 input producer 与 output consumer；映像
中不出现设备、socket、fd、厂商地址字符串或 backend 原生 handle。所有容量在初始化时固定，发布和锁存
不分配、不阻塞，最多锁存两次；缺条、多条、乱序、重叠、越界、旧 lease/config/layout、非零 padding、
odd generation、撕裂、过期 output，以及以 batch/group Good 掩盖非 Good value 均拒绝。Linux-only
适配器为每个 lease 创建全新 `memfd`，固定长度后设置精确
`F_SEAL_GROW | F_SEAL_SHRINK | F_SEAL_SEAL`，以独立 mmap 向 Guardian/Control 暴露同一原子映像；
descriptor 只能从 Guardian owner 导出一次，导入端在映射前重验长度、`FD_CLOEXEC`、seal 和完整不可变
identity。实际 UDS 会话与 `SCM_RIGHTS` 编排仍由后续 Guardian 进程集成消费 R3-01 peer policy，本项不
建立 socket/server，也不另造映像语义。

R3-03 在同一 `aurora-io-guardian` crate 中实现 backend-neutral Update Group 执行边界。构建期要求 group
与 operation 对 R3-02 mapping 精确闭包，固定周期、相位、排序、输入采样/输出刷新窗口、最坏 jitter 与
operation/frame/request/queue 预算；缺失、额外、重复、错序、未证明 retry 或最坏工作超预算均拒绝。
运行期使用绝对 Guardian monotonic release grid，miss 后跳到下一合法点且不 catch-up；慢组、in-flight
组或性能已拒绝组不阻塞其他健康组。每次 release authority 绑定完整 lease/config/layout/capability
identity，旧 generation/lease 的响应不得提交或重放。

固定队列对 input 采用 `DropNewest`、对 output 采用 `RejectNewest`，不增长也不覆盖未消费数据；overflow、
stale、window miss、timeout、CRC/WKC/error frame、预算不足与 `OutcomeUnknown` 均进入显式质量、gap 和
饱和诊断。只有有证明的 absolute `IdempotentSet` 可在当前预算内 retry；非幂等与 pulse/edge 写 timeout
保持物理结果未知。R3-03 不执行 reconnect、RTU turnaround、CAN bus-off、LIN schedule recovery，不创建
Driver Host、真实 backend、Fallback/watchdog 或网络 Connector。
