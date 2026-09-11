//! R1-07 可复现的逐周期参考/AOT 差分模型。

use serde::Serialize;

use crate::{CanonicalFaultSiteId, CheckpointSiteId, RuntimeFaultCode, SymbolId, TaskHandle};

/// 一条按稳定 storage identity 排序的值快照。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct DifferentialValue {
    /// 拥有该值的 Task。
    pub task: TaskHandle,
    /// `u32::MAX` 表示 Program/global；其他值是静态 Function/FB activation。
    pub activation: u32,
    /// Canonical declaration identity。
    pub symbol: SymbolId,
    /// 完整 canonical little-endian bytes，包括固定 padding。
    pub bytes: Vec<u8>,
}

/// 一次运行 Fault；一个周期最多只能发布一条。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DifferentialFault {
    /// Canonical Fault site。
    pub site: CanonicalFaultSiteId,
    /// 本次实际发生的唯一 Fault code。
    pub code: RuntimeFaultCode,
}

/// 参考执行器和 AOT 共同使用的稳定运行诊断。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DifferentialDiagnostic {
    /// 产生诊断的周期序号，从 0 开始。
    pub cycle: u64,
    /// Canonical Fault site。
    pub site: CanonicalFaultSiteId,
    /// 对应 Preview 1.0 runtime Fault code。
    pub code: RuntimeFaultCode,
}

/// 一个 Task 周期的事务结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferentialStatus {
    /// ST 正常返回，可提交 staging。
    Completed,
    /// 一个显式 checkpoint 请求停止，staging 必须丢弃。
    CheckpointStop,
    /// 一个 ST Fault 已报告，staging 必须丢弃。
    Faulted,
}

/// 一个周期所有可比较观察值。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DifferentialCycle {
    /// 从 0 开始且连续的周期序号。
    pub cycle: u64,
    /// Task identity。
    pub task: TaskHandle,
    /// 事务状态。
    pub status: DifferentialStatus,
    /// 成功提交后或失败回滚后的持久 Task state。
    pub state: Vec<DifferentialValue>,
    /// 成功提交后或失败回滚后的该 Task 可写 Tag。
    pub outputs: Vec<DifferentialValue>,
    /// 本周期唯一 Fault，正常或 checkpoint-stop 周期为空。
    pub fault: Option<DifferentialFault>,
    /// 稳定有序运行诊断；Preview 1.0 Fault 周期恰好一条。
    pub diagnostics: Vec<DifferentialDiagnostic>,
    /// 实际到达的非 Task-return checkpoint，按发生顺序保存。
    pub checkpoints: Vec<CheckpointSiteId>,
}

/// 带固定 seed 的完整逐周期观察 Trace。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DifferentialTrace {
    /// 生成输入向量的固定 seed。
    pub seed: u64,
    /// 从周期 0 开始的连续观察值。
    pub cycles: Vec<DifferentialCycle>,
}

/// 首个差异的稳定分类；枚举顺序也是同周期内的比较优先级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DifferentialKind {
    /// 两侧输入 seed 不同，不能进行结果比较。
    Seed,
    /// 周期 identity 不同。
    CycleIdentity,
    /// 事务状态不同。
    Status,
    /// 持久 Task state 不同。
    State,
    /// 输出 Tag 不同。
    Output,
    /// Fault occurrence 不同。
    Fault,
    /// 运行诊断不同。
    Diagnostic,
    /// checkpoint occurrence 不同。
    Checkpoint,
    /// actual 少了一个周期。
    MissingCycle,
    /// actual 多了一个周期。
    UnexpectedCycle,
}

/// 确定性最小差异：保留到首个失败周期即足以复现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct DifferentialMismatch {
    /// 首个差异所在周期；seed 差异固定为 0。
    pub cycle: usize,
    /// 能复现差异的最短 Trace 前缀长度。
    pub minimal_cycle_count: usize,
    /// 首个差异分类。
    pub kind: DifferentialKind,
}

/// 按固定字段优先级比较两个逐周期 Trace，并返回最短失败前缀。
///
/// 函数不执行随机搜索、不修改输入，也不读取时间或环境状态；相同输入始终得到相同
/// [`DifferentialMismatch`]。
///
/// # Errors
///
/// 两侧 seed、周期数或任一周期观察值不同时，返回首个稳定差异。
pub fn compare_differential_traces(
    expected: &DifferentialTrace,
    actual: &DifferentialTrace,
) -> Result<(), DifferentialMismatch> {
    if expected.seed != actual.seed {
        return Err(mismatch(0, DifferentialKind::Seed));
    }
    let common = expected.cycles.len().min(actual.cycles.len());
    for index in 0..common {
        let left = &expected.cycles[index];
        let right = &actual.cycles[index];
        let kind = if left.cycle != right.cycle || left.task != right.task {
            Some(DifferentialKind::CycleIdentity)
        } else if left.status != right.status {
            Some(DifferentialKind::Status)
        } else if left.state != right.state {
            Some(DifferentialKind::State)
        } else if left.outputs != right.outputs {
            Some(DifferentialKind::Output)
        } else if left.fault != right.fault {
            Some(DifferentialKind::Fault)
        } else if left.diagnostics != right.diagnostics {
            Some(DifferentialKind::Diagnostic)
        } else if left.checkpoints != right.checkpoints {
            Some(DifferentialKind::Checkpoint)
        } else {
            None
        };
        if let Some(kind) = kind {
            return Err(mismatch(index, kind));
        }
    }
    if expected.cycles.len() > common {
        return Err(mismatch(common, DifferentialKind::MissingCycle));
    }
    if actual.cycles.len() > common {
        return Err(mismatch(common, DifferentialKind::UnexpectedCycle));
    }
    Ok(())
}

const fn mismatch(cycle: usize, kind: DifferentialKind) -> DifferentialMismatch {
    DifferentialMismatch {
        cycle,
        minimal_cycle_count: cycle.saturating_add(1),
        kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cycle(index: u64, value: u8) -> DifferentialCycle {
        DifferentialCycle {
            cycle: index,
            task: TaskHandle(7),
            status: DifferentialStatus::Completed,
            state: vec![DifferentialValue {
                task: TaskHandle(7),
                activation: u32::MAX,
                symbol: SymbolId(3),
                bytes: vec![value],
            }],
            outputs: Vec::new(),
            fault: None,
            diagnostics: Vec::new(),
            checkpoints: Vec::new(),
        }
    }

    #[test]
    fn first_difference_has_one_reproducible_minimal_prefix() {
        let expected = DifferentialTrace {
            seed: 42,
            cycles: vec![cycle(0, 1), cycle(1, 2), cycle(2, 3)],
        };
        let mut actual = expected.clone();
        actual.cycles[1].state[0].bytes[0] = 9;
        let first = compare_differential_traces(&expected, &actual);
        let second = compare_differential_traces(&expected, &actual);
        assert_eq!(first, second);
        assert_eq!(
            first,
            Err(DifferentialMismatch {
                cycle: 1,
                minimal_cycle_count: 2,
                kind: DifferentialKind::State,
            })
        );
    }
}
