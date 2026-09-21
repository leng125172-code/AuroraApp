# ADR-0010：Aurora Vision 与独立 Recovery Console

- 状态：Accepted
- 日期：2026-09-21
- 决策人：Aurora 产品负责人
- 关联需求/问题：H0/G0/I0 HMI 技术路线、UE5 动态生成边界、UE 故障降级、Royalty Product 分发与开源边界

## 背景

Aurora 的生产 HMI 需要同时覆盖 Linux 本机操作、Windows/macOS 远程操作、二维工业画面和高质量三维呈现。原基线采用 Avalonia 作为完整 HMI，能够降低图形栈复杂度，但无法在三维场景、材质、光照、动画和大规模交互表现上达到产品目标。单纯把 UE5 作为 Avalonia 内的三维视口又会形成两套页面、输入、主题和生命周期模型。

另一方面，UE5、GPU 驱动或三维内容可能整体不可用。本机操作不能因此失去最小状态显示和受控处置能力，也不能让表现层故障传播到 Control Engine、I/O Guardian 或独立安全系统。

Aurora Vision 将向集团外公开销售或订阅，收费直接归属于软件访问、功能或产品内权益，而不是只通过设备销售取得间接收入。产品因此按 Epic EULA 第 4(b) 与 Royalty Addendum 下的 `Royalty Product` 规划；仅从事该 Royalty Product 开发的 UE 使用按第 3(a) 不购买 Seat，并履行发布通知、收入记录、报表和适用版税义务。

“上架分发平台”或“开放源码”本身不会把免费、内部或仅产生间接收入的产品变成 Royalty Product。二进制分发中的 Licensed Technology 只能作为产品不可分离的 object code；开放源码只覆盖 Aurora 自有且许可证兼容的部分，不包括 Epic Engine Code、Starter Content 源格式、受限资产或未获准的 Engine Tools。

## 备选方案

1. **Avalonia 完整 HMI，UE5 仅作为三维视口。** 二维与保底能力简单，但需要桥接两套渲染、焦点、输入、生命周期和设计器预览，最终效果和编辑体验不统一。
2. **UE5 完整生产 HMI，Avalonia 仅作独立 Recovery Console。** 生产体验统一，三维能力最强；代价是需要额外维护监督、切换、命令租约和低依赖恢复路径。
3. **仅使用 UE5，不提供图形保底界面。** 组件最少，但 UE/GPU/内容故障时只剩命令行，不能满足本机可诊断和最小受控操作需求。
4. **采用其他跨平台引擎。** 可降低部分许可证或体积压力，但会牺牲当前最看重的视觉能力、工业资产生态和团队选定的开发方向。

## 决策

选择方案 2，并固定以下边界：

1. **Aurora Vision 是唯一完整生产 HMI。** `Aurora Vision` 是正式产品名；`Aurora UE5 HMI` 只作为内部架构/实现名称。面向使用者的产品标题、安装包显示名、界面和文档不得暴露内部名称。同一套中立工程模型和 UE Runtime 代码面向 Linux 本机与 Windows/macOS 远程客户端；Studio 仍为 Windows 专用 WinUI 3 工程 IDE。
2. **Aurora Recovery Console 不是 HMI。** 它使用 .NET/Avalonia，仅提供 UE5 完全不可用时的保底状态显示、诊断和最小受控操作。它不承载页面设计、三维场景、配方编辑、完整趋势、插件 Widget 或常规生产操作体验。
3. **两个客户端相互独立。** Aurora Vision 与 Recovery Console 分别向 Data Bridge/Command Broker 建立认证会话、完整快照和独立 SPSC 数据通道；二者不嵌入、不启动也不依赖对方。
4. **HMI Supervisor 位于二者之外。** 它监测 UE 进程、窗口/渲染心跳、数据新鲜度和 GPU 状态。达到项目定义的失败门限后，先撤销 UE 命令租约，再激活 Recovery Console。UE 恢复后必须通过连续健康窗口，并由操作员明确确认，才允许撤销 Recovery 租约并恢复 UE 控制，不自动来回抖动。
5. **命令所有权排他。** 任一时刻只有一个本机图形客户端持有写命令租约。Recovery Console 重新认证并校验 `ProducerEpoch`、`SchemaHash`、连续序列和完整快照后才可启用命令。切换、拒绝、超时和租约代际全部审计。
6. **Recovery Console 仅开放白名单。** 显示 Runtime、Guardian、Data Bridge、关键 Tag、当前 Alarm、质量/陈旧状态和 UE 故障原因；只允许受控停止、普通控制 `Fallback` 请求、Alarm 确认，以及项目显式签名批准的少量命令。禁止任意 Tag 写入、Force、配方下发、部署、调试和插件扩展。
7. **Recovery Console 不依赖 UE、Vulkan 或独立显卡。** 它必须在 UE 进程、UE 内容包和离散 GPU 不可用时运行，优先使用 CPU 软件渲染或集成显卡。认证本地 CLI/状态指示保留为第三层恢复入口。
8. **动态生成受构建边界约束。** Studio 编辑版本化、引擎中立的 Aurora HMI Schema。构建期完成语义校验、资源预算、资产解析、Cooking、签名和兼容检查；运行期只允许从固定白名单实例化已烹饪的 UMG/Slate Widget、Actor、材质实例和行为。禁止运行期编译 Blueprint、C++ 或 Shader，禁止加载任意 Pak、脚本或未签名资产。公开 Schema 不暴露 UE 类型。
9. **Studio 预览进程隔离。** Studio 通过打包的 UE Preview Host 预览真实画面；不得随 Studio 分发 Unreal Editor、Editor module 或 Developer module。Preview Host 崩溃不能破坏 Studio 或已保存工程。
10. **更新生命周期分离。** Aurora Vision 主程序、`.aurhmi` 页面包、`.aur3d` 三维资产包、Recovery Console/HMI Supervisor 和 Runtime 应用镜像分别 staged、校验与回滚。Vision/内容更新失败不得破坏 Recovery Console；普通 Runtime `.aurpkg` 不隐式升级任何上述槽外组件。
11. **控制与安全边界不变。** UE 和 Recovery Console 都不得直连物理 I/O、数据库、Redis、设备凭据或周期线程；所有操作经过版本化契约、授权、类型/范围/状态校验和审计。二者及其 `Fallback` 请求均不是功能安全系统，不能替代独立安全回路。
12. **Royalty Product、分发和开源门禁。** Aurora Vision 作为面向集团外公开销售或订阅并直接产生软件收入的 Royalty Product；仅从事该产品开发的 UE 使用不购买 Seat。H0-00 必须冻结适用 Epic EULA/Royalty Addendum、接受主体、直接收入模型、销售/订阅与分发渠道、Release Form 时点、费率与排除项、收入归集、报表/付款、最终用户许可及审计责任。任何免费内部变体、仅依赖设备收入的变体或其他 Royalty-Free Product 必须单独评审，不得继承免 Seat 结论。开源仓库只发布 Aurora 自有、许可证兼容且不包含 Epic Licensed Technology 的源码；UE 组合代码禁止使用 GPL、AGPL、CC BY-SA 等会把 Licensed Technology 置于其他条款下的许可证。Epic Product ID 与 Release Form 回执只有在官方申报完成并核验后才可录入。

## 后果

- 生产 HMI 的视觉、三维和跨平台体验可以统一，Studio 仍保持适合复杂工程工具的 WinUI 3 外壳。
- Avalonia 的责任显著缩小，但需要长期保持低依赖、可独立启动和严格白名单，不能逐步膨胀成第二套 HMI。
- 新增 HMI Supervisor、命令租约切换、UE 健康探针、GPU 故障注入和多生命周期兼容矩阵，实施复杂度高于单一 UI 技术栈。
- 中立 HMI Schema、Cooking 产物和 Preview Host 成为 Studio 与 UE 的稳定边界；升级 UE 主版本不得暗中改变公开工程语义。
- Royalty Product 模式取消仅从事该产品开发人员的 Seat 成本，但引入发布前 Release Form、全球直接收入归集、季度/终身排除项、报表、付款和审计责任。具体费率与阈值以发布时已接受并存档的 EULA/Royalty Addendum 为准，不在代码中硬编码。
- 公开源码必须拆分 Aurora 自有代码、UE 组合代码、Epic Licensed Technology 和第三方资产；任何源码或许可证边界不清的发布都必须停止。公开源码不等于公开 UE Engine，也不替代最终用户二进制许可。
- 本决策不改变 Runtime R0-R5、Guardian 独占 I/O、周期路径、A/B 应用槽和独立功能安全系统的既有边界。

## 验证

- 在 UE 进程崩溃、渲染线程卡死、窗口无心跳、GPU reset、Vulkan 不可用、内容包损坏和 Data Bridge 断连场景中，Control Engine 继续按既有策略运行，HMI Supervisor 能撤销 UE 租约并进入 Recovery Console。
- Recovery Console 在无 UE 安装、无 Vulkan 和禁用离散 GPU 的目标环境中可启动，并正确显示质量、陈旧、缺口和故障状态。
- UE 与 Recovery 并发或重复连接时，Command Broker 证明同一时刻最多一个有效写租约；旧 epoch、旧序列、错误 Schema、未完成快照和租约撤销后的命令全部被拒绝并审计。
- Recovery 仅能调用签名项目清单中的白名单命令；任意写值、Force、配方、部署和调试请求均被拒绝。
- UE 恢复不会自动抢回控制；连续健康窗口和操作员确认缺一不可，切换抖动测试通过。
- `.aurhmi`、`.aur3d`、UE 主程序、Recovery/Supervisor 和 Runtime 各自完成中断安装、损坏包、版本不兼容和回滚测试。
- Studio/CLI 对同一 HMI 工程产生一致的规范化模型与摘要；运行包中不存在 Editor/Developer module、运行期编译入口或未签名动态资产。
- H0-00 在任何 UE 安装或仓库引入前保存适用 Epic EULA/Royalty Addendum、接受协议的法律实体、Aurora Vision 直接收入模型、获准渠道、开源许可证矩阵、决策人和复核日期；未满足时 Gate 保持关闭。
- 正式销售、订阅、付费访问或其他直接变现开始前提交并归档 Release Form；Epic Product ID/回执在申报完成并核验前不得录入仓库或 Project。
- 发布包只包含不可分离的 UE object code；自动扫描拒绝 Epic Engine Code、Starter Content 源格式、Editor/Developer module、受限资产和不兼容许可证进入公开源码或普通产品分发。
- 财务测试覆盖全球直接收入归属、退款/税费/平台分成、季度和终身排除项、报表截止日与付款追踪；任何免费内部或间接收入变体触发独立许可复核。
