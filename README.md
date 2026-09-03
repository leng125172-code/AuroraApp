# AuroraApp

AuroraApp 项目仓库。

## 目录结构

- `Sources/`：项目源代码
- `Documents/`：设计、开发和使用文档
- `Builds/`：本地构建输出（构建产物默认不提交到 Git）
- `Tools/`：开发、构建和维护工具

## 开始使用

仓库使用固定的 Rust 1.98.0 与 .NET SDK 10.0.300。完成依赖恢复后，执行本地 F0 核心门禁：

```text
cargo run --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- verify
```

该入口验证 Schema、Rust 格式/lint/测试/Linux x64 交叉检查、C# 契约测试，并在 `Builds/` 生成确定性的 CycloneDX SBOM 和 SLSA v1 provenance。覆盖率、依赖/许可证和 Secret 扫描由 CI 的独立最小权限作业执行；覆盖率与 NuGet 检查也提供 `Tools/` 本地脚本。

## 开发约束

- AI 辅助开发、问题诊断、代码生成和代码审查必须遵守 [AGENTS.md](AGENTS.md)。
- 实现前应阅读 [Aurora 架构文档](Documents/Architecture/README.md)，不得把已接受的架构边界作为实现细节暗中修改。
- 兼容、发布、安全和依赖变更分别遵守 `Documents/Governance/`、`Documents/Security/` 与 `Documents/SupplyChain/` 中的规则。
