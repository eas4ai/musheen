use musheen_core::{
    CapabilityMatrix, CapabilityState, ProviderId, ResourceLimitConfig, ResourceLimits, StorePath,
};
use musheen_ops::{
    Clock, JobState, OperationKind, OperationPlan, ProviderLimits, ProviderSnapshot, Scheduler,
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
    let mut scheduler = Scheduler::with_clock(&limits, clock.clone());
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
    let mut scheduler = Scheduler::with_clock(&limits, ManualClock::default());
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
    let mut scheduler = Scheduler::with_clock(&limits, ManualClock::default());
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
    let mut scheduler = Scheduler::with_clock(&source, ManualClock::default());
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
