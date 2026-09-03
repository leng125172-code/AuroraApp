# Control Layout Header v1

所有多字节整数使用 little-endian。Header 固定为 64 字节，不依赖 Rust、C# 或 C ABI padding。

| Offset | Size | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 8 | Magic | ASCII `AURCTL01` |
| 8 | 2 | Layout major | `1` |
| 10 | 2 | Layout minor | additive revision |
| 12 | 2 | Header size | `64` |
| 14 | 2 | Flags | 未定义位必须为 `0` |
| 16 | 8 | Total size | 包含 Header 的映射总字节数 |
| 24 | 32 | Schema hash | 原始 32 字节 SHA-256 |
| 56 | 4 | Capability table offset | `0` 表示不存在 |
| 60 | 4 | Capability count | 表项数，不是字节数 |

`Capability table offset` 与 `Capability count` 必须同时为零（表不存在）或同时非零；reader 在映射数据体前先验证 Magic、major、Header size、未定义 flags、总长度及该存在性约束。

F0 黄金 Header：

```text
41555243544c3031 0100 0000 4000 0000
4000000000000000
0000000000000000000000000000000000000000000000000000000000000000
00000000 00000000
```

后续 Ring、记录和数据体布局必须另立版本规格。Major 不兼容时拒绝映射；Minor 只允许在已声明长度和 capability 协商下兼容新增。
