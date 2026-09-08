# SPEC-R1-001：Aurora ST Language Preview 1.0

- 生命周期：Preview
- 语言版本：`1.0`
- 规范文法：[aurora-st.ebnf](aurora-st.ebnf)
- 执行目标：普通 Linux x64、Rust `std`、工程机或 CI AOT

本文是 Aurora ST Preview 1.0 的规范语义源。Aurora ST 是自定义方言，不声明 IEC 61131-3、CODESYS 或厂商 ST 源码兼容。编译器、格式化器、参考执行器、AOT 与 Studio 必须使用相同版本和诊断目录；实现不得以“兼容模式”接受本文没有定义的语法。

## 1. 版本、输入与资源

每个 `.st` 文件必须以以下指令开始；BOM、注释或其他 token 不得出现在它之前：

```iecst
AURORA_ST VERSION 1.0;
```

- 输入必须是无 BOM 的有效 UTF-8。换行接受 LF 或 CRLF，规范格式输出 LF。
- Preview 1.0 编译器只接受精确 `1.0`。缺失、未知 major/minor 或重复指令报 `ST0006`，不按其他 ST 方言重试。
- 项目相对路径使用 `/`、区分大小写并先做 `.`/`..` 和根逃逸校验；诊断路径使用该规范路径。
- Target Profile 必须显式提供单文件 bytes、token、AST node、嵌套层级、字符串/数组容量、POU/FB 实例数、调用深度、单次 `FOR` 迭代数和任务总静态工作量上限。任一上限缺失、为零、算术溢出或被超过都报 `ST0007`/`ST3005`，不分配部分产物继续。
- 成功构建才允许发布 AST、Canonical IR、Source Map、AOT 或地址表。失败构建只产生按第 11 节排序的诊断，不留下可被部署误用的旧/部分新产物。

## 2. 词法

### 2.1 标识符与关键字

- 标识符只允许 ASCII `[A-Za-z_][A-Za-z0-9_]*`，长度为 `1..=128` bytes。
- 关键字、标准函数名和标识符比较均为 ASCII case-insensitive；`Motor` 与 `MOTOR` 是同一名称。符号表 canonical key 是 ASCII lowercase，诊断和格式化保留首次声明拼写。
- 关键字、`CHECKED_*`/`SATURATING_*`/`WRAPPING_*`、第 9 节标准函数和以 `__aurora_` 开头的名称均保留，不得声明或遮蔽。
- 保留关键字集合是 EBNF 中全部大写字母 terminal，外加用于明确拒绝未来/传统构造的 `WHILE`、`END_WHILE`、`REPEAT`、`UNTIL`、`END_REPEAT`、`VAR_IN_OUT`、`RETAIN`、`PERSISTENT`、`METHOD`、`INTERFACE`、`EXTENDS`。这些额外 token 在 Preview 1.0 只产生 `ST0103`，不进入可执行 AST。
- 不执行 Unicode normalization，因为标识符不含非 ASCII；字符串内容保持解码后的 Unicode scalar sequence。

### 2.2 空白、注释和 token

- 空格、tab、CR 和 LF 分隔 token。tab 只影响显示列，不影响 byte offset。
- 行注释从 `//` 到换行前；块注释为 `(* ... *)` 且不得嵌套。未闭合块注释报一次 `ST0003`，锚定起始 `(*`。
- 运算符采用最长匹配：`:=`、`=>`、`<=`、`>=`、`<>`、`..` 必须作为单 token。
- 地址 token 必须匹配 SPEC-R1-002 的规范形式。`%` 后不匹配统一地址的文本报 `ST5001`；厂商地址样式报 `ST5016`。

### 2.3 字面量

- 十进制整数：`0` 或 `[1-9][0-9]*`；十六进制：`16#[0-9A-Fa-f]+`；二进制：`2#[01]+`。负号是独立一元运算符。首尾、连续下划线均不接受；Preview 1.0 不接受数字分隔下划线。
- 实数字面量：`[0-9]+\.[0-9]+([eE][+-]?[0-9]+)?`。不接受省略整数/小数部分、hex float、`NaN` 或 `Inf` token。
- 未限定数字字面量在编译期以任意精度值存在，必须由赋值、参数或显式 `TYPE#value` 得到唯一目标类型；无唯一类型报 `ST2002`。
- `STRING[N]` 使用单引号、内容为 UTF-8、`N` 表示 payload bytes；`WSTRING[N]` 使用双引号、内容为 UTF-16 code units、内存序为 little-endian。两者长度不含终止符，运行布局保存显式长度且不使用 NUL 终止。
- 字符串只接受 `\\`、`\'`、`\"`、`\n`、`\r`、`\t` 和 `\u{1..6 hex}`；surrogate、超过 `U+10FFFF` 或不适用的引号转义报 `ST0005`。未闭合字面量报一次 `ST0004`。
- 类型限定写作 `DINT#42`、`REAL#1.5`、`State#Running`。限定值不可表示报 `ST2004`；常量算术越界使用 `ST4001`。

## 3. 声明与名称

一个 compilation unit 由版本指令和零个或多个 `TYPE`、`VAR_GLOBAL`、`FUNCTION`、`FUNCTION_BLOCK`、`PROGRAM` 组成。跨文件处理顺序按规范项目相对路径 bytewise 升序；同文件按起始 byte offset。

- 顶层名称共享一个 project namespace；类型、POU 和 global 不能同名。重复名称对每个第二及后续声明各报一次 `ST1001`，原声明不重复报。
- Preview 1.0 的每个 `VAR_GLOBAL` declaration 都必须包含一个 SPEC-R1-002 `AT` 地址；不带地址的共享变量不属于 v1。task 私有状态放在 Program/FB `VAR`，不通过隐式 global 或进程静态变量共享。
- 局部 scope 为 POU；输入、输出、state、temporary 和局部名称共享该 scope。局部名称遮蔽 global 报 `ST1001`，避免同一源码在工具中解析成不同对象。
- 引用必须在名称收集完成后解析；未定义引用每个 source span 报一次 `ST1002`。由同一未定义根节点产生的 member/call 类型错误不再级联报告。
- `FUNCTION` 无持久状态和副作用，只能读取参数、常量与局部/temporary，且必须在所有路径执行 `RETURN expression;`。
- `FUNCTION_BLOCK` 是静态实例，`VAR` 保存跨成功周期状态；`VAR_TEMP` 每次调用从声明初值重建；`VAR_INPUT` 在调用开始复制，`VAR_OUTPUT` 在调用成功后按声明顺序复制到 `=>` 目标。
- `PROGRAM` 由静态 task plan 实例化。一个 Program 实例只属于一个 R0 task；实例和调用图在构建期闭合。
- 不支持 `VAR_IN_OUT`、引用、指针、动态实例、方法、继承、接口、递归调用或递归 FB/STRUCT 实例图。语法可识别但未定义的传统 ST 构造统一报 `ST0103`，不得静默忽略。
- FB state 只在 R0 周期事务成功时提交。task Fault 丢弃本周期全部 state/output；reset、进程重启和 A/B 激活从声明初值建立新 task epoch。Preview 1.0 不支持 `RETAIN`、`PERSISTENT` 或 Online Change。

## 4. 类型与布局

### 4.1 标量

| 类型 | 表示 | 值域/规则 |
| --- | --- | --- |
| `BOOL` | 1 byte canonical storage | 只接受 `FALSE`/`TRUE`；布局值为 0/1 |
| `SINT/INT/DINT/LINT` | two's complement 8/16/32/64-bit | 对应固定有符号值域 |
| `USINT/UINT/UDINT/ULINT` | 8/16/32/64-bit | 对应固定无符号值域 |
| `REAL/LREAL` | IEEE 754 binary32/binary64 | round-to-nearest ties-to-even；每个运算后落到声明宽度；禁用 contraction/extended precision |

运行布局统一 little-endian。非有限浮点不能成为 committed state/output；任何运算、转换或输入产生 `NaN`/`±Inf` 都触发 `STF0003`。

### 4.2 固定容量复合类型

- `STRING[N]`、`WSTRING[N]` 的 `N` 必须是 `1..=TargetProfileLimit` 的编译期常量。
- `ARRAY[L..U] OF T` 的 `L/U` 必须是同一整数类型的编译期常量且 `L <= U`；元素数使用 checked arithmetic 计算并满足 Target Profile。
- `STRUCT` 字段按声明顺序布局；编译器插入的 padding 必须显式归零并进入 Canonical IR layout，不得使用 Rust/C ABI padding。
- `ENUM` 默认从 0 递增；显式值必须是唯一、可表示的 `DINT` 常量。枚举只与同一声明类型赋值/比较。
- 复合类型不得直接或间接递归。完整大小、对齐、实例数或 task state/output 总量不可表示/超预算报 `ST2006` 或 `ST3005`。
- 未显式初始化时：BOOL=false、数字=+0、string 长度 0、array/struct 递归使用元素初值、enum 使用声明的第一项。空 enum、无可表示初值或非有限初值报 `ST2005`。

## 5. 转换与公共类型

只允许以下对所有源值都无损的隐式转换：

- signed：`SINT→INT→DINT→LINT`；
- unsigned：`USINT→UINT→UDINT→ULINT`；
- unsigned 到 signed：`USINT→INT/DINT/LINT`、`UINT→DINT/LINT`、`UDINT→LINT`；
- float：`REAL→LREAL`。

`BOOL`、enum、string、array、struct 不参与其他隐式转换。整数与浮点之间、signed/unsigned 的其他组合、缩窄和 `LREAL→REAL` 必须调用第 9 节 `TO_*`。二元操作数需要公共类型时，选择能无损容纳两种完整值域的最窄类型；不存在则报 `ST2003`。

显式转换仍不得静默截断、取模或产生非有限值。运行值越界触发 `STF0004`；编译期常量越界报 `ST2004`。

## 6. 表达式顺序

- precedence 和结合由 EBNF 固定；同级二元运算左结合。比较不可链式书写。
- 函数参数、FB input 参数和一般子表达式按源码从左到右求值。
- `AND`/`OR` 总是左右均求值；`AND_THEN`/`OR_ELSE` 对 BOOL 短路。`XOR` 总是求值两侧。
- `AND/OR/XOR/NOT` 对转换到第 5 节公共类型的整数执行 bitwise，对 BOOL 执行逻辑运算；混合 BOOL/整数报 `ST2001`。
- `=`/`<>` 接受 BOOL、可形成公共类型的 numeric，或同一 enum 类型；`< <= > >=` 只接受 numeric。string、array 和 struct 在 Preview 1.0 不支持比较，报 `ST2001`。
- 赋值先完整求右侧，再写 staging 左值。数组 index 和 member 从左到右解析；任一 Fault 后本语句及本周期不再产生可提交写入。
- 一个 FB invocation 的 input 实参按源码顺序求值，但按形参名绑定；FB body 执行一次；成功后 output 按 FB 声明顺序写入。重复、未知、缺失 input 或重复 output target 报 `ST2007`，不执行部分调用。

## 7. 控制流与静态上界

- `IF/ELSIF/ELSE` 只接受 BOOL 条件，按首个 true 分支执行。
- Preview 1.0 的唯一循环是 `FOR`。初值、终值和可选 `BY` 必须是编译期整数常量；省略 `BY` 为 1，0 报 `ST3003`。
- 正 step 在 control value `<= end` 时迭代，负 step 在 `>= end` 时迭代；迭代数以任意精度整数在构建期精确计算。0 次合法，超过 per-loop/task 上限报 `ST3002`，不截断。
- control variable 必须是 POU 的局部整数变量，循环体不得赋值它；每次递增以编译期已证明可表示的数学整数计算。
- `WHILE`、`REPEAT`、无界 retry、递归和运行期循环边界在 Preview 1.0 均报 `ST0103`/`ST3001`。以后允许它们必须升级语言 minor 并冻结一种可复现的上界证明，不得由不同编译器自行决定。
- 编译器在每个循环回边、POU/FB 调用前后和 R0 要求的 task checkpoint 插入有界检查点；不得以异步信号宣称可抢占任意源码。

## 8. 整数与浮点 Fault

### 8.1 整数

- 普通整数 `+`、`-`、`*` 和一元 `-` 只允许整个结果是编译期常量且可表示；否则报 `ST4002`。
- 动态运算必须使用同宽整数的 `CHECKED_ADD/SUB/MUL/NEG`、`SATURATING_ADD/SUB/MUL/NEG` 或 `WRAPPING_ADD/SUB/MUL/NEG`。
- `CHECKED_*` 溢出触发 `STF0001`；`SATURATING_*` 钳制到目标 min/max；`WRAPPING_*` 按目标宽度 modulo `2^N`，signed 结果按 two's complement 解释。
- 整数 `/` 向 0 截断；除数 0 或 signed MIN/-1 触发 `STF0002`/`STF0001`。`MOD` 满足 `a = (a / b) * b + a MOD b` 且余数与 `a` 同号；除数 0 触发 `STF0002`，MIN MOD -1 精确定义为 0。
- 编译期相同错误分别报一次 `ST4001` 或 `ST4003`，不生成运行 Fault site。

### 8.2 浮点

- `+ - * /` 和一元负号允许用于同宽 float；`REAL` 可无损扩宽到 `LREAL`。
- 每个基本运算及标准函数返回后立即检查 finite。除 0、无效定义域或非有限结果触发 `STF0003`；`-0.0` 是有限值并保留其 sign bit。
- 比较遵循 IEEE ordered comparison；由于非有限值不能进入表达式，Preview 1.0 不暴露 unordered/NaN 比较。
- 常量求值必须产生与目标宽度运行运算相同的 bit pattern；非有限常量报 `ST4004`。

## 9. Preview 1.0 标准函数

名称不可遮蔽；除下表外没有隐式系统、时间、随机、I/O、网络、文件、日志或分配函数。

| 函数 | 接受类型 | 结果与错误 |
| --- | --- | --- |
| `CHECKED_ADD/SUB/MUL(T,T)`、`CHECKED_NEG(T)` | 任一固定宽度整数 `T` | 同 `T`；溢出 `STF0001` |
| `SATURATING_ADD/SUB/MUL(T,T)`、`SATURATING_NEG(T)` | 任一固定宽度整数 `T` | 同 `T`；钳制，不 Fault |
| `WRAPPING_ADD/SUB/MUL(T,T)`、`WRAPPING_NEG(T)` | 任一固定宽度整数 `T` | 同 `T`；按位宽回绕，不 Fault |
| `MIN/MAX(T,T)` | 同一 scalar `T`（BOOL 除外） | 同 `T`；相等时返回第一个参数，float 非有限检查 |
| `LIMIT(value,low,high)` | 三个同一 scalar `T`（BOOL 除外） | `MIN(MAX(value, low), high)`；`low>high` 编译期报 `ST2004`，运行期触发 `STF0004` |
| `ABS(T)` | signed integer 或 float `T` | integer MIN 触发 `STF0001`；float 非有限检查 |
| `SQRT(T)` | `REAL/LREAL` | 负数或非有限结果触发 `STF0003`；`SQRT(-0.0)=-0.0` |
| `CONCAT(T,T)` | 两个相同的 `STRING[N]` 或 `WSTRING[N]` | 同 `T`；合并长度超过 N 触发 `STF0006`，不截断 |
| `TO_SINT/INT/DINT/LINT` | integer 或 float | 向 0 截断 float；不可表示/非有限触发 `STF0004` |
| `TO_USINT/UINT/UDINT/ULINT` | integer 或 float | 向 0 截断 float；不可表示/负值/非有限触发 `STF0004` |
| `TO_REAL/TO_LREAL` | integer 或 float | round-to-nearest ties-to-even；非有限结果触发 `STF0003` |

standard function overload 解析只使用第 5 节转换。无唯一 overload 报 `ST2007`，不得按平台 native integer 或源码出现顺序猜测。

## 10. 运行 Fault 与 R0 绑定

| Site code | 条件 | R0 FaultReason |
| --- | --- | --- |
| `STF0001` | checked/ABS/division integer overflow | `TaskExecutionFault` |
| `STF0002` | integer division or MOD by zero | `TaskExecutionFault` |
| `STF0003` | non-finite float or invalid float domain | `TaskExecutionFault` |
| `STF0004` | explicit conversion or runtime LIMIT range invalid | `TaskExecutionFault` |
| `STF0005` | dynamic ARRAY index outside declared bounds | `TaskExecutionFault` |
| `STF0006` | STRING/WSTRING result exceeds destination capacity | `CapacityExceeded` |

每个可 Fault 操作在 Canonical IR/Source Map 中只生成一个 site，site code 加规范 source span 稳定标识根因。触发后立即停止当前 task 的 ST 执行，R0 丢弃整个 staging bank、锁定 task 并发布一个 Fallback 请求；不得通过返回默认值、截断、继续执行或重复请求掩盖错误。

## 11. 稳定诊断目录与数量规则

### 11.1 编译诊断

| Code | 名称 | 条件 |
| --- | --- | --- |
| `ST0001` | InvalidEncoding | 输入不是无 BOM UTF-8 |
| `ST0002` | InvalidToken | 无法形成任何合法 token |
| `ST0003` | UnterminatedComment | 块注释未闭合 |
| `ST0004` | UnterminatedString | string/wstring 未闭合 |
| `ST0005` | InvalidEscape | 转义或 Unicode scalar 非法 |
| `ST0006` | UnsupportedLanguageVersion | 版本缺失、重复或不是精确 1.0 |
| `ST0007` | SourceLimitExceeded | source/token/node/nesting 上限失败 |
| `ST0101` | UnexpectedToken | token 不符合 EBNF 当前位置 |
| `ST0102` | MissingTerminator | 可唯一确定缺少 `;` 或 END token |
| `ST0103` | UnsupportedConstruct | 传统 ST/未来语法不属于 Preview 1.0 |
| `ST1001` | DuplicateSymbol | 同一有效 scope canonical name 重复 |
| `ST1002` | UndefinedSymbol | 名称无法解析 |
| `ST1003` | ReservedIdentifier | 声明使用保留名称 |
| `ST1004` | RecursiveCall | Function/POU 调用图成环 |
| `ST1005` | RecursiveInstance | FB/STRUCT 实例图成环 |
| `ST1006` | InvalidPouAccess | Function 访问 state/global/side effect |
| `ST2001` | TypeMismatch | 运算、条件、赋值类型不兼容 |
| `ST2002` | AmbiguousLiteral | 未限定字面量没有唯一目标类型 |
| `ST2003` | LossyImplicitConversion | 请求了未列出的隐式转换 |
| `ST2004` | InvalidExplicitConversion | 常量转换/参数范围不可表示 |
| `ST2005` | InvalidInitializer | 初值缺失语义或不匹配 |
| `ST2006` | InvalidTypeCapacity | 固定类型大小/边界无效或不可表示 |
| `ST2007` | InvalidCall | 参数、overload 或 FB binding 无效 |
| `ST2008` | InvalidAssignmentTarget | 左值不可写或 FOR control 被赋值 |
| `ST3001` | UnboundedLoop | 循环/递归工作量无静态上界 |
| `ST3002` | LoopLimitExceeded | 精确 FOR 迭代数超过限制 |
| `ST3003` | InvalidForStep | FOR step 为 0 或不可表示 |
| `ST3004` | DynamicCyclicStorage | 动态容量、实例或分配请求 |
| `ST3005` | ResourceBudgetExceeded | 静态总工作量/存储/调用深度超预算 |
| `ST4001` | ConstantOverflow | 常量整数运算越界 |
| `ST4002` | ArithmeticModeRequired | 动态整数 +/-/*/neg 未选 mode |
| `ST4003` | ConstantDivisionByZero | 常量除数为 0 |
| `ST4004` | NonFiniteConstant | 常量 float 非有限 |

地址诊断 `ST5001..ST5021` 由 SPEC-R1-002 定义，和本表构成 Preview 1.0 完整公开目录。编号一旦发布不得复用；新增诊断只能使用未分配编号并升级 language minor。

### 11.2 排序、抑制和 cardinality

1. 诊断按 project-relative path 的 UTF-8 bytes、起始 byte offset、结束 byte offset、code 升序。
2. span 使用半开 byte range `[start,end)`；同时提供 1-based Unicode scalar line/column 仅供显示。稳定比较以 path/byte/code 为准。
3. lexer 无法恢复的 region 每个根因只报一个 lexical diagnostic；parser 对同一 token 最多报一个 primary diagnostic，并同步到 `;` 或当前 block 的匹配 END。
4. 语法无效的 declaration 不进入 symbol/type/address pass，避免为同一根因生成级联名称、类型或映射错误。
5. 每个重复定义只在第二及后续声明各报一个；每个未定义引用 span 报一个；每个 call site、算术 site、index site 或 capacity site最多报一个最具体诊断。
6. 构建只要存在 Error 就不发布任何新生成产物；诊断数量不影响该原子边界。

## 12. 规范正反例

### 12.1 正例：显式算术与有界循环

```iecst
AURORA_ST VERSION 1.0;

PROGRAM Counter
VAR
    Value : DINT := DINT#0;
    Index : UINT := UINT#0;
END_VAR
FOR Index := UINT#0 TO UINT#3 DO
    Value := CHECKED_ADD(Value, DINT#1);
END_FOR;
END_PROGRAM
```

该程序静态迭代 4 次，只生成 4 个循环 body 执行和一个 `CHECKED_ADD` Fault site，不为常量、循环边界或赋值额外生成运行项。

### 12.2 反例矩阵

| Source fragment | 唯一主诊断 |
| --- | --- |
| `AURORA_ST VERSION 2.0;` | `ST0006` |
| `VAR A : DINT; a : DINT; END_VAR` | 后一个 `a`：`ST1001` |
| `X := UnknownValue;` | `UnknownValue`：`ST1002`；不再报其派生类型错误 |
| `Value := Value + DINT#1;` | `+`：`ST4002` |
| `FOR I := 0 TO 10 BY 0 DO END_FOR;` | `BY 0`：`ST3003` |
| `WHILE Ready DO END_WHILE;` | `WHILE`：`ST0103`；不解析为可执行循环 |
| `TO_USINT(DINT#-1)` | conversion：`ST2004` |
| `REAL#1.0 / REAL#0.0`（常量） | expression：`ST4004` |

## 13. 兼容与后续实现边界

- Preview minor 只能增加不改变既有 token、precedence、类型、layout、Fault、诊断编号或地址含义的能力；compiler 仍需显式声明支持该 minor。
- 改变既有语法接受集、隐式转换、数值 bit pattern、诊断 cardinality 或运行 Fault 映射必须增加 major，并提供迁移/显式拒绝路径。
- R1-01 实现 Lexer/Parser/AST；R1-02 实现名称、类型和诊断；R1-03 实现固定容量类型/FB；R1-04 实现 Fault；R1-05 实现地址绑定；R1-06/07 实现 IR/AOT/参考执行器。本文不提前创建这些实现。
- 图形 ST 编辑器属于 I0；Online Change、RETAIN/PERSISTENT 跨版本迁移、Target 编译、RTOS、裸机、`no_std` 和功能安全能力均不在本规格。
