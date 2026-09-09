# ADR-0006：R1 Aurora ST 语言与地址映射语义

- 状态：Accepted
- 日期：2026-09-08
- 决策人：Caymir
- 关联需求/问题：GitHub Project `R1-00`、SPEC-R1-001、SPEC-R1-002、R-004、R-005、R-024～R-028、R-32、R-35～R-38、R-42

## 背景

R0 已冻结任务、Fault、事务提交与固定容量执行边界。R1 在实现 Lexer、Parser、类型检查、Canonical IR 和 AOT 前，必须固定 Aurora ST 的首版语法、数值错误、诊断编号与 `%I/%Q/%M` 绑定规则，否则参考执行器和 AOT 可能接受不同程序，或对同一地址生成重复、缺失的运行项。

Aurora ST 是自定义方言，不承诺 IEC 61131-3 或 CODESYS 源码兼容。首版只面向普通 Linux x64、Rust `std` 和工程机/CI AOT，不允许 Target 运行期编译、Online Change、动态实例、递归或无界周期工作量。

## 备选方案

### 语言范围

- 直接采用某一厂商 ST：迁移体验较熟悉，但会把厂商扩展、隐式转换和地址语义带入 Aurora，无法维持已接受边界。
- 一次覆盖完整 IEC 61131-3：表面兼容面较大，但首版实现与验证范围不可控。
- 冻结一个显式、固定容量的 Aurora ST 子集：需要用户按 Aurora 规则迁移源码，但语法、资源和 Fault 行为可逐项验证。

### 整数算术

- 依赖构建模式或目标指令的默认溢出：相同源码可能产生不同结果，不接受。
- 所有整数运算都隐式 checked：简单，但隐藏了控制程序的溢出策略选择。
- 常量表达式静态验证，动态加减乘和取负使用显式 `CHECKED_*`、`SATURATING_*` 或 `WRAPPING_*`：源码较冗长，但策略可审查且参考执行器/AOT 可逐项差分。

### 地址与 handle

- 在 ST 中解析厂商地址：会让 Control Engine 绕过 Device Mapping 和 Guardian，不接受。
- 按源码遍历顺序分配 handle：文件重排会改变 Payload，不可复现。
- ST 只保存统一逻辑地址；构建期以稳定 `TagId` 绑定 Device Mapping，并按 UUIDv7 网络字节序分配连续 payload-local handle：重命名和文件顺序不改变结果。

## 决策

- 接受 [SPEC-R1-001](../../Sources/Contracts/st/v1/language.md) 与规范文法 [aurora-st.ebnf](../../Sources/Contracts/st/v1/aurora-st.ebnf) 作为 Aurora ST Preview 1.0 的语言源。
- 源文件必须声明精确语言版本；关键字和标识符比较采用 ASCII 不区分大小写，格式化后的规范形式使用大写关键字和原声明标识符。
- Cyclic v1 只接受静态 POU、固定布局类型和编译期可界定的 `FOR`。`WHILE`、`REPEAT`、递归、动态实例、动态分配、`VAR_IN_OUT`、RETAIN/PERSISTENT 和直接系统访问均拒绝。
- 动态整数加、减、乘和取负必须显式选择 arithmetic mode。除零、checked 溢出、非有限浮点、非法转换、索引越界和容量不足按规格在当前任务产生确定性 Fault，并沿用 R0 的整周期 discard/FaultLocked/Fallback 语义。
- 一个可 Fault 源码操作只分配一个稳定 site identity，并保存有界、排序且去重的 possible-outcome 集合；signed integer division 可在同一 site 声明 overflow 与除零两种 outcome，运行 occurrence 只报告实际触发的一种，不通过复制 site 扩大执行项。常量 ARRAY index 越界在构建期报 `ST2004`，只有 dynamic index 生成 `STF0005` site。
- 固定容量数据使用 SPEC-R1-001 的规范布局：STRING/WSTRING 采用 little-endian `u32` 长度前缀且容量不得超过 `u32::MAX`，标量自然对齐且最大为 8 bytes，ARRAY 使用对齐后的 element stride，STRUCT 按声明顺序放置字段并补齐到最大字段对齐；所有未使用区域和 padding 归零。ARRAY 单侧显式整数类型为另一侧提供目标类型，两侧均未限定时按 `DINT` 验证。Target Profile 必须显式给出容量、类型大小、Program 静态存储、FB 实例数和 invocation frame 上限，不使用平台 ABI 或隐式默认值。
- 接受 [SPEC-R1-002](../../Sources/Contracts/st/v1/address-mapping.md) 作为 `%I/%Q/%M` 与 Device Mapping 的语义源。逻辑映像采用 little-endian、bit 0 为最低有效位；厂商地址、字节序和位序只存在于 Device Mapping/Device Package 边界。
- 每个有效、可观察的全局 Tag 必须有且只有一个稳定 `TagId`。成功构建按 `TagId` 的 16-byte RFC 9562 网络字节序升序分配 `0..N-1` handle；`u32::MAX` 永不分配。无效构建不发布部分 handle、IR、AOT 或映射表。
- `%I/%Q` 每个声明必须恰有一个 Device Mapping，`%M` 必须没有物理映射。逻辑/物理重叠、多写者、缺失或重复映射都在构建期拒绝，不通过别名或隐式默认继续。
- 诊断使用规格中冻结的稳定编号和确定排序。一个主因只产生规定的一项主诊断；重复项、重叠项和缺失项按规格的锚点规则计数，避免相同错误多报或漏报。

## 影响

- R1-01～R1-07 必须以这两份规格为输入，不得由实现反向改变语义。
- 首版有意少于传统 ST；以后增加语法、标准函数或地址能力必须按 Preview 版本规则演进，并增加正反例和参考执行器/AOT 差分证据。
- R1-00 不创建编译器、运行期解析器、Canonical IR 单元、Device Mapping Schema 或生成代码；这些产物分别由后续工作项实现。
- 本 ADR 不改变 R0 `FaultReason`、Trace layout、Guardian 所有权或物理 I/O 边界。
