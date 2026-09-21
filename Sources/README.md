# Sources

此目录用于存放 AuroraApp 的源代码和契约源文件。

- `Rust/`：Runtime、Target、Gateway、构建工具和 Rust SDK。
- `DotNet/`：Studio、Recovery Console、.NET SDK 和不依赖具体 UI 框架的核心库。
- `Unreal/`：Aurora Vision 与独立 UE Preview Host；仅在 H0 的许可证、版本和工具链门禁通过后创建，不在当前阶段安装或引入 UE。
- `Contracts/`：Protobuf、WIT、JSON Schema 和控制共享布局的唯一源文件。
- `Sdk/`：对外 SDK、模板和示例。

当前只建立 Phase F0 所需项目；后续项目达到对应阶段时再加入，禁止提前生成空业务工程。

Aurora Vision 按面向集团外公开销售或订阅、直接产生软件访问/功能收入的 UE `Royalty Product` 交付；适用的 Royalty Addendum、Release Form、收入记录、报表和版税由 H0 发布门禁管理。可公开的源码只包含 Aurora 自有且许可证兼容的部分，不包含 Epic Engine Code、Starter Content 源格式、受限资产或未获准的 Engine Tools；UE 二进制只作为产品不可分离的 object code 分发。
