# Cyclic Workflow Action Binding Preview 1.0

本文件冻结 R2-05 的构建输入和 Target Runtime 边界。作者 Graph Schema 仍为 Preview 1.0，
Action payload 不写回 YAML；host build 将已解析的 R1 POU、I/O image 动作和类型命令绑定作为
独立强类型输入，与展开后的静态计划一起校验和发布。

## 1. 版本与完整闭包

- binding envelope 版本为 `{ major: 1, minor: 0 }`。未知 major、未知 Action kind 或保留句柄
  必须原子拒绝，不得降级成字符串命令或动态插件查找。
- Static Workflow Plan writer 为 1.1；每个展开后的 Action step 恰有一个 binding，每个展开后的
  Subworkflow resource 恰好没有 Action binding。
- 每个展开实例内的 Graph condition identity 恰有一个 BOOL source。Action guard、Decision guard
  和 condition Wait 可以在同一实例内共享 source；同一模板的不同 Subworkflow call site 必须
  生成不同目录项，不能按 `(task, conditionId)` 合并。
- 编译器分别构造 expected set 与 actual set，再比较条目数和完整集合；任何 missing、extra、
  duplicate、wrong-kind 或 call-site merge 都抑制整份 artifact。输入顺序不得改变表、JCS bytes
  或 plan digest。

## 2. Action kind

| Kind | 允许行为 | 禁止行为 |
| --- | --- | --- |
| `st_pou` | 以构建期稠密 target handle 调用静态链接的 R1 ST POU wrapper；端口映射到固定 task staging slot | 运行期符号发现、动态装载、隐藏状态或自行选择 Workflow edge |
| `io_image` | 只读取锁存 state image，并只写 task output staging image | 物理 I/O、I/O Guardian 绕行、阻塞设备调用、`InOut` port |
| `typed_command` | 读取固定 state 输入并写恰好一个类型化 output command slot | 网络/Hosted 调用、字符串命令、直接发送设备请求、`InOut` port |

这些 kind 都在周期 task 内同步、有界执行。R1 POU 失败保留明确 `FaultReason`，使同一 R0
`CycleTransaction` 的 Workflow control、application state 与 output staging 全部回滚。

## 3. 类型端口与所有权

- Preview 1.0 标量目录为 `BOOL/SINT/INT/DINT/LINT/USINT/UINT/UDINT/ULINT/REAL/LREAL`，
  分别使用固定的 1/2/4/8 字节宽度；不允许隐式转换、变长字符串、集合或 opaque value。
- port index 从 0 连续、声明顺序有语义。每个 port 显式声明 `input`、`output` 或 `in_out`，同时
  保留 target-relative ownership offset 和布局解析后的 area 内绝对 image offset。bound build
  必须接收每个 task 恰一份 state/output image capacity，并证明 logical byte 与 physical byte
  双向一一对应；越界、别名合并、missing/extra task image 均原子拒绝。
- 编译器只从 `output`/`in_out` port 推导完整 write regions，并与资源声明排序后逐项相等比较。
  state bytes、staging bytes 和 Trace event reservation 也必须逐项相等；调用方不能少报或多报。
- 全局单一静态写者证明仍以 stable target identity、offset 和 width 为准。不同 Subworkflow
  call site 必须保留不同展开 Action/condition binding identity 和独立 state；不能按模板、
  condition ID 或 target handle 合并。

## 4. 固定容量 Runtime

- Target Profile 新增非零 `max_action_ports_per_node` 与
  `max_condition_bindings_per_task`。Runtime 另接收 actions、conditions、ports 和
  guards 的显式非零容量，并在构造期接受 equality、拒绝 first excess。
- runtime 表的 Action/condition handle 必须从 0 稠密；每个 Action 独占一个连续 port range，
  每个 Decision 独占一个按静态 priority 排列、与 outgoing edge 完全相等的 guard range。
- Runtime 构造器对 Action、Decision、condition Wait 重新形成精确 callback-node closure，并
  拒绝 missing、extra、duplicate、wrong-kind、悬空 edge/condition/action 以及 range gap/overlap。
- host bridge 必须重新序列化并核对 Canonical IR、Static Plan 的 JCS bytes 与 SHA-256 digest，
  再从 steps/resources/conditions 生成一份不可拆分的 owned runtime plan。外层装载必须再次核对
  plan identity；不得手工交换或拼接 Action、condition、port、guard 表。
- 周期路径只遍历固定表并读写预分配 staging image，不分配、不阻塞、不发现插件。Action 后端
  是无实例静态接口，只能通过 invocation 专属 transaction state range 和固定 ports 修改语义
  状态；回调显式接收 invocation handle 与 target handle，不能保留隐藏可变状态，也不能返回 edge。
  Action guard false 与 Decision 全 false 均 `Retain`，Decision 只采用 priority 最早的 true edge。
  因此 guarded Action 与 Decision 不能作为 `WaitAtBoundary` 的有界 cancellation boundary；编译器
  必须拒绝把“到达节点”等同于“本扫描一定进入 boundary-take”。

## 5. 明确排除

R2-05 不实现真实物理 I/O、设备通信、HTTP/DNS/数据库、Hosted Workflow、运行期插件发现、
Online Change、Trace producer 或跨版本进程内状态恢复。`io_image` 与 `typed_command` 只描述
本周期 staging 行为；周期成功提交后的 I/O Guardian/外层交付仍由既有架构边界负责。
