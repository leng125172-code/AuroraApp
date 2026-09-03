# .NET sources

`Aurora.slnx` 是唯一 .NET solution。Phase F0 只包含构建时生成的 `Aurora.Contracts` 和双语言兼容测试。

此目录将在 H0/I0 阶段增加 `Aurora.Hmi`、`Aurora.Hmi.Core`、`Aurora.Hmi.PreviewHost`、`Aurora.Studio`、`Aurora.Studio.Core` 和 `Aurora.Sdk`。F0 不提前创建 UI 或业务项目。

所有项目必须继承仓库根目录的 `global.json`、`Directory.Build.props`、`Directory.Packages.props` 和 `.editorconfig`，并提交 NuGet lock file。
