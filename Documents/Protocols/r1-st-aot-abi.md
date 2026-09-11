# Aurora ST AOT / Runtime ABI Preview 1.0

本文冻结 R1-06 生成的 Linux x64 ELF relocatable object 与 Runtime 静态适配层之间的首版边界。所有函数使用 x86-64 System V C ABI；`context` 是调用期间有效的不透明 64 位句柄，其他整数均为固定宽度。任何函数都不得 unwind、阻塞、分配、记录日志或访问文件、网络、数据库及物理 I/O。

## Task 入口与状态

每个 Task 导出且只导出一个 `aurora_st_task_<task-handle-8位小写十六进制>_v1(context: u64) -> u32`。Task 按 `TaskHandle` 升序生成。

返回值：

| 值 | 含义 | R0 行为 |
|---:|---|---|
| 0 | `Completed` | 进入 `CycleTransaction::finish`；由 `finish` 执行唯一 Task-return checkpoint 后决定 commit/discard |
| 1 | `CheckpointStop` | 停止本次执行并 discard staging |
| 2 | `Faulted` | Fault 已报告，锁定任务并 discard staging |

其他返回值属于 ABI 违反，Runtime 必须拒绝提交并转换为可观测的执行故障。

## 静态导入

对象仅允许引用以下符号；链接时缺少任一实际引用的符号必须失败，不允许运行期查找或降级：

```text
aurora_st_read_bits_v1(
  context: u64, task: u32, activation: u32,
  symbol: u32, offset_bytes: u64, width_bytes: u32
) -> u64

aurora_st_write_bits_v1(
  context: u64, task: u32, activation: u32,
  symbol: u32, offset_bytes: u64, width_bytes: u32, value_bits: u64
) -> void

aurora_st_checkpoint_v1(context: u64, checkpoint_site: u32) -> u32

aurora_st_report_fault_v1(
  context: u64, canonical_fault_site: u32, fault_code: u32
) -> void

aurora_st_reset_frame_v1(
  context: u64, task: u32, activation: u32, pou: u32
) -> void

aurora_st_concat_string_v1(
  destination: u64, left: u64, right: u64,
  capacity_units: u32, unit_width_bytes: u32
) -> u32
```

`width_bytes` 仅允许 1、2、4、8。整数和浮点均按 Canonical little-endian bit pattern 传递；窄有符号整数由 AOT 代码显式符号扩展，Runtime 不得依赖 C/Rust 隐式转换。`activation = 0xffffffff` 表示 Program 或无状态调用；Function 使用其 POU `SymbolId`，Function Block 使用静态实例的 `SymbolId`。Runtime 必须在构建期完成 `task + activation + symbol + offset + width` 到已验证 staging image 的有界映射，周期调用只做有界索引访问。

`aurora_st_checkpoint_v1` 返回 0 表示继续，任何非零值表示停止。每个用户 Function/Function Block 调用前后各有一个静态调用点，每个 `FOR` 只有一个静态回边调用点并在每次实际回边执行。Task return 不生成该导入调用，由 R0 `finish` 独占，防止重复检查。

`aurora_st_concat_string_v1` 只接收 AOT 函数固定栈槽的临时地址，不接收也不得保留 image 地址；三个栈槽均采用 `4-byte little-endian length + fixed payload` 布局。`capacity_units` 是目标 STRING 的 UTF-8 byte 容量或 WSTRING 的 UTF-16 code-unit 容量，`unit_width_bytes` 仅允许 1 或 2。回调必须先验证两个输入长度和 checked sum，再完整写入目标长度、payload 并清零未使用 payload；成功返回 0，容量越界返回 1，其他返回值属于 ABI 违反。AOT 将非零结果映射到当前 Canonical Fault site 的 `STF0006` 后立即返回 `Faulted`。该回调工作量上界为 `capacity_units`，不得分配、阻塞或访问 image。

Fault 编号固定为：1 `STF0001`、2 `STF0002`、3 `STF0003`、4 `STF0004`、5 `STF0005`、6 `STF0006`。报告 Fault 后生成代码立即返回 `Faulted`，不得继续写 staging。

## 对象与 Source Map

- 对象必须为 little-endian ELF64、x86-64、relocatable，不含动态依赖、时间戳、绝对构建路径或主机 CPU 探测结果。
- 原生 Source Map 记录 `function_symbol + section-relative [start, end) + CanonicalNodeId`。范围必须非空，并按函数、起点、终点和节点稳定排序。
- Task 导出、实际引用的 Runtime 导入、重定位、单函数字节、对象字节、原生范围和每个 POU 的固定 aggregate 临时栈总字节都必须通过调用方的非零容量上限；越界时不得发布部分对象或映射。字符串/数组/结构体 RHS 必须先完整物化到该固定栈，再开始写 staging，防止自重叠赋值或中途 Fault 产生部分写入。
