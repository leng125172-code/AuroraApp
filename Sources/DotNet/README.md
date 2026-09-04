# .NET sources

`Aurora.slnx` 是唯一 .NET solution。Phase F0 只包含构建时生成的 `Aurora.Contracts`、进入领域代码前使用的 `CommonContractValidator` 和双语言兼容测试。生成 DTO 只负责 wire 表示；接收方必须在传输层限制消息字节数，并在信任边界执行语义校验。

此目录将在 H0/I0 阶段增加 `Aurora.Hmi`、`Aurora.Hmi.Core`、`Aurora.Hmi.PreviewHost`、`Aurora.Studio`、`Aurora.Studio.Core` 和 `Aurora.Sdk`。F0 不提前创建 UI 或业务项目。

所有项目必须继承仓库根目录的 `global.json`、`Directory.Build.props`、`Directory.Packages.props` 和 `.editorconfig`，并提交 NuGet lock file。
