use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, ItemId, ProviderId,
    StorePath,
};
use musheen_ops::{
    EventGeneration, InverseTemplate, JobEvent, JobId, JobState, JobStateMachine, OperationKind,
    OperationPlan, Progress, ProgressUnit, ProviderLimits, ProviderSnapshot, StateError,
};

fn provider(capabilities: CapabilityMatrix) -> ProviderSnapshot {
    ProviderSnapshot::new(
        ProviderId::new("local").expect("fixture provider ID is valid"),
        capabilities,
        ProviderLimits::unbounded(),
    )
}

fn all_supported() -> CapabilityMatrix {
    CapabilityMatrix::new(|_| CapabilityState::Supported)
}

#[test]
fn transition_matrix_accepts_only_documented_edges() {
    let allowed = [
        (JobState::Planned, JobState::Queued),
        (JobState::Planned, JobState::Failed),
        (JobState::Queued, JobState::Running),
        (JobState::Queued, JobState::Cancelling),
        (JobState::Queued, JobState::Cancelled),
        (JobState::Queued, JobState::Failed),
        (JobState::Running, JobState::Paused),
        (JobState::Running, JobState::Cancelling),
        (JobState::Running, JobState::Failed),
        (JobState::Running, JobState::Recoverable),
        (JobState::Running, JobState::Interrupted),
        (JobState::Running, JobState::Completed),
        (JobState::Paused, JobState::Running),
        (JobState::Paused, JobState::Cancelling),
        (JobState::Paused, JobState::Failed),
        (JobState::Paused, JobState::Recoverable),
        (JobState::Paused, JobState::Interrupted),
        (JobState::Paused, JobState::Completed),
        (JobState::Cancelling, JobState::Cancelled),
        (JobState::Cancelling, JobState::Failed),
        (JobState::Cancelling, JobState::Recoverable),
        (JobState::Cancelling, JobState::RolledBack),
        (JobState::Failed, JobState::Recoverable),
        (JobState::Recoverable, JobState::RolledBack),
    ];

    for from in JobState::ALL {
        for to in JobState::ALL {
            assert_eq!(
                from.allows(to),
                allowed.contains(&(from, to)),
                "unexpected transition verdict for {from:?} -> {to:?}"
            );
        }
    }
}

#[test]
fn stale_generations_and_invalid_progress_are_rejected() {
    let id = JobId::new(7).expect("non-zero IDs are valid");
    let mut machine = JobStateMachine::new(id);
    let generation = machine.generation();
    assert!(matches!(
        machine.apply(JobEvent::transition(id, generation, 1, JobState::Completed)),
        Err(StateError::InvalidTransition { .. })
    ));
    assert!(matches!(
        machine.apply(JobEvent::progress(
            id,
            generation,
            2,
            Progress::new(1, Some(10), ProgressUnit::Items).unwrap(),
        )),
        Err(StateError::ProgressUnavailable(JobState::Planned))
    ));
    machine
        .apply(JobEvent::transition(id, generation, 10, JobState::Queued))
        .expect("planned work can queue");
    machine
        .apply(JobEvent::transition(id, generation, 11, JobState::Running))
        .expect("queued work can start");
    machine
        .apply(JobEvent::transition(
            id,
            generation,
            12,
            JobState::Recoverable,
        ))
        .expect("running work can become recoverable");

    let retry_generation = machine.retry(13).expect("recoverable work can retry");
    assert_eq!(retry_generation, EventGeneration::new(1));
    assert_eq!(machine.state(), JobState::Queued);
    assert!(matches!(
        machine.apply(JobEvent::transition(id, generation, 14, JobState::Running)),
        Err(StateError::StaleGeneration { .. })
    ));
    assert!(Progress::new(11, Some(10), ProgressUnit::Items).is_err());
    assert!(Progress::new(1, Some(10), ProgressUnit::Items).is_ok());
    assert!(Progress::new(4_096, None, ProgressUnit::Bytes).is_ok());
}

#[test]
fn undo_is_a_new_plan_only_while_identity_capability_and_state_are_safe() {
    let provider = provider(all_supported());
    let identity = ItemId::new(provider.id().clone(), b"destination-v1".to_vec())
        .expect("fixture identity is valid");
    let source = StorePath::from_unix_path("/source");
    let destination = StorePath::from_unix_path("/destination");
    let inverse = OperationPlan::new(
        OperationKind::Move,
        provider.clone(),
        Some(destination.clone()),
        source.clone(),
    )
    .expect("inverse plan is valid");
    let original = OperationPlan::new(
        OperationKind::Move,
        provider.clone(),
        Some(source),
        destination,
    )
    .expect("forward plan is valid")
    .with_inverse(InverseTemplate::new(
        inverse,
        identity.clone(),
        Some(CapabilityKind::AtomicRename),
    ));

    let validated = original
        .validated_inverse(
            JobState::Completed,
            Some(&identity),
            provider.id(),
            provider.capabilities(),
        )
        .expect("a completed unchanged move can be undone");
    assert_eq!(validated.kind(), OperationKind::Move);
    assert!(!std::ptr::eq(&original, &validated));

    let changed = ItemId::new(provider.id().clone(), b"destination-v2".to_vec())
        .expect("fixture identity is valid");
    assert!(
        original
            .validated_inverse(
                JobState::Completed,
                Some(&changed),
                provider.id(),
                provider.capabilities(),
            )
            .is_none()
    );
    assert!(
        original
            .validated_inverse(
                JobState::Running,
                Some(&identity),
                provider.id(),
                provider.capabilities(),
            )
            .is_none()
    );

    let unsupported = CapabilityMatrix::new(|kind| {
        if kind == CapabilityKind::AtomicRename {
            CapabilityState::Unsupported(
                CapabilityReason::new("provider cannot rename atomically")
                    .expect("fixture reason is valid"),
            )
        } else {
            CapabilityState::Supported
        }
    });
    assert!(
        original
            .validated_inverse(
                JobState::Completed,
                Some(&identity),
                provider.id(),
                &unsupported,
            )
            .is_none()
    );
}
