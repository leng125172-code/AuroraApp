# Cyclic Workflow contracts

本目录保存 Cyclic Workflow 图、扫描状态机、画布布局和节点级 Trace 的规范源。Schema、
验证器、静态计划、Runtime、离线回放和 Studio 必须消费同一版本，不得各自扩展语义。

## Preview v1

- [SPEC-R2-001：Cyclic Workflow Preview 1.0](v1/language.md)：节点目录、扫描、并行、
  等待、回环、子工作流、Fault、资源预算与诊断目录。
- [Workflow Layout Preview 1.0](v1/layout.md)：不参与控制语义的独立画布布局文档。
- [Workflow Trace Binary Layout Preview 1.0](v1/trace-layout.md)：与 R0 Trace 关联的固定宽度
  节点事件流。

R2-00 只冻结规范，不提供 YAML parser、Graph Schema、编译器、Runtime 或完整 Studio UI。
对应实现分别属于 R2-01～R2-06。
