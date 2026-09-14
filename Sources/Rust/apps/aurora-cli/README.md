# aurora-cli

`aurora-cli` 是 R1 的 host-only Aurora ST Preview 1.0 命令行入口。它只在工程机或 CI 上运行，
不会进入 Target Runtime 或周期线程。所有源码和 mapping 路径都会先解析为 `--project-root` 下的
规范相对路径；重复路径、越界路径和超容量输入在生成产物前拒绝。

## 命令

```text
aurora-cli parse [--project-root <dir>] <source.st>
aurora-cli check --source <source.st>... [--mapping <mapping.json>] --task <Program=u32>...
aurora-cli build --source <source.st>... [--mapping <mapping.json>] --task <Program=u32>... --output <empty-dir>
aurora-cli inspect ir --source <source.st>... [--mapping <mapping.json>] --task <Program=u32>...
aurora-cli inspect source-map --source <source.st>... [--mapping <mapping.json>] --task <Program=u32>... --source-path <path> --byte-offset <u32>
```

- `parse` 在 stdout 输出单文件 RFC 8785 canonical AST JSON，不写文件。
- `check` 执行 parser、名称/类型、固定容量、Fault、地址和静态工作量门禁，不生成 IR/AOT。
- `build` 只接受不存在或空的输出目录；相对 `--output` 按 `--project-root` 解析。成功时恰好发布
  `canonical-ir.json`、`source-map.json`、`checkpoint-plan.json`、
  `native-source-map.json` 和 `program.o` 五个文件；失败会移除本次 staging/部分发布文件，
  不覆盖调用方已有内容。相对路径的既有父目录和目标会解析符号链接并校验仍位于工程根目录内；
  发布期间检测到目录被并发加入额外文件时会拒绝本次发布，并只撤回本次创建的内容。
- `inspect ir` 重新验证相同输入后在 stdout 输出完整 Canonical ST IR；
  `inspect source-map` 返回半开 span 包含指定 UTF-8 byte offset 的全部 symbol、node 和 Fault，
  不把嵌套节点错误折叠为一条。

编译拒绝以按 path/span/code 排序的 canonical diagnostic JSON 写入 stderr。相同源码、mapping、
task 绑定和固定工具链必须生成逐字节相同的五个文件。

## R1 gate profile

CLI 作为现有编译 API 的调用方，显式固定以下 host 构建上限；它们不是性能保证，也不是后续
Target Profile 的替代品。

| 资源 | 上限 |
| --- | ---: |
| source files / 每文件 bytes / token / AST node / nesting | 64 / 1 MiB / 131072 / 131072 / 256 |
| I/Q/M logical image | 各 1 MiB |
| Tag、DeviceBinding、task | 各 65536 |
| 单循环迭代 / 单 task source operations | 1048576 / 10000000 |
| IR node / POU | 1048576 / 65536 |
| IR、Source Map、CheckpointPlan 单文件 JSON | 各 64 MiB |
| AOT object / 单函数 / relocation / native range | 256 MiB / 16 MiB / 1048576 / 1048576 |

边界恰好等于上限时由各 compiler pass 接受，超过即原子拒绝。R1 尚未定义 Device Package 的工程
输入 Schema，因此 CLI 不伪造 package endpoint：带 `%I/%Q` binding 的项目在没有后续 R3 package
解析输入时会得到 `ST5025`；纯 `%M` 和静态 Program 可完成 R1 AOT 构建。

本工具不提供图形 ST 编辑器、Online Change、RETAIN/PERSISTENT 跨版本迁移、运行期编译、动态
装载、物理 I/O 或功能安全能力。
