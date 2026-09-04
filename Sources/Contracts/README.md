# Contract sources

本目录是跨语言、跨进程和持久化契约的唯一源位置。契约必须先评审再生成代码，生成产物不得反向成为契约来源。

## 目录边界

- `proto/`：gRPC/HTTP2 控制面与管理面契约。
- `wit/`：Hosted WebAssembly Component 契约。
- `schema/`：工程、Canonical IR、Target Profile、Payload 和 Envelope 的 JSON Schema。
- `control/`：共享内存布局、SPSC 记录和周期控制二进制格式。

## 版本规则

- Protobuf package 和路径包含 major 版本，例如 `aurora.control.v1` 与 `aurora/control/v1/`。
- WIT package 使用 `aurora:<domain>@<semver>`；宿主必须声明兼容范围。
- JSON Schema 使用稳定 `$id`，路径包含 major 版本，并通过显式 Schema 版本字段演进。
- 控制二进制布局必须携带 magic、layout major/minor、总长度和能力位；不兼容 major 必须拒绝映射。
- Capability 标识符最多 128 个 ASCII 字符，major 是无前导零的 `1..=4294967295`。
- 已发布字段编号、枚举值、字段含义和二进制偏移不得复用。
- Stable 变更必须保持约定的兼容窗口；Preview 的破坏性变更也必须提供迁移工具和黄金样本。

具体字段、容量和默认值属于下层规格，不得从本目录说明中自行推断。
