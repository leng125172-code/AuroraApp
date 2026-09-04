# Aurora F0 SLSA build type

该文档定义 provenance 中 `urn:caymir:aurora:build-type:f0` URI 的语义。使用稳定的 Caymir URN，避免把证明格式绑定到代码托管账号或仓库位置。

## External parameters

- `configuration`: 固定为 `F0`。
- `target`: 固定为 `x86_64-unknown-linux-gnu`，表示首版 Runtime 编译基线；`aurora-build` 本身是 host-only。

## Resolved dependencies

干净工作树至少包含规范化 Git remote URI 与完整 commit SHA；HTTPS 与 SSH GitHub clone 统一为同一 HTTPS URI。存在未提交输入时改为记录包含所有 tracked 与非 ignored untracked 文件的确定性工作树摘要，避免把脏构建错误归因于 HEAD commit。锁文件和工具链声明作为独立 resolved dependencies，SBOM 记录具体组件版本。

## Internal parameters

F0 为空。以后新增不可由调用方控制的 Builder 参数时必须在这里版本化说明。

## Builder、byproducts 与 subject

Subject 是本次 F0 门禁实际生成的 `Builds/sbom.cdx.json`，其 SHA-256 可直接与归档 SBOM 复验。源码状态、产品版本、工具链、集中包版本和 Rust/.NET 锁文件属于 resolved dependencies，不冒充 byproducts；F0 暂无额外 byproduct。

GitHub Actions 与本地执行使用不同 builder ID。本地产物只声明 local builder，不得冒充 GitHub Actions；F0 省略 invocation ID 和不确定时间戳，使相同 builder、source state 与 subject 的 JSON 可复现。CI run 与 artifact metadata 由 GitHub 独立保存。

F0 provenance 是测试用未签名来源说明，不是生产 attestation。R4 必须在隔离 Builder/Signer 流程中生成并签署生产证明。
