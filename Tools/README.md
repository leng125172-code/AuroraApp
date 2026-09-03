# Tools

此目录用于存放开发、构建和维护工具。

- `Verify-RustCoverage.ps1`：使用固定 nightly 的真实 LLVM branch instrumentation，门禁 hand-written Rust core 行 90% / 分支 85%。需先安装 `cargo-llvm-cov 0.9.0`。
- `Verify-NuGetVulnerabilities.ps1`：读取 `dotnet package list --format json`，发现任意 direct/transitive vulnerability 时失败。

跨平台核心入口仍是 Rust `aurora-build`；这里的 PowerShell 脚本用于 CI 专项门禁和本地复验。
