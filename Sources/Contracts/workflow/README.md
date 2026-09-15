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
RFC 8785 JCS 摘要和逐项 no-extra/no-missing 生成审计。运行期扫描、并行/取消状态机、Action
binding 与 Trace producer 仍按 R2-03～R2-06 的依赖顺序交付。传统 LD、Hosted Workflow 和完整
Studio UI 不在本阶段范围内。
