# JSON Schema

保存工程模型、Canonical IR、Target Profile、Payload 和 Envelope 的 Schema。Schema 必须声明稳定 `$id`、版本、必填字段、容量/范围和未知字段策略，并配套有效与无效黄金样本。

F0 已定义 Canonical IR、Target Profile、Payload 与 Envelope 的 Preview v1 最小契约。未知字段默认拒绝；扩展只能进入 `extensions`，且键必须为小写命名空间。所有可重复集合必须按规范键排序且唯一，`u64` 使用十进制字符串避免跨语言精度损失。

## F0 协议硬上限

| 项目 | 上限 |
| --- | ---: |
| capability 列表 | 4096 项；单项 128 字符 |
| CPU feature 列表 | 256 项；单项 64 字符 |
| Payload artifacts | 4096 项；path 1024 字符 |
| Payload contracts | 256 项 |
| Envelope approvals / signatures | 64 / 16 项 |
| certificate / Base64 signature | 各 32768 字符 |
| extensions | 128 个命名空间属性 |

这些是协议拒绝上限，不是建议容量；Target Profile 应给出更低的项目预算。每个消费方仍必须在 JSON 解析前实施总输入字节上限（F0 建议不高于 16 MiB）和超时，不能依赖 Schema 防止深层扩展值造成资源耗尽。

验证命令：

```text
cargo run --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- schemas
```

JSON 摘要先按 RFC 8785 JCS 规范化，再计算 SHA-256：

```text
cargo run --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- digest <json-file>
```
