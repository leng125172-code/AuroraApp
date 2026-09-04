//! Static task timing and miss-policy contracts.

use aurora_types::LocalHandle;

use crate::{ExecutionContractError, ExecutionContractVersion};

/// A task period in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskPeriodNanos(u64);

impl TaskPeriodNanos {
    /// Creates a non-zero task period.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::InvalidPeriod`] for zero.
    pub const fn new(value: u64) -> Result<Self, ExecutionContractError> {
        if value == 0 {
            Err(ExecutionContractError::InvalidPeriod)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the period in nanoseconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A task phase offset in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskPhaseNanos(u64);

impl TaskPhaseNanos {
    /// Creates an unchecked phase value; [`TaskTiming::new`] validates it
    /// against the period.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the phase in nanoseconds.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

macro_rules! non_zero_nanos {
    ($name:ident, $error:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u64);

        impl $name {
            /// Creates a non-zero nanosecond value.
            ///
            /// # Errors
            ///
            /// Returns the corresponding execution-contract error for zero.
            pub const fn new(value: u64) -> Result<Self, ExecutionContractError> {
                if value == 0 {
                    Err(ExecutionContractError::$error)
                } else {
                    Ok(Self(value))
                }
            }

            /// Returns the duration in nanoseconds.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }
    };
}

non_zero_nanos!(
    RelativeDeadlineNanos,
    InvalidDeadline,
    "A relative task deadline in nanoseconds."
);
non_zero_nanos!(
    ExecutionBudgetNanos,
    InvalidExecutionBudget,
    "An admitted task execution budget in nanoseconds."
);
non_zero_nanos!(
    HardLimitNanos,
    InvalidHardLimit,
    "A task execution hard limit in nanoseconds."
);

/// Timing values whose cross-field ordering has been validated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskTiming {
    period: TaskPeriodNanos,
    phase: TaskPhaseNanos,
    relative_deadline: RelativeDeadlineNanos,
    execution_budget: ExecutionBudgetNanos,
    hard_limit: HardLimitNanos,
}

impl TaskTiming {
    /// Creates timing with `phase < period` and
    /// `budget <= hard_limit <= deadline <= period`.
    ///
    /// # Errors
    ///
    /// Returns a field-specific [`ExecutionContractError`] for the first
    /// violated ordering rule.
    pub const fn new(
        period: TaskPeriodNanos,
        phase: TaskPhaseNanos,
        relative_deadline: RelativeDeadlineNanos,
        execution_budget: ExecutionBudgetNanos,
        hard_limit: HardLimitNanos,
    ) -> Result<Self, ExecutionContractError> {
        if phase.get() >= period.get() {
            return Err(ExecutionContractError::InvalidPhase);
        }
        if relative_deadline.get() > period.get() {
            return Err(ExecutionContractError::InvalidDeadline);
        }
        if hard_limit.get() > relative_deadline.get() {
            return Err(ExecutionContractError::InvalidHardLimit);
        }
        if execution_budget.get() > hard_limit.get() {
            return Err(ExecutionContractError::InvalidExecutionBudget);
        }
        Ok(Self {
            period,
            phase,
            relative_deadline,
            execution_budget,
            hard_limit,
        })
    }

    /// Returns the task period.
    #[must_use]
    pub const fn period(self) -> TaskPeriodNanos {
        self.period
    }

    /// Returns the phase offset.
    #[must_use]
    pub const fn phase(self) -> TaskPhaseNanos {
        self.phase
    }

    /// Returns the relative deadline.
    #[must_use]
    pub const fn relative_deadline(self) -> RelativeDeadlineNanos {
        self.relative_deadline
    }

    /// Returns the admitted execution budget.
    #[must_use]
    pub const fn execution_budget(self) -> ExecutionBudgetNanos {
        self.execution_budget
    }

    /// Returns the hard execution limit.
    #[must_use]
    pub const fn hard_limit(self) -> HardLimitNanos {
        self.hard_limit
    }
}

/// The number of recent releases retained by miss accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MissWindow(u32);

impl MissWindow {
    /// Creates a window bounded by a target-declared maximum.
    ///
    /// # Errors
    ///
    /// Returns [`ExecutionContractError::InvalidMissWindow`] for zero or a
    /// value above `target_maximum`.
    pub const fn new(value: u32, target_maximum: u32) -> Result<Self, ExecutionContractError> {
        if value == 0 || value > target_maximum {
            Err(ExecutionContractError::InvalidMissWindow)
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the number of retained releases.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// A validated deadline-miss policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MissPolicy {
    window: MissWindow,
    max_misses: u32,
    consecutive_misses: u32,
}

impl MissPolicy {
    /// Creates a policy with thresholds bounded by `window`.
    ///
    /// # Errors
    ///
    /// Returns a threshold-specific error when a value is outside the window.
    pub const fn new(
        window: MissWindow,
        max_misses: u32,
        consecutive_misses: u32,
    ) -> Result<Self, ExecutionContractError> {
        if max_misses > window.get() {
            return Err(ExecutionContractError::InvalidMaxMisses);
        }
        if consecutive_misses == 0 || consecutive_misses > window.get() {
            return Err(ExecutionContractError::InvalidConsecutiveMisses);
        }
        Ok(Self {
            window,
            max_misses,
            consecutive_misses,
        })
    }

    /// Returns the retained release count.
    #[must_use]
    pub const fn window(self) -> MissWindow {
        self.window
    }

    /// Returns the maximum tolerated misses in the window.
    #[must_use]
    pub const fn max_misses(self) -> u32 {
        self.max_misses
    }

    /// Returns the consecutive-miss Fault threshold.
    #[must_use]
    pub const fn consecutive_misses(self) -> u32 {
        self.consecutive_misses
    }
}

/// Static priority used only for deterministic same-release ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskPriority(i16);

impl TaskPriority {
    /// Creates a priority; larger values run first at the same release.
    #[must_use]
    pub const fn new(value: i16) -> Self {
        Self(value)
    }

    /// Returns the signed priority value.
    #[must_use]
    pub const fn get(self) -> i16 {
        self.0
    }
}

/// A fully validated static R0 task declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TaskSpec {
    version: ExecutionContractVersion,
    handle: LocalHandle,
    priority: TaskPriority,
    timing: TaskTiming,
    miss_policy: MissPolicy,
}

impl TaskSpec {
    /// Creates a static task declaration from validated components.
    #[must_use]
    pub const fn new(
        version: ExecutionContractVersion,
        handle: LocalHandle,
        priority: TaskPriority,
        timing: TaskTiming,
        miss_policy: MissPolicy,
    ) -> Self {
        Self {
            version,
            handle,
            priority,
            timing,
            miss_policy,
        }
    }

    /// Returns the execution-contract version.
    #[must_use]
    pub const fn version(self) -> ExecutionContractVersion {
        self.version
    }

    /// Returns the payload-local task handle.
    #[must_use]
    pub const fn handle(self) -> LocalHandle {
        self.handle
    }

    /// Returns the deterministic same-release priority.
    #[must_use]
    pub const fn priority(self) -> TaskPriority {
        self.priority
    }

    /// Returns validated timing.
    #[must_use]
    pub const fn timing(self) -> TaskTiming {
        self.timing
    }

    /// Returns the deadline-miss policy.
    #[must_use]
    pub const fn miss_policy(self) -> MissPolicy {
        self.miss_policy
    }
}

#[cfg(test)]
mod tests {
    use aurora_types::LocalHandle;

    use super::{
        ExecutionBudgetNanos, HardLimitNanos, MissPolicy, MissWindow, RelativeDeadlineNanos,
        TaskPeriodNanos, TaskPhaseNanos, TaskPriority, TaskSpec, TaskTiming,
    };
    use crate::{ExecutionContractError, ExecutionContractVersion};

    #[test]
    fn task_spec_preserves_all_explicit_units() {
        let timing = valid_timing();
        let policy = MissWindow::new(8, 16).and_then(|window| MissPolicy::new(window, 2, 3));
        assert!(timing.is_ok());
        assert!(policy.is_ok());
        if let (Ok(timing), Ok(policy)) = (timing, policy) {
            let task = TaskSpec::new(
                ExecutionContractVersion::V1_0,
                LocalHandle::ZERO,
                TaskPriority::new(7),
                timing,
                policy,
            );
            assert_eq!(task.handle(), LocalHandle::ZERO);
            assert_eq!(task.priority().get(), 7);
            assert_eq!(task.timing().period().get(), 10_000);
            assert_eq!(task.timing().phase().get(), 500);
            assert_eq!(task.timing().relative_deadline().get(), 9_000);
            assert_eq!(task.timing().execution_budget().get(), 4_000);
            assert_eq!(task.timing().hard_limit().get(), 8_000);
            assert_eq!(task.miss_policy().window().get(), 8);
            assert_eq!(task.miss_policy().max_misses(), 2);
            assert_eq!(task.miss_policy().consecutive_misses(), 3);
            assert_eq!(task.version(), ExecutionContractVersion::V1_0);
        }
    }

    #[test]
    fn timing_rejects_zero_and_every_cross_field_boundary() {
        assert_eq!(
            TaskPeriodNanos::new(0),
            Err(ExecutionContractError::InvalidPeriod)
        );
        assert_eq!(
            RelativeDeadlineNanos::new(0),
            Err(ExecutionContractError::InvalidDeadline)
        );
        assert_eq!(
            ExecutionBudgetNanos::new(0),
            Err(ExecutionContractError::InvalidExecutionBudget)
        );
        assert_eq!(
            HardLimitNanos::new(0),
            Err(ExecutionContractError::InvalidHardLimit)
        );

        let period = TaskPeriodNanos::new(10);
        let deadline = RelativeDeadlineNanos::new(9);
        let budget = ExecutionBudgetNanos::new(4);
        let hard_limit = HardLimitNanos::new(8);
        if let (Ok(period), Ok(deadline), Ok(budget), Ok(hard_limit)) =
            (period, deadline, budget, hard_limit)
        {
            assert_eq!(
                TaskTiming::new(
                    period,
                    TaskPhaseNanos::new(10),
                    deadline,
                    budget,
                    hard_limit,
                ),
                Err(ExecutionContractError::InvalidPhase)
            );
            let long_deadline = RelativeDeadlineNanos::new(11);
            if let Ok(long_deadline) = long_deadline {
                assert_eq!(
                    TaskTiming::new(
                        period,
                        TaskPhaseNanos::new(0),
                        long_deadline,
                        budget,
                        hard_limit,
                    ),
                    Err(ExecutionContractError::InvalidDeadline)
                );
            }
            let long_limit = HardLimitNanos::new(10);
            if let Ok(long_limit) = long_limit {
                assert_eq!(
                    TaskTiming::new(period, TaskPhaseNanos::new(0), deadline, budget, long_limit,),
                    Err(ExecutionContractError::InvalidHardLimit)
                );
            }
            let large_budget = ExecutionBudgetNanos::new(9);
            if let Ok(large_budget) = large_budget {
                assert_eq!(
                    TaskTiming::new(
                        period,
                        TaskPhaseNanos::new(0),
                        deadline,
                        large_budget,
                        hard_limit,
                    ),
                    Err(ExecutionContractError::InvalidExecutionBudget)
                );
            }
        }
    }

    #[test]
    fn miss_policy_rejects_capacity_and_threshold_edges() {
        assert_eq!(
            MissWindow::new(0, 8),
            Err(ExecutionContractError::InvalidMissWindow)
        );
        assert_eq!(
            MissWindow::new(9, 8),
            Err(ExecutionContractError::InvalidMissWindow)
        );
        let window = MissWindow::new(8, 8);
        if let Ok(window) = window {
            assert_eq!(
                MissPolicy::new(window, 9, 1),
                Err(ExecutionContractError::InvalidMaxMisses)
            );
            assert_eq!(
                MissPolicy::new(window, 8, 0),
                Err(ExecutionContractError::InvalidConsecutiveMisses)
            );
            assert_eq!(
                MissPolicy::new(window, 8, 9),
                Err(ExecutionContractError::InvalidConsecutiveMisses)
            );
            assert!(MissPolicy::new(window, 8, 8).is_ok());
        }
    }

    fn valid_timing() -> Result<TaskTiming, ExecutionContractError> {
        TaskTiming::new(
            TaskPeriodNanos::new(10_000)?,
            TaskPhaseNanos::new(500),
            RelativeDeadlineNanos::new(9_000)?,
            ExecutionBudgetNanos::new(4_000)?,
            HardLimitNanos::new(8_000)?,
        )
    }
}
