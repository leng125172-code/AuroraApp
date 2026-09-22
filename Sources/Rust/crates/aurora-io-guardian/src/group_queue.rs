//! Fixed-capacity per-group queue with direction-specific overflow semantics.

use crate::{GroupDescriptor, ImageDirection, UpdateGroupError, UpdateGroupSpec};

/// Result of one non-blocking queue admission attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueAdmission<T> {
    /// The item was stored without replacing an older item.
    Enqueued,
    /// A full input queue dropped the newest sample and retained every queued sample.
    DroppedNewest(T),
    /// A full output queue rejected the newest command and retained every unconfirmed command.
    RejectedNewest(T),
}

/// Immutable observation of one bounded queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupQueueSnapshot {
    pub(crate) descriptor: GroupDescriptor,
    pub(crate) direction: ImageDirection,
    pub(crate) capacity: u32,
    pub(crate) depth: u32,
    pub(crate) high_water: u32,
    pub(crate) dropped_newest: u64,
    pub(crate) rejected_newest: u64,
}

impl GroupQueueSnapshot {
    /// Returns the exact mapped group that owns this queue.
    #[must_use]
    pub const fn descriptor(self) -> GroupDescriptor {
        self.descriptor
    }

    /// Returns the image direction whose overflow policy is applied.
    #[must_use]
    pub const fn direction(self) -> ImageDirection {
        self.direction
    }

    /// Returns the immutable queue capacity.
    #[must_use]
    pub const fn capacity(self) -> u32 {
        self.capacity
    }

    /// Returns the current queue depth.
    #[must_use]
    pub const fn depth(self) -> u32 {
        self.depth
    }

    /// Returns the highest observed depth.
    #[must_use]
    pub const fn high_water(self) -> u32 {
        self.high_water
    }

    /// Returns the saturated count of newest input samples dropped while full.
    #[must_use]
    pub const fn dropped_newest(self) -> u64 {
        self.dropped_newest
    }

    /// Returns the saturated count of newest output commands rejected while full.
    #[must_use]
    pub const fn rejected_newest(self) -> u64 {
        self.rejected_newest
    }
}

/// Preallocated non-blocking FIFO for one update group.
///
/// `T: Copy` prevents queue removal or overflow from running user-defined destructors in the
/// cyclic path. Capacity never changes after construction.
pub struct BoundedGroupQueue<T: Copy> {
    descriptor: GroupDescriptor,
    direction: ImageDirection,
    slots: Box<[Option<T>]>,
    head: usize,
    length: usize,
    high_water: usize,
    dropped_newest: u64,
    rejected_newest: u64,
}

impl<T: Copy> BoundedGroupQueue<T> {
    /// Allocates exactly the capacity frozen in the group specification.
    ///
    /// # Errors
    ///
    /// Returns an allocation or capacity error before a partially usable queue is exposed.
    pub fn new(specification: UpdateGroupSpec) -> Result<Self, UpdateGroupError> {
        let capacity = usize::try_from(specification.queue_capacity())
            .map_err(|_| UpdateGroupError::InvalidCapacity)?;
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| UpdateGroupError::AllocationFailed)?;
        slots.resize(capacity, None);
        Ok(Self {
            descriptor: specification.descriptor(),
            direction: specification.descriptor().direction(),
            slots: slots.into_boxed_slice(),
            head: 0,
            length: 0,
            high_water: 0,
            dropped_newest: 0,
            rejected_newest: 0,
        })
    }

    /// Attempts one admission without blocking, allocating, or overwriting queued data.
    pub fn try_push(&mut self, item: T) -> QueueAdmission<T> {
        if self.length == self.slots.len() {
            return match self.direction {
                ImageDirection::Input => {
                    self.dropped_newest = self.dropped_newest.saturating_add(1);
                    QueueAdmission::DroppedNewest(item)
                }
                ImageDirection::Output => {
                    self.rejected_newest = self.rejected_newest.saturating_add(1);
                    QueueAdmission::RejectedNewest(item)
                }
            };
        }
        let tail = (self.head + self.length) % self.slots.len();
        self.slots[tail] = Some(item);
        self.length += 1;
        self.high_water = self.high_water.max(self.length);
        QueueAdmission::Enqueued
    }

    /// Removes the oldest item, or returns `None` when empty.
    pub fn pop(&mut self) -> Option<T> {
        if self.length == 0 {
            return None;
        }
        let item = self.slots[self.head].take();
        self.head = (self.head + 1) % self.slots.len();
        self.length -= 1;
        item
    }

    /// Returns current depth, high-water mark, and saturated overflow counters.
    #[must_use]
    pub fn snapshot(&self) -> GroupQueueSnapshot {
        GroupQueueSnapshot {
            descriptor: self.descriptor,
            direction: self.direction,
            capacity: u32::try_from(self.slots.len()).unwrap_or(u32::MAX),
            depth: u32::try_from(self.length).unwrap_or(u32::MAX),
            high_water: u32::try_from(self.high_water).unwrap_or(u32::MAX),
            dropped_newest: self.dropped_newest,
            rejected_newest: self.rejected_newest,
        }
    }
}
