# ADR-0001：F0 基础类型与稳定表示

- 状态：Accepted
- 日期：2026-09-03
- 决策人：Caymir
- 关联需求/问题：Phase F0 公共类型冻结

## 背景

后续 Runtime、Gateway、HMI、Studio 和 SDK 必须共享不会依赖语言 ABI、平台字宽或本地时区的值表示。若每层自行选择 ID、时间、错误和能力表示，跨进程兼容与确定性回放无法成立。

## 备选方案

- 持久 ID 使用自增整数：紧凑，但跨设备/离线合并需要中央分配。
- UUIDv4：分布式安全，但不具时间有序性。
- UUIDv7 + payload-local `u32` handle：持久身份可分布式生成，运行布局仍保持紧凑；生成所需时间与随机源必须由外层显式提供。
- 时间只用 Unix 毫秒：简单，但精度不足且不能区分重启后的单调时间。
- 自由文本错误/质量：易显示，但不能稳定匹配、聚合或跨语言处理。

## 决策

- 持久 ID 使用 RFC 9562 UUIDv7 canonical network bytes；`aurora-types` 只表示和校验，不读取系统时间或随机源。Payload/布局内引用使用 `u32` local handle，`u32::MAX` 保留为无效值。
- UTC 使用 `i64 seconds + u32 nanos`；调度/时序使用 `u64 elapsed_nanos + BootEpochId`，两者不能混用。
- 质量码固定为 `severity:2/domain:6/reason:16/flags:8`；错误码固定为 `domain:u16/code:u16`，零只表示成功。
- Capability 使用小写点分命名空间与显式 major（例如 `aurora.io.read@1`），构建期解析为表/位集。
- 产品版本遵循 SemVer；每类公开契约独立使用 non-zero major、minor 与 preview/stable 生命周期。

## 后果

领域类型与 wire DTO 必须分离并显式转换。持久数据略大于单纯整数；每个接收边界都必须校验 UUID 版本、时间 normalization、保留位和未知 enum。该成本换取跨语言稳定性、离线创建和可解释拒绝行为。

## 验证

- Rust 类型单测覆盖成功、边界和保留值。
- Rust/C# Protobuf 共享 UUID、UTC、质量/错误 fixed32 黄金字节。
- Control Header 使用固定 64-byte little-endian 黄金向量。
- 手写 core 行覆盖率不低于 90%，分支不低于 85%。
