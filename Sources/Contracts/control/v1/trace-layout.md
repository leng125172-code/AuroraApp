# R0 Trace Binary Layout v1

- 生命周期：Preview
- Layout：1.0
- 字节序：little-endian
- 文件 Header：64 bytes
- Record：320 bytes

本布局只定义 R0 单进程 Control Engine 的离线 Trace 文件和进程内固定 record bytes，
不使用 Rust/C/C# ABI padding，不是 R3/R5 共享内存、网络或持久化协议。所有 reserved
字节必须为零；reader 必须拒绝未知 major/minor、flags、非规范 optional 字段、截断输入
和尾随字节，不得猜测或填充缺失记录。

## 1. 文件 Header

| Offset | Size | Field | Rule |
| ---: | ---: | --- | --- |
| 0 | 8 | Magic | ASCII `AURTRC01` |
| 8 | 2 | Layout major | `1` |
| 10 | 2 | Layout minor | `0`；当前 reader 只接受精确版本 |
| 12 | 2 | Header size | `64` |
| 14 | 2 | Record size | `320` |
| 16 | 4 | Flags | 当前必须为 `0` |
| 20 | 4 | Reserved | `0` |
| 24 | 16 | EngineEpoch | RFC 9562 UUIDv7 网络字节序 |
| 40 | 8 | Record count | 文件中实际 record 数 |
| 48 | 8 | Dropped records | 导出时已知 producer drop 累计值 |
| 56 | 8 | Reserved | `0` |

文件总长度必须恰好为 `64 + RecordCount * 320`，乘加不可表示时拒绝。Header 的
`EngineEpoch` 必须与每项 record 完全一致。

## 2. Record flags

| Bit | Meaning |
| ---: | --- |
| 0 | start/finish/execution elapsed 存在 |
| 1 | UTC 与 TimeQuality 存在 |
| 2 | TimeQuality max error 存在；要求 bit 1 |
| 3 | TimeQuality last sync UTC 存在；要求 bit 1 |
| 4 | MissOutcome 存在 |
| 5 | FaultReason 存在 |
| 6 | Fallback request sequence 存在 |
| 7 | Trace counter saturated sticky 标志 |
| 8 | skipped release range 存在 |
| 9 | input snapshot evidence 存在 |
| 10 | output snapshot evidence 存在 |

bits 11..15 必须为零。optional flag 未置位时对应字段全部为零。flag 置位时，只有枚举
自身定义了零值的 `TimeQualityState::Unknown` 和 `TimeSource::Unknown` 接受零；其他 optional
枚举的零 sentinel 仍无效。这样 absent 零值不会被误解释为存在的 sequence/timestamp。

## 3. 固定 Record

| Offset | Size | Field |
| ---: | ---: | --- |
| 0 | 8 | Magic `AURTRR01` |
| 8 | 2 | Layout major `1` |
| 10 | 2 | Layout minor `0` |
| 12 | 2 | Record length `320` |
| 14 | 2 | Presence/health flags |
| 16 | 1 | TraceEventKind |
| 17 | 1 | TaskState before |
| 18 | 1 | TaskState after |
| 19 | 1 | optional MissOutcome |
| 20 | 2 | optional FaultReason |
| 22 | 1 | optional TimeQualityState |
| 23 | 1 | optional TimeSource |
| 24 | 4 | TaskHandle |
| 28 | 4 | optional UTC nanos |
| 32 | 16 | EngineEpoch |
| 48 | 8 | TaskEpoch |
| 56 | 8 | EventSequence |
| 64 | 8 | ReleaseSequence |
| 72 | 8 | CommitSequence before |
| 80 | 8 | CommitSequence after |
| 88 | 8 | scheduled release monotonic nanos |
| 96 | 8 | absolute deadline monotonic nanos |
| 104 | 8 | optional start monotonic nanos |
| 112 | 8 | optional finish monotonic nanos |
| 120 | 8 | optional execution elapsed nanos；必须等于 `finish-start` |
| 128 | 8 | optional UTC seconds (`i64`) |
| 136 | 8 | optional TimeQuality max error nanos |
| 144 | 8 | optional last sync UTC seconds (`i64`) |
| 152 | 4 | optional last sync UTC nanos |
| 156 | 4 | Reserved `0` |
| 160 | 8 | optional FallbackRequestSequence |
| 168 | 4 | Ring capacity |
| 172 | 4 | Ring occupancy |
| 176 | 4 | Ring high-water mark |
| 180 | 4 | Reserved `0` |
| 184 | 8 | attempted count |
| 192 | 8 | published count |
| 200 | 8 | dropped count |
| 208 | 8 | full count |
| 216 | 8 | optional skipped first ReleaseSequence |
| 224 | 8 | optional skipped last ReleaseSequence |
| 232 | 8 | optional skipped count |
| 240 | 32 | optional input snapshot evidence |
| 272 | 32 | optional output snapshot evidence |
| 304 | 16 | Reserved `0` |

每个 snapshot evidence 子布局固定为：source TaskHandle `u32`、reserved `u32 = 0`、
source TaskEpoch `u64`、CommitSequence `u64`、missed commits `u64`。`missed commits = 0`
表示没有观察到前序 gap；大于零时表示 `[observed - gap, observed)` 中不可见的 commit 数。

skipped range 必须非空且满足 `last = first + count - 1`，计算溢出拒绝。Record 的
commit after 只能等于 before 或加一。UTC、时间顺序、enum、capacity/counter 关系继续
服从 `r0-execution-semantics.md`。

## 4. Producer、Observe 与离线工具

周期 producer 在初始化期预分配 `TraceCapacity` 个固定 bytes 槽。每次事件先校验严格
连续的 `EventSequence`，再固定次数编码并且只执行一次非阻塞 `DropNewest` push；full
时消耗 sequence、增加 dropped/full，不读取或覆盖 consumer 槽。Observe endpoint 只有
非阻塞读取和统计 API，不具有写值、Force、暂停或调度权限。

host-only `aurora-build trace-decode <file>` 校验并输出逐项语义；
`aurora-build trace-compare <expected> <actual>` 在两个文件均完整通过校验后按 header 和
record 顺序精确比较，首个差异即失败。工具不连接 Runtime，也不把缺口补造成记录。
