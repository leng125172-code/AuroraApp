# ADR-0009：R3 Guardian、现场协议与 EtherCAT 后端边界

- 状态：Accepted
- 日期：2026-09-20
- 决策人：Caymir
- 关联需求/问题：GitHub Project `R3-00`、SPEC-R3-001、SPEC-R3-002、R-027～R-034、R-061、R-066

## 背景

R0～R2 已冻结周期任务、事务提交、Aurora ST、Cyclic Workflow 和逻辑 I/O handle。R3 在实现
Guardian、共享 I/O 映像和真实现场驱动前，必须固定物理 I/O 所有权、跨进程发布、租约、
Fallback、Driver Adapter、协议角色和 Target Profile 证据。否则不同驱动可能对相同的过期输出、
WKC、CRC、bus-off 或恢复请求产生不一致行为，Control Engine 也可能通过后端专用接口绕过 Guardian。

目标系统是普通 Ubuntu Linux x64 与 Rust `std`，不是 RT-Linux、RTOS 或功能安全系统。用户在
Dell Precision 7920 Tower 上提供的初步对比显示：IgH 最大观测约 `40 μs`，EtherCrab 通常约
`50 μs`、多数在 `50～80 μs`、最大观测约 `200 μs`。这些数据缺少完整硬件、内核、拓扑、样本量
和负载记录，只作为选型输入，不是性能门禁或平台保证。

## 备选方案

### EtherCAT 后端

- 只使用 IgH：尾延迟初测更小，但引入槽外内核模块、C FFI、DKMS/MOK、Secure Boot 与内核更新
  耦合，升级和回退证据面更大。
- 只把 EtherCrab API 暴露给 Guardian：集成简单，但会把 `0.x` 上游类型、异步模型和版本变化传播到
  Guardian Contract，未来无法在不破坏上层的情况下验证其他主站。
- Aurora EtherCAT Driver Adapter + 受控多后端：固定 Aurora 语义，EtherCrab 为首选，IgH 为获批的
  替代后端；系统可以同时安装两者，但每个 interface 只能独占激活一个，切换必须经过 Fallback、
  资源释放、候选重验、健康窗口和新租约。

### 上游同步

- 构建时直接拉取浮动分支：能最快获得上游变化，但同一 Aurora commit 会产生不同依赖和 Payload，
  无法复现、审计或回滚。
- 复制上游源码并长期维护私有分叉：构建独立，但同步冲突、许可证和安全修复责任最大。
- manifest 跟踪官方 `main`、锁文件固定审核过的 commit、显式同步提交：不硬编码上游 crate 版本，
  同时保持每个 Aurora commit 可复现。同步仍须重新执行依赖与目标硬件门禁。

## 决策

- 接受 [SPEC-R3-001](../../Sources/Contracts/io/v1/guardian-contract.md)、
  [协议矩阵](../../Sources/Contracts/io/v1/protocol-matrix.md) 和
  [SPEC-R3-002](../../Sources/Contracts/io/v1/target-profile.md) 为 R3 Preview 1.0 规范源。
- I/O Guardian 位于应用 A/B 槽外并排他控制设备。Control Engine 只能交换带 epoch、sequence、
  lease 和固定 layout digest 的有界 I/O 映像，不取得 NIC、socket、serial fd 或 CAN interface。
- Guardian Contract 独立版本化，至少支持 N/N-1。输入与输出各使用两个固定槽、单写者、发布 token
  和 slot generation；reader 只发布经前后校验的完整副本，争用时返回显式状态，不阻塞 writer。
- Driver Adapter 固定 identity、capability、configuration digest、静态 I/O layout、bounded work、
  deadline、quality、diagnostics、fault、fallback 和 recovery 语义。后端原生类型不得穿过 Adapter。
- EtherCrab 是首选后端。Aurora 依赖自己的 ESI/拓扑/PDO 审计，不把 EtherCrab 的 SII 自动配置当作
  完整工程校验。IgH 是受控替代后端，使用相同 Adapter 并默认进入 Guardian 管理的隔离 Driver Host；
  C FFI、内核模块、GPL/LGPL 组合、DKMS/MOK 和更新回退仍分别审批。
- 系统维护包可以并存安装已签名的 EtherCrab/IgH 后端；普通 Runtime `.aurpkg` 只能声明
  `preferredBackend` 与有序 allowlist，不能安装或更新槽外后端。每个 interface 同时只允许一个 owner。
- 后端切换不是 hot swap：Guardian 先进入并确认 Fallback、撤销 lease、停止旧后端并证明 NIC/设备已
  释放，再验证候选 source/kernel/capability/topology/PDO/Fallback，完成健康窗口后建立新 epoch/lease。
  默认只允许人工确认或已签名策略触发；不按 jitter/timeout 自动切换。
- 上游同步不允许浮动构建。未来 EtherCrab manifest 可以声明官方 `main`，但 `Cargo.lock` 必须保存
  审核过的 commit；更新只能通过可审查提交，重新运行许可证、advisory、ABI、仿真和目标硬件测试。
- R3 协议角色固定为 EtherCAT MainDevice、Modbus TCP Client、Modbus RTU Master、有界
  RS-485/RS-232、SocketCAN CAN 2.0/CAN FD raw transport 和 LIN controller。OPC UA、MQTT 5 与
  Sparkplug B 只消费后续 R5 数据契约，不进入 Guardian 或周期路径。
- 所有容量、周期、timeout、retry、恢复次数和性能预算由 Target Profile 显式提供；不存在平台默认
  合格线。Dell Precision 7920 Tower 是首个测试机系列，实际报告必须记录可复现的具体配置。

## 后果

- R3-01～R3-11 必须消费同一 Adapter、状态和质量语义，不能由具体协议后端反向修改 Guardian Contract。
- EtherCrab 的低内核耦合降低 Ubuntu/Secure Boot 维护成本，但其 DC、复杂拓扑、ESI 和尾延迟仍是
  明确风险；能力不足或超过预算时必须拒绝激活。
- 同一工程可以批准多个已安装后端，但每个后端具有独立 capability、依赖摘要和 Target Profile 证据，
  不能互相沿用。受控切换不恢复 Control 私有状态、不重放未确认写，且始终产生新 epoch/lease。
- R3-00 只冻结规范并增加 host-only 完整性门禁，不添加 EtherCrab/IgH 依赖，不创建产品驱动空壳，
  不批准 `unsafe`、C FFI 或内核模块。

## 验证

- host-only 门禁检查规范目录、角色矩阵、状态/错误目录、共享内存偏移、原子序和 Target Profile
  必填证据无遗漏或重复。
- R3-02 对共享内存执行大小、对齐、偏移、字节序、发布争用和撕裂拒绝黄金测试。
- R3-05 对 Adapter、单 owner 和受控切换执行相同仿真 trace 的跨后端契约测试；IgH 后端必须通过同一
  套测试后才可进入 allowlist。
- R3-06 在 Precision 7920 的精确配置上分别测量 EtherCrab 与 IgH 的 baseline/tuned 结果，并验证受控
  切换；覆盖 WKC/DC、掉站、断线、进程终止、内核更新、Secure Boot 和旧内核回退，不得以初步观测
  或另一后端的证据替代报告。
- R3-11 只对被测硬件、工程、内核、后端 commit 和调优配置给出结论，不声明硬实时或功能安全。
