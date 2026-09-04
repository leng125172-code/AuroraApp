# Phase F0 状态与退出检查

- 状态：Implemented，PR #1 等待修复复验与 CODEOWNERS approval
- 基线日期：2026-09-03
- 产品版本：0.1.0

## 交付映射

| 路线图交付物 | 实现与证据 |
| --- | --- |
| 固定工具链与质量基线 | Rust 1.98.0、.NET 10.0.300、EditorConfig、rustfmt、Clippy、xUnit v3、Linux/Windows CI；覆盖测量单独固定 nightly-2026-08-30 |
| 公共基础类型 | `aurora-types`：UUIDv7、时间/BootEpoch、质量码、错误码、版本、capability、local handle |
| 跨语言契约 | 唯一 `.proto` 源，Rust prost 与 C# Grpc.Tools 构建期生成，Rust adapter/C# validator、黄金字节与语义拒绝测试 |
| 版本化格式 | Protobuf、WIT 规则、64-byte Control Header、4 个 JSON Schema 及正反例 |
| 确定性测试支持 | 手动时钟、定容虚拟 I/O、故障计划、稳定 Replay RNG |
| 安全与供应链 | STRIDE、SDL、依赖策略、CycloneDX 1.6、以实际 SBOM 为 subject 且区分 builder 的 SLSA v1、依赖/Secret 扫描 CI |
| 治理 | AGENTS 限制、ADR 模板、兼容策略、CODEOWNERS、发布职责分离、规格任务清单 |

## 退出门槛

- [x] `aurora-build` 在固定工具链下验证 Schema、格式、lint、测试和 Linux x64 编译。
- [x] RFC 8785 + SHA-256 摘要、锁文件、构建期代码生成和黄金字节测试已建立。
- [x] Rust/C# 对公共 Protobuf 值类型的编码兼容和异常输入拒绝测试通过。
- [x] 手写 Rust core 覆盖率达到行 90%、分支 85% 门槛；分支插桩使用固定 nightly，不参与产品构建。
- [x] CI 定义 Ubuntu 完整门禁、Windows smoke、覆盖率、安全扫描和供应链产物归档。
- [x] 架构方向问题关闭；后续接口细节已有阶段、负责人和验收条件。
- [x] 遗留项目审计完成，未发现待迁移工程。
- [x] GitHub PR 的 Ubuntu、Windows、覆盖率、依赖/许可证和 Secret 扫描全部通过。
- [x] `main` 和 `develop` 已启用管理员同样受约束的 branch protection、必需检查和 CODEOWNERS 审批。
- [ ] 完成 CODEOWNERS 评审。

## 远端验证

- PR：`AuroraApp#1`，目标分支为 `develop`，源分支为 `feature/f0-foundation`。
- `CI`：Ubuntu full gate、Windows smoke、Rust core coverage 均通过。
- `Security`：Dependencies and licenses、Secret scanning 均通过。
- 全部 workflow 另经 `actionlint` 1.7.12 验证，CODEOWNERS 解析无错误。

## F0 明确不包含

无可运行 PLC/控制程序、真实设备驱动、生产部署、签名服务、工作流/梯形图编辑器或 UI。进入 R0 前不得用 F0 类型和 Schema 暗示这些功能已经实现。
