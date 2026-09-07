# ADR-0005：R0 reset 后的绝对 release 接续

- 状态：Accepted
- 日期：2026-09-07
- 决策确认：用户在当前 R0-04 任务中明确确认（2026-09-07）
- 关联：R0-04、ADR-0004、R0 Execution Semantics v1 第 3.2/4.3 节

## 背景

Preview 1.0 要求 reset 后 TaskEpoch 递增、ReleaseSequence 归零，但旧措辞将绝对
网格 ordinal 与 task epoch 内序列混为一谈。将二者同时归零会重新指向历史 release，
把锁定期间误记为新 epoch 的 miss。R0-03 尚未提供 reset API。

## 决策

保持 EngineEpoch 的原始绝对时间网格。初始化成功后读取单调时间，以严格晚于该
时刻的首个网格 release 恢复；等于网格边界时选下一项。新 task epoch 序列从 0
开始，锁定和初始化期间不补跑、不计入新 epoch 的 miss。恢复后的迟到照常统计。
恢复时间算术失败时保持锁定，不发布新 epoch 或初值。

## 备选方案与影响

- 归零网格 ordinal：回到历史 release，产生不属于新执行代际的 miss，拒绝。
- 从 reset 时刻重新计算 phase：改变任务间静态相位关系，拒绝。
- 接续原网格：保留绝对调度关系，需要显式分离网格 ordinal 和 ReleaseSequence，采用。

## 验证与兼容

覆盖 reset 完成于 release 前、恰好边界、边界后、初始化推进时钟、长时间锁定、
新 epoch 恢复后迟到、时间/序列溢出以及旧 reset 身份拒绝。
这是未交付 reset API 的边界补全；不改变既有二进制格式或首次启动行为。
