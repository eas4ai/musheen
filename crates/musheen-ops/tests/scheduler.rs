use musheen_core::{
    CapabilityMatrix, CapabilityState, ProviderId, ResourceLimitConfig, ResourceLimits, StorePath,
};
use musheen_ops::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase, ArchiveOperationPlan, Clock, JobState,
    OperationKind, OperationPlan, ProviderLimits, ProviderSnapshot, Scheduler, SchedulerError,
    WorkClass,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Default)]
struct ManualClock(Arc<AtomicU64>);

impl ManualClock {
    fn set(&self, value: u64) {
        self.0.store(value, Ordering::Release);
    }
}

#[test]
fn archive_jobs_use_scheduler_plans_conflicts_and_events() {
    let limits = ResourceLimits::default();
    let clock = ManualClock::default();
    let scheduler = Scheduler::with_clock(&limits, clock.clone());
    let local = provider("archive-local", ProviderLimits::unbounded());
    let archive = ArchiveOperationPlan::create(
        vec![
            StorePath::from_unix_path("/data/one"),
            StorePath::from_unix_path("/data/two"),
        ],
        StorePath::from_unix_path("/out/data.zip"),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .unwrap();
    let archive_id = scheduler
        .enqueue_archive(archive.clone(), local.clone())
        .unwrap();
    let conflicting = scheduler
        .enqueue(
            OperationPlan::new(
                OperationKind::Move,
                local,
                Some(StorePath::from_unix_path("/data/two/child")),
                StorePath::from_unix_path("/elsewhere/child"),
            )
            .unwrap(),
        )
        .unwrap();

    let started = scheduler.start_ready().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].id(), archive_id);
    assert_eq!(started[0].archive_plan(), Some(&archive));
    assert_eq!(scheduler.state(conflicting), Some(JobState::Queued));

    clock.set(10);
    scheduler
        .emit_archive_phase(archive_id, ArchiveEventPhase::Preflight)
        .unwrap();
    scheduler
        .emit_archive_phase(archive_id, ArchiveEventPhase::Encoding)
        .unwrap();
    assert_eq!(
        scheduler
            .events()
            .iter()
            .filter_map(|event| event.archive_phase_value())
            .collect::<Vec<_>>(),
        vec![ArchiveEventPhase::Preflight, ArchiveEventPhase::Encoding]
    );
}

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

fn provider(name: &str, limits: ProviderLimits) -> ProviderSnapshot {
    ProviderSnapshot::new(
        ProviderId::new(name).expect("fixture provider ID is valid"),
        CapabilityMatrix::new(|_| CapabilityState::Supported),
        limits,
    )
}

fn plan(index: usize, kind: OperationKind, provider: ProviderSnapshot) -> OperationPlan {
    let source = kind
        .requires_source()
        .then(|| StorePath::from_unix_path(format!("/{}/source-{index}", provider.id().as_str())));
    let destination =
        StorePath::from_unix_path(format!("/{}/destination-{index}", provider.id().as_str()));
    OperationPlan::new(kind, provider, source, destination).expect("fixture plan is valid")
}

#[test]
fn defaults_provider_limits_and_fifo_progress_are_enforced() {
    let limits = ResourceLimits::default();
    assert_eq!(limits.operation_data_mutations(), 2);
    assert_eq!(limits.operation_metadata_jobs(), 4);
    assert_eq!(limits.operation_hash_preview_jobs(), 4);

    let clock = ManualClock::default();
    clock.set(100);
    let scheduler = Scheduler::with_clock(&limits, clock.clone());
    let local = provider("local", ProviderLimits::unbounded());
    let data = (0..3)
        .map(|index| {
            scheduler
                .enqueue(plan(index, OperationKind::Copy, local.clone()))
                .expect("data job queues")
        })
        .collect::<Vec<_>>();
    let metadata = (0..5)
        .map(|index| {
            scheduler
                .enqueue(plan(
                    index + 10,
                    OperationKind::SetPermissions,
                    local.clone(),
                ))
                .expect("metadata job queues")
        })
        .collect::<Vec<_>>();
    let auxiliary = (0..5)
        .map(|index| {
            scheduler
                .enqueue(plan(index + 20, OperationKind::Preview, local.clone()))
                .expect("preview job queues")
        })
        .collect::<Vec<_>>();

    let started = scheduler.start_ready().expect("ready work starts");
    assert_eq!(
        started
            .iter()
            .filter(|job| job.class() == WorkClass::DataMutation)
            .count(),
        2
    );
    assert_eq!(
        started
            .iter()
            .filter(|job| job.class() == WorkClass::Metadata)
            .count(),
        4
    );
    assert_eq!(
        started
            .iter()
            .filter(|job| job.class() == WorkClass::HashOrPreview)
            .count(),
        4
    );
    assert_eq!(started[0].id(), data[0]);
    assert_eq!(started[1].id(), data[1]);
    assert!(started.iter().all(|job| job.started_at() == 100));
    assert_eq!(scheduler.state(data[2]), Some(JobState::Queued));
    assert_eq!(scheduler.state(metadata[4]), Some(JobState::Queued));
    assert_eq!(scheduler.state(auxiliary[4]), Some(JobState::Queued));

    clock.set(110);
    scheduler
        .complete(data[0])
        .expect("the first data job completes");
    scheduler
        .complete(metadata[0])
        .expect("the first metadata job completes");
    let resumed = scheduler.start_ready().expect("queued work advances");
    assert_eq!(
        resumed.iter().map(|job| job.id()).collect::<Vec<_>>(),
        vec![data[2], metadata[4]]
    );
    assert!(resumed.iter().all(|job| job.started_at() == 110));
}

#[test]
fn stricter_provider_limits_do_not_starve_other_providers() {
    let limits = ResourceLimits::try_from(ResourceLimitConfig {
        operation_data_mutations: 3,
        ..ResourceLimitConfig::default()
    })
    .expect("fixture limits are valid");
    let scheduler = Scheduler::with_clock(&limits, ManualClock::default());
    let slow = provider(
        "slow",
        ProviderLimits::new(1, 1, 1).expect("positive provider limits are valid"),
    );
    let fast = provider("fast", ProviderLimits::unbounded());

    let slow_first = scheduler
        .enqueue(plan(0, OperationKind::Copy, slow.clone()))
        .unwrap();
    let slow_second = scheduler
        .enqueue(plan(1, OperationKind::Copy, slow))
        .unwrap();
    let fast_first = scheduler
        .enqueue(plan(2, OperationKind::Copy, fast))
        .unwrap();

    let started = scheduler.start_ready().unwrap();
    assert_eq!(
        started.iter().map(|job| job.id()).collect::<Vec<_>>(),
        vec![slow_first, fast_first]
    );
    assert_eq!(scheduler.state(slow_second), Some(JobState::Queued));
    scheduler.complete(slow_first).unwrap();
    assert_eq!(scheduler.start_ready().unwrap()[0].id(), slow_second);
}

#[test]
fn overlapping_read_write_sets_are_serialized() {
    let limits = ResourceLimits::default();
    let scheduler = Scheduler::with_clock(&limits, ManualClock::default());
    let local = provider("local", ProviderLimits::unbounded());
    let first = OperationPlan::new(
        OperationKind::Move,
        local.clone(),
        Some(StorePath::from_unix_path("/tree")),
        StorePath::from_unix_path("/archive/tree"),
    )
    .unwrap();
    let overlapping = OperationPlan::new(
        OperationKind::Copy,
        local.clone(),
        Some(StorePath::from_unix_path("/tree/child")),
        StorePath::from_unix_path("/backup/child"),
    )
    .unwrap();
    let independent = OperationPlan::new(
        OperationKind::Copy,
        local,
        Some(StorePath::from_unix_path("/other")),
        StorePath::from_unix_path("/backup/other"),
    )
    .unwrap();
    let first_id = scheduler.enqueue(first).unwrap();
    let overlapping_id = scheduler.enqueue(overlapping).unwrap();
    let independent_id = scheduler.enqueue(independent).unwrap();

    let started = scheduler.start_ready().unwrap();
    assert_eq!(
        started.iter().map(|job| job.id()).collect::<Vec<_>>(),
        vec![first_id, independent_id]
    );
    assert_eq!(scheduler.state(overlapping_id), Some(JobState::Queued));
    scheduler.complete(first_id).unwrap();
    assert_eq!(scheduler.start_ready().unwrap()[0].id(), overlapping_id);
}

#[test]
fn cancellation_tokens_are_per_job_and_scheduler_limits_are_snapshotted() {
    let source = ResourceLimits::default();
    let scheduler = Scheduler::with_clock(&source, ManualClock::default());
    let local = provider("local", ProviderLimits::unbounded());
    let first = scheduler
        .enqueue(plan(0, OperationKind::Copy, local.clone()))
        .unwrap();
    let second = scheduler
        .enqueue(plan(1, OperationKind::Copy, local))
        .unwrap();
    let started = scheduler.start_ready().unwrap();
    let first_token = started
        .iter()
        .find(|job| job.id() == first)
        .unwrap()
        .cancellation()
        .clone();
    let second_token = started
        .iter()
        .find(|job| job.id() == second)
        .unwrap()
        .cancellation()
        .clone();

    scheduler.cancel(first).expect("running work can cancel");
    assert!(first_token.is_cancelled());
    assert!(!second_token.is_cancelled());
    assert_eq!(scheduler.limits(), &source);

    let lowered = ResourceLimits::try_from(ResourceLimitConfig {
        operation_data_mutations: 1,
        ..ResourceLimitConfig::default()
    })
    .unwrap();
    assert_eq!(scheduler.limits().operation_data_mutations(), 2);
    assert_eq!(lowered.operation_data_mutations(), 1);
}

#[test]
fn commit_admission_atomically_rejects_late_controls_until_completion() {
    let scheduler = Scheduler::with_clock(&ResourceLimits::default(), ManualClock::default());
    let id = scheduler
        .enqueue(plan(
            90,
            OperationKind::Copy,
            provider("commit-controls", ProviderLimits::unbounded()),
        ))
        .unwrap();
    let job = scheduler.start_ready().unwrap().pop().unwrap();

    scheduler
        .begin_commit(id)
        .expect("running job admits commit");
    assert!(matches!(
        scheduler.cancel(id),
        Err(SchedulerError::CommitInProgress(job_id)) if job_id == id
    ));
    assert!(matches!(
        scheduler.pause(id),
        Err(SchedulerError::CommitInProgress(job_id)) if job_id == id
    ));
    assert!(matches!(
        scheduler.interrupt(id),
        Err(SchedulerError::CommitInProgress(job_id)) if job_id == id
    ));
    assert!(!job.cancellation().is_cancelled());
    assert_eq!(scheduler.state(id), Some(JobState::Running));

    scheduler.complete(id).expect("commit completes atomically");
    assert_eq!(scheduler.state(id), Some(JobState::Completed));
}

#[test]
fn pause_resume_cancel_retry_and_restart_states_are_explicit() {
    let limits = ResourceLimits::default();
    let clock = ManualClock::default();
    let scheduler = Scheduler::with_clock(&limits, clock.clone());
    let local = provider("local-controls", ProviderLimits::unbounded());
    let paused = scheduler
        .enqueue(plan(100, OperationKind::Copy, local.clone()))
        .unwrap();
    let failed = scheduler
        .enqueue(plan(101, OperationKind::Copy, local.clone()))
        .unwrap();
    let interrupted = scheduler
        .enqueue(plan(102, OperationKind::SetPermissions, local))
        .unwrap();
    let started = scheduler.start_ready().unwrap();
    assert_eq!(started.len(), 3);

    scheduler.pause(paused).unwrap();
    assert_eq!(scheduler.state(paused), Some(JobState::Paused));
    scheduler.resume(paused).unwrap();
    assert_eq!(scheduler.state(paused), Some(JobState::Running));
    scheduler.cancel(paused).unwrap();
    assert_eq!(scheduler.state(paused), Some(JobState::Cancelling));
    scheduler.finish_cancel(paused).unwrap();
    assert_eq!(scheduler.state(paused), Some(JobState::Cancelled));

    scheduler.fail(failed).unwrap();
    let old_generation = started
        .iter()
        .find(|job| job.id() == failed)
        .unwrap()
        .generation();
    scheduler.retry(failed).unwrap();
    assert_eq!(scheduler.state(failed), Some(JobState::Queued));
    let retried = scheduler.start_ready().unwrap();
    assert_eq!(retried[0].generation().get(), old_generation.get() + 1);

    scheduler.interrupt(interrupted).unwrap();
    assert_eq!(scheduler.state(interrupted), Some(JobState::Interrupted));
    scheduler.retry(interrupted).unwrap();
    assert_eq!(scheduler.state(interrupted), Some(JobState::Queued));
}
