# R3 I/O Guardian STRIDE 威胁模型

- 状态：Accepted for R3-00 specification
- 所有者：Caymir
- 评审日期：2026-09-20
- 范围：Guardian Contract、共享 I/O 映像、Driver Adapter、多后端选择/切换、现场帧和 Target Profile
- 不包含：R4 身份/签名实现、功能安全认证、远程 Connector、未批准第三方动态驱动

## 资产与信任边界

受保护资产是物理 I/O 排他所有权、active Fallback、Control lease、输入真实性、输出完整性、设备身份、
拓扑/PDO layout、后端制品与 Target Profile 证据。边界包括 Control↔Guardian 共享内存、Guardian↔静态
Driver、Guardian↔隔离 Driver Host、Driver↔现场设备、Target Agent↔槽外组件，以及不可信 ESI/SII、
网络/串口/CAN/LIN 帧、内核/驱动和上游依赖。

独立安全系统位于 Aurora 信任边界之外并具有最高输出优先级。Guardian Fallback 是普通运行保护，
不能作为安全 PLC、安全继电器或硬件联锁的替代。

## STRIDE 结论

| 类别 | 主要场景 | R3 控制 | 自动证据 | 剩余风险 |
| --- | --- | --- | --- | --- |
| Spoofing | 旧 Control/Driver Host 冒充当前 lease；伪造设备、拓扑或 backend | epoch+LeaseId+严格 sequence；peer/owner 校验；identity/topology/config/layout/source digest；候选 allowlist | 跨 epoch/lease、重复/乱序、identity mismatch 拒绝测试 | R4 才提供完整设备身份、证书和签名分发 |
| Tampering | 共享槽撕裂；修改 PDO/ESI/Fallback；替换 EtherCrab/IgH 制品 | generation+publication token Acquire/Release；签名配置摘要；active/pending；锁文件与系统维护包 | 黄金布局、争用、writer crash、digest mismatch、SBOM/来源门禁 | 共享内存映射与系统包签名实现在 R3/R4 后续项完成 |
| Repudiation | 操作者否认 backend 切换、Force 或恢复；丢失故障因果 | 切换请求携带 identity/reason/target/nonce/expiry；新 epoch/lease；固定诊断序列 | 重复/过期 nonce、状态机和 trace 闭包测试 | R4 防篡改审计 WAL 未实现前只能保留进程内/文件化测试证据 |
| Information Disclosure | 帧、ESI、设备身份或诊断泄漏 Secret/生产拓扑 | 契约禁止 Secret；最小设备权限；固定诊断字段；Driver Host 无 Control 私有内存权限 | Secret scanning、日志/报告字段检查 | 原始帧和设备拓扑仍属敏感运维数据，部署 ACL 在 R4 固化 |
| Denial of Service | 畸形长度、帧洪泛、慢设备、queue full、重连风暴、后端反复切换 | 全部容量/timeout/retry/recovery 有界；慢 group 隔离；饱和计数；Preview 禁止自动 backend 切换 | 长度/容量/queue/timeout、consumer stall、重复故障与切换频率拒绝测试 | 普通 Linux 调度、NIC/USB 固件和总线物理故障仍会产生不可消除尾延迟 |
| Elevation of Privilege | raw socket、io_uring/XDP、IgH kernel module/C FFI 或设备节点扩大权限 | systemd/Target Agent 最小授权；槽内无特权；IgH 默认隔离；每项 unsafe/FFI/module 单独审批 | capability/permission 拒绝、cargo-deny、模块签名/Secure Boot/内核矩阵 | 内核、NIC driver、IgH 模块和上游 crate 是高权限供应链面 |

## 双后端特有风险

- 同一 NIC 双主站是禁止状态。候选启动前必须证明旧 owner 已停止并释放 NIC；超时保持 Fallback。
- 后端切换不继承旧 lease、sequence、健康、性能或未确认写；成功也必须建立新 GuardianEpoch/LeaseId。
- 普通 Runtime 包不能安装槽外 backend。系统维护包可以并存安装多个获批 backend，但工程只选择
  preferred + allowlist；目标不在 allowlist、source/kernel/capability 不匹配时拒绝。
- Preview 1.0 不根据 jitter 或 fault 自动切换。人工/签名策略请求也必须先确认 Fallback，防止攻击者
  通过反复切换造成输出抖动、资源耗尽或保护窗口绕过。
- 回到旧后端同样是一次完整候选验证，不是恢复旧 session。切换失败不自动尝试列表中的下一项。

## R3 接受条件

- Control Engine 无法取得物理设备句柄，所有输出经当前 Guardian lease 和完整映像提交。
- 共享 ABI 的版本、offset、alignment、atomic、overflow、writer crash 和 contention 拒绝路径有测试。
- 每个协议的畸形、过龄、掉线、queue full 和恢复路径不能伪造 Good 或成功。
- EtherCrab/IgH 各自的依赖、许可证、权限、内核、Secure Boot 与更新回退证据独立，不互相沿用。
- Fallback、设备 watchdog 与外部安全保护边界在目标硬件上故障注入；任何缺失都阻止相关输出激活。
