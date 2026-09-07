//! 初始化期构建、周期期固定索引访问的工作集。

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::iter::FusedIterator;
use std::mem::size_of;

/// 工作集的非零逻辑槽位容量。
///
/// 容量由已验证的工程配置和 Target Profile 提供；平台不提供默认容量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkSetCapacity(usize);

impl WorkSetCapacity {
    /// 创建一个非零工作集容量。
    ///
    /// # Errors
    ///
    /// 当 `value` 为零时返回 [`WorkSetError::InvalidCapacity`]。
    pub const fn new(value: usize) -> Result<Self, WorkSetError> {
        if value == 0 {
            Err(WorkSetError::InvalidCapacity { requested: value })
        } else {
            Ok(Self(value))
        }
    }

    /// 返回逻辑槽位数。
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// 工作集的零基槽位索引。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkSetIndex(usize);

impl WorkSetIndex {
    /// 创建零基槽位索引；索引是否属于某个工作集由访问边界显式校验。
    #[must_use]
    pub const fn new(value: usize) -> Self {
        Self(value)
    }

    /// 返回零基索引值。
    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

/// 工作集在启动前必须满足的静态资源上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkSetLimits {
    maximum_capacity: WorkSetCapacity,
    maximum_allocation_bytes: usize,
}

impl WorkSetLimits {
    /// 创建工程/Target Profile 声明的槽位和分配字节上限。
    ///
    /// `maximum_allocation_bytes` 以字节为单位，可以为零；任何需要内存的工作集
    /// 随后都会以 [`WorkSetError::ResourceBudgetExceeded`] 拒绝启动。
    #[must_use]
    pub const fn new(maximum_capacity: WorkSetCapacity, maximum_allocation_bytes: usize) -> Self {
        Self {
            maximum_capacity,
            maximum_allocation_bytes,
        }
    }

    /// 返回允许的最大逻辑槽位数。
    #[must_use]
    pub const fn maximum_capacity(self) -> WorkSetCapacity {
        self.maximum_capacity
    }

    /// 返回允许的最大工作集分配字节数。
    #[must_use]
    pub const fn maximum_allocation_bytes(self) -> usize {
        self.maximum_allocation_bytes
    }
}

/// 固定容量工作集的配置、初始化或访问错误。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkSetError {
    /// 请求了零容量。
    InvalidCapacity {
        /// 被拒绝的槽位数。
        requested: usize,
    },
    /// 请求容量超过工程/Target Profile 声明上限。
    CapacityExceedsLimit {
        /// 请求槽位数。
        requested: WorkSetCapacity,
        /// 允许的最大槽位数。
        maximum: WorkSetCapacity,
    },
    /// 槽位数与单槽大小的乘法无法用 `usize` 字节数表示。
    AllocationSizeOverflow {
        /// 请求槽位数。
        capacity: WorkSetCapacity,
        /// `Option<T>` 单槽大小，单位为字节。
        slot_size_bytes: usize,
    },
    /// 逻辑工作集或分配器给出的实际容量超过声明内存预算。
    ResourceBudgetExceeded {
        /// 所需字节数。
        required_bytes: usize,
        /// 允许的最大字节数。
        maximum_bytes: usize,
    },
    /// 初始化期无法为全部槽位预留内存。
    AllocationFailed {
        /// 请求槽位数。
        capacity: WorkSetCapacity,
        /// 请求的逻辑字节数。
        requested_bytes: usize,
    },
    /// 顺序初始化时没有剩余槽位。
    Full {
        /// 已满工作集的容量。
        capacity: WorkSetCapacity,
    },
    /// 索引不属于工作集。
    OutOfRange {
        /// 被拒绝的索引。
        index: WorkSetIndex,
        /// 工作集容量。
        capacity: WorkSetCapacity,
    },
    /// 同一槽位被初始化了多次。
    AlreadyInitialized {
        /// 重复初始化的索引。
        index: WorkSetIndex,
    },
    /// 槽位尚未初始化，工作集不能进入周期使用阶段。
    Uninitialized {
        /// 首个未初始化索引。
        index: WorkSetIndex,
    },
}

impl Display for WorkSetError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCapacity { requested } => {
                write!(
                    formatter,
                    "work-set capacity must be non-zero, got {requested}"
                )
            }
            Self::CapacityExceedsLimit { requested, maximum } => write!(
                formatter,
                "work-set capacity {} exceeds declared maximum {}",
                requested.get(),
                maximum.get()
            ),
            Self::AllocationSizeOverflow {
                capacity,
                slot_size_bytes,
            } => write!(
                formatter,
                "work-set allocation size overflows: {} slots at {slot_size_bytes} bytes",
                capacity.get()
            ),
            Self::ResourceBudgetExceeded {
                required_bytes,
                maximum_bytes,
            } => write!(
                formatter,
                "work-set requires {required_bytes} bytes, exceeding {maximum_bytes} byte budget"
            ),
            Self::AllocationFailed {
                capacity,
                requested_bytes,
            } => write!(
                formatter,
                "failed to allocate {} work-set slots ({requested_bytes} bytes)",
                capacity.get()
            ),
            Self::Full { capacity } => {
                write!(formatter, "work set is full at {} slots", capacity.get())
            }
            Self::OutOfRange { index, capacity } => write!(
                formatter,
                "work-set index {} is outside capacity {}",
                index.get(),
                capacity.get()
            ),
            Self::AlreadyInitialized { index } => {
                write!(
                    formatter,
                    "work-set index {} is already initialized",
                    index.get()
                )
            }
            Self::Uninitialized { index } => {
                write!(
                    formatter,
                    "work-set index {} is not initialized",
                    index.get()
                )
            }
        }
    }
}

impl Error for WorkSetError {}

/// 初始化期使用的固定容量工作集构建器。
///
/// 构造时一次性预留存储并写入每个逻辑槽位。任意索引初始化和顺序初始化都只在
/// 启动路径使用；[`FixedWorkSetBuilder::seal`] 成功后才可把工作集交给周期路径。
#[derive(Debug)]
pub struct FixedWorkSetBuilder<T> {
    slots: Vec<Option<T>>,
    capacity: WorkSetCapacity,
    allocation_size_bytes: usize,
    initialized_count: usize,
    next_uninitialized: usize,
}

impl<T> FixedWorkSetBuilder<T> {
    /// 在初始化期为工作集分配并物化全部逻辑槽位。
    ///
    /// 内存预算按实际 `Vec` 槽位容量乘以 `size_of::<Option<T>>()` 校验；分配失败
    /// 返回领域错误，不进入周期执行。此函数不执行 I/O、重试或创建线程。
    ///
    /// # Errors
    ///
    /// 容量超过声明上限、字节计算溢出、超过内存预算或分配失败时返回对应的
    /// [`WorkSetError`]。
    pub fn new(capacity: WorkSetCapacity, limits: WorkSetLimits) -> Result<Self, WorkSetError> {
        if capacity > limits.maximum_capacity() {
            return Err(WorkSetError::CapacityExceedsLimit {
                requested: capacity,
                maximum: limits.maximum_capacity(),
            });
        }

        let slot_size_bytes = size_of::<Option<T>>();
        let requested_bytes = capacity.get().checked_mul(slot_size_bytes).ok_or(
            WorkSetError::AllocationSizeOverflow {
                capacity,
                slot_size_bytes,
            },
        )?;
        if requested_bytes > limits.maximum_allocation_bytes() {
            return Err(WorkSetError::ResourceBudgetExceeded {
                required_bytes: requested_bytes,
                maximum_bytes: limits.maximum_allocation_bytes(),
            });
        }

        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity.get())
            .map_err(|_| WorkSetError::AllocationFailed {
                capacity,
                requested_bytes,
            })?;

        let allocation_size_bytes = slots.capacity().checked_mul(slot_size_bytes).ok_or(
            WorkSetError::AllocationSizeOverflow {
                capacity,
                slot_size_bytes,
            },
        )?;
        if allocation_size_bytes > limits.maximum_allocation_bytes() {
            return Err(WorkSetError::ResourceBudgetExceeded {
                required_bytes: allocation_size_bytes,
                maximum_bytes: limits.maximum_allocation_bytes(),
            });
        }

        slots.resize_with(capacity.get(), || None);
        for slot in &mut slots {
            // 初始化路径显式触及每个逻辑槽；锁页和平台级驻留策略属于 Linux 适配层。
            *std::hint::black_box(slot) = None;
        }

        Ok(Self {
            slots,
            capacity,
            allocation_size_bytes,
            initialized_count: 0,
            next_uninitialized: 0,
        })
    }

    /// 返回不可变的逻辑槽位容量。
    #[must_use]
    pub const fn capacity(&self) -> WorkSetCapacity {
        self.capacity
    }

    /// 返回当前已初始化槽位数。
    #[must_use]
    pub const fn initialized_count(&self) -> usize {
        self.initialized_count
    }

    /// 返回已预留工作集槽位的字节数，不包含分配器元数据。
    #[must_use]
    pub const fn allocation_size_bytes(&self) -> usize {
        self.allocation_size_bytes
    }

    /// 返回任何遍历全部逻辑槽位的最大迭代次数。
    #[must_use]
    pub const fn maximum_iteration_count(&self) -> usize {
        self.capacity.get()
    }

    /// 在显式索引初始化一个槽位。
    ///
    /// 该操作只用于启动路径，不改变工作集容量，也不执行分配或重试。
    ///
    /// # Errors
    ///
    /// 索引越界时返回 [`WorkSetError::OutOfRange`]；槽位已有值时返回
    /// [`WorkSetError::AlreadyInitialized`]。
    pub fn initialize_at(&mut self, index: WorkSetIndex, value: T) -> Result<(), WorkSetError> {
        let capacity = self.capacity;
        let slot = self
            .slots
            .get_mut(index.get())
            .ok_or(WorkSetError::OutOfRange { index, capacity })?;
        if slot.is_some() {
            return Err(WorkSetError::AlreadyInitialized { index });
        }

        *slot = Some(value);
        self.initialized_count += 1;
        if index.get() == self.next_uninitialized {
            self.advance_next_uninitialized();
        }
        Ok(())
    }

    /// 初始化当前最小的空槽位并返回其索引。
    ///
    /// 查找仅跨越固定逻辑容量，并且只发生在启动路径；方法不增长集合。
    ///
    /// # Errors
    ///
    /// 所有槽位都已初始化时返回 [`WorkSetError::Full`]。
    pub fn initialize_next(&mut self, value: T) -> Result<WorkSetIndex, WorkSetError> {
        if self.next_uninitialized == self.capacity.get() {
            return Err(WorkSetError::Full {
                capacity: self.capacity,
            });
        }

        let index = WorkSetIndex::new(self.next_uninitialized);
        self.initialize_at(index, value)?;
        Ok(index)
    }

    /// 验证所有槽位已初始化，并将所有权移交给周期侧工作集。
    ///
    /// `seal` 只执行至多 `capacity` 次初始化状态检查；它移动既有 `Vec`，不重新
    /// 分配或复制元素。
    ///
    /// # Errors
    ///
    /// 存在空槽位时返回首个 [`WorkSetError::Uninitialized`]。
    pub fn seal(self) -> Result<FixedWorkSet<T>, WorkSetError> {
        if let Some(index) = self.slots.iter().position(Option::is_none) {
            return Err(WorkSetError::Uninitialized {
                index: WorkSetIndex::new(index),
            });
        }

        Ok(FixedWorkSet {
            slots: self.slots,
            capacity: self.capacity,
            allocation_size_bytes: self.allocation_size_bytes,
        })
    }

    fn advance_next_uninitialized(&mut self) {
        while self.next_uninitialized < self.capacity.get()
            && self
                .slots
                .get(self.next_uninitialized)
                .is_some_and(Option::is_some)
        {
            self.next_uninitialized += 1;
        }
    }
}

/// 启动前完成初始化、周期期容量不变的工作集。
///
/// 该类型不暴露插入、删除、替换存储或容量增长 API。周期调用只生成有界索引并
/// 访问已预分配槽位；不执行 I/O、锁等待、重试或堆分配。`T` 的业务方法仍由调用方
/// 负责遵守周期路径约束。
#[derive(Debug)]
pub struct FixedWorkSet<T> {
    slots: Vec<Option<T>>,
    capacity: WorkSetCapacity,
    allocation_size_bytes: usize,
}

impl<T> FixedWorkSet<T> {
    /// 返回不可变的逻辑槽位容量。
    #[must_use]
    pub const fn capacity(&self) -> WorkSetCapacity {
        self.capacity
    }

    /// 返回已预留工作集槽位的字节数，不包含分配器元数据。
    #[must_use]
    pub const fn allocation_size_bytes(&self) -> usize {
        self.allocation_size_bytes
    }

    /// 返回周期代码遍历全部槽位的最大迭代次数。
    #[must_use]
    pub const fn maximum_iteration_count(&self) -> usize {
        self.capacity.get()
    }

    /// 按确定顺序生成恰好 `capacity` 个零基索引。
    ///
    /// 迭代器只保存起止索引且实现 [`ExactSizeIterator`]，不会分配或访问槽位。
    #[must_use]
    pub const fn indices(&self) -> WorkSetIndices {
        WorkSetIndices {
            next: 0,
            end: self.capacity.get(),
        }
    }

    /// 读取一个已初始化槽位。
    ///
    /// 该操作为一次边界检查和一次固定槽位访问，不分配、不阻塞且不重试。
    ///
    /// # Errors
    ///
    /// 索引越界时返回 [`WorkSetError::OutOfRange`]。若内部初始化不变量被破坏，返回
    /// [`WorkSetError::Uninitialized`]，不得伪造默认值继续执行。
    pub fn get(&self, index: WorkSetIndex) -> Result<&T, WorkSetError> {
        let slot = self
            .slots
            .get(index.get())
            .ok_or(WorkSetError::OutOfRange {
                index,
                capacity: self.capacity,
            })?;
        slot.as_ref().ok_or(WorkSetError::Uninitialized { index })
    }

    /// 可变访问一个已初始化槽位。
    ///
    /// 该操作为一次边界检查和一次固定槽位访问，不分配、不阻塞且不重试；不能改变
    /// 工作集容量或槽位初始化状态。
    ///
    /// # Errors
    ///
    /// 索引越界时返回 [`WorkSetError::OutOfRange`]。若内部初始化不变量被破坏，返回
    /// [`WorkSetError::Uninitialized`]。
    pub fn get_mut(&mut self, index: WorkSetIndex) -> Result<&mut T, WorkSetError> {
        let slot = self
            .slots
            .get_mut(index.get())
            .ok_or(WorkSetError::OutOfRange {
                index,
                capacity: self.capacity,
            })?;
        slot.as_mut().ok_or(WorkSetError::Uninitialized { index })
    }
}

/// 固定工作集的有界索引迭代器。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkSetIndices {
    next: usize,
    end: usize,
}

impl Iterator for WorkSetIndices {
    type Item = WorkSetIndex;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == self.end {
            return None;
        }

        let index = WorkSetIndex::new(self.next);
        self.next += 1;
        Some(index)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.end - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for WorkSetIndices {}

impl FusedIterator for WorkSetIndices {}

#[cfg(test)]
mod tests {
    use std::mem::size_of;

    use super::{FixedWorkSetBuilder, WorkSetCapacity, WorkSetError, WorkSetIndex, WorkSetLimits};

    #[test]
    fn zero_capacity_and_declared_limits_are_rejected_before_allocation() {
        assert_eq!(
            WorkSetCapacity::new(0),
            Err(WorkSetError::InvalidCapacity { requested: 0 })
        );

        let capacity = valid_capacity(3);
        let maximum = valid_capacity(2);
        let limits = WorkSetLimits::new(maximum, usize::MAX);
        assert_eq!(
            FixedWorkSetBuilder::<u64>::new(capacity, limits).err(),
            Some(WorkSetError::CapacityExceedsLimit {
                requested: capacity,
                maximum,
            })
        );
    }

    #[test]
    fn allocation_math_and_resource_budget_are_checked_before_use() {
        let capacity = valid_capacity(2);
        let maximum = valid_capacity(2);
        let slot_size_bytes = size_of::<Option<u64>>();
        let required_bytes = capacity.get() * slot_size_bytes;
        let limits = WorkSetLimits::new(maximum, required_bytes - 1);
        assert_eq!(
            FixedWorkSetBuilder::<u64>::new(capacity, limits).err(),
            Some(WorkSetError::ResourceBudgetExceeded {
                required_bytes,
                maximum_bytes: required_bytes - 1,
            })
        );

        let overflowing_capacity = valid_capacity(usize::MAX);
        let limits = WorkSetLimits::new(overflowing_capacity, usize::MAX);
        assert_eq!(
            FixedWorkSetBuilder::<u64>::new(overflowing_capacity, limits).err(),
            Some(WorkSetError::AllocationSizeOverflow {
                capacity: overflowing_capacity,
                slot_size_bytes,
            })
        );
    }

    #[test]
    fn explicit_initialization_reports_out_of_range_and_duplicate_slots() {
        let capacity = valid_capacity(3);
        let builder = builder::<u64>(capacity);
        assert!(builder.is_ok());
        let one = WorkSetIndex::new(1);
        let outside = WorkSetIndex::new(3);

        if let Ok(mut builder) = builder {
            assert_eq!(builder.initialize_at(one, 10), Ok(()));
            assert_eq!(
                builder.initialize_at(one, 11),
                Err(WorkSetError::AlreadyInitialized { index: one })
            );
            assert_eq!(
                builder.initialize_at(outside, 12),
                Err(WorkSetError::OutOfRange {
                    index: outside,
                    capacity,
                })
            );
            assert_eq!(builder.initialized_count(), 1);
            assert_eq!(builder.initialize_next(20), Ok(WorkSetIndex::new(0)));
            assert_eq!(builder.initialize_next(30), Ok(WorkSetIndex::new(2)));
        }
    }

    #[test]
    fn sequential_initialization_stops_at_full_capacity() {
        let capacity = valid_capacity(2);
        let builder = builder::<u64>(capacity);
        assert!(builder.is_ok());

        if let Ok(mut builder) = builder {
            assert_eq!(builder.initialize_next(10), Ok(WorkSetIndex::new(0)));
            assert_eq!(builder.initialize_next(20), Ok(WorkSetIndex::new(1)));
            assert_eq!(
                builder.initialize_next(30),
                Err(WorkSetError::Full { capacity })
            );
            assert_eq!(builder.initialized_count(), capacity.get());
        }
    }

    #[test]
    fn seal_rejects_the_first_uninitialized_slot() {
        let capacity = valid_capacity(3);
        let builder = builder::<u64>(capacity);
        assert!(builder.is_ok());

        if let Ok(mut builder) = builder {
            assert_eq!(builder.initialize_at(WorkSetIndex::new(1), 20), Ok(()));
            assert_eq!(
                builder.seal().err(),
                Some(WorkSetError::Uninitialized {
                    index: WorkSetIndex::new(0),
                })
            );
        }
    }

    #[test]
    fn sealed_work_set_has_exact_bounded_indices_and_checked_access() {
        let capacity = valid_capacity(3);
        let builder = builder::<u64>(capacity);
        assert!(builder.is_ok());

        if let Ok(mut builder) = builder {
            assert_eq!(builder.initialize_next(10), Ok(WorkSetIndex::new(0)));
            assert_eq!(builder.initialize_next(20), Ok(WorkSetIndex::new(1)));
            assert_eq!(builder.initialize_next(30), Ok(WorkSetIndex::new(2)));
            let work_set = builder.seal();
            assert!(work_set.is_ok());

            if let Ok(work_set) = work_set {
                assert_eq!(work_set.capacity(), capacity);
                assert_eq!(work_set.maximum_iteration_count(), capacity.get());
                assert_eq!(work_set.indices().len(), capacity.get());
                assert_eq!(
                    work_set.indices().collect::<Vec<_>>(),
                    vec![
                        WorkSetIndex::new(0),
                        WorkSetIndex::new(1),
                        WorkSetIndex::new(2)
                    ]
                );
                assert_eq!(work_set.get(WorkSetIndex::new(1)), Ok(&20));
                assert_eq!(
                    work_set.get(WorkSetIndex::new(3)),
                    Err(WorkSetError::OutOfRange {
                        index: WorkSetIndex::new(3),
                        capacity,
                    })
                );
            }
        }
    }

    #[test]
    fn cyclic_index_access_preserves_preallocated_storage_and_iteration_bound() {
        let capacity = valid_capacity(4);
        let builder = builder::<u64>(capacity);
        assert!(builder.is_ok());

        if let Ok(mut builder) = builder {
            for value in 0_u64..4 {
                assert!(builder.initialize_next(value).is_ok());
            }

            let allocation_size_bytes = builder.allocation_size_bytes();
            let storage_address = builder.slots.as_ptr();
            let work_set = builder.seal();
            assert!(work_set.is_ok());

            if let Ok(mut work_set) = work_set {
                assert_eq!(storage_address, work_set.slots.as_ptr());
                for _cycle in 0..1_024 {
                    let mut iterations = 0;
                    for index in work_set.indices() {
                        let value = work_set.get_mut(index);
                        assert!(value.is_ok());
                        if let Ok(value) = value {
                            *value += 1;
                        }
                        iterations += 1;
                    }
                    assert_eq!(iterations, work_set.maximum_iteration_count());
                }

                assert_eq!(storage_address, work_set.slots.as_ptr());
                assert_eq!(work_set.capacity(), capacity);
                assert_eq!(work_set.allocation_size_bytes(), allocation_size_bytes);
                assert_eq!(work_set.get(WorkSetIndex::new(0)), Ok(&1_024));
                assert_eq!(work_set.get(WorkSetIndex::new(3)), Ok(&1_027));
            }
        }
    }

    fn valid_capacity(value: usize) -> WorkSetCapacity {
        WorkSetCapacity::new(value).unwrap_or(WorkSetCapacity(1))
    }

    fn builder<T>(capacity: WorkSetCapacity) -> Result<FixedWorkSetBuilder<T>, WorkSetError> {
        let limits = WorkSetLimits::new(capacity, usize::MAX);
        FixedWorkSetBuilder::new(capacity, limits)
    }
}
