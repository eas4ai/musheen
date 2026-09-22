use musheen_core::StorePath;
use musheen_ops::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase, ArchiveOperationPlan, EventGeneration,
    JobEvent, JobId, JournalPhase, RecoveryContext, RecoveryDecision, decide_recovery,
};

fn path(value: &str) -> StorePath {
    StorePath::from_unix_path(value)
}

#[test]
fn archive_plans_are_typed_and_reject_invalid_shapes() {
    let sources = vec![path("/data/one"), path("/data/two")];
    let create = ArchiveOperationPlan::create(
        sources.clone(),
        path("/data/out.zip"),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("valid create plan");
    assert_eq!(create.sources(), sources.as_slice());
    assert!(!create.encrypted());

    assert!(
        ArchiveOperationPlan::create(
            Vec::new(),
            path("/data/out.zip"),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .is_err()
    );
    assert!(
        ArchiveOperationPlan::extract(
            path("/data/in.zip"),
            path("/data/out"),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .is_ok()
    );
    assert!(
        ArchiveOperationPlan::extract(
            path("/data/in.zip"),
            path("/data/in.zip"),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Replace,
            false,
        )
        .is_err()
    );
}

#[test]
fn archive_events_report_recoverable_phases() {
    let event = JobEvent::archive_phase(
        JobId::new(7).expect("job id"),
        EventGeneration::new(2),
        99,
        ArchiveEventPhase::Publishing,
    );
    assert_eq!(
        event.archive_phase_value(),
        Some(ArchiveEventPhase::Publishing)
    );
    assert!(event.state().is_none());
    assert!(event.progress_value().is_none());
}

#[test]
fn restart_recovery_never_auto_publishes_unfinished_archive_staging() {
    let owned = RecoveryContext {
        continuation_verified: false,
        staging_owned: true,
        destination_verified: false,
        source_identity_current: true,
    };
    for phase in [
        JournalPhase::Planned,
        JournalPhase::StagingCreated,
        JournalPhase::DataCopied,
        JournalPhase::MetadataApplied,
    ] {
        assert_eq!(decide_recovery(phase, owned), RecoveryDecision::Rollback);
    }

    let published = RecoveryContext {
        continuation_verified: true,
        staging_owned: true,
        destination_verified: true,
        source_identity_current: true,
    };
    assert_eq!(
        decide_recovery(JournalPhase::DestinationPublished, published),
        RecoveryDecision::Resume
    );
    assert_eq!(
        decide_recovery(JournalPhase::Completed, published),
        RecoveryDecision::NoAction
    );
}
