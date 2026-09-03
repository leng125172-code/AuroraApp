# 接口规格任务清单

Runtime 架构 R-001 至 R-067 已闭合。以下项目是后续阶段规格工作，不是未决架构方向；负责人暂由 Caymir 承担，团队扩充后再通过评审移交。

| ID | 阶段 | 规格任务 | 验收条件 |
| --- | --- | --- | --- |
| SPEC-R1-001 | R1 | Aurora ST 文法、标准函数、算术模式和诊断号 | 版本化文法、正反例、解释器/AOT 差分与诊断黄金样本通过 |
| SPEC-R1-002 | R1 | PLC 兼容地址 `%I/%Q/%M`、Device Mapping 与 local handle 解析 | 地址语法/范围/重叠正反例通过；构建期解析到稳定 TagId + payload-local handle，周期路径不解析厂商字符串 |
| SPEC-R2-001 | R2 | Cyclic Workflow 节点、属性、交互和 Trace 布局 | Schema、静态计划、跨周期时序回放和布局黄金样本通过 |
| SPEC-R3-001 | R3 | Guardian Driver SDK、总线帧、共享内存与原子序 | 目标硬件压力测试、ABI/偏移测试、故障与断连恢复通过 |
| SPEC-R4-001 | R4 | Protobuf service/message、错误、deadline、幂等与 capability 编号 | 跨版本互操作、授权拒绝、超时和重试测试通过 |
| SPEC-R3-002 | R3 | 队列容量、资源/时间阈值和 Target Profile 模板 | 静态预算校验及目标硬件 p99.9/max 报告通过 |
| SPEC-R5-001 | R5 | PostgreSQL 模型、迁移、保留和备份工具 | 升降级、断电恢复、备份还原和兼容矩阵通过 |
| SPEC-H0-001 | H0 | HMI 设计器、控件属性和响应式布局 Schema | 1080P 基线、缩放/断点、工作流+梯形图时序呈现验收通过 |
| SPEC-E0-001 | E0 | IEC 62443/NIST SSDF 证据、法规 Profile 和认证范围 | 证据清单、差距分析、审计追踪和范围审批完成 |

每项开始前必须建立可追踪 Issue，补充资源预算、威胁模型和不包含范围；改变已接受架构时另建 ADR。
