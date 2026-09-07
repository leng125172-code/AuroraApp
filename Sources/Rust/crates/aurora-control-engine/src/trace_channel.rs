//! 固定宽度 R0 Trace 的预分配、单 producer/Observe consumer 非阻塞通道。

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use aurora_control_contracts::{
    EventSequence, TraceCapacity, TraceCodecError, TraceRecord, TraceRecordBytes,
};
use aurora_types::BootEpochId;

use crate::{
    BoundedSpscConsumer, BoundedSpscProducer, SpscBuildError, SpscCapacity, SpscOverflowPolicy,
    SpscPopError, SpscPushError, SpscPushOutcome, SpscStatistics, bounded_spsc,
};

/// Trace channel 初始化错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceChannelBuildError {
    /// 当前平台无法无损表示 `u32` Trace 容量。
    PlatformCapacityUnsupported {
        /// Trace contract 声明的槽位数。
        capacity: u32,
    },
    /// 固定 ring 布局或容量被底层有界 SPSC 拒绝。
    Spsc(SpscBuildError),
}

impl Display for TraceChannelBuildError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Trace channel build error: {self:?}")
    }
}

impl Error for TraceChannelBuildError {}

/// 周期 producer 的一次确定性发布结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracePublishOutcome {
    /// 固定 record 已进入 ring。
    Published(EventSequence),
    /// ring 已满，固定 `DropNewest` 策略丢弃本 record，但消耗 event sequence。
    DroppedNewest(EventSequence),
}

/// Trace producer 拒绝输入或 endpoint 失效。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TracePublishError {
    /// record 不属于 channel 启动时冻结的 engine epoch。
    EngineEpochMismatch {
        /// channel engine epoch。
        expected: BootEpochId,
        /// record engine epoch。
        actual: BootEpochId,
    },
    /// record 没有使用本 engine epoch 严格连续的下一 `EventSequence`。
    UnexpectedEventSequence {
        /// 当前应使用的 sequence；`None` 表示已经耗尽。
        expected: Option<EventSequence>,
        /// record 携带的 sequence。
        actual: EventSequence,
    },
    /// Observe consumer 已析构；本次尝试仍消耗 `EventSequence` 并保留统计。
    ObserverDropped,
    /// 底层与 Trace sequence 出现不应发生的分歧。
    SequenceInvariantViolation,
}

impl Display for TracePublishError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Trace publish error: {self:?}")
    }
}

impl Error for TracePublishError {}

/// Observe 一次读取到的 record 及其前序缺口。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TraceObservation {
    record: TraceRecord,
    missed_before: u64,
}

impl TraceObservation {
    /// 返回已经完成固定布局验证的语义 record。
    #[must_use]
    pub const fn record(self) -> TraceRecord {
        self.record
    }

    /// 返回 `[上次可见 sequence + 1, 当前 sequence)` 的缺失数量。
    #[must_use]
    pub const fn missed_before(self) -> u64 {
        self.missed_before
    }
}

/// Observe consumer 的非阻塞读取错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceObserveError {
    /// ring 当前为空或 producer 生命周期结束。
    Spsc(SpscPopError),
    /// ring 中固定 bytes 未通过布局验证。
    Codec(TraceCodecError),
    /// ring sequence 与 record `EventSequence` 不一致。
    SequenceInvariantViolation,
}

impl Display for TraceObserveError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(formatter, "Trace observe error: {self:?}")
    }
}

impl Error for TraceObserveError {}

/// 周期线程唯一 Trace producer。
///
/// 初始化后每次调用只做一次固定 320-byte 编码和一次 `DropNewest` push；不分配、
/// 不格式化、不重试、不等待，也不读取 consumer 槽。
#[derive(Debug)]
pub struct TracePublisher {
    inner: BoundedSpscProducer<TraceRecordBytes>,
    engine_epoch: BootEpochId,
    next_event_sequence: Option<EventSequence>,
}

impl TracePublisher {
    /// 尝试发布一个使用严格下一 `EventSequence` 的 record。
    ///
    /// # Errors
    ///
    /// 重复/缺口 sequence、Observe endpoint 析构或内部 sequence 分歧时显式拒绝。
    pub fn try_publish(
        &mut self,
        record: TraceRecord,
    ) -> Result<TracePublishOutcome, TracePublishError> {
        if record.engine_epoch() != self.engine_epoch {
            return Err(TracePublishError::EngineEpochMismatch {
                expected: self.engine_epoch,
                actual: record.engine_epoch(),
            });
        }
        let actual = record.event_sequence();
        if self.next_event_sequence != Some(actual) {
            return Err(TracePublishError::UnexpectedEventSequence {
                expected: self.next_event_sequence,
                actual,
            });
        }
        // EventSequence 属于尝试，不属于成功入队；通过准入后即消耗，禁止重试同一项。
        self.next_event_sequence = actual.checked_next().ok();
        let encoded = TraceRecordBytes::encode(record);
        match self.inner.try_push(encoded) {
            Ok(SpscPushOutcome::Published(sequence)) if sequence.get() == actual.get() => {
                Ok(TracePublishOutcome::Published(actual))
            }
            Ok(SpscPushOutcome::DroppedNewest(sequence)) if sequence.get() == actual.get() => {
                Ok(TracePublishOutcome::DroppedNewest(actual))
            }
            Ok(SpscPushOutcome::Published(_) | SpscPushOutcome::DroppedNewest(_))
            | Err(SpscPushError::Full(_) | SpscPushError::SequenceExhausted(_)) => {
                Err(TracePublishError::SequenceInvariantViolation)
            }
            Err(SpscPushError::ConsumerDropped(_)) => Err(TracePublishError::ObserverDropped),
        }
    }

    /// 返回不改变 producer 的累计计数和当前水位。
    #[must_use]
    pub fn statistics(&self) -> SpscStatistics {
        self.inner.statistics()
    }
}

/// 只读 Observe endpoint。
///
/// API 仅提供非阻塞读取和统计；没有写值、Force、暂停或调度控制能力。
///
/// ```compile_fail
/// # fn cannot_force(mut observer: aurora_control_engine::TraceObserver) {
/// observer.force();
/// # }
/// ```
#[derive(Debug)]
pub struct TraceObserver {
    inner: BoundedSpscConsumer<TraceRecordBytes>,
}

impl TraceObserver {
    /// 尝试读取一个 record；为空时立即返回，不轮询或等待 producer。
    ///
    /// # Errors
    ///
    /// 返回空/abandoned 状态、固定 bytes 解码错误或 sequence 分歧。
    pub fn try_observe(&mut self) -> Result<TraceObservation, TraceObserveError> {
        let read = self.inner.try_pop().map_err(TraceObserveError::Spsc)?;
        let record = read.value().decode().map_err(TraceObserveError::Codec)?;
        if read.sequence().get() != record.event_sequence().get() {
            return Err(TraceObserveError::SequenceInvariantViolation);
        }
        Ok(TraceObservation {
            record,
            missed_before: read.missed_before(),
        })
    }

    /// 返回不改变 consumer 的累计计数和当前水位。
    #[must_use]
    pub fn statistics(&self) -> SpscStatistics {
        self.inner.statistics()
    }
}

/// 在启动期预分配固定容量 Trace ring，并冻结 `DropNewest` 策略。
///
/// # Errors
///
/// 平台无法表示容量或底层固定布局无法分配时拒绝启动，不返回部分 endpoint。
pub fn bounded_trace_channel(
    engine_epoch: BootEpochId,
    capacity: TraceCapacity,
) -> Result<(TracePublisher, TraceObserver), TraceChannelBuildError> {
    let value = usize::try_from(capacity.get()).map_err(|_| {
        TraceChannelBuildError::PlatformCapacityUnsupported {
            capacity: capacity.get(),
        }
    })?;
    let spsc_capacity = SpscCapacity::new(value, value).map_err(TraceChannelBuildError::Spsc)?;
    let (producer, consumer) = bounded_spsc(spsc_capacity, SpscOverflowPolicy::DropNewest)
        .map_err(TraceChannelBuildError::Spsc)?;
    Ok((
        TracePublisher {
            inner: producer,
            engine_epoch,
            next_event_sequence: Some(EventSequence::ZERO),
        },
        TraceObserver { inner: consumer },
    ))
}

#[cfg(test)]
#[path = "trace_channel_tests.rs"]
mod tests;
