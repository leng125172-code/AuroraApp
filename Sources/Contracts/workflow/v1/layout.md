# Workflow Layout Preview 1.0

- 生命周期：Preview
- Schema version：`1.0`
- 作者格式：YAML 1.2
- 文件后缀：`.aurora-workflow-layout.yaml`

Layout 是 host-only 编辑信息，不是控制契约。缺失、拒绝或修改 Layout 不得改变 Workflow
Graph 的校验、semantic digest、静态计划、plan digest、Runtime payload、输出或 Trace 语义。

## 1. 文档模型

顶层必须包含 `kind: aurora.workflow-layout`、精确 `schemaVersion`、UUIDv7 `documentId` 和被
引用的 `workflowId`。一个 Layout 只能引用一个 Workflow。

- `nodes` 按 NodeId 关联位置和大小；`edges` 按 EdgeId 关联折线路由；`groups` 只用于视觉分组。
- 坐标和尺寸使用带单位名称的 signed `i32` canvas units，不接受 float、百分比、像素或依赖
  DPI 的隐式值。routing points 按声明顺序保存。
- zoom、滚动位置、窗口位置、当前选择、断点和在线状态属于用户会话，不写入共享 Layout。
- 人类注释允许 UTF-8，但不充当 ID、guard、Action 参数、权限、Fault 或执行顺序。
- 未知字段和更高 minor 拒绝。YAML encoding、key、tag、alias 与 source budget 规则沿用
  SPEC-R2-001，但使用独立 host-only layout bytes/nodes/points 上限。

## 2. 校验和恢复

- NodeId/EdgeId 必须存在于目标 Workflow。不存在或属于其他 Workflow 报 `WF3010`。
- 重复布局项在第二及后续项各报一个 `WF0011`；坐标/尺寸不可表示或超过 host 预算报
  `WF3011`。
- Graph 合法而 Layout 非法时，构建可以忽略 Layout 并继续生成相同 Runtime 产物，但必须
  返回独立 Layout 诊断；不得伪造或移动语义节点修复布局。
- Studio 可从合法 Graph 生成新的默认 Layout，但生成结果必须作为显式文件变更保存，不能
  成为构建所需隐藏状态。

## 3. 兼容边界

Layout v1 可以增加不改变既有字段含义的 host-only presentation 字段，但 reader 仍只接受其
明确支持的 minor。任何执行相关字段都必须进入 Workflow Graph 新版本，禁止借 Layout 扩展
Runtime 语义。
