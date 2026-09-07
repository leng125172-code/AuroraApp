//! 单写者发布、多个 reader 独立锁存的进程内固定容量快照通道。

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{
    AtomicBool, AtomicI64, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence,
};

use aurora_control_contracts::{
    CommitSequence, ExecutionContractError, ExecutionContractVersion, ReleaseSequence,
    SnapshotMetadata, SnapshotObservation, SnapshotProgress, TaskEpoch, UtcObservation,
};
use aurora_types::{
    BootEpochId, MonotonicTimestamp, QualityCode, TimeQuality, TimeQualityState, TimeSource,
    UtcTimestamp,
};

/// 快照 payload 的非零固定容量，单位为字节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SnapshotPayloadCapacity(usize);

impl SnapshotPayloadCapacity {
    /// 校验工程请求容量不超过 Target Profile 声明上限和契约的 `u32` 长度边界。
    ///
    /// # Errors
    ///
    /// 零容量、超过 `maximum` 或无法写入 `SnapshotMetadata.payload_length` 时拒绝。
    pub fn new(value: usize, maximum: usize) -> Result<Self, SnapshotChannelError> {
        if value == 0 || value > maximum || u32::try_from(value).is_err() {
            Err(SnapshotChannelError::InvalidCapacity {
                requested: value,
                maximum,
            })
        } else {
            Ok(Self(value))
        }
    }

    /// 返回固定 payload 字节数上限。
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// 初始化期冻结的快照来源定义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotChannelDefinition {
    version: ExecutionContractVersion,
    engine_epoch: BootEpochId,
    schema_hash: [u8; 32],
    payload_capacity: SnapshotPayloadCapacity,
}

impl SnapshotChannelDefinition {
    /// 创建一个不允许在运行期改变布局的快照来源定义。
    #[must_use]
    pub const fn new(
        version: ExecutionContractVersion,
        engine_epoch: BootEpochId,
        schema_hash: [u8; 32],
        payload_capacity: SnapshotPayloadCapacity,
    ) -> Self {
        Self {
            version,
            engine_epoch,
            schema_hash,
            payload_capacity,
        }
    }

    /// 返回固定 payload 容量。
    #[must_use]
    pub const fn payload_capacity(self) -> SnapshotPayloadCapacity {
        self.payload_capacity
    }
}

/// 快照通道初始化、发布或锁存错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotChannelError {
    /// payload 容量为零、超过 Target Profile 上限或超过契约长度表示范围。
    InvalidCapacity {
        /// 请求字节数。
        requested: usize,
        /// Target Profile 允许的最大字节数。
        maximum: usize,
    },
    /// 初始化期无法预分配双槽或 reader 私有双副本。
    AllocationFailed {
        /// 单个 payload 的固定容量。
        payload_capacity: SnapshotPayloadCapacity,
    },
    /// 发布 payload 超过固定容量。
    PayloadTooLarge {
        /// 实际 payload 字节数。
        actual: usize,
        /// 固定容量。
        capacity: SnapshotPayloadCapacity,
    },
    /// metadata 声明长度与实际 payload 不同。
    PayloadLengthMismatch {
        /// metadata 声明长度。
        declared: u32,
        /// 实际 payload 字节数。
        actual: usize,
    },
    /// metadata 不属于本通道冻结的版本、epoch 或 schema。
    DefinitionMismatch,
    /// 发布版本回退、重复或违反 task epoch 规则。
    InvalidPublicationOrder(ExecutionContractError),
    /// 发布代际已经耗尽；通道拒绝回绕和后续发布。
    PublicationGenerationExhausted,
    /// 尚未发布过完整快照。
    NoPublication,
    /// writer 在 reader 复制期间发布了更新；本次 staging 副本被丢弃。
    Contended,
    /// 稳定代际中的内部字段不能重建为有效契约。
    InvalidPublishedMetadata(ExecutionContractError),
    /// reader 的观测时间不属于当前 engine epoch 或早于发布时间。
    InvalidObservationTime(ExecutionContractError),
}

impl Display for SnapshotChannelError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity { requested, maximum } => write!(
                formatter,
                "snapshot payload capacity {requested} must be in 1..={maximum} bytes and fit u32"
            ),
            Self::AllocationFailed { payload_capacity } => write!(
                formatter,
                "failed to preallocate snapshot buffers of {} bytes",
                payload_capacity.get()
            ),
            Self::PayloadTooLarge { actual, capacity } => write!(
                formatter,
                "snapshot payload has {actual} bytes, exceeding capacity {}",
                capacity.get()
            ),
            Self::PayloadLengthMismatch { declared, actual } => write!(
                formatter,
                "snapshot metadata declares {declared} bytes but payload has {actual}"
            ),
            Self::DefinitionMismatch => {
                formatter.write_str("snapshot metadata does not match the channel definition")
            }
            Self::InvalidPublicationOrder(error) => {
                write!(formatter, "snapshot publication order is invalid: {error}")
            }
            Self::PublicationGenerationExhausted => {
                formatter.write_str("snapshot publication generation is exhausted")
            }
            Self::NoPublication => formatter.write_str("no snapshot has been published"),
            Self::Contended => formatter.write_str("snapshot changed while the reader copied it"),
            Self::InvalidPublishedMetadata(error) => {
                write!(formatter, "published snapshot metadata is invalid: {error}")
            }
            Self::InvalidObservationTime(error) => {
                write!(formatter, "snapshot observation time is invalid: {error}")
            }
        }
    }
}

impl Error for SnapshotChannelError {}

/// 单个 reader 相对上次成功锁存所观察到的发布进度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotLatchProgress {
    /// 该 reader 第一次成功锁存。
    First,
    /// 发布代际未变化；返回 reader 已有的私有副本。
    Unchanged,
    /// metadata 相对该 reader 上次接受版本的显式进度。
    Advanced(SnapshotProgress),
}

/// reader 本地统计；所有计数在 `u64::MAX` 饱和并设置 `saturated`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotReaderStatistics {
    /// 成功接受的新发布数。
    pub accepted_publications: u64,
    /// 未变化读取次数。
    pub unchanged_reads: u64,
    /// 因并发发布而丢弃 staging 的次数。
    pub contended_reads: u64,
    /// 该 reader 明确检测到的缺失 commit 总数。
    pub missed_commits: u64,
    /// 任一计数是否已经饱和。
    pub saturated: bool,
}

impl SnapshotReaderStatistics {
    const ZERO: Self = Self {
        accepted_publications: 0,
        unchanged_reads: 0,
        contended_reads: 0,
        missed_commits: 0,
        saturated: false,
    };
}

/// 一个 reader 私有且在下一次成功锁存前稳定的完整快照视图。
#[derive(Debug, Clone, Copy)]
pub struct LatchedSnapshot<'reader> {
    publication_generation: u64,
    metadata: SnapshotMetadata,
    observation: SnapshotObservation,
    observed_quality: QualityCode,
    progress: SnapshotLatchProgress,
    payload: &'reader [u8],
}

impl LatchedSnapshot<'_> {
    /// 返回通道内部不回绕的发布代际。
    #[must_use]
    pub const fn publication_generation(&self) -> u64 {
        self.publication_generation
    }

    /// 返回该完整 payload 对应的契约 metadata。
    #[must_use]
    pub const fn metadata(&self) -> SnapshotMetadata {
        self.metadata
    }

    /// 返回 reader 本地的年龄和 Fresh/Stale 判断。
    #[must_use]
    pub const fn observation(&self) -> SnapshotObservation {
        self.observation
    }

    /// 返回叠加 reader 本地 `Stale` 标志后的质量。
    #[must_use]
    pub const fn observed_quality(&self) -> QualityCode {
        self.observed_quality
    }

    /// 返回该 reader 相对上次成功锁存的进度或缺口。
    #[must_use]
    pub const fn progress(&self) -> SnapshotLatchProgress {
        self.progress
    }

    /// 返回 reader 私有预分配副本中的有效 payload。
    #[must_use]
    pub const fn payload(&self) -> &[u8] {
        self.payload
    }
}

/// 唯一 writer 端；类型不可复制，`publish` 还要求独占可变借用。
///
/// 构造和 [`Self::create_reader`] 仅允许在初始化路径调用。周期路径中的 `publish`
/// 最多写 `payload_capacity` 个 `AtomicU8`，不分配、不等待、不执行 I/O。每次发布把
/// 完整 staging 槽通过 `Release` 代际发布；reader 用 `Acquire` 锁存。
#[derive(Debug)]
pub struct SnapshotPublisher {
    shared: Arc<SharedSnapshot>,
    last_metadata: Option<SnapshotMetadata>,
    descriptor_generation: u64,
    slot_generations: [u64; 2],
    published_slot: usize,
}

impl SnapshotPublisher {
    /// 在初始化期预分配两个共享原子槽。
    ///
    /// # Errors
    ///
    /// 内存无法按固定容量预留时返回 [`SnapshotChannelError::AllocationFailed`]。
    pub fn new(definition: SnapshotChannelDefinition) -> Result<Self, SnapshotChannelError> {
        let first = AtomicSnapshotSlot::new(definition.payload_capacity)?;
        let second = AtomicSnapshotSlot::new(definition.payload_capacity)?;
        Ok(Self {
            shared: Arc::new(SharedSnapshot {
                definition,
                slots: [first, second],
                descriptor: AtomicSnapshotDescriptor::new(),
            }),
            last_metadata: None,
            descriptor_generation: 0,
            slot_generations: [0; 2],
            published_slot: 1,
        })
    }

    /// 在初始化期为一个 reader 预分配两个私有副本；reader 停止消费不持有共享槽。
    ///
    /// # Errors
    ///
    /// 私有双副本无法预留时返回 [`SnapshotChannelError::AllocationFailed`]。
    pub fn create_reader(&self) -> Result<SnapshotReader, SnapshotChannelError> {
        let capacity = self.shared.definition.payload_capacity;
        Ok(SnapshotReader {
            shared: Arc::clone(&self.shared),
            buffers: [byte_buffer(capacity)?, byte_buffer(capacity)?],
            active_buffer: 0,
            accepted_descriptor_generation: 0,
            accepted_metadata: None,
            statistics: SnapshotReaderStatistics::ZERO,
        })
    }

    /// 把一个完整版本发布到当前非活动槽，并以偶数槽 generation 和 descriptor 生效。
    ///
    /// payload 循环上界是初始化期固定容量；本方法不分配、阻塞、重试或覆盖仍被
    /// reader 引用的普通内存。reader 只访问原子槽并复制到自己的 staging。
    ///
    /// # Errors
    ///
    /// 拒绝容量、布局、长度、版本顺序和发布代际回绕错误；失败不改变已发布代际。
    pub fn publish(
        &mut self,
        metadata: SnapshotMetadata,
        payload: &[u8],
    ) -> Result<u64, SnapshotChannelError> {
        self.validate_publication(metadata, payload)?;
        let descriptor_odd = self
            .descriptor_generation
            .checked_add(1)
            .ok_or(SnapshotChannelError::PublicationGenerationExhausted)?;
        let descriptor_even = descriptor_odd
            .checked_add(1)
            .ok_or(SnapshotChannelError::PublicationGenerationExhausted)?;
        let slot = 1 - self.published_slot;
        let slot_odd = self.slot_generations[slot]
            .checked_add(1)
            .ok_or(SnapshotChannelError::PublicationGenerationExhausted)?;
        let slot_even = slot_odd
            .checked_add(1)
            .ok_or(SnapshotChannelError::PublicationGenerationExhausted)?;

        self.shared.slots[slot]
            .generation
            .store(slot_odd, Ordering::Release);
        self.shared.slots[slot].write(metadata, payload);
        self.shared.slots[slot]
            .generation
            .store(slot_even, Ordering::Release);
        self.shared.descriptor.publish(
            descriptor_odd,
            descriptor_even,
            slot,
            metadata.task_epoch(),
            metadata.commit_sequence(),
        );
        self.descriptor_generation = descriptor_even;
        self.slot_generations[slot] = slot_even;
        self.published_slot = slot;
        self.last_metadata = Some(metadata);
        Ok(descriptor_even / 2)
    }

    fn validate_publication(
        &self,
        metadata: SnapshotMetadata,
        payload: &[u8],
    ) -> Result<(), SnapshotChannelError> {
        let definition = self.shared.definition;
        if payload.len() > definition.payload_capacity.get() {
            return Err(SnapshotChannelError::PayloadTooLarge {
                actual: payload.len(),
                capacity: definition.payload_capacity,
            });
        }
        if usize::try_from(metadata.payload_length()) != Ok(payload.len()) {
            return Err(SnapshotChannelError::PayloadLengthMismatch {
                declared: metadata.payload_length(),
                actual: payload.len(),
            });
        }
        if metadata.version() != definition.version
            || metadata.engine_epoch() != definition.engine_epoch
            || metadata.schema_hash() != definition.schema_hash
        {
            return Err(SnapshotChannelError::DefinitionMismatch);
        }
        if let Some(previous) = self.last_metadata {
            metadata
                .progress_after(previous)
                .map_err(SnapshotChannelError::InvalidPublicationOrder)?;
        }
        Ok(())
    }
}

/// 一个 reader 的独立版本、私有双副本和 Fresh/Stale 观测状态。
///
/// `try_latch` 每次尝试最多复制固定 payload 容量，首次争用后只重试一次最新 descriptor。
/// 第二次仍争用时丢弃 staging 并返回 `Contended`；上次成功副本保持不变。
#[derive(Debug)]
pub struct SnapshotReader {
    shared: Arc<SharedSnapshot>,
    buffers: [Vec<u8>; 2],
    active_buffer: usize,
    accepted_descriptor_generation: u64,
    accepted_metadata: Option<SnapshotMetadata>,
    statistics: SnapshotReaderStatistics,
}

impl SnapshotReader {
    /// 尝试锁存最新完整快照并计算该 reader 自己的年龄、Stale 和 sequence gap。
    ///
    /// # Errors
    ///
    /// 未发布、两次并发争用、损坏的内部 metadata 或无效观测时间均显式返回。
    pub fn try_latch(
        &mut self,
        now: MonotonicTimestamp,
        maximum_age_nanos: u64,
    ) -> Result<LatchedSnapshot<'_>, SnapshotChannelError> {
        let copied = match self.copy_latest_once() {
            Err(SnapshotChannelError::Contended) => self.copy_latest_once(),
            result => result,
        };
        let copied = match copied {
            Ok(copied) => copied,
            Err(SnapshotChannelError::Contended) => {
                increment_saturating(
                    &mut self.statistics.contended_reads,
                    1,
                    &mut self.statistics.saturated,
                );
                return Err(SnapshotChannelError::Contended);
            }
            Err(error) => return Err(error),
        };
        let CopyLatest::New(candidate) = copied else {
            increment_saturating(
                &mut self.statistics.unchanged_reads,
                1,
                &mut self.statistics.saturated,
            );
            return self.current_view(now, maximum_age_nanos, SnapshotLatchProgress::Unchanged);
        };

        let metadata = candidate.raw.into_metadata(self.shared.definition)?;
        if metadata.task_epoch().get() != candidate.descriptor.task_epoch
            || metadata.commit_sequence().get() != candidate.descriptor.commit_sequence
        {
            return Err(SnapshotChannelError::InvalidPublishedMetadata(
                ExecutionContractError::InvalidCommitSequence,
            ));
        }
        let progress = match self.accepted_metadata {
            Some(previous) => SnapshotLatchProgress::Advanced(
                metadata
                    .progress_after(previous)
                    .map_err(SnapshotChannelError::InvalidPublishedMetadata)?,
            ),
            None => SnapshotLatchProgress::First,
        };
        let observation = metadata
            .observe_at(now, maximum_age_nanos)
            .map_err(SnapshotChannelError::InvalidObservationTime)?;
        let observed_quality = metadata
            .observed_quality(now, maximum_age_nanos)
            .map_err(SnapshotChannelError::InvalidObservationTime)?;
        if let SnapshotLatchProgress::Advanced(SnapshotProgress::Gap { missed_commits }) = progress
        {
            increment_saturating(
                &mut self.statistics.missed_commits,
                missed_commits,
                &mut self.statistics.saturated,
            );
        }
        self.active_buffer = candidate.staging_buffer;
        self.accepted_descriptor_generation = candidate.descriptor.generation;
        self.accepted_metadata = Some(metadata);
        increment_saturating(
            &mut self.statistics.accepted_publications,
            1,
            &mut self.statistics.saturated,
        );
        Ok(LatchedSnapshot {
            publication_generation: candidate.descriptor.generation / 2,
            metadata,
            observation,
            observed_quality,
            progress,
            payload: &self.buffers[self.active_buffer][..metadata.payload_length() as usize],
        })
    }

    /// 返回该 reader 的本地可观测统计。
    #[must_use]
    pub const fn statistics(&self) -> SnapshotReaderStatistics {
        self.statistics
    }

    fn copy_latest_once(&mut self) -> Result<CopyLatest, SnapshotChannelError> {
        let descriptor = self.shared.descriptor.read()?;
        if descriptor.generation == self.accepted_descriptor_generation {
            return Ok(CopyLatest::Unchanged);
        }
        let slot = &self.shared.slots[descriptor.slot];
        let slot_generation = slot.generation.load(Ordering::Acquire);
        if slot_generation == 0 || slot_generation & 1 == 1 {
            return Err(SnapshotChannelError::Contended);
        }
        let raw = slot.read_raw();
        let payload_length = raw.payload_length as usize;
        if payload_length > self.shared.definition.payload_capacity.get() {
            return Err(SnapshotChannelError::InvalidPublishedMetadata(
                ExecutionContractError::InvalidCapacity,
            ));
        }
        let staging_buffer = 1 - self.active_buffer;
        for (destination, source) in self.buffers[staging_buffer][..payload_length]
            .iter_mut()
            .zip(slot.payload[..payload_length].iter())
        {
            *destination = source.load(Ordering::Relaxed);
        }
        // Acquire fence 防止 metadata/payload load 越过后续槽 generation 与 descriptor
        // 复核；所有共享字段均为原子，因此失败只丢弃 staging，不产生数据竞争。
        fence(Ordering::Acquire);
        let confirmed_slot_generation = slot.generation.load(Ordering::Relaxed);
        let confirmed_descriptor = self.shared.descriptor.read()?;
        if confirmed_slot_generation != slot_generation || confirmed_descriptor != descriptor {
            return Err(SnapshotChannelError::Contended);
        }
        Ok(CopyLatest::New(CopyCandidate {
            descriptor,
            raw,
            staging_buffer,
        }))
    }

    fn current_view(
        &self,
        now: MonotonicTimestamp,
        maximum_age_nanos: u64,
        progress: SnapshotLatchProgress,
    ) -> Result<LatchedSnapshot<'_>, SnapshotChannelError> {
        let metadata = self
            .accepted_metadata
            .ok_or(SnapshotChannelError::NoPublication)?;
        let observation = metadata
            .observe_at(now, maximum_age_nanos)
            .map_err(SnapshotChannelError::InvalidObservationTime)?;
        let observed_quality = metadata
            .observed_quality(now, maximum_age_nanos)
            .map_err(SnapshotChannelError::InvalidObservationTime)?;
        Ok(LatchedSnapshot {
            publication_generation: self.accepted_descriptor_generation / 2,
            metadata,
            observation,
            observed_quality,
            progress,
            payload: &self.buffers[self.active_buffer][..metadata.payload_length() as usize],
        })
    }
}

#[derive(Debug)]
struct SharedSnapshot {
    definition: SnapshotChannelDefinition,
    slots: [AtomicSnapshotSlot; 2],
    descriptor: AtomicSnapshotDescriptor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SnapshotDescriptor {
    generation: u64,
    slot: usize,
    task_epoch: u64,
    commit_sequence: u64,
}

#[derive(Debug)]
struct AtomicSnapshotDescriptor {
    generation: AtomicU64,
    slot: AtomicUsize,
    task_epoch: AtomicU64,
    commit_sequence: AtomicU64,
}

impl AtomicSnapshotDescriptor {
    const fn new() -> Self {
        Self {
            generation: AtomicU64::new(0),
            slot: AtomicUsize::new(0),
            task_epoch: AtomicU64::new(0),
            commit_sequence: AtomicU64::new(0),
        }
    }

    fn publish(
        &self,
        odd_generation: u64,
        even_generation: u64,
        slot: usize,
        task_epoch: TaskEpoch,
        commit_sequence: CommitSequence,
    ) {
        self.generation.store(odd_generation, Ordering::Release);
        self.slot.store(slot, Ordering::Relaxed);
        self.task_epoch.store(task_epoch.get(), Ordering::Relaxed);
        self.commit_sequence
            .store(commit_sequence.get(), Ordering::Relaxed);
        self.generation.store(even_generation, Ordering::Release);
    }

    fn read(&self) -> Result<SnapshotDescriptor, SnapshotChannelError> {
        let generation = self.generation.load(Ordering::Acquire);
        if generation == 0 {
            return Err(SnapshotChannelError::NoPublication);
        }
        if generation & 1 == 1 {
            return Err(SnapshotChannelError::Contended);
        }
        let descriptor = SnapshotDescriptor {
            generation,
            slot: self.slot.load(Ordering::Relaxed),
            task_epoch: self.task_epoch.load(Ordering::Relaxed),
            commit_sequence: self.commit_sequence.load(Ordering::Relaxed),
        };
        fence(Ordering::Acquire);
        if self.generation.load(Ordering::Relaxed) != generation || descriptor.slot > 1 {
            return Err(SnapshotChannelError::Contended);
        }
        Ok(descriptor)
    }
}

#[derive(Debug, Clone, Copy)]
enum CopyLatest {
    Unchanged,
    New(CopyCandidate),
}

#[derive(Debug, Clone, Copy)]
struct CopyCandidate {
    descriptor: SnapshotDescriptor,
    raw: RawSnapshotMetadata,
    staging_buffer: usize,
}

#[derive(Debug)]
struct AtomicSnapshotSlot {
    generation: AtomicU64,
    task_epoch: AtomicU64,
    commit_sequence: AtomicU64,
    release_sequence: AtomicU64,
    published_elapsed_nanos: AtomicU64,
    utc_present: AtomicBool,
    utc_seconds: AtomicI64,
    utc_nanos: AtomicU32,
    time_quality_state: AtomicU8,
    time_source: AtomicU8,
    max_error_present: AtomicBool,
    max_error_nanos: AtomicU64,
    last_sync_present: AtomicBool,
    last_sync_seconds: AtomicI64,
    last_sync_nanos: AtomicU32,
    quality: AtomicU32,
    payload_length: AtomicU32,
    payload: Box<[AtomicU8]>,
}

impl AtomicSnapshotSlot {
    fn new(capacity: SnapshotPayloadCapacity) -> Result<Self, SnapshotChannelError> {
        let mut payload = Vec::new();
        payload.try_reserve_exact(capacity.get()).map_err(|_| {
            SnapshotChannelError::AllocationFailed {
                payload_capacity: capacity,
            }
        })?;
        for _ in 0..capacity.get() {
            payload.push(AtomicU8::new(0));
        }
        Ok(Self {
            generation: AtomicU64::new(0),
            task_epoch: AtomicU64::new(1),
            commit_sequence: AtomicU64::new(0),
            release_sequence: AtomicU64::new(0),
            published_elapsed_nanos: AtomicU64::new(0),
            utc_present: AtomicBool::new(false),
            utc_seconds: AtomicI64::new(0),
            utc_nanos: AtomicU32::new(0),
            time_quality_state: AtomicU8::new(TimeQualityState::Unknown as u8),
            time_source: AtomicU8::new(TimeSource::Unknown as u8),
            max_error_present: AtomicBool::new(false),
            max_error_nanos: AtomicU64::new(0),
            last_sync_present: AtomicBool::new(false),
            last_sync_seconds: AtomicI64::new(0),
            last_sync_nanos: AtomicU32::new(0),
            quality: AtomicU32::new(QualityCode::GOOD.raw()),
            payload_length: AtomicU32::new(0),
            payload: payload.into_boxed_slice(),
        })
    }

    fn write(&self, metadata: SnapshotMetadata, payload: &[u8]) {
        self.task_epoch
            .store(metadata.task_epoch().get(), Ordering::Relaxed);
        self.commit_sequence
            .store(metadata.commit_sequence().get(), Ordering::Relaxed);
        self.release_sequence
            .store(metadata.release_sequence().get(), Ordering::Relaxed);
        self.published_elapsed_nanos
            .store(metadata.published_at().elapsed_nanos(), Ordering::Relaxed);
        write_utc(self, metadata.utc());
        self.quality
            .store(metadata.quality().raw(), Ordering::Relaxed);
        self.payload_length
            .store(metadata.payload_length(), Ordering::Relaxed);
        for (destination, source) in self.payload.iter().zip(payload.iter().copied()) {
            destination.store(source, Ordering::Relaxed);
        }
    }

    fn read_raw(&self) -> RawSnapshotMetadata {
        RawSnapshotMetadata {
            task_epoch: self.task_epoch.load(Ordering::Relaxed),
            commit_sequence: self.commit_sequence.load(Ordering::Relaxed),
            release_sequence: self.release_sequence.load(Ordering::Relaxed),
            published_elapsed_nanos: self.published_elapsed_nanos.load(Ordering::Relaxed),
            utc_present: self.utc_present.load(Ordering::Relaxed),
            utc_seconds: self.utc_seconds.load(Ordering::Relaxed),
            utc_nanos: self.utc_nanos.load(Ordering::Relaxed),
            time_quality_state: self.time_quality_state.load(Ordering::Relaxed),
            time_source: self.time_source.load(Ordering::Relaxed),
            max_error_present: self.max_error_present.load(Ordering::Relaxed),
            max_error_nanos: self.max_error_nanos.load(Ordering::Relaxed),
            last_sync_present: self.last_sync_present.load(Ordering::Relaxed),
            last_sync_seconds: self.last_sync_seconds.load(Ordering::Relaxed),
            last_sync_nanos: self.last_sync_nanos.load(Ordering::Relaxed),
            quality: self.quality.load(Ordering::Relaxed),
            payload_length: self.payload_length.load(Ordering::Relaxed),
        }
    }
}

fn write_utc(slot: &AtomicSnapshotSlot, utc: Option<UtcObservation>) {
    let Some(utc) = utc else {
        slot.utc_present.store(false, Ordering::Relaxed);
        return;
    };
    let timestamp = utc.timestamp();
    let quality = utc.quality();
    slot.utc_seconds
        .store(timestamp.seconds(), Ordering::Relaxed);
    slot.utc_nanos.store(timestamp.nanos(), Ordering::Relaxed);
    slot.time_quality_state
        .store(quality.state() as u8, Ordering::Relaxed);
    slot.time_source
        .store(quality.source() as u8, Ordering::Relaxed);
    write_optional_u64(
        &slot.max_error_present,
        &slot.max_error_nanos,
        quality.max_error_nanos(),
    );
    match quality.last_sync_utc() {
        Some(last_sync) => {
            slot.last_sync_seconds
                .store(last_sync.seconds(), Ordering::Relaxed);
            slot.last_sync_nanos
                .store(last_sync.nanos(), Ordering::Relaxed);
            slot.last_sync_present.store(true, Ordering::Relaxed);
        }
        None => slot.last_sync_present.store(false, Ordering::Relaxed),
    }
    slot.utc_present.store(true, Ordering::Relaxed);
}

fn write_optional_u64(present: &AtomicBool, value: &AtomicU64, source: Option<u64>) {
    match source {
        Some(source) => {
            value.store(source, Ordering::Relaxed);
            present.store(true, Ordering::Relaxed);
        }
        None => present.store(false, Ordering::Relaxed),
    }
}

#[derive(Debug, Clone, Copy)]
struct RawSnapshotMetadata {
    task_epoch: u64,
    commit_sequence: u64,
    release_sequence: u64,
    published_elapsed_nanos: u64,
    utc_present: bool,
    utc_seconds: i64,
    utc_nanos: u32,
    time_quality_state: u8,
    time_source: u8,
    max_error_present: bool,
    max_error_nanos: u64,
    last_sync_present: bool,
    last_sync_seconds: i64,
    last_sync_nanos: u32,
    quality: u32,
    payload_length: u32,
}

impl RawSnapshotMetadata {
    fn into_metadata(
        self,
        definition: SnapshotChannelDefinition,
    ) -> Result<SnapshotMetadata, SnapshotChannelError> {
        let contract_error = SnapshotChannelError::InvalidPublishedMetadata;
        let task_epoch = TaskEpoch::new(self.task_epoch).map_err(contract_error)?;
        let utc = if self.utc_present {
            let timestamp = UtcTimestamp::new(self.utc_seconds, self.utc_nanos)
                .map_err(|_| contract_error(ExecutionContractError::InvalidTimestampOrder))?;
            let last_sync = if self.last_sync_present {
                Some(
                    UtcTimestamp::new(self.last_sync_seconds, self.last_sync_nanos).map_err(
                        |_| contract_error(ExecutionContractError::InvalidTimestampOrder),
                    )?,
                )
            } else {
                None
            };
            let state = decode_time_quality_state(self.time_quality_state)
                .ok_or_else(|| contract_error(ExecutionContractError::InvalidEnum))?;
            let source = decode_time_source(self.time_source)
                .ok_or_else(|| contract_error(ExecutionContractError::InvalidEnum))?;
            Some(UtcObservation::new(
                timestamp,
                TimeQuality::new(
                    state,
                    source,
                    self.max_error_present.then_some(self.max_error_nanos),
                    last_sync,
                ),
            ))
        } else {
            None
        };
        let quality = QualityCode::from_raw(self.quality)
            .map_err(|_| contract_error(ExecutionContractError::InvalidEnum))?;
        SnapshotMetadata::new(
            definition.version,
            definition.engine_epoch,
            task_epoch,
            CommitSequence::new(self.commit_sequence),
            ReleaseSequence::new(self.release_sequence),
            MonotonicTimestamp::new(definition.engine_epoch, self.published_elapsed_nanos),
            utc,
            quality,
            definition.schema_hash,
            self.payload_length,
        )
        .map_err(contract_error)
    }
}

fn decode_time_quality_state(value: u8) -> Option<TimeQualityState> {
    match value {
        0 => Some(TimeQualityState::Unknown),
        1 => Some(TimeQualityState::Synchronizing),
        2 => Some(TimeQualityState::Good),
        3 => Some(TimeQualityState::Holdover),
        4 => Some(TimeQualityState::Degraded),
        5 => Some(TimeQualityState::Invalid),
        _ => None,
    }
}

fn decode_time_source(value: u8) -> Option<TimeSource> {
    match value {
        0 => Some(TimeSource::Unknown),
        1 => Some(TimeSource::System),
        2 => Some(TimeSource::Ntp),
        3 => Some(TimeSource::Ptp),
        4 => Some(TimeSource::Gnss),
        5 => Some(TimeSource::Manual),
        _ => None,
    }
}

fn byte_buffer(capacity: SnapshotPayloadCapacity) -> Result<Vec<u8>, SnapshotChannelError> {
    let mut buffer = Vec::new();
    buffer.try_reserve_exact(capacity.get()).map_err(|_| {
        SnapshotChannelError::AllocationFailed {
            payload_capacity: capacity,
        }
    })?;
    buffer.resize(capacity.get(), 0);
    Ok(buffer)
}

fn increment_saturating(value: &mut u64, increment: u64, saturated: &mut bool) {
    if let Some(next) = value.checked_add(increment) {
        *value = next;
    } else {
        *value = u64::MAX;
        *saturated = true;
    }
}

#[cfg(test)]
#[path = "snapshot_channel_tests.rs"]
mod tests;
