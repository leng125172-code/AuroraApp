# 供应链

- [依赖策略与直接依赖记录](dependency-policy.md)
- [Aurora F0 SLSA build type](build-type.md)

`aurora-build sbom` 生成 CycloneDX 1.6，`aurora-build provenance` 生成 in-toto Statement v1 / SLSA Provenance v1。产物位于 `Builds/`，由 CI 归档但不提交。
