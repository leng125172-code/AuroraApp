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
diagnostics 字节布局、精确 source/group/value mapping 闭包和安全 Rust SPSC 原子双缓冲核心。Control
只能获得 input consumer 与 output producer，Guardian 只能获得 input producer 与 output consumer；映像
中不出现设备、socket、fd、厂商地址字符串或 backend 原生 handle。所有容量在初始化时固定，发布和锁存
不分配、不阻塞，最多锁存两次；缺条、多条、乱序、重叠、越界、旧 lease/config/layout、非零 padding、
odd generation、撕裂、过期 output，以及以 batch/group Good 掩盖非 Good value 均拒绝。Linux sealed
`memfd`/UDS 适配器仍是后续集成范围，必须逐字节保持本 ABI，不得另造映像语义。
