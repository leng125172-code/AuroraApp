# Cyclic Workflow contracts

本目录保存 Cyclic Workflow 图、扫描状态机、画布布局和节点级 Trace 的规范源。Schema、
验证器、静态计划、Runtime、离线回放和 Studio 必须消费同一版本，不得各自扩展语义。

## Preview v1

- [SPEC-R2-001：Cyclic Workflow Preview 1.0](v1/language.md)：节点目录、扫描、并行、
  等待、回环、子工作流、Fault、资源预算与诊断目录。
- [Workflow Layout Preview 1.0](v1/layout.md)：不参与控制语义的独立画布布局文档。
- [Workflow Trace Binary Layout Preview 1.0](v1/trace-layout.md)：与 R0 Trace 关联的固定宽度
  节点事件流。

R2-00 只冻结规范。R2-01 已增加：

- [Cyclic Workflow Graph JSON Schema](../schema/aurora/cyclic-workflow/v1/cyclic-workflow.schema.json)
  与 [Workflow Layout JSON Schema](../schema/aurora/workflow-layout/v1/workflow-layout.schema.json)；
- `aurora-workflow-graph` 中有界的 host-only YAML 1.2 reader、强类型 Graph/Layout 模型、
  项目闭包引用校验和稳定诊断；
- `v1/examples/` 下的 YAML 正反黄金样本，以及 `schema/examples/` 下的 JSON Schema 样本。

R2-02 已在 `aurora-workflow-graph` 增加 host-only Canonical Workflow IR、显式 task root 与
Subworkflow call-site 展开、单线程静态执行顺序、全局单一写者检查、Target Profile 资源证明、
Fork/Join 结构区域闭合检查、无损十进制 `u64` 的 RFC 8785 JCS 摘要，以及覆盖节点资源和 watch
描述的逐项 no-extra/no-missing 生成审计。其并行/取消证明由 R2-04 Runtime 消费；Action binding
与 Trace producer 仍按 R2-05～R2-06 的依赖顺序交付。传统 LD、Hosted Workflow 和完整
Studio UI 不在本阶段范围内。

R2-03 已在 `aurora-workflow-cyclic` 增加固定容量 PLC 扫描内核：当前活动集先锁存，所有转移只写
下一周期活动集，每个活动节点按静态顺序恰好执行一次；Workflow control state 与任务 state/output
共用 R0 `CycleTransaction`，只允许外层周期末统一提交。运行期构造器会拒绝所提供 edge 表内部的
区间缺口、重复覆盖、区间交叉、owner/target 不一致和初始活动集重复；完整节点/edge 切片必须来自
已经过 R2-02 生成审计的产物，扫描期不做发现或扩容。

R2-04 已在同一 crate 增加 `StructuredWorkflowRuntime`：Fork/JoinAll/JoinAny、三种败方策略、
release 序号 Wait、独立展开子实例和 backedge 共用同一事务。逻辑分支按静态顺序扫描，token
在扫描开始锁存，所有后继仍到下一扫描执行。控制状态采用符合 R2-02 准入容量的紧凑位图与
固定计数器；取消保留本扫描合法写入，分支和子实例 Fault 均回滚整个 task。运行期表只含可执行
steps，Entry/End 折叠成入口和 Complete 边；没有增加运行期线程、发现、分配或阻塞 I/O。
Action binding 与 Trace producer 仍属于 R2-05/R2-06。
