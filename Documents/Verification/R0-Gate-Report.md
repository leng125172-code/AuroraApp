# R0 Linux x64 性能与退出门禁报告

> 本报告只描述下列机器、WSL/Linux 环境、工程负载和 commit，不是 Aurora 的跨硬件性能保证，也不代表 RT-Linux、PREEMPT_RT、RTOS、裸机或功能安全能力。

## 被测环境

| 项目 | 值 |
|---|---|
| 生成时间 | `2026-09-08T09:20:11+08:00` |
| Commit | `85e7b3a87c5cd83f1693766615f87244ae1314eb` |
| Git 工作树 | `clean` |
| 构建类型 | Cargo `release` |
| OS | Ubuntu 26.04 LTS |
| Kernel | `6.6.114.1-microsoft-standard-WSL2` |
| Architecture | `x86_64` |
| CPU | 11th Gen Intel(R) Core(TM) i7-11800H @ 2.30GHz |
| Logical CPUs | 16 |
| Linux 可见内存 | 7993412 KiB |
| Rust | `rustc 1.98.0 (88d9e12ae 2026-08-18)` |
| Cargo | `cargo 1.98.0 (797e8a9bc 2026-08-05)` |

## 固定工程负载

- 基准周期：1,000,000 ns；warm-up：1000 个基准周期；测量：10000 个基准周期。
- 静态任务：4；每任务固定 64 个 `u64`，总工作集 2048 bytes；任务体执行固定 64 次更新。
- Trace：固定 320-byte record，SPSC `DropNewest`，容量 256 slots。
- Consumer stall：测量周期 [2500, 3500)，固定 1000 个基准周期不读取；恢复后每次最多读取 8 项。
- Fault：独立探针在 state/output 各部分写入后注入 `TaskExecutionFault`，检查旧 bank 保留且无 output 可发布。
- 数据库、网络和物理 I/O：R0 范围不包含，负载为 none。

| Handle | Period (ns) | Phase (ns) | Priority | Observed releases |
|---:|---:|---:|---:|---:|
| 0 | 1000000 | 0 | 10 | 11000 |
| 1 | 2000000 | 250000 | 8 | 5500 |
| 2 | 5000000 | 500000 | 6 | 2200 |
| 3 | 10000000 | 750000 | 4 | 1100 |

## 测量结果

`actual period` 是相邻 handle 0 实际开始点之差；`jitter` 是该值与 1,000,000 ns 的绝对差；`release lateness` 是实际开始点减绝对计划 release。百分位使用 nearest-rank，所有时间均为 ns。

| 指标 | Samples | Min | p50 | p99.9 | Max |
|---|---:|---:|---:|---:|---:|
| Actual period | 9999 | 610066 | 999918 | 1165223 | 1471460 |
| Period jitter | 9999 | 4 | 15472 | 180312 | 471460 |
| Release lateness | 10000 | 62389 | 92769 | 274107 | 553337 |

- Deadline misses：0；skipped releases：0。
- Execution budget exceeded：0；HardLimit exceeded：0。
- 测量 wall time：11.000 s；process CPU time：0.220 s；平均单核 CPU：2.00%。
- 测量结束 RSS：4444 KiB；process peak RSS：4444 KiB。
- Trace attempts/published/dropped/full：19800/18255/1545/1545。
- Trace high-water：256/256；consumed：18255；observed sequence gaps：1545；counter saturated：false。
- Fault 探针：passed；固定工作集 checksum：`072e905a337eb900`。

## R0 Gate 退出清单

- [x] 同一输入 Trace 的状态、输出和诊断一致：[`r0_verification.rs`](../../Sources/Rust/crates/aurora-control-engine/tests/r0_verification.rs)。
- [x] 多周期/多相位顺序、UTC 跳变和 release 数量边界：同一 R0-08 套件。
- [x] Snapshot reader/stall、SPSC full/empty/wrap/gap/high-water：同一 R0-08 套件。
- [x] Fault discard/reset：同一 R0-08 套件；本报告另执行最小 Fault 探针。
- [x] Rust R0 自动化门禁：报告生成前同次执行 `fmt --check`、workspace `clippy -D warnings` 和 workspace tests；任一失败不会写报告。
- [x] Linux x64 性能字段：本报告记录实际周期、p50/p99.9/max jitter、deadline miss、CPU、内存和队列水位。

复现命令：

```bash
cargo run --locked --release --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- r0-report --output Builds/r0-linux-x64-report.md
```

## 限制与剩余风险

- 当前结果来自 WSL2 普通 Linux 调度器，不是独立 target、实时内核或生产硬件准入结果。
- 固定负载不含 R1+ 的 ST、Workflow、Guardian、真实 I/O、数据库或网络；这些阶段必须重新测量。
- CPU 为本进程在测量窗口内的平均单核占用；内存来自 `/proc/self/status`，不代表系统总压力。
- 本报告关闭 R0 实现 Gate，但具体部署仍需在目标硬件和目标工程上生成新的同格式报告并由部署方判断。
