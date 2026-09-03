# Rust workspace

本 workspace 承载 Aurora 的 Runtime、Target、Gateway、host-only 构建工具和 Rust SDK。

## 当前范围

Phase F0 只创建：

- `aurora-types`：无 I/O、网络、存储和平台依赖的基础领域类型边界。
- `aurora-control-contracts`：版本化控制契约及生成类型的承载边界。
- `aurora-test-support`：仅供测试使用的仿真时钟、虚拟 I/O、故障计划和确定性回放工具。
- `aurora-build`：host-only 的跨平台验证、摘要与供应链产物入口。

当前只实现 F0 已确认的公共值类型和契约；不包含控制程序、设备驱动、生产部署或 UI。

## 后续 crate 名称

达到对应路线图阶段后，只能按架构基线使用以下名称：

- Runtime：`aurora-control-engine`、`aurora-io-guardian`、`aurora-st-ir`、`aurora-workflow-cyclic`、`aurora-workflow-hosted`、`aurora-runtime-supervisor`、`aurora-data-bridge`、`aurora-storage-service`
- 平台与管理：`aurora-platform-linux`、`aurora-target-agent`、`aurora-gateway`
- Host-only：`aurora-build`、`aurora-cli`

新增 crate 前必须先确认所属阶段、职责、允许依赖和验收测试。可执行程序放入 `apps/`，不得把进程入口与领域实现混在同一 crate。

## 验证

在本目录运行：

```text
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --target x86_64-unknown-linux-gnu
```

也可从仓库根目录执行 `cargo run --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- verify` 运行完整跨语言门禁。
