//! 基于 `rtrb` 的进程内固定容量 SPSC 所有权转移与可观测溢出语义。
//!
//! R0 仅允许 `RejectNewest` 和 `DropNewest`。producer 不读取 consumer 槽，因此不会覆盖
//! 最旧值；latest-wins 数据使用 [`crate::SnapshotPublisher`] 的双槽发布。R5 的共享
//! 内存 `OverwriteOldest`、跨进程 ABI 和多消费者分发不在本模块边界内。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::{alloc::Layout, mem::size_of};

use rtrb::{Consumer, PopError, Producer, PushError, RingBuffer};

/// 经过 Target Profile 上限校验的非零 SPSC 槽位数。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpscCapacity(usize);

impl SpscCapacity {
    /// 创建不超过目标声明上限的固定容量。
    ///
    /// # Errors
    ///
    /// `value` 为零、超过 `maximum`，或超过 `rtrb` 双倍位置空间时返回
    /// [`SpscBuildError::InvalidCapacity`]。
    pub const fn new(value: usize, maximum: usize) -> Result<Self, SpscBuildError> {
        let implementation_maximum = usize::MAX / 2;
        let effective_maximum = if maximum < implementation_maximum {
            maximum
        } else {
            implementation_maximum
        };
        if value == 0 || value > effective_maximum {
            Err(SpscBuildError::InvalidCapacity {
                requested: value,
                maximum: effective_maximum,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// 返回固定槽位数。
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// SPSC 初始化错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpscBuildError {
    /// 请求容量为零或超过 Target Profile 上限。
    InvalidCapacity {
        /// 请求槽位数。
        requested: usize,
        /// Target Profile 与实现边界共同允许的最大槽位数。
        maximum: usize,
    },
    /// 容量虽在槽位范围内，但 `Sequenced<T>` 的完整 ring 分配布局不可表示。
    AllocationLayoutOverflow {
        /// 请求槽位数。
        capacity: usize,
        /// 单个带序列槽位的字节数。
        slot_size_bytes: usize,
    },
}

impl std::fmt::Display for SpscBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCapacity { requested, maximum } => write!(
                formatter,
                "SPSC capacity {requested} must be in the range 1..={maximum}"
            ),
            Self::AllocationLayoutOverflow {
                capacity,
                slot_size_bytes,
            } => write!(
                formatter,
                "SPSC allocation layout for {capacity} slots of {slot_size_bytes} bytes is not representable"
            ),
        }
    }
}

impl std::error::Error for SpscBuildError {}

/// Ring 满时的固定 producer 策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpscOverflowPolicy {
    /// 保留 item 所有权并返回 `Full`，不消耗 sequence，调用方可在非周期路径决定重试。
    RejectNewest,
    /// 丢弃本次新 item、消耗 sequence 并累计 drop；不读取或覆盖 consumer 槽。
    DropNewest,
}

/// 一次成功接收或显式丢弃的尝试序列；永不回绕。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpscSequence(u64);

impl SpscSequence {
    /// 返回固定宽度序列值。
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// producer 一次非阻塞 push 的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpscPushOutcome {
    /// item 已发布到 ring。
    Published(SpscSequence),
    /// ring 已满，`DropNewest` 丢弃该 item。
    DroppedNewest(SpscSequence),
}

/// producer 拒绝 push 时返回 item 所有权的错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpscPushError<T: Copy> {
    /// `RejectNewest` 策略观察到满队列；sequence 未消耗。
    Full(T),
    /// consumer 已析构；item 未进入 ring。
    ConsumerDropped(T),
    /// 尝试序列已经使用到 `u64::MAX`；item 未进入 ring。
    SequenceExhausted(T),
}

impl<T: Copy> std::fmt::Display for SpscPushError<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Full(_) => formatter.write_str("SPSC ring is full"),
            Self::ConsumerDropped(_) => formatter.write_str("SPSC consumer was dropped"),
            Self::SequenceExhausted(_) => formatter.write_str("SPSC sequence is exhausted"),
        }
    }
}

impl<T: Copy + std::fmt::Debug> std::error::Error for SpscPushError<T> {}

/// consumer 非阻塞 pop 的错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpscPopError {
    /// ring 当前为空，但 producer 仍可能继续发布。
    Empty,
    /// ring 已排空且 producer 已析构。
    ProducerDropped,
    /// ring 内序列回退；这表示依赖契约被破坏。
    SequenceRegression,
}

impl std::fmt::Display for SpscPopError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => formatter.write_str("SPSC ring is empty"),
            Self::ProducerDropped => formatter.write_str("SPSC producer was dropped"),
            Self::SequenceRegression => formatter.write_str("SPSC sequence regressed"),
        }
    }
}

impl std::error::Error for SpscPopError {}

/// 一个带序列与显式前序缺口的 consumer item。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpscRead<T: Copy> {
    sequence: SpscSequence,
    missed_before: u64,
    value: T,
}

impl<T: Copy> SpscRead<T> {
    /// 返回该 item 的尝试序列。
    #[must_use]
    pub const fn sequence(&self) -> SpscSequence {
        self.sequence
    }

    /// 返回自上次成功 pop 后明确缺失的 `DropNewest` item 数。
    #[must_use]
    pub const fn missed_before(&self) -> u64 {
        self.missed_before
    }

    /// 返回 Copy item。
    #[must_use]
    pub const fn value(&self) -> T {
        self.value
    }
}

/// 可从 producer 或 consumer 非阻塞读取的累计统计快照。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpscStatistics {
    /// 固定槽位数。
    pub capacity: usize,
    /// 本次观测可读槽位数。
    pub readable: usize,
    /// 本次观测可写槽位数。
    pub writable: usize,
    /// producer API 调用总数，包含成功、full、abandoned 和 sequence exhausted。
    pub push_attempts: u64,
    /// 成功进入 ring 的 item 数。
    pub published: u64,
    /// `DropNewest` 主动丢弃的新 item 数。
    pub dropped_newest: u64,
    /// `RejectNewest` 因满队列退回的 item 数。
    pub rejected_full: u64,
    /// 两种静态策略合计观察到的 full 次数。
    pub full: u64,
    /// consumer API 调用总数，包含成功、empty 和 abandoned。
    pub pop_attempts: u64,
    /// consumer 成功 pop 的 item 数。
    pub consumed: u64,
    /// consumer 从序列中确认的缺失 item 总数。
    pub observed_sequence_gaps: u64,
    /// producer 观察到的历史最大占用槽位数。
    pub high_water_mark: usize,
    /// producer endpoint 是否已经析构。
    pub producer_dropped: bool,
    /// consumer endpoint 是否已经析构。
    pub consumer_dropped: bool,
    /// 任一统计计数是否已在最大值饱和。
    pub saturated: bool,
}

/// SPSC 唯一 producer；不可复制且 push 需要独占可变借用。
///
/// 每次 push 只调用一次 `rtrb::Producer::push`，随后常数次统计更新；不分配、不等待、
/// 不重试。`T: Copy` 排除了在周期 producer 上执行自定义析构的隐藏工作。
#[derive(Debug)]
pub struct BoundedSpscProducer<T: Copy> {
    inner: Producer<Sequenced<T>>,
    shared_statistics: Arc<SharedStatistics>,
    capacity: SpscCapacity,
    policy: SpscOverflowPolicy,
    next_sequence: Option<u64>,
    push_attempts: u64,
    published: u64,
    dropped_newest: u64,
    rejected_full: u64,
    full: u64,
    high_water_mark: usize,
}

impl<T: Copy> BoundedSpscProducer<T> {
    /// 尝试发布一次，不等待 consumer，也不在满队列时重试。
    ///
    /// # Errors
    ///
    /// `RejectNewest` 满队列、consumer 已析构或 sequence 耗尽时返回原 item。
    pub fn try_push(&mut self, value: T) -> Result<SpscPushOutcome, SpscPushError<T>> {
        increment_local(
            &mut self.push_attempts,
            &self.shared_statistics.push_attempts,
            &self.shared_statistics.saturated,
        );
        let Some(sequence) = self.next_sequence else {
            return Err(SpscPushError::SequenceExhausted(value));
        };
        if self.inner.is_abandoned() {
            return Err(SpscPushError::ConsumerDropped(value));
        }
        let sequenced = Sequenced { sequence, value };
        match self.inner.push(sequenced) {
            Ok(()) => {
                self.advance_sequence(sequence);
                increment_local(
                    &mut self.published,
                    &self.shared_statistics.published,
                    &self.shared_statistics.saturated,
                );
                let occupancy = self.capacity.get() - self.inner.slots();
                if occupancy > self.high_water_mark {
                    self.high_water_mark = occupancy;
                    self.shared_statistics
                        .high_water_mark
                        .store(occupancy, Ordering::Release);
                }
                Ok(SpscPushOutcome::Published(SpscSequence(sequence)))
            }
            Err(PushError::Full(returned)) => match self.policy {
                SpscOverflowPolicy::RejectNewest => {
                    self.record_full();
                    increment_local(
                        &mut self.rejected_full,
                        &self.shared_statistics.rejected_full,
                        &self.shared_statistics.saturated,
                    );
                    Err(SpscPushError::Full(returned.value))
                }
                SpscOverflowPolicy::DropNewest => {
                    self.record_full();
                    self.advance_sequence(sequence);
                    increment_local(
                        &mut self.dropped_newest,
                        &self.shared_statistics.dropped_newest,
                        &self.shared_statistics.saturated,
                    );
                    Ok(SpscPushOutcome::DroppedNewest(SpscSequence(sequence)))
                }
            },
        }
    }

    /// 返回当前统计的 Acquire 快照。
    #[must_use]
    pub fn statistics(&self) -> SpscStatistics {
        let writable = self.inner.slots();
        self.shared_statistics.snapshot(
            self.capacity,
            self.capacity.get() - writable,
            writable,
            false,
            self.inner.is_abandoned(),
        )
    }

    /// 返回 consumer 是否已经析构。
    #[must_use]
    pub fn consumer_dropped(&self) -> bool {
        self.inner.is_abandoned()
    }

    fn advance_sequence(&mut self, used: u64) {
        self.next_sequence = used.checked_add(1);
    }

    fn record_full(&mut self) {
        increment_local(
            &mut self.full,
            &self.shared_statistics.full,
            &self.shared_statistics.saturated,
        );
    }
}

/// SPSC 唯一 consumer；不可复制且 pop 需要独占可变借用。
#[derive(Debug)]
pub struct BoundedSpscConsumer<T: Copy> {
    inner: Consumer<Sequenced<T>>,
    shared_statistics: Arc<SharedStatistics>,
    capacity: SpscCapacity,
    previous_sequence: Option<u64>,
    pop_attempts: u64,
    consumed: u64,
    observed_sequence_gaps: u64,
}

impl<T: Copy> BoundedSpscConsumer<T> {
    /// 尝试取出一个 item；为空时立即返回，不等待 producer。
    ///
    /// # Errors
    ///
    /// 返回 Empty、已析构且排空的 producer，或不应出现的 sequence 回退。
    pub fn try_pop(&mut self) -> Result<SpscRead<T>, SpscPopError> {
        increment_local(
            &mut self.pop_attempts,
            &self.shared_statistics.pop_attempts,
            &self.shared_statistics.saturated,
        );
        let item = match self.inner.pop() {
            Ok(item) => item,
            Err(PopError::Empty) if self.inner.is_abandoned() => {
                return Err(SpscPopError::ProducerDropped);
            }
            Err(PopError::Empty) => return Err(SpscPopError::Empty),
        };
        let missed_before = match self.previous_sequence {
            Some(previous) => {
                let expected = previous
                    .checked_add(1)
                    .ok_or(SpscPopError::SequenceRegression)?;
                item.sequence
                    .checked_sub(expected)
                    .ok_or(SpscPopError::SequenceRegression)?
            }
            None => item.sequence,
        };
        self.previous_sequence = Some(item.sequence);
        increment_local(
            &mut self.consumed,
            &self.shared_statistics.consumed,
            &self.shared_statistics.saturated,
        );
        increment_local_by(
            &mut self.observed_sequence_gaps,
            missed_before,
            &self.shared_statistics.observed_sequence_gaps,
            &self.shared_statistics.saturated,
        );
        Ok(SpscRead {
            sequence: SpscSequence(item.sequence),
            missed_before,
            value: item.value,
        })
    }

    /// 返回当前统计的 Acquire 快照。
    #[must_use]
    pub fn statistics(&self) -> SpscStatistics {
        let readable = self.inner.slots();
        self.shared_statistics.snapshot(
            self.capacity,
            readable,
            self.capacity.get() - readable,
            self.inner.is_abandoned(),
            false,
        )
    }

    /// 返回 producer 是否已经析构；ring 中可能仍有尚未 pop 的 item。
    #[must_use]
    pub fn producer_dropped(&self) -> bool {
        self.inner.is_abandoned()
    }
}

/// 在初始化期创建一对固定容量、wait-free 的 SPSC endpoints。
///
/// `rtrb = 0.4.0` 只负责进程内单生产者/单消费者所有权转移。构造函数完成唯一一次
/// ring 与统计分配；两个 endpoint 后续均不会增长容量。调用依赖前会验证
/// `Sequenced<T>` 的完整分配布局，避免容量算术或 `Vec` 布局 panic。
///
/// # Errors
///
/// 容量对应的完整槽位布局无法安全表示时返回
/// [`SpscBuildError::AllocationLayoutOverflow`]。
pub fn bounded_spsc<T: Copy>(
    capacity: SpscCapacity,
    policy: SpscOverflowPolicy,
) -> Result<(BoundedSpscProducer<T>, BoundedSpscConsumer<T>), SpscBuildError> {
    Layout::array::<Sequenced<T>>(capacity.get()).map_err(|_| {
        SpscBuildError::AllocationLayoutOverflow {
            capacity: capacity.get(),
            slot_size_bytes: size_of::<Sequenced<T>>(),
        }
    })?;
    let (producer, consumer) = RingBuffer::new(capacity.get());
    let shared_statistics = Arc::new(SharedStatistics::default());
    Ok((
        BoundedSpscProducer {
            inner: producer,
            shared_statistics: Arc::clone(&shared_statistics),
            capacity,
            policy,
            next_sequence: Some(0),
            push_attempts: 0,
            published: 0,
            dropped_newest: 0,
            rejected_full: 0,
            full: 0,
            high_water_mark: 0,
        },
        BoundedSpscConsumer {
            inner: consumer,
            shared_statistics,
            capacity,
            previous_sequence: None,
            pop_attempts: 0,
            consumed: 0,
            observed_sequence_gaps: 0,
        },
    ))
}

#[derive(Debug, Clone, Copy)]
struct Sequenced<T: Copy> {
    sequence: u64,
    value: T,
}

#[derive(Debug, Default)]
struct SharedStatistics {
    push_attempts: AtomicU64,
    published: AtomicU64,
    dropped_newest: AtomicU64,
    rejected_full: AtomicU64,
    full: AtomicU64,
    pop_attempts: AtomicU64,
    consumed: AtomicU64,
    observed_sequence_gaps: AtomicU64,
    high_water_mark: AtomicUsize,
    saturated: AtomicBool,
}

impl SharedStatistics {
    fn snapshot(
        &self,
        capacity: SpscCapacity,
        readable: usize,
        writable: usize,
        producer_dropped: bool,
        consumer_dropped: bool,
    ) -> SpscStatistics {
        SpscStatistics {
            capacity: capacity.get(),
            readable,
            writable,
            push_attempts: self.push_attempts.load(Ordering::Acquire),
            published: self.published.load(Ordering::Acquire),
            dropped_newest: self.dropped_newest.load(Ordering::Acquire),
            rejected_full: self.rejected_full.load(Ordering::Acquire),
            full: self.full.load(Ordering::Acquire),
            pop_attempts: self.pop_attempts.load(Ordering::Acquire),
            consumed: self.consumed.load(Ordering::Acquire),
            observed_sequence_gaps: self.observed_sequence_gaps.load(Ordering::Acquire),
            high_water_mark: self.high_water_mark.load(Ordering::Acquire),
            producer_dropped,
            consumer_dropped,
            saturated: self.saturated.load(Ordering::Acquire),
        }
    }
}

fn increment_local(local: &mut u64, shared: &AtomicU64, saturated: &AtomicBool) {
    increment_local_by(local, 1, shared, saturated);
}

fn increment_local_by(local: &mut u64, increment: u64, shared: &AtomicU64, saturated: &AtomicBool) {
    if let Some(next) = local.checked_add(increment) {
        *local = next;
        shared.store(next, Ordering::Release);
    } else {
        *local = u64::MAX;
        shared.store(u64::MAX, Ordering::Release);
        saturated.store(true, Ordering::Release);
    }
}

#[cfg(test)]
#[path = "bounded_spsc_tests.rs"]
mod tests;
