# ADR-0007：R1 Linux x64 AOT 后端与 Runtime ABI

- 状态：Accepted
- 日期：2026-09-10
- 决策人：Caymir
- 关联需求/问题：R1-06、SPEC-R1-001、SPEC-R1-002

## 背景

R1-06 需要在工程机或 CI 将 Canonical ST IR 确定性地编译为普通 Linux x64 原生代码。Target 不得携带编译器、动态装载器或运行期发现机制；生成代码必须保留 R0 已冻结的事务、Fault 和检查点边界。现有架构只规定 `IR -> Plan -> AOT -> Link Runtime Image`，尚未冻结具体后端、对象格式以及 AOT 代码调用 Runtime 的 ABI。

## 备选方案

- Cranelift 生成 ELF relocatable object：纯 Rust host-only 后端，可在 Windows 和 Linux 构建机确定性地产出 Linux x64 对象。
- 生成 Rust 后调用固定 `rustc`：实现直接，但生成源码和编译器诊断扩大产物及工具链表面。
- 自行实现 x86-64/ELF emitter：依赖最少，但指令选择、重定位和调试映射的维护及验证成本过高。
- 动态 `.so` ABI：部署灵活，但偏离静态链接 Runtime Image，并增加装载、发现和重定位失败面。

## 决策

- `aurora-st-ir` 作为 host-only 编译 crate，使用精确锁定的 Cranelift 0.135.0 生成 `x86_64-unknown-linux-gnu`、ELF64、小端、relocatable object。禁止依据构建机探测 CPU feature；Preview 1.0 仅使用固定 x86-64 baseline。
- AOT 对象只导出按 `TaskHandle` 排序和命名的任务入口，只导入版本化白名单 Runtime 函数。所有跨边界值使用 System V C ABI、固定宽度整数、字节指针和不透明上下文，不暴露 Rust ABI 或 Rust 类型。
- Runtime 通过静态链接提供固定 image read/write、checkpoint、Fault 和有界 string concat 回调。concat 只操作 AOT 固定栈临时值，不接收或保留 image 指针；AOT 不读取物理 PLC 地址，不执行网络、文件、数据库、日志、分配、动态加载或运行期符号发现。
- POU 调用前后和每次实际 `FOR` 回边按 `CheckpointPlan` 调用 Runtime checkpoint。Task return 由 R0 `CycleTransaction::finish` 的最终 checkpoint 实现，AOT 对象不得生成相邻的第二次调用。
- 对象、导入导出、重定位、每个 POU 的 aggregate 固定临时栈和原生 Source Map 都受调用方显式非零容量限制。任一边界失败时原子拒绝整个 AOT 产物；复合 RHS 在任何 staging 写入前完整物化，避免自重叠赋值或中途 Fault 留下部分结果。
- 原生 Source Map 使用函数符号及 section-relative 半开区间，不使用尚未链接的虚拟地址。相同 Canonical IR、Target 和容量配置必须产生逐字节相同对象及映射。

## 依赖评估

- `cranelift-codegen`、`cranelift-frontend`、`cranelift-module`、`cranelift-object` 固定为 0.135.0，许可证为 Apache-2.0 WITH LLVM-exception，MSRV 1.95，低于仓库 Rust 1.98 基线；仅进入 host-only `aurora-st-ir`，不得成为 Target Runtime 依赖。
- `target-lexicon` 0.13.5 用于显式目标解析，`object` 0.39.0 用于产物自检；二者同样精确锁定。`sha2` 复用仓库既有版本生成产物摘要。
- 供应链风险由现有 lockfile、`cargo audit`、`cargo deny`、许可证和来源门禁覆盖。若 Cranelift 后续升级改变对象字节或指令选择，必须作为工具链变更重新生成确定性黄金样本，不允许浮动升级。

## 后果

Runtime 后续需要实现该静态 ABI 的安全适配层，并在进入周期路径前验证对象、Target Profile 和 ABI 版本。该决策不引入 Online Change、跨版本进程内状态迁移、动态插件或硬实时承诺；R1-07 仍负责参考执行器与 AOT 逐周期差分验证。

## 验证

- Windows 和 Linux 对同一输入重复生成的 ELF 对象、符号表、重定位与 Source Map 完全一致。
- 对象解析门禁拒绝错误格式、非白名单导入、容量越界和未知 ABI/Target。
- Linux 使用固定测试 shim 静态链接并验证任务入口、checkpoint、Fault 和事务返回路径。
