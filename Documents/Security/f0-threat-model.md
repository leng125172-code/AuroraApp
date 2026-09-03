# F0 STRIDE 威胁模型

- 状态：Accepted for F0
- 所有者：Caymir
- 评审日期：2026-09-03
- 范围：基础类型、契约源、构建期代码生成、测试支持、CI 和供应链证据
- 不包含：运行控制、真实 I/O、认证服务、生产部署或生产签名

## 资产与边界

受保护资产是契约语义、版本/能力判断、Schema 与生成代码一致性、依赖来源、构建证据和审查历史。边界包括不可信 JSON/Protobuf 输入、NuGet/crates.io 包、GitHub Actions、开发机到 CI 以及生成器输出到领域适配器。

## STRIDE 结论

| 类别 | 主要场景 | F0 控制 | 自动证据 | 剩余风险 |
| --- | --- | --- | --- | --- |
| Spoofing | 伪造 ID、发布者或 capability | UUIDv7 强类型；命名空间 capability；Envelope 将发布者与签名字段显式分离 | 类型/Schema 正反例 | F0 不验证真实身份；R4 建立证书与 mTLS |
| Tampering | 修改契约、锁文件、Payload 或生成代码 | CODEOWNERS；锁文件；JCS+SHA-256；构建期生成；固定工具链 | CI、黄金字节、SBOM/provenance | Private 仓库当前无法启用服务端 branch protection |
| Repudiation | Builder 否认输入或审批 | in-toto/SLSA statement；Git commit；发布职责分离 | provenance 生成与归档 | F0 provenance 未签名；R4 接入隔离签名和审计 WAL |
| Information Disclosure | Secret 进入代码、日志、测试或命令行 | AGENTS 禁令；PR 检查；Gitleaks；无生产 Secret | Secret scanning | 本地未推送内容仍依赖开发者检查 |
| Denial of Service | 巨大/畸形集合、溢出、递归或依赖阻塞 | Schema 范围；`u64` 解析；定容虚拟 I/O；未知字段拒绝 | 无效样本、边界单测、Clippy | F0 Schema 尚非流式解析；各消费方仍须设置输入字节上限 |
| Elevation of Privilege | 构建脚本、依赖或扩展获得额外能力 | 官方 registry only；固定/锁定版本；无 git 依赖；host-only 工具不进入 Target | cargo-deny/audit、NuGet audit | GitHub runner/registry 是外部供应链信任；发布前需隔离 Builder |

## F0 接受条件

- 所有外部契约有显式版本、范围和异常输入测试。
- Rust/C# 生成类型只在 wire adapter 边界使用，不取代领域类型。
- CI 无高危 advisory、非法许可证/来源或 Secret 告警。
- 任何生产身份、部署与签名需求留在 R4，不得用 F0 Envelope 字段伪装已实现安全机制。
