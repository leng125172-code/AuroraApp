# ADR-0003：F0 验证与供应链证据入口

- 状态：Accepted
- 日期：2026-09-03
- 决策人：Caymir
- 关联需求/问题：跨平台门禁、SBOM、来源证明

## 背景

分散的本地命令和 CI 片段容易产生“本地通过、流水线不同”的状态。F0 还需证明锁定输入、依赖清单和构建来源，但不能提前实现 R4 生产签名。

## 备选方案

- 仅维护 CI YAML：简单，但本地无法一致复验。
- 平台专属脚本：易实现，但 Windows/Linux 逻辑会漂移。
- Rust host-only CLI：可复用固定类型与摘要库，能在两平台运行，且通过依赖边界禁止进入 Target。
- 依赖外部 SBOM/provenance CLI：格式丰富，但增加工具下载、版本和来源面。

## 决策

- `aurora-build` 是本地与 CI 共用的 host-only 入口，提供 Schema、digest、contract test、verify、CycloneDX 1.6 和 SLSA v1 命令。
- 产品门禁固定 Rust 1.98.0 / .NET 10.0.300；Ubuntu 运行完整门禁，Windows 运行 smoke。
- 覆盖率行 90%、分支 85%；真实 LLVM branch instrumentation 单独固定 nightly-2026-08-30，不参与产品编译。
- SBOM/provenance 确定性生成到 `Builds/`，provenance subject 绑定实际 SBOM；源码和锁定输入记录为 resolved dependencies，local/GitHub builder 显式区分。CI 归档但不提交。F0 provenance 未签名，不能作为生产 attestation。
- 依赖只允许官方 crates.io/NuGet，锁文件提交；CI 运行 RustSec、cargo-deny、NuGet 和 Secret 扫描。

## 后果

`aurora-build` 不得成为 Runtime/Target 依赖。覆盖作业需额外下载固定 nightly；分支插桩仍属 Rust unstable，但日期锁定和阈值脚本让结果可复验。生产 Builder/Verifier/Signer 隔离延后到 R4。

## 验证

- 本地 `aurora-build verify` 核心门禁通过并产生确定性供应链 JSON；独立覆盖率与安全门禁也通过。
- CI action 以 commit SHA 固定，Ubuntu/Windows 作业显式超时和只读权限。
- Rust advisory、许可证/来源、NuGet vulnerability 与 Gitleaks 门禁已定义并可本地复验适用部分。
