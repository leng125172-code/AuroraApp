# 依赖策略与直接依赖记录

## 来源和锁定

- Rust 只允许官方 crates.io registry，NuGet 只允许 `https://api.nuget.org/v3/index.json`；禁止未批准的 git/path 外部依赖和私有镜像回退。
- `Cargo.lock` 与各 `packages.lock.json` 必须提交，CI 使用 `--locked` / `--locked-mode`。
- Protobuf Rust 使用 `protoc-bin-vendored`，C# 使用 `Grpc.Tools`；生成器版本随锁文件固定，生成代码不提交。
- Dependabot 每周提出小批量更新；每次升级重新执行契约黄金样本、安全扫描和许可证门禁。
- 产品构建固定 Rust 1.98.0；由于 LLVM 分支插桩尚属 unstable，覆盖率作业单独固定 `nightly-2026-08-30` 与 `cargo-llvm-cov 0.9.0`，其输出不进入产品产物。

## F0 直接依赖决策

| 依赖 | 用途 | 已知许可证 | 选择与替代 |
| --- | --- | --- | --- |
| clap 4.6.6 | host-only CLI 参数解析 | MIT OR Apache-2.0 | 避免自写易错解析；不进入 Target |
| jsonschema 0.53.0 | Draft 2020-12 Schema 验证 | MIT | 比仅做结构反序列化更能复用唯一 Schema |
| serde / serde_json / serde_jcs | JSON、RFC 8785 规范化 | MIT OR Apache-2.0 / Apache-2.0 | 统一确定性摘要；替代是维护自有 canonicalizer，风险更高 |
| sha2 0.11.0 | SHA-256 摘要 | MIT OR Apache-2.0 | 纯库实现，避免平台命令差异 |
| uuid 1.26.0 | UUIDv7 表示与校验 | MIT OR Apache-2.0 | 禁用默认 feature，不在 `aurora-types` 读取系统时间或随机源；生成由外层显式注入 |
| semver 1.0.28 | 产品版本解析 | MIT OR Apache-2.0 | 精确版本与锁文件双重固定，避免自写边界错误 |
| thiserror 2.0.20 | 结构化内部错误 | MIT OR Apache-2.0 | 只生成标准 Error 实现，无运行服务 |
| prost / prost-build 0.14.4 | Rust Protobuf wire/codegen | Apache-2.0 | 与 C# 官方 Protobuf wire 互操作 |
| protoc-bin-vendored 3.2.0 | 固定 protoc 可执行文件 | MIT（crate；随附上游许可证） | 避免依赖开发机全局 protoc；增加二进制供应链面，由锁文件/audit 控制 |
| Google.Protobuf 3.36.1 | C# Protobuf runtime | BSD-3-Clause | 官方实现 |
| Grpc.Tools 2.83.0 | C# 构建期 protoc | Apache-2.0 | `PrivateAssets=All`，不进入运行发布面 |
| xunit.v3 / Microsoft.NET.Test.Sdk / coverlet.collector | 测试与覆盖率 | Apache-2.0 / MIT / MIT | 仅测试依赖，不进入产品 |

许可证最终以锁定包附带元数据和 CI `cargo-deny` 结果为准。维护风险通过每周 advisory/更新检查、固定来源和可移除的窄用途边界控制；新增依赖必须先扩充本表。

## 已知可见警告

`cargo-deny` 当前会报告 `foldhash`、`hashbrown` 与 `syn` 的双版本 warning。它们来自 `prost-build` 与 host-only `jsonschema` 的不同已锁定传递依赖，未出现三版本、git 来源或 advisory。F0 保持 warning 可见并由 Dependabot 复查，不使用全局 skip 隐藏；若重复进入 Target 热路径、出现安全公告或上游版本可统一，应在对应更新 PR 中消除。
