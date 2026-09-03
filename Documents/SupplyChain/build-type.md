# Aurora F0 SLSA build type

该文档定义 provenance 中 `urn:caymir:aurora:build-type:f0` URI 的语义。使用稳定的 Caymir URN，避免把证明格式绑定到代码托管账号或仓库位置。

## External parameters

- `configuration`: 固定为 `F0`。
- `target`: 固定为 `x86_64-unknown-linux-gnu`，表示首版 Runtime 编译基线；`aurora-build` 本身是 host-only。

## Resolved dependencies

至少包含当前 Git remote URI 与完整 commit SHA。依赖包由提交内的 Cargo/NuGet 锁文件解析，SBOM 记录具体组件版本。

## Internal parameters

F0 为空。以后新增不可由调用方控制的 Builder 参数时必须在这里版本化说明。

## Byproducts and subject

Byproducts 是产品版本、工具链、集中包版本和 Rust/.NET 锁文件的 SHA-256 材料清单。Subject `aurora-f0-inputs` 是该清单经过 RFC 8785 后的聚合 SHA-256。文档省略不确定时间戳，保证同一提交与相同锁定输入产生相同 JSON；CI invocation 和 artifact metadata 另由 GitHub 保存。

F0 provenance 是测试用未签名来源说明，不是生产 attestation。R4 必须在隔离 Builder/Signer 流程中生成并签署生产证明。
