# Phase R1 状态与退出检查

- 状态：Implemented，等待远端 CI 复验
- 基线日期：2026-09-14
- 产品版本：0.1.0

## 交付映射

| 路线图交付物 | 实现与证据 |
| --- | --- |
| 版本化语言前端 | `aurora-st-ir` 的 Lexer/Parser、AST、名称/类型检查、稳定诊断与 canonical JSON |
| 固定容量与静态 FB | fixed layout、初始化 image、静态实例展开和显式容量拒绝测试 |
| 确定算术与 Fault | checked/saturating/wrapping、除零、浮点、字符串和 ARRAY Fault site 正反例 |
| 地址与 Device Mapping | `%I/%Q/%M`、TagId、writer、overlap、mapping、handle 和 snapshot 拒绝/基数测试 |
| Canonical IR 与 Linux x64 AOT | IR、Source Map、CheckpointPlan、ELF object、静态 ABI、Windows/Linux SHA-256 黄金值 |
| 参考执行器与差分 | 固定 seed 的正常提交、checkpoint rollback、Fault rollback 逐周期比较和最短失败前缀 |
| CLI | `aurora-cli parse/check/build/inspect`；一次成功 build 恰好发布五个原子、确定性产物 |

## 退出门槛

- [x] 语言正常、边界、Fault 和初始化语义测试通过。
- [x] Linux x64 参考执行器与静态链接 AOT 对固定输入逐周期一致。
- [x] 无界循环、动态周期内存、多写输出、非法地址/mapping 和容量越界均有拒绝测试。
- [x] Windows 与 Linux 对固定向量生成相同 AOT object SHA-256；Linux linker/runtime shim 门禁通过。
- [x] CLI 正反例、canonical diagnostic、重复输入、source-map 嵌套命中和输出基数测试通过。
- [x] CLI 相同输入两次构建的五个文件逐字节一致；失败不生成或遗留部分文件。
- [x] 全仓库格式、Clippy、Rust/.NET 单元与集成测试以及 Linux x64 check 通过。

## 复现入口

从仓库根目录执行：

```text
cargo run --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- verify
```

R1 CLI 的定向门禁：

```text
cargo fmt --manifest-path Sources/Rust/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-cli --all-targets -- -D warnings
cargo test --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-cli
cargo test --locked --manifest-path Sources/Rust/Cargo.toml -p aurora-st-ir
```

Linux x64 还必须执行 workspace target check，并由 `aurora-st-ir` 的 Linux-only integration test
调用系统 C linker 静态组合真实 `program.o` 与固定 Runtime ABI shim。

## R1 明确不包含

不包含图形 ST 编辑器、Online Change、RETAIN/PERSISTENT 跨版本迁移、Target 运行期 compiler、
动态装载、R3 Device Package 工程输入/真实物理 I/O 或功能安全能力。AOT 产物在 R4 签名与 A/B
交付链完成前不得表述为已签名、可部署镜像。
