# ADR-0002：契约源、构建期生成与确定性摘要

- 状态：Accepted
- 日期：2026-09-03
- 决策人：Caymir
- 关联需求/问题：跨语言契约、可复现 Schema/代码摘要

## 背景

Aurora 同时包含 Rust 与 C#，后续还会出现 WIT、共享内存和持久清单。如果提交多份生成代码或使用开发机全局生成器，契约漂移和不可复现构建很难在评审中发现。

## 备选方案

- 提交所有生成代码：消费方便，但易与源契约漂移并制造大面积机械 diff。
- 使用系统 `protoc`：配置简单，但版本不可控。
- 构建期固定生成器：恢复成本略高，但唯一源和版本清晰。
- 普通 JSON 文本摘要：受空白、属性顺序和语言序列化差异影响。
- RFC 8785 JCS + SHA-256：实现成熟且跨语言可复现。

## 决策

- `Sources/Contracts/` 是 Protobuf、WIT、JSON Schema 和 control layout 唯一源。
- Rust 使用锁定的 `prost-build` 和 vendored protoc；C# 使用锁定的 `Grpc.Tools`。均在构建期生成，产物不提交。
- JSON Schema 固定 Draft 2020-12，默认拒绝未知字段；扩展只进入命名空间 `extensions`。
- JSON 摘要采用 RFC 8785 JCS 后 SHA-256，输出格式 `sha256:<lowercase-hex>`。
- 清单集合严格排序且唯一；`u64` 使用十进制字符串避免 JSON number 精度差异。

## 后果

干净环境必须先恢复锁定依赖；任何生成器升级都会触发双语言黄金测试。F0 Schema 是 Preview v1 最小模型，后续扩展必须按兼容策略增加版本，不能在实现中静默接受字段。

## 验证

- `aurora-build schemas` 校验 meta-schema、正例、反例及排序/u64 语义。
- `aurora-build digest` 对任意 JSON 输出 JCS SHA-256。
- Rust/C# 从同一 `.proto` 生成并通过黄金 wire vector 和截断输入测试。
