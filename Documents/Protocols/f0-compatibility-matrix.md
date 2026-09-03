# F0 契约兼容矩阵

F0 产品版本为 0.1.0，全部公开契约处于 Preview；本表描述实现事实，不构成生产稳定性承诺。

| 契约 | Writer | Reader | 当前版本 | 兼容与拒绝 |
| --- | --- | --- | --- | --- |
| `aurora.common.v1` Protobuf | Rust prost 0.14.4、C# Google.Protobuf 3.36.1 | 同左 | major 1 | 同 major 遵循 Protobuf additive 规则；未知 enum/缺失必填语义在 domain adapter 拒绝 |
| Control Header | Rust `ControlHeader`；其他语言按布局文档 | Rust 与 C# 黄金向量 | layout 1.0 | major 非 1、size 非 64、未知 flag、非法总长度/能力表存在性均在映射前拒绝 |
| Canonical IR | JCS JSON writer | Draft 2020-12 validator | preview 1.0 | major/lifecycle 固定；F0 `units` 必须为空；未知顶层字段拒绝 |
| Target Profile | JCS JSON writer | Draft 2020-12 + 语义验证 | preview 1.0 | 只接受 Linux x64 首版 triple；`u64` 溢出、未排序集合拒绝 |
| Payload | JCS JSON writer | Draft 2020-12 + 语义验证 | preview 1.0 | artifacts/contracts/capabilities 排序唯一；artifact path 仅允许规范相对路径；未知字段和越界集合拒绝 |
| Envelope | JCS JSON writer | Draft 2020-12 validator | preview 1.0 | 时间 normalization、签名算法、Base64 和数量上限验证；F0 不执行真实身份/签名验证 |

## 黄金证据

- UUIDv7 canonical bytes、UTC `sint64/uint32` 和 quality/error `fixed32` 在 Rust/C# 测试中共享固定向量。
- Control Header 的 64 字节向量同时由 Rust encode/decode 与 C# offset 读取测试验证。
- 四类 JSON Schema 均有有效与无效样本；无效样本覆盖未知字段、时间越界、版本范围倒置、排序、路径穿越和 `u64` 溢出。

生成器版本变化、字段号/偏移变化或 Schema minor 增加时必须更新本表和黄金样本。Stable 字段移除遵守至少两个产品 minor 且 180 天、仅下一 major 移除的策略。
