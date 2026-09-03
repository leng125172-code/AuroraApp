# 兼容与版本策略

## 版本轴

- 产品使用 SemVer `major.minor.patch`，仓库根 `VERSION` 是当前产品版本唯一来源。
- Protobuf package、JSON Schema、WIT 与控制二进制布局分别独立版本化，不能用产品版本代替契约版本。
- 契约生命周期为 `preview` 或 `stable`。F0 契约均为 `preview`，不得据此承诺生产稳定性。

## 兼容规则

- 已发布 Protobuf 字段号、枚举值、Schema 字段语义、错误码、质量码和二进制偏移永不复用。
- Reader 必须拒绝未知 major；minor 仅允许文档明确的向后兼容增加。
- Schema 默认 `additionalProperties: false`。可扩展数据只能位于 `extensions`，键必须为命名空间标识。
- 稳定字段进入弃用后，至少保留两个产品 minor 且不少于 180 天；只能在下一个产品 major 移除。
- Preview 的破坏性调整也必须增加契约版本，保留黄金样本，并提供迁移或显式拒绝路径。

## 确定性与跨语言

- JSON 使用 RFC 8785 JCS 后计算 SHA-256；集合按规格键严格排序且唯一。
- `u64` 清单值使用十进制字符串；UUID 使用 RFC 9562 网络字节序；时间使用 UTC `i64 seconds/u32 nanos`。
- Protobuf 代码由构建期固定工具生成，不提交生成文件。Rust 和 C# 必须共享 `.proto` 源并通过黄金字节测试。
- 控制布局使用固定宽度字段、显式小端序和固定偏移，禁止语言 ABI、隐式 padding 和平台原生指针。

任何兼容规则例外都需要 Accepted ADR、迁移计划、回滚方式和代码所有者批准。
