//! Workflow Trace 固定 192-byte record 的有界非阻塞通道。

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use aurora_control_contracts::{
    EventSequence, TraceCapacity, WorkflowTraceCodecError, WorkflowTraceRecord,
    WorkflowTraceRecordBytes,
};
use aurora_types::BootEpochId;

use crate::{
    BoundedSpscConsumer, BoundedSpscProducer, SpscBuildError, SpscCapacity, SpscOverflowPolicy,
    SpscPopError, SpscPushError, SpscPushOutcome, SpscStatistics, bounded_spsc,
};

/// Workflow Trace channel 初始化错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTraceChannelBuildError {
    /// 当前平台不能无损表示容量。
    PlatformCapacityUnsupported {
        /// 契约声明的槽位数。
        capacity: u32,
    },
    /// 固定 SPSC 拒绝容量或布局。
    Spsc(SpscBuildError),
}

impl Display for WorkflowTraceChannelBuildError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Workflow Trace channel build error: {self:?}")
    }
}
impl Error for WorkflowTraceChannelBuildError {}

/// Workflow Trace 一次发布结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTracePublishOutcome {
    /// record 已进入 ring。
    Published(EventSequence),
    /// ring 已满，本次新 record 被丢弃且 sequence 已消耗。
    DroppedNewest(EventSequence),
}

/// Workflow Trace producer 拒绝原因。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTracePublishError {
    /// record `EngineEpoch` 与 channel 不同。
    EngineEpochMismatch {
        /// channel epoch。
        expected: BootEpochId,
        /// record epoch。
        actual: BootEpochId,
    },
    /// record 未使用严格下一 `EventSequence`。
    UnexpectedEventSequence {
        /// 应使用的 sequence；None 表示已耗尽。
        expected: Option<EventSequence>,
        /// record 实际 sequence。
        actual: EventSequence,
    },
    /// Observe endpoint 已析构；本次 API 尝试仍不会阻塞。
    ObserverDropped,
    /// SPSC sequence 与 record sequence 分歧。
    SequenceInvariantViolation,
}

impl Display for WorkflowTracePublishError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Workflow Trace publish error: {self:?}")
    }
}
impl Error for WorkflowTracePublishError {}

/// Observe 得到的 record 与其前序可见 gap。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowTraceObservation {
    record: WorkflowTraceRecord,
    missed_before: u64,
}

impl WorkflowTraceObservation {
    /// 返回已验证 record。
    #[must_use]
    pub const fn record(self) -> WorkflowTraceRecord {
        self.record
    }
    /// 返回本 record 前明确缺失的尝试数。
    #[must_use]
    pub const fn missed_before(self) -> u64 {
        self.missed_before
    }
}

/// Observe 非阻塞读取错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowTraceObserveError {
    /// ring 为空或 producer 生命周期结束。
    Spsc(SpscPopError),
    /// 固定 record bytes 无效。
    Codec(WorkflowTraceCodecError),
    /// ring sequence 与 record `EventSequence` 分歧。
    SequenceInvariantViolation,
}

impl Display for WorkflowTraceObserveError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Workflow Trace observe error: {self:?}")
    }
}
impl Error for WorkflowTraceObserveError {}

/// 周期线程唯一 Workflow Trace producer。
///
/// 每次发布固定编码一次并尝试一次 `DropNewest` push；不等待、不重试、不读取 consumer。
#[derive(Debug)]
pub struct WorkflowTracePublisher {
    inner: BoundedSpscProducer<WorkflowTraceRecordBytes>,
    engine_epoch: BootEpochId,
    next_event_sequence: Option<EventSequence>,
    observer_dropped_newest: u64,
    observer_drop_saturated: bool,
}

impl WorkflowTracePublisher {
    /// 返回下一次发布必须使用的全局 event sequence；`None` 表示计数已耗尽。
    ///
    /// 多个 task recorder 必须从同一个 publisher 取得序列，不能各自维护会重复的计数器。
    #[must_use]
    pub const fn next_event_sequence(&self) -> Option<EventSequence> {
        self.next_event_sequence
    }

    /// 尝试发布严格下一 `EventSequence`。
    ///
    /// # Errors
    /// identity、sequence、endpoint 或内部 sequence 不一致时拒绝。
    pub fn try_publish(
        &mut self,
        record: WorkflowTraceRecord,
    ) -> Result<WorkflowTracePublishOutcome, WorkflowTracePublishError> {
        if record.engine_epoch() != self.engine_epoch {
            return Err(WorkflowTracePublishError::EngineEpochMismatch {
                expected: self.engine_epoch,
                actual: record.engine_epoch(),
            });
        }
        let actual = record.event_sequence();
        if self.next_event_sequence != Some(actual) {
            return Err(WorkflowTracePublishError::UnexpectedEventSequence {
                expected: self.next_event_sequence,
                actual,
            });
        }
        // attempt identity 在通过准入后立即消耗；满 ring 不允许重试相同事件。
        self.next_event_sequence = actual.checked_next().ok();
        match self
            .inner
            .try_push(WorkflowTraceRecordBytes::encode(record))
        {
            Ok(SpscPushOutcome::Published(sequence)) if sequence.get() == actual.get() => {
                Ok(WorkflowTracePublishOutcome::Published(actual))
            }
            Ok(SpscPushOutcome::DroppedNewest(sequence)) if sequence.get() == actual.get() => {
                Ok(WorkflowTracePublishOutcome::DroppedNewest(actual))
            }
            Err(SpscPushError::ConsumerDropped(_)) => {
                let (next, overflowed) = self.observer_dropped_newest.overflowing_add(1);
                if overflowed {
                    self.observer_dropped_newest = u64::MAX;
                    self.observer_drop_saturated = true;
                } else {
                    self.observer_dropped_newest = next;
                }
                Err(WorkflowTracePublishError::ObserverDropped)
            }
            Ok(SpscPushOutcome::Published(_) | SpscPushOutcome::DroppedNewest(_))
            | Err(SpscPushError::Full(_) | SpscPushError::SequenceExhausted(_)) => {
                Err(WorkflowTracePublishError::SequenceInvariantViolation)
            }
        }
    }

    /// 返回累计计数和水位，不改变 producer。
    #[must_use]
    pub fn statistics(&self) -> SpscStatistics {
        let mut statistics = self.inner.statistics();
        let (dropped_newest, overflowed) = statistics
            .dropped_newest
            .overflowing_add(self.observer_dropped_newest);
        statistics.dropped_newest = if overflowed { u64::MAX } else { dropped_newest };
        statistics.saturated |= self.observer_drop_saturated || overflowed;
        statistics
    }
}

/// Workflow Trace 只读 Observe endpoint。
#[derive(Debug)]
pub struct WorkflowTraceObserver {
    inner: BoundedSpscConsumer<WorkflowTraceRecordBytes>,
}

impl WorkflowTraceObserver {
    /// 尝试读取一条 record；为空立即返回。
    ///
    /// # Errors
    /// ring 状态、codec 或 sequence 不一致时返回明确错误。
    pub fn try_observe(&mut self) -> Result<WorkflowTraceObservation, WorkflowTraceObserveError> {
        let read = self
            .inner
            .try_pop()
            .map_err(WorkflowTraceObserveError::Spsc)?;
        let record = read
            .value()
            .decode()
            .map_err(WorkflowTraceObserveError::Codec)?;
        if read.sequence().get() != record.event_sequence().get() {
            return Err(WorkflowTraceObserveError::SequenceInvariantViolation);
        }
        Ok(WorkflowTraceObservation {
            record,
            missed_before: read.missed_before(),
        })
    }

    /// 返回累计计数和水位，不改变 consumer。
    #[must_use]
    pub fn statistics(&self) -> SpscStatistics {
        self.inner.statistics()
    }
}

/// 初始化期预分配固定容量 Workflow Trace ring。
///
/// # Errors
/// 平台容量转换或 SPSC 固定布局失败时不返回部分 endpoint。
pub fn bounded_workflow_trace_channel(
    engine_epoch: BootEpochId,
    capacity: TraceCapacity,
) -> Result<(WorkflowTracePublisher, WorkflowTraceObserver), WorkflowTraceChannelBuildError> {
    let raw = usize::try_from(capacity.get()).map_err(|_| {
        WorkflowTraceChannelBuildError::PlatformCapacityUnsupported {
            capacity: capacity.get(),
        }
    })?;
    let capacity = SpscCapacity::new(raw, raw).map_err(WorkflowTraceChannelBuildError::Spsc)?;
    let (producer, consumer) = bounded_spsc(capacity, SpscOverflowPolicy::DropNewest)
        .map_err(WorkflowTraceChannelBuildError::Spsc)?;
    Ok((
        WorkflowTracePublisher {
            inner: producer,
            engine_epoch,
            next_event_sequence: Some(EventSequence::ZERO),
            observer_dropped_newest: 0,
            observer_drop_saturated: false,
        },
        WorkflowTraceObserver { inner: consumer },
    ))
}

#[cfg(test)]
#[path = "workflow_trace_channel_tests.rs"]
mod tests;
