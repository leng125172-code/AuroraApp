//! Linux x64 R0 性能测量与硬件限定报告生成。

use std::cell::Cell;
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use aurora_control_contracts::{
    CommitSequence, EventSequence, ExecutionBudgetNanos, ExecutionContractVersion, FaultReason,
    HardLimitNanos, MissPolicy, MissWindow, RelativeDeadlineNanos, ReleaseSequence, TaskEpoch,
    TaskPeriodNanos, TaskPhaseNanos, TaskPriority, TaskSpec, TaskState, TaskTiming, TraceCapacity,
    TraceCounters, TraceEventKind, TraceRecord, TraceTiming,
};
use aurora_control_engine::{
    CycleStart, MonotonicClock, ReleaseReadiness, ScheduleAction, ScheduleControl, SpscPopError,
    StaticTaskPlanBuilder, TaskTransaction, TraceObserveError, TraceObserver, TracePublishOutcome,
    TracePublisher, WorkSetCapacity, WorkSetIndex, WorkSetLimits, bounded_trace_channel,
};
use aurora_types::{BootEpochId, LocalHandle, MonotonicTimestamp};

use crate::error::{BuildError, BuildResult};

const BASE_PERIOD_NANOS: u64 = 1_000_000;
const WARMUP_BASE_CYCLES: u64 = 1_000;
const MEASURED_BASE_CYCLES: usize = 10_000;
const WORKSET_WORDS_PER_TASK: usize = 64;
const WORKSET_BYTES: usize = TASKS.len() * WORKSET_WORDS_PER_TASK * size_of::<u64>();
const TRACE_SLOTS: u32 = 256;
const STALL_START_CYCLE: usize = 2_500;
const STALL_CYCLES: usize = 1_000;
const STALL_END_CYCLE: usize = STALL_START_CYCLE + STALL_CYCLES;

#[derive(Debug, Clone, Copy)]
struct TaskLoad {
    handle: u32,
    priority: i16,
    period_nanos: u64,
    phase_nanos: u64,
}

const TASKS: [TaskLoad; 4] = [
    TaskLoad {
        handle: 0,
        priority: 10,
        period_nanos: BASE_PERIOD_NANOS,
        phase_nanos: 0,
    },
    TaskLoad {
        handle: 1,
        priority: 8,
        period_nanos: 2_000_000,
        phase_nanos: 250_000,
    },
    TaskLoad {
        handle: 2,
        priority: 6,
        period_nanos: 5_000_000,
        phase_nanos: 500_000,
    },
    TaskLoad {
        handle: 3,
        priority: 4,
        period_nanos: 10_000_000,
        phase_nanos: 750_000,
    },
];

#[derive(Debug, Clone, Copy)]
struct Distribution {
    count: usize,
    minimum: u64,
    p50: u64,
    p999: u64,
    maximum: u64,
}

#[derive(Debug)]
struct BenchmarkResult {
    actual_period: Distribution,
    period_jitter: Distribution,
    release_lateness: Distribution,
    deadline_misses: u64,
    skipped_releases: u64,
    budget_exceeded: u64,
    hard_limit_exceeded: u64,
    task_release_counts: [u64; TASKS.len()],
    trace_attempts: u64,
    trace_published: u64,
    trace_dropped: u64,
    trace_full: u64,
    trace_consumed: u64,
    trace_observed_gaps: u64,
    trace_high_water: usize,
    trace_saturated: bool,
    workload_checksum: u64,
}

#[derive(Debug)]
struct SystemInfo {
    generated_at: String,
    operating_system: String,
    kernel: String,
    architecture: String,
    cpu_model: String,
    logical_cpu_count: usize,
    memory_total_kib: u64,
    commit: String,
    tree_state: &'static str,
    rustc: String,
    cargo: String,
}

#[derive(Debug, Clone, Copy)]
struct ProcessSnapshot {
    cpu_ticks: u64,
    resident_kib: u64,
    peak_resident_kib: u64,
}

/// 运行固定 R0 负载并将报告写入调用方指定路径。
pub(crate) fn generate_report(repository_root: &Path, output: &Path) -> BuildResult<()> {
    if !cfg!(target_os = "linux") || std::env::consts::ARCH != "x86_64" {
        return Err(validation(
            "R0 performance report requires a Linux x86_64 process",
        ));
    }
    if cfg!(debug_assertions) {
        return Err(validation(
            "R0 performance report requires `cargo run --release`",
        ));
    }

    let system = SystemInfo::collect(repository_root)?;
    let clock_ticks_per_second = clock_ticks_per_second(repository_root)?;
    let before = process_snapshot()?;
    let measured_at = Instant::now();
    let fault_probe_passed = run_fault_probe()?;
    if !fault_probe_passed {
        return Err(validation(
            "Fault probe accepted a partial bank or publishable output",
        ));
    }
    let benchmark = run_benchmark()?;
    let wall_seconds = measured_at.elapsed().as_secs_f64();
    let after = process_snapshot()?;
    let cpu_ticks = after
        .cpu_ticks
        .checked_sub(before.cpu_ticks)
        .ok_or_else(|| validation("process CPU ticks moved backwards"))?;
    let cpu_ticks = u32::try_from(cpu_ticks)
        .map_err(|_| validation("process CPU ticks exceed the report numeric range"))?;
    let clock_ticks_per_second = u32::try_from(clock_ticks_per_second)
        .map_err(|_| validation("CLK_TCK exceeds the report numeric range"))?;
    let cpu_seconds = f64::from(cpu_ticks) / f64::from(clock_ticks_per_second);
    let cpu_percent = if wall_seconds > 0.0 {
        cpu_seconds * 100.0 / wall_seconds
    } else {
        0.0
    };

    let report = render_report(
        &system,
        &benchmark,
        fault_probe_passed,
        wall_seconds,
        cpu_seconds,
        cpu_percent,
        after,
    );
    write_file(output, report.as_bytes())
}

impl SystemInfo {
    fn collect(repository_root: &Path) -> BuildResult<Self> {
        let cpu_info = read_file(Path::new("/proc/cpuinfo"))?;
        let memory_info = read_file(Path::new("/proc/meminfo"))?;
        let os_release = read_file(Path::new("/etc/os-release"))?;
        let status = command_output(repository_root, "git", &["status", "--porcelain"])?;
        Ok(Self {
            generated_at: command_output(repository_root, "date", &["--iso-8601=seconds"])?,
            operating_system: os_release_value(&os_release, "PRETTY_NAME")
                .unwrap_or_else(|| "unknown".to_owned()),
            kernel: read_file(Path::new("/proc/sys/kernel/osrelease"))?
                .trim()
                .to_owned(),
            architecture: command_output(repository_root, "uname", &["-m"])?,
            cpu_model: cpu_info_value(&cpu_info, "model name")
                .unwrap_or_else(|| "unknown".to_owned()),
            logical_cpu_count: cpu_info
                .lines()
                .filter(|line| line.starts_with("processor"))
                .count(),
            memory_total_kib: keyed_kib(&memory_info, "MemTotal:")?,
            commit: command_output(repository_root, "git", &["rev-parse", "HEAD"])?,
            tree_state: if status.is_empty() { "clean" } else { "dirty" },
            rustc: command_output(repository_root, "rustc", &["--version"])?,
            cargo: command_output(repository_root, "cargo", &["--version"])?,
        })
    }
}

// 测量循环按调度、执行、采样、Trace 的固定顺序线性展开，避免拆分后隐藏周期边界。
#[allow(clippy::too_many_lines)]
fn run_benchmark() -> BuildResult<BenchmarkResult> {
    let epoch = benchmark_epoch()?;
    let clock = PerformanceClock::new(epoch);
    let capacity = WorkSetCapacity::new(TASKS.len()).map_err(contract_error)?;
    let limits = WorkSetLimits::new(capacity, 64 * 1024);
    let mut builder =
        StaticTaskPlanBuilder::new(clock.now(), capacity, limits).map_err(engine_error)?;
    for task in TASKS {
        builder.add_task(task_spec(task)?).map_err(engine_error)?;
    }
    let mut plan = builder.seal().map_err(engine_error)?;
    let trace_capacity = TraceCapacity::new(TRACE_SLOTS, TRACE_SLOTS).map_err(contract_error)?;
    let (mut trace_publisher, mut trace_observer) =
        bounded_trace_channel(epoch, trace_capacity).map_err(engine_error)?;
    let mut worksets = [[0_u64; WORKSET_WORDS_PER_TASK]; TASKS.len()];
    let mut actual_periods = Vec::with_capacity(MEASURED_BASE_CYCLES.saturating_sub(1));
    let mut period_jitter = Vec::with_capacity(MEASURED_BASE_CYCLES.saturating_sub(1));
    let mut release_lateness = Vec::with_capacity(MEASURED_BASE_CYCLES);
    let mut previous_base_start = None;
    let mut base_release_count = 0_u64;
    let mut measured_base_count = 0_usize;
    let mut event_sequence = 0_u64;
    let mut deadline_misses = 0_u64;
    let mut skipped_releases = 0_u64;
    let mut budget_exceeded = 0_u64;
    let mut hard_limit_exceeded = 0_u64;
    let mut task_release_counts = [0_u64; TASKS.len()];
    let mut trace_consumed = 0_u64;
    let mut trace_observed_gaps = 0_u64;

    while measured_base_count < MEASURED_BASE_CYCLES {
        let selected = match plan
            .observe(&clock, ScheduleControl::Continue)
            .map_err(engine_error)?
        {
            ScheduleAction::WaitUntil { release } => {
                clock.wait_until(release.elapsed_nanos());
                continue;
            }
            ScheduleAction::Release(selected) => selected,
            ScheduleAction::Stopped { .. } => {
                return Err(validation("R0 benchmark plan stopped unexpectedly"));
            }
        };

        let spec = selected.task();
        let task_index = usize::try_from(spec.handle().get())
            .map_err(|_| validation("task handle does not fit usize"))?;
        let release_sequence = selected.release_sequence();
        let scheduled_release = selected.scheduled_release();
        let absolute_deadline = selected.absolute_deadline();
        let skipped = selected
            .skipped_releases()
            .map_or(0, aurora_control_engine::SkippedReleases::count);
        add_counter(&mut skipped_releases, skipped, "skipped releases")?;
        add_counter(&mut deadline_misses, skipped, "deadline misses")?;
        let task_release_count = task_release_counts
            .get_mut(task_index)
            .ok_or_else(|| validation("task handle is outside the fixed task set"))?;
        add_counter(task_release_count, 1, "task releases")?;

        let readiness = selected
            .begin(&clock, ScheduleControl::Continue)
            .map_err(engine_error)?;
        let (started_at, finished_at, state_after) = match readiness {
            ReleaseReadiness::Execute(mut window) => {
                let started = window.started_at();
                let task_workset = worksets
                    .get_mut(task_index)
                    .ok_or_else(|| validation("task handle is outside the fixed workset"))?;
                exercise_workset(task_workset, release_sequence.get());
                let checkpoint = window.checkpoint(&clock).map_err(engine_error)?;
                add_counter(
                    &mut budget_exceeded,
                    u64::from(checkpoint.execution_budget_exceeded()),
                    "execution budget exceedances",
                )?;
                add_counter(
                    &mut hard_limit_exceeded,
                    u64::from(checkpoint.hard_limit_exceeded()),
                    "HardLimit exceedances",
                )?;
                add_counter(
                    &mut deadline_misses,
                    u64::from(checkpoint.deadline_missed()),
                    "deadline misses",
                )?;
                (
                    Some(started),
                    Some(checkpoint.observed_at()),
                    if checkpoint.deadline_missed() {
                        TaskState::Degraded
                    } else {
                        TaskState::Running
                    },
                )
            }
            ReleaseReadiness::StartAfterDeadline => {
                add_counter(&mut deadline_misses, 1, "deadline misses")?;
                (None, None, TaskState::Degraded)
            }
            ReleaseReadiness::Stopped => {
                return Err(validation("R0 benchmark release stopped unexpectedly"));
            }
        };

        if task_index == 0 {
            let observed_start = started_at.unwrap_or_else(|| clock.now());
            add_counter(&mut base_release_count, 1, "base task releases")?;
            if base_release_count > WARMUP_BASE_CYCLES {
                let lateness = observed_start
                    .elapsed_nanos()
                    .saturating_sub(scheduled_release.elapsed_nanos());
                release_lateness.push(lateness);
                if let Some(previous) = previous_base_start {
                    let actual = observed_start.elapsed_nanos().saturating_sub(previous);
                    actual_periods.push(actual);
                    period_jitter.push(actual.abs_diff(BASE_PERIOD_NANOS));
                }
                previous_base_start = Some(observed_start.elapsed_nanos());
                measured_base_count = measured_base_count
                    .checked_add(1)
                    .ok_or_else(|| validation("measured base cycle count overflow"))?;
            }
        }

        let consumer_stalled = measured_base_count
            .checked_sub(1)
            .is_some_and(|index| (STALL_START_CYCLE..STALL_END_CYCLE).contains(&index));
        if !consumer_stalled {
            let drain_limit = if measured_base_count >= STALL_END_CYCLE {
                8
            } else {
                1
            };
            drain_trace(
                &mut trace_observer,
                drain_limit,
                &mut trace_consumed,
                &mut trace_observed_gaps,
            )?;
        }
        publish_trace(
            &mut trace_publisher,
            trace_capacity,
            epoch,
            spec,
            EventSequence::new(event_sequence),
            release_sequence,
            scheduled_release,
            absolute_deadline,
            started_at,
            finished_at,
            state_after,
        )?;
        event_sequence = event_sequence
            .checked_add(1)
            .ok_or_else(|| validation("Trace event sequence exhausted"))?;
    }

    drain_trace(
        &mut trace_observer,
        usize::try_from(TRACE_SLOTS)
            .map_err(|_| validation("Trace capacity does not fit usize"))?,
        &mut trace_consumed,
        &mut trace_observed_gaps,
    )?;
    if actual_periods.len() != MEASURED_BASE_CYCLES - 1
        || period_jitter.len() != MEASURED_BASE_CYCLES - 1
        || release_lateness.len() != MEASURED_BASE_CYCLES
    {
        return Err(validation(
            "R0 benchmark generated too many or too few timing samples",
        ));
    }
    if clock.saturated.get() {
        return Err(validation("R0 benchmark monotonic nanoseconds saturated"));
    }
    let trace = trace_publisher.statistics();
    if trace.saturated {
        return Err(validation("R0 benchmark Trace counters saturated"));
    }
    let workload_checksum = worksets
        .iter()
        .flatten()
        .fold(0_u64, |total, value| total.wrapping_add(*value));

    Ok(BenchmarkResult {
        actual_period: distribution(&mut actual_periods)?,
        period_jitter: distribution(&mut period_jitter)?,
        release_lateness: distribution(&mut release_lateness)?,
        deadline_misses,
        skipped_releases,
        budget_exceeded,
        hard_limit_exceeded,
        task_release_counts,
        trace_attempts: trace.push_attempts,
        trace_published: trace.published,
        trace_dropped: trace.dropped_newest,
        trace_full: trace.full,
        trace_consumed,
        trace_observed_gaps,
        trace_high_water: trace.high_water_mark,
        trace_saturated: trace.saturated,
        workload_checksum,
    })
}

#[allow(clippy::too_many_arguments)]
fn publish_trace(
    publisher: &mut TracePublisher,
    capacity: TraceCapacity,
    epoch: BootEpochId,
    spec: TaskSpec,
    event_sequence: EventSequence,
    release_sequence: ReleaseSequence,
    scheduled_release: MonotonicTimestamp,
    absolute_deadline: MonotonicTimestamp,
    started_at: Option<MonotonicTimestamp>,
    finished_at: Option<MonotonicTimestamp>,
    state_after: TaskState,
) -> BuildResult<()> {
    let statistics = publisher.statistics();
    let counters = TraceCounters::new(
        capacity,
        u32::try_from(statistics.readable)
            .map_err(|_| validation("Trace occupancy does not fit u32"))?,
        u32::try_from(statistics.high_water_mark)
            .map_err(|_| validation("Trace high-water does not fit u32"))?,
        statistics.push_attempts,
        statistics.published,
        statistics.dropped_newest,
        statistics.full,
        statistics.saturated,
    )
    .map_err(contract_error)?;
    let timing = TraceTiming::new(
        scheduled_release,
        absolute_deadline,
        started_at,
        finished_at,
    )
    .map_err(contract_error)?;
    let record = TraceRecord::new(
        ExecutionContractVersion::V1_0,
        epoch,
        spec.handle(),
        TaskEpoch::new(1).map_err(contract_error)?,
        event_sequence,
        release_sequence,
        CommitSequence::ZERO,
        CommitSequence::ZERO,
        TraceEventKind::ReleaseCompleted,
        timing,
        None,
        TaskState::Running,
        state_after,
        None,
        None,
        None,
        None,
        None,
        None,
        counters,
    )
    .map_err(contract_error)?;
    match publisher.try_publish(record).map_err(engine_error)? {
        TracePublishOutcome::Published(_) | TracePublishOutcome::DroppedNewest(_) => Ok(()),
    }
}

fn drain_trace(
    observer: &mut TraceObserver,
    limit: usize,
    consumed: &mut u64,
    gaps: &mut u64,
) -> BuildResult<()> {
    for _ in 0..limit {
        match observer.try_observe() {
            Ok(observation) => {
                add_counter(consumed, 1, "consumed Trace records")?;
                add_counter(gaps, observation.missed_before(), "observed Trace gaps")?;
            }
            Err(TraceObserveError::Spsc(SpscPopError::Empty)) => break,
            Err(error) => return Err(engine_error(error)),
        }
    }
    Ok(())
}

fn exercise_workset(workset: &mut [u64; WORKSET_WORDS_PER_TASK], sequence: u64) {
    for value in &mut *workset {
        *value = value.rotate_left(13) ^ sequence.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
    black_box(workset);
}

fn add_counter(counter: &mut u64, amount: u64, name: &str) -> BuildResult<()> {
    *counter = counter
        .checked_add(amount)
        .ok_or_else(|| validation(format!("{name} counter overflow")))?;
    Ok(())
}

fn run_fault_probe() -> BuildResult<bool> {
    let epoch = benchmark_epoch()?;
    let clock = ProbeClock::new(epoch);
    let load = TaskLoad {
        handle: 0,
        priority: 0,
        period_nanos: 100,
        phase_nanos: 0,
    };
    let spec = task_spec(load)?;
    let capacity = WorkSetCapacity::new(1).map_err(contract_error)?;
    let mut builder =
        StaticTaskPlanBuilder::new(clock.now(), capacity, WorkSetLimits::new(capacity, 1024))
            .map_err(engine_error)?;
    builder.add_task(spec).map_err(engine_error)?;
    let mut plan = builder.seal().map_err(engine_error)?;
    let selected = match plan
        .observe(&clock, ScheduleControl::Continue)
        .map_err(engine_error)?
    {
        ScheduleAction::Release(selected) => selected,
        ScheduleAction::WaitUntil { .. } | ScheduleAction::Stopped { .. } => {
            return Err(validation("Fault probe did not receive its first release"));
        }
    };
    let image_capacity = WorkSetCapacity::new(2).map_err(contract_error)?;
    let mut transaction = TaskTransaction::new(
        spec,
        epoch,
        &[1],
        &[2],
        WorkSetLimits::new(image_capacity, 1024),
    )
    .map_err(engine_error)?;
    let CycleStart::Execute(mut cycle) = transaction
        .begin(selected, &clock, ScheduleControl::Continue)
        .map_err(engine_error)?
    else {
        return Err(validation("Fault probe release was not executable"));
    };
    cycle
        .write_state(WorkSetIndex::new(0), 99)
        .map_err(engine_error)?;
    cycle
        .write_output(WorkSetIndex::new(0), 100)
        .map_err(engine_error)?;
    let fault = cycle.discard(FaultReason::TaskExecutionFault);
    let values = transaction.diagnostic().values();
    Ok(fault.reason == FaultReason::TaskExecutionFault
        && values.state(WorkSetIndex::new(0)).map_err(engine_error)? == 1
        && values.output(WorkSetIndex::new(0)).map_err(engine_error)? == 2
        && transaction.publishable().is_none())
}

fn task_spec(load: TaskLoad) -> BuildResult<TaskSpec> {
    let deadline = load.period_nanos;
    let budget = load.period_nanos / 2;
    let hard_limit = load.period_nanos.saturating_mul(9) / 10;
    Ok(TaskSpec::new(
        ExecutionContractVersion::V1_0,
        LocalHandle::new(load.handle).map_err(contract_error)?,
        TaskPriority::new(load.priority),
        TaskTiming::new(
            TaskPeriodNanos::new(load.period_nanos).map_err(contract_error)?,
            TaskPhaseNanos::new(load.phase_nanos),
            RelativeDeadlineNanos::new(deadline).map_err(contract_error)?,
            ExecutionBudgetNanos::new(budget).map_err(contract_error)?,
            HardLimitNanos::new(hard_limit).map_err(contract_error)?,
        )
        .map_err(contract_error)?,
        MissPolicy::new(MissWindow::new(128, 128).map_err(contract_error)?, 64, 64)
            .map_err(contract_error)?,
    ))
}

fn distribution(values: &mut [u64]) -> BuildResult<Distribution> {
    if values.is_empty() {
        return Err(validation("performance distribution has no samples"));
    }
    values.sort_unstable();
    Ok(Distribution {
        count: values.len(),
        minimum: values[0],
        p50: percentile(values, 500, 1000)?,
        p999: percentile(values, 999, 1000)?,
        maximum: values[values.len() - 1],
    })
}

fn percentile(values: &[u64], numerator: usize, denominator: usize) -> BuildResult<u64> {
    if values.is_empty() || numerator == 0 || numerator > denominator || denominator == 0 {
        return Err(validation("invalid percentile request"));
    }
    let rank = values
        .len()
        .checked_mul(numerator)
        .and_then(|value| value.checked_add(denominator - 1))
        .ok_or_else(|| validation("percentile rank overflow"))?
        / denominator;
    Ok(values[rank - 1])
}

// 报告字段集中在一个固定模板中，便于审查是否漏项且不会进入周期路径。
#[allow(clippy::too_many_lines)]
fn render_report(
    system: &SystemInfo,
    benchmark: &BenchmarkResult,
    fault_probe_passed: bool,
    wall_seconds: f64,
    cpu_seconds: f64,
    cpu_percent: f64,
    process: ProcessSnapshot,
) -> String {
    format!(
        "# R0 Linux x64 性能与退出门禁报告\n\n\
> 本报告只描述下列机器、WSL/Linux 环境、工程负载和 commit，不是 Aurora 的跨硬件性能保证，\
也不代表 RT-Linux、PREEMPT_RT、RTOS、裸机或功能安全能力。\n\n\
## 被测环境\n\n\
| 项目 | 值 |\n|---|---|\n\
| 生成时间 | `{generated_at}` |\n\
| Commit | `{commit}` |\n\
| Git 工作树 | `{tree_state}` |\n\
| 构建类型 | Cargo `release` |\n\
| OS | {operating_system} |\n\
| Kernel | `{kernel}` |\n\
| Architecture | `{architecture}` |\n\
| CPU | {cpu_model} |\n\
| Logical CPUs | {logical_cpu_count} |\n\
| Linux 可见内存 | {memory_total_kib} KiB |\n\
| Rust | `{rustc}` |\n\
| Cargo | `{cargo}` |\n\n\
## 固定工程负载\n\n\
- 基准周期：1,000,000 ns；warm-up：{warmup} 个基准周期；测量：{measured} 个基准周期。\n\
- 静态任务：4；每任务固定 64 个 `u64`，总工作集 {workset_bytes} bytes；任务体执行固定 64 次更新。\n\
- Trace：固定 320-byte record，SPSC `DropNewest`，容量 {trace_slots} slots。\n\
- Consumer stall：测量周期 [{stall_start}, {stall_end})，固定 {stall_cycles} 个基准周期不读取；恢复后每次最多读取 8 项。\n\
- Fault：独立探针在 state/output 各部分写入后注入 `TaskExecutionFault`，检查旧 bank 保留且无 output 可发布。\n\
- 数据库、网络和物理 I/O：R0 范围不包含，负载为 none。\n\n\
| Handle | Period (ns) | Phase (ns) | Priority | Observed releases |\n|---:|---:|---:|---:|---:|\n\
| 0 | 1000000 | 0 | 10 | {task0} |\n\
| 1 | 2000000 | 250000 | 8 | {task1} |\n\
| 2 | 5000000 | 500000 | 6 | {task2} |\n\
| 3 | 10000000 | 750000 | 4 | {task3} |\n\n\
## 测量结果\n\n\
`actual period` 是相邻 handle 0 实际开始点之差；`jitter` 是该值与 1,000,000 ns 的绝对差；\
`release lateness` 是实际开始点减绝对计划 release。百分位使用 nearest-rank，所有时间均为 ns。\n\n\
| 指标 | Samples | Min | p50 | p99.9 | Max |\n|---|---:|---:|---:|---:|---:|\n\
| Actual period | {actual_count} | {actual_min} | {actual_p50} | {actual_p999} | {actual_max} |\n\
| Period jitter | {jitter_count} | {jitter_min} | {jitter_p50} | {jitter_p999} | {jitter_max} |\n\
| Release lateness | {late_count} | {late_min} | {late_p50} | {late_p999} | {late_max} |\n\n\
- Deadline misses：{deadline_misses}；skipped releases：{skipped_releases}。\n\
- Execution budget exceeded：{budget_exceeded}；HardLimit exceeded：{hard_limit_exceeded}。\n\
- 测量 wall time：{wall_seconds:.3} s；process CPU time：{cpu_seconds:.3} s；平均单核 CPU：{cpu_percent:.2}%。\n\
- 测量结束 RSS：{resident_kib} KiB；process peak RSS：{peak_resident_kib} KiB。\n\
- Trace attempts/published/dropped/full：{trace_attempts}/{trace_published}/{trace_dropped}/{trace_full}。\n\
- Trace high-water：{trace_high_water}/{trace_slots}；consumed：{trace_consumed}；observed sequence gaps：{trace_observed_gaps}；counter saturated：{trace_saturated}。\n\
- Fault 探针：{fault_result}；固定工作集 checksum：`{checksum:016x}`。\n\n\
## R0 Gate 退出清单\n\n\
- [x] 同一输入 Trace 的状态、输出和诊断一致：[`r0_verification.rs`](../../Sources/Rust/crates/aurora-control-engine/tests/r0_verification.rs)。\n\
- [x] 多周期/多相位顺序、UTC 跳变和 release 数量边界：同一 R0-08 套件。\n\
- [x] Snapshot reader/stall、SPSC full/empty/wrap/gap/high-water：同一 R0-08 套件。\n\
- [x] Fault discard/reset：同一 R0-08 套件；本报告另执行最小 Fault 探针。\n\
- [x] Rust R0 自动化门禁：报告生成前同次执行 `fmt --check`、workspace `clippy -D warnings` 和 workspace tests；任一失败不会写报告。\n\
- [x] Linux x64 性能字段：本报告记录实际周期、p50/p99.9/max jitter、deadline miss、CPU、内存和队列水位。\n\n\
复现命令：\n\n\
```bash\n\
cargo run --locked --release --manifest-path Sources/Rust/Cargo.toml -p aurora-build -- r0-report --output Builds/r0-linux-x64-report.md\n\
```\n\n\
## 限制与剩余风险\n\n\
- 当前结果来自 WSL2 普通 Linux 调度器，不是独立 target、实时内核或生产硬件准入结果。\n\
- 固定负载不含 R1+ 的 ST、Workflow、Guardian、真实 I/O、数据库或网络；这些阶段必须重新测量。\n\
- CPU 为本进程在测量窗口内的平均单核占用；内存来自 `/proc/self/status`，不代表系统总压力。\n\
- 本报告关闭 R0 实现 Gate，但具体部署仍需在目标硬件和目标工程上生成新的同格式报告并由部署方判断。\n",
        generated_at = system.generated_at,
        commit = system.commit,
        tree_state = system.tree_state,
        operating_system = system.operating_system,
        kernel = system.kernel,
        architecture = system.architecture,
        cpu_model = system.cpu_model,
        logical_cpu_count = system.logical_cpu_count,
        memory_total_kib = system.memory_total_kib,
        rustc = system.rustc,
        cargo = system.cargo,
        warmup = WARMUP_BASE_CYCLES,
        measured = MEASURED_BASE_CYCLES,
        workset_bytes = WORKSET_BYTES,
        trace_slots = TRACE_SLOTS,
        stall_start = STALL_START_CYCLE,
        stall_end = STALL_END_CYCLE,
        stall_cycles = STALL_CYCLES,
        task0 = benchmark.task_release_counts[0],
        task1 = benchmark.task_release_counts[1],
        task2 = benchmark.task_release_counts[2],
        task3 = benchmark.task_release_counts[3],
        actual_count = benchmark.actual_period.count,
        actual_min = benchmark.actual_period.minimum,
        actual_p50 = benchmark.actual_period.p50,
        actual_p999 = benchmark.actual_period.p999,
        actual_max = benchmark.actual_period.maximum,
        jitter_count = benchmark.period_jitter.count,
        jitter_min = benchmark.period_jitter.minimum,
        jitter_p50 = benchmark.period_jitter.p50,
        jitter_p999 = benchmark.period_jitter.p999,
        jitter_max = benchmark.period_jitter.maximum,
        late_count = benchmark.release_lateness.count,
        late_min = benchmark.release_lateness.minimum,
        late_p50 = benchmark.release_lateness.p50,
        late_p999 = benchmark.release_lateness.p999,
        late_max = benchmark.release_lateness.maximum,
        deadline_misses = benchmark.deadline_misses,
        skipped_releases = benchmark.skipped_releases,
        budget_exceeded = benchmark.budget_exceeded,
        hard_limit_exceeded = benchmark.hard_limit_exceeded,
        wall_seconds = wall_seconds,
        cpu_seconds = cpu_seconds,
        cpu_percent = cpu_percent,
        resident_kib = process.resident_kib,
        peak_resident_kib = process.peak_resident_kib,
        trace_attempts = benchmark.trace_attempts,
        trace_published = benchmark.trace_published,
        trace_dropped = benchmark.trace_dropped,
        trace_full = benchmark.trace_full,
        trace_high_water = benchmark.trace_high_water,
        trace_consumed = benchmark.trace_consumed,
        trace_observed_gaps = benchmark.trace_observed_gaps,
        trace_saturated = benchmark.trace_saturated,
        fault_result = if fault_probe_passed {
            "passed"
        } else {
            "failed"
        },
        checksum = benchmark.workload_checksum,
    )
}

#[derive(Debug)]
struct PerformanceClock {
    epoch: BootEpochId,
    started_at: Instant,
    saturated: Cell<bool>,
}

impl PerformanceClock {
    fn new(epoch: BootEpochId) -> Self {
        Self {
            epoch,
            started_at: Instant::now(),
            saturated: Cell::new(false),
        }
    }

    fn elapsed_nanos(&self) -> u64 {
        if let Ok(value) = u64::try_from(self.started_at.elapsed().as_nanos()) {
            value
        } else {
            self.saturated.set(true);
            u64::MAX
        }
    }

    fn wait_until(&self, target_nanos: u64) {
        let elapsed = self.elapsed_nanos();
        if let Some(remaining) = target_nanos.checked_sub(elapsed) {
            thread::sleep(Duration::from_nanos(remaining));
        }
    }
}

impl MonotonicClock for PerformanceClock {
    fn now(&self) -> MonotonicTimestamp {
        MonotonicTimestamp::new(self.epoch, self.elapsed_nanos())
    }
}

#[derive(Debug)]
struct ProbeClock {
    now: Cell<MonotonicTimestamp>,
}

impl ProbeClock {
    fn new(epoch: BootEpochId) -> Self {
        Self {
            now: Cell::new(MonotonicTimestamp::new(epoch, 0)),
        }
    }
}

impl MonotonicClock for ProbeClock {
    fn now(&self) -> MonotonicTimestamp {
        self.now.get()
    }
}

fn process_snapshot() -> BuildResult<ProcessSnapshot> {
    let stat = read_file(Path::new("/proc/self/stat"))?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| validation("/proc/self/stat has no command terminator"))?;
    let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    let user_ticks = parse_field(&fields, 11, "/proc/self/stat utime")?;
    let system_ticks = parse_field(&fields, 12, "/proc/self/stat stime")?;
    let status = read_file(Path::new("/proc/self/status"))?;
    Ok(ProcessSnapshot {
        cpu_ticks: user_ticks
            .checked_add(system_ticks)
            .ok_or_else(|| validation("process CPU tick sum overflow"))?,
        resident_kib: keyed_kib(&status, "VmRSS:")?,
        peak_resident_kib: keyed_kib(&status, "VmHWM:")?,
    })
}

fn parse_field(fields: &[&str], index: usize, name: &str) -> BuildResult<u64> {
    fields
        .get(index)
        .ok_or_else(|| validation(format!("missing {name}")))?
        .parse::<u64>()
        .map_err(|error| validation(format!("invalid {name}: {error}")))
}

fn clock_ticks_per_second(repository_root: &Path) -> BuildResult<u64> {
    command_output(repository_root, "getconf", &["CLK_TCK"])?
        .parse::<u64>()
        .map_err(|error| validation(format!("invalid CLK_TCK: {error}")))
}

fn keyed_kib(contents: &str, key: &str) -> BuildResult<u64> {
    let line = contents
        .lines()
        .find(|line| line.starts_with(key))
        .ok_or_else(|| validation(format!("missing {key}")))?;
    line.split_whitespace()
        .nth(1)
        .ok_or_else(|| validation(format!("missing {key} value")))?
        .parse::<u64>()
        .map_err(|error| validation(format!("invalid {key} value: {error}")))
}

fn cpu_info_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (candidate, value) = line.split_once(':')?;
        (candidate.trim() == key).then(|| value.trim().to_owned())
    })
}

fn os_release_value(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (candidate, value) = line.split_once('=')?;
        (candidate == key).then(|| value.trim_matches('"').to_owned())
    })
}

fn command_output(
    repository_root: &Path,
    program: &str,
    arguments: &[&str],
) -> BuildResult<String> {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(repository_root)
        .output()
        .map_err(|source| BuildError::StartProcess {
            program: format!("{program} {}", arguments.join(" ")),
            source,
        })?;
    if !output.status.success() {
        return Err(BuildError::ProcessFailed {
            program: format!("{program} {}", arguments.join(" ")),
            status: output.status.code().map_or_else(
                || "terminated by signal".to_owned(),
                |code| code.to_string(),
            ),
        });
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| validation(format!("{program} output is not UTF-8: {error}")))
}

fn read_file(path: &Path) -> BuildResult<String> {
    fs::read_to_string(path).map_err(|source| BuildError::Io {
        operation: "read",
        path: path.to_path_buf(),
        source,
    })
}

fn write_file(path: &Path, bytes: &[u8]) -> BuildResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| BuildError::Io {
            operation: "create directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }
    fs::write(path, bytes).map_err(|source| BuildError::Io {
        operation: "write",
        path: path.to_path_buf(),
        source,
    })
}

fn benchmark_epoch() -> BuildResult<BootEpochId> {
    BootEpochId::from_bytes([
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0xa9, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ])
    .map_err(contract_error)
}

fn validation(message: impl Into<String>) -> BuildError {
    BuildError::Validation(message.into())
}

fn contract_error(error: impl std::fmt::Display) -> BuildError {
    validation(format!("R0 performance contract error: {error}"))
}

fn engine_error(error: impl std::fmt::Display) -> BuildError {
    validation(format!("R0 performance engine error: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{distribution, os_release_value, percentile};

    #[test]
    fn nearest_rank_percentiles_preserve_exact_boundaries() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut values = vec![5, 1, 4, 2, 3];
        let measured = distribution(&mut values)?;
        assert_eq!(measured.minimum, 1);
        assert_eq!(measured.p50, 3);
        assert_eq!(measured.p999, 5);
        assert_eq!(measured.maximum, 5);
        assert!(percentile(&values, 0, 1000).is_err());
        assert!(percentile(&[], 999, 1000).is_err());
        Ok(())
    }

    #[test]
    fn os_release_parser_accepts_quoted_value_without_overreading() {
        let contents = "NAME=Ubuntu\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"\nEXTRA=value\n";
        assert_eq!(
            os_release_value(contents, "PRETTY_NAME").as_deref(),
            Some("Ubuntu 26.04 LTS")
        );
        assert_eq!(os_release_value(contents, "MISSING"), None);
    }
}
