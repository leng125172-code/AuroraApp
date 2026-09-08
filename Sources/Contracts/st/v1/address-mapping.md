# SPEC-R1-002：Aurora ST Address Mapping Preview 1.0

- 生命周期：Preview
- Mapping 语义版本：`1.0`
- 依赖语言：[SPEC-R1-001 Preview 1.0](language.md)
- 身份表示：RFC 9562 UUIDv7 `TagId` + payload-local `u32` handle

本文冻结 Aurora ST 统一逻辑地址、Device Mapping 绑定和 deterministic handle 分配。它不定义 R3 Guardian 共享内存 ABI、Driver SDK 或厂商总线帧；物理 I/O 始终由 Guardian 独占，Control Engine/AOT 只访问已验证的逻辑 I/O 映像。

## 1. 规范地址语法

```text
%<area>X<byte>.<bit>
%<area><width><byte>

area  = I | Q | M
width = B | W | D | L
byte  = 0 | [1-9][0-9]*
bit   = 0..7
```

字母输入大小写不敏感，规范格式统一大写且十进制 offset 无前导零。只接受以下两种形状：

| 形状 | width bits | 允许 ST 类型 | alignment |
| --- | ---: | --- | ---: |
| `%IX/%QX/%MX<byte>.<bit>` | 1 | `BOOL` | bit 0..7 |
| `%IB/%QB/%MB<byte>` | 8 | `SINT/USINT` | 1 byte |
| `%IW/%QW/%MW<byte>` | 16 | `INT/UINT` | byte offset mod 2 = 0 |
| `%ID/%QD/%MD<byte>` | 32 | `DINT/UDINT/REAL` | byte offset mod 4 = 0 |
| `%IL/%QL/%ML<byte>` | 64 | `LINT/ULINT/LREAL` | byte offset mod 8 = 0 |

address interval 统一换算为 bit 半开区间：X 为 `[byte*8+bit, +1)`，其他为 `[byte*8, +width)`。所有乘加使用无界整数验证后再写目标宽度；不可表示报 `ST5006`，不得回绕或截断。

## 2. 区域与生命周期

| Area | 所有者和方向 | 读取含义 | 生命周期 |
| --- | --- | --- | --- |
| `%I` | Guardian 单写逻辑输入；ST 只读 | 本周期开始锁存的输入映像 | Guardian/engine epoch；过龄质量由 I/O 契约处理 |
| `%Q` | 一个正常控制 owner 写 staging；Guardian 读取 committed 命令 | 读回当前 task staging/committed 命令，不是设备反馈 | Fault 周期不提交；Fallback 优先于正常命令 |
| `%M` | Runtime 内部，一个 task owner 写 | task 内部逻辑 memory | task reset、进程重启和 A/B 激活回声明初值；不持久化 |

- `AT` 只允许在顶层 `VAR_GLOBAL` 的单个 identifier declaration 上。一个 `AT` 不得应用到 identifier list、POU local、FB field、array/struct 整体或 alias。
- 直接地址类型必须与上表精确匹配；不做窄/宽视图、union、overlay 或隐式 reinterpret。
- 同一 area 的任意 bit interval 不得重叠，包括完全相同地址、不同宽度、struct/array 展开或仅输入别名。需要多处读取时引用同一 global symbol。
- `%I` 赋值在赋值 target 报 `ST5008`。`%Q` 必须解析到恰好一个正常 writer task；0 个报 `ST5009`，2 个及以上对该 Tag 报一个 `ST5010` 并列出按 task handle 排序的 writers。
- `%M` 可只读或未使用；一旦有写入，只能来自一个 task。跨 task 读取 `%Q/%M` 不直接共享 bank，必须由构建计划生成 R0 snapshot 依赖；无法确定唯一 source task 报 `ST5020`。

## 3. TagId 清单

稳定身份来自工程 Tag catalog，不从名称、路径、地址或厂商字符串计算：

```text
TagIdentity {
  tag_id: UUIDv7,
  symbol: canonical fully-qualified global name
}
```

- 每个有效 `AT` global 必须恰有一个 catalog entry，按 canonical symbol 匹配；缺失各报一个 `ST5002`。
- 一个 catalog entry 必须匹配恰好一个有效 global。无声明的多余 entry 报 `ST5019`，不生成 phantom Tag。
- `TagId` 必须是有效 UUIDv7 且在整个 project 唯一。每个第二及后续重复 entry 报一个 `ST5003`，锚定后项。
- catalog 不重复保存 type 或 logical address；两者只从已验证 ST declaration 取得，避免两份来源漂移。
- rename 只更新 symbol，保留 `TagId`；改变地址或 type 是显式工程变更但仍保留身份，由兼容/部署层判断影响。

## 4. Device Mapping 语义记录

每项物理绑定在规范化后必须包含以下显式字段；具体工程 JSON Schema 在 R1-05 交付，但不得改变这里的字段语义：

```text
DeviceBinding {
  mapping_version: 1.0,
  binding_id: UUIDv7,
  tag_id: UUIDv7,
  device_id: UUIDv7,
  direction: input | output,
  vendor_endpoint: non-empty opaque Device-Package string,
  width_bits: 1 | 8 | 16 | 32 | 64,
  byte_order: little | big,
  bit_order: lsb0 | msb0
}
```

- `%I` 必须恰有一个 `direction=input` binding，`%Q` 必须恰有一个 `direction=output` binding。缺失每个 Tag 报一个 `ST5011`；第二及后续 binding 各报一个 `ST5013`。
- `%M` 或不存在的 TagId 必须没有 DeviceBinding；每个意外 binding 报 `ST5012`，不生成 phantom I/O，也不将 memory 暗中变成物理 I/O。
- direction 或 width 与逻辑声明不一致分别报 `ST5014`/`ST5015`。byte/bit order 缺失、未知或组合不被对应 Device Package 支持报 `ST5017`；不得使用 host native 默认。
- `vendor_endpoint` 对 ST compiler 是 opaque identity，仅由已锁定 Device Package 在构建/激活前解析。Siemens `DB1.DBW0`、Mitsubishi `D100`、Modbus `40001` 等文本不得出现在 ST token 中。
- `binding_id` 必须是 project 内唯一 UUIDv7，作为 mapping 诊断和规范排序身份；它不成为周期 handle。
- 同一 `device_id + vendor endpoint` 经 Device Package 解析后的物理 bit interval 不得由多个 binding 覆盖；按 `binding_id` 网络字节序排序后的每个后冲突项各报一个 `ST5018`，与逻辑地址是否不同无关。输入 fan-out 通过同一 Tag 的多 reader 完成，不复制物理绑定。
- Device Package 未锁定、endpoint 不存在、范围/对齐/访问方向/transform 不受支持时构建失败；R1 不打开设备，不探测网络，也不通过现场值验证 mapping。

## 5. 逻辑映像布局

- `%I/%Q/%M` 是三个互不重叠的 byte image，各自容量由 Target Profile 显式给出；缺失容量或声明末端 `> capacity_bits` 报 `ST5006`。
- 逻辑 image 采用 little-endian；bit 0 是一个 byte 的 least-significant bit。Device Mapping 的 byte/bit order 转换由 Guardian/Driver 边界执行，周期 ST 代码不解释厂商字符串或转换规则。
- 非 X 标量按第 1 节对齐。未占用 gap 必须为 0 且不能生成 Tag、handle 或 writer。
- array、string、struct 和 enum 在 Preview 1.0 不能直接 `AT`；以后支持必须定义逐字段布局、原子提交与 overlap 规则并升级 mapping minor。
- `%Q/%M` 写入只修改所属 task 的 R0 staging bank，成功周期一次提交。Fault、deadline miss 或 capacity 错误不得发布部分 bytes。

## 6. Payload-local handle 分配

handle 只在一次 resolved Payload 内有效，不持久化也不由作者输入。算法固定为：

1. 完成语法、名称、类型、Tag catalog、逻辑 interval、writer 和 Device Mapping 全部验证。
2. 收集所有且仅有有效、可观察的 Tag；按 `TagId` 16-byte RFC 9562 network bytes 严格升序。
3. 对排序后第 `i` 项分配 `LocalHandle(i)`，范围 `0..=u32::MAX-1`。N 大于 `u32::MAX` 或任一索引不可表示报一个 project-level `ST5021`。
4. 生成一张双射表 `TagId <-> LocalHandle`；同一 Tag 在 symbol、IR、Source Map、I/O image 和 snapshot 引用中复用同一 handle。
5. 任一前置诊断存在时步骤 2～4 不发布结果；不得保留旧 handle 配新源码，也不得为无效/多余 catalog 或 mapping entry 生成 handle。

因此 N 个唯一有效 Tag 必须恰好生成 N 个 handle，连续且无 gap；source file 顺序、声明顺序、symbol rename 和 mapping entry 顺序都不改变分配结果。`u32::MAX` 永远是 invalid sentinel。

## 7. 校验顺序与 diagnostic cardinality

校验 pass 顺序固定为：syntax → symbol/type → Tag catalog → logical interval → writer ownership → Device Mapping → handle assignment。前一 pass 使 declaration 无效时，后一 pass 不为同一 declaration 生成级联错误。

| Code | 名称 | 唯一主因与数量 |
| --- | --- | --- |
| `ST5001` | InvalidDirectAddress | 每个不规范/不可解析逻辑地址一个 |
| `ST5002` | MissingTagIdentity | 每个有效 AT global 缺 catalog entry 一个 |
| `ST5003` | DuplicateTagIdentity | 每个第二及后续相同 TagId entry 一个 |
| `ST5004` | AddressTypeMismatch | 每个地址 width/type 不匹配声明一个 |
| `ST5005` | AddressMisaligned | 每个未满足 width alignment 的声明一个 |
| `ST5006` | AddressOutOfRange | 每个 offset 算术失败或超 image 的声明一个 |
| `ST5007` | AddressOverlap | 每个后声明相对最早冲突声明一个，不枚举冲突 pair |
| `ST5008` | InputWriteForbidden | 每个 `%I` assignment target span 一个 |
| `ST5009` | OutputWriterMissing | 每个没有正常 writer 的 `%Q` Tag 一个 |
| `ST5010` | MultipleWriters | 每个存在多个 writer task 的 `%Q/%M` Tag 一个 |
| `ST5011` | MappingMissing | 每个缺 binding 的 `%I/%Q` Tag 一个 |
| `ST5012` | MappingUnexpected | 每个 `%M` 或 orphan TagId binding 一个 |
| `ST5013` | MappingDuplicate | 每个第二及后续 binding 一个 |
| `ST5014` | MappingDirectionMismatch | 每个 input/output 方向不一致 binding 一个 |
| `ST5015` | MappingWidthMismatch | 每个物理/logical width 不一致 binding 一个 |
| `ST5016` | VendorAddressInSource | 每个 ST 中厂商地址 token span 一个 |
| `ST5017` | InvalidMappingTransform | 每个缺失/未知/不支持 byte/bit order 一个 |
| `ST5018` | PhysicalAddressOverlap | 每个按 binding_id 排序后的物理 overlap binding 一个 |
| `ST5019` | OrphanTagIdentity | 每个无有效 global 的 catalog entry 一个 |
| `ST5020` | CrossTaskAccessUnresolved | 每个无法确定 snapshot source 的 `%Q/%M` Tag 一个 |
| `ST5021` | LocalHandleExhausted | handle 总量或索引不可表示时 project-level 一个 |

逻辑 overlap 检测按 `(area, interval_start, interval_end, TagId)` 排序。对每个后声明只引用排序中最早的重叠声明，故一个声明最多一个 `ST5007`。物理 overlap 使用 `(device_id bytes, vendor interval, binding_id bytes)` 同样处理。诊断最终仍服从 SPEC-R1-001 的 path/byte/code 排序。

## 8. 正反例

### 8.1 正例

```iecst
AURORA_ST VERSION 1.0;

VAR_GLOBAL
    StartButton AT %IX0.0 : BOOL;
    SpeedCommand AT %QW2 : UINT := UINT#0;
    WorkCounter AT %MD100 : DINT := DINT#0;
END_VAR
```

- 三个 declaration 对应三个唯一 TagId，成功时恰好生成三个 handle。
- StartButton/SpeedCommand 各恰好一个 input/output DeviceBinding；WorkCounter 无 DeviceBinding。
- `%QW2` interval `[16,32)`，`%MD100` interval `[800,832)`；area 不同所以不重叠。

### 8.2 地址与映射反例

| 输入 | 唯一主诊断 |
| --- | --- |
| `%IX0.8` | `ST5001` |
| `%IW1 : UINT` | `ST5005` |
| `%QW2 : UDINT` | `ST5004` |
| `%QB2` 与 `%QW2` | 后声明 `ST5007` 一个 |
| 对 `%I` 赋值两次 | 两个 assignment span 各 `ST5008`；Tag 级错误不重复 |
| 一个 `%Q` 被 task 2/5/9 写 | 该 Tag `ST5010` 一个，writer 列表 `[2,5,9]` |
| 一个 `%I` 无 binding | 该 Tag `ST5011` 一个 |
| 一个 `%I` 有三个 binding | 第二、第三项各 `ST5013`，不再报 missing |
| `%M` 有两个 binding | 每项各 `ST5012` |
| ST 出现 `DB1.DBW0` | token span `ST5016`，不生成逻辑地址或 handle |
| catalog 多一个未声明 Tag | 多余 entry `ST5019`，不生成 phantom handle |

## 9. 兼容与后续边界

- 改变既有地址 width/type、alignment、endianness、overlap、TagId 或 handle 排序是 breaking change，必须增加 mapping major。
- 添加新 area/width、复合 AT 或 mapping transform 至少增加 minor，并要求 compiler/Target Profile/Device Package 显式协商；Preview 1.0 实现不得先行接受。
- R1-05 才实现 Tag catalog/Device Mapping 工程 Schema、编译期 binder、writer analysis 和 handle 表；R3 定义 Guardian ABI、驱动 endpoint 和真实 byte/bit transform。
- 本规格不授予 ST 访问设备文件、PLC 地址、串口、现场总线、网络或 Guardian 控制面的能力，也不改变独立功能安全系统边界。
