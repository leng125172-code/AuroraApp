# 分支、评审与发布审批

## 分支

- `develop` 是日常集成分支，`main` 只接收已批准的发布变更。
- 功能、修复和维护分别使用 `feature/*`、`fix/*`、`chore/*`，通过 PR 合入 `develop`。
- 禁止直接向 `main` 推送实现提交、改写共享历史或绕过门禁合并。

## 提交与评审

- 按可审查的逻辑单元提交；提交信息说明意图。
- 至少需要 CODEOWNERS 审批、完整 CI 通过、无未解决高危依赖/Secret 告警。
- 契约、权限、部署、安全边界和发布流程变更必须说明兼容、威胁、迁移及回滚影响。
- 架构边界变化必须先有 Accepted ADR；生成文件不得代替契约源参与评审。

## 发布职责分离

| 职责 | 可执行 | 不可兼任的最终动作 |
| --- | --- | --- |
| Builder | 从已审提交生成 Payload、SBOM、测试证据和 provenance | 生产签名 |
| Verifier | 复验摘要、兼容范围、安全门禁和可复现性 | 修改待签 Payload |
| Approver | 接受风险与发布范围 | 代替 Builder 生成产物 |
| Signer | 在隔离环境签署已批准摘要 | 修改、构建或审批内容 |

F0 只建立流程和测试产物，不产生生产包或生产签名。

## GitHub 仓库限制

仓库保持 Private。当前 GitHub 套餐对 Private 仓库的 branch protection API 返回 HTTP 403，因此 F0 不能服务端强制必需检查和审批。合并者必须按本文件人工执行；获得支持该能力的套餐后，应立即为 `main` 和 `develop` 启用禁止 force-push、必需 PR、CODEOWNERS 审批和必需 CI，并记录验证截图或 API 输出。
