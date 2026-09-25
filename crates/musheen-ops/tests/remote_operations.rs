use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, ProviderId,
};
use musheen_ops::{
    OperationKind, ProviderLimits, ProviderSnapshot, RemoteTransferCapabilities, RemoteTransferGap,
    RemoteTransferPlan, RemoteTransferStrategy, ResumePolicy,
};

fn snapshot(id: &str, supported: &[CapabilityKind]) -> ProviderSnapshot {
    ProviderSnapshot::new(
        ProviderId::new(id).unwrap(),
        CapabilityMatrix::new(|kind| {
            if supported.contains(&kind) {
                CapabilityState::Supported
            } else {
                CapabilityState::Unsupported(CapabilityReason::new("not supported").unwrap())
            }
        }),
        ProviderLimits::unbounded(),
    )
}

#[test]
fn cross_provider_move_requires_streaming_and_reports_lost_guarantees() {
    let source = snapshot(
        "local",
        &[
            CapabilityKind::AtomicRename,
            CapabilityKind::Ownership,
            CapabilityKind::ExtendedAttributes,
            CapabilityKind::SparseFiles,
        ],
    );
    let destination = snapshot("remote", &[]);
    let source_io = RemoteTransferCapabilities::readable().with_stable_identity();
    let destination_io = RemoteTransferCapabilities::writable();

    let plan = RemoteTransferPlan::new(
        OperationKind::Move,
        &source,
        &destination,
        source_io,
        destination_io,
    )
    .unwrap();

    assert_eq!(plan.strategy(), RemoteTransferStrategy::Streamed);
    assert_eq!(plan.resume_policy(), ResumePolicy::RestartOnly);
    assert!(plan.requires_metadata_review_before_source_removal());
    assert!(plan.gaps().contains(&RemoteTransferGap::AtomicPublication));
    assert!(plan.gaps().contains(&RemoteTransferGap::Ownership));
    assert!(plan.gaps().contains(&RemoteTransferGap::ExtendedAttributes));
    assert!(plan.gaps().contains(&RemoteTransferGap::SparseLayout));
    assert!(plan.gaps().contains(&RemoteTransferGap::CrashDurability));
}

#[test]
fn server_side_optimization_never_crosses_provider_boundary() {
    let source = snapshot("remote_a", &[]);
    let other = snapshot("remote_b", &[]);
    let remote_io = RemoteTransferCapabilities::readable()
        .with_write()
        .with_server_copy()
        .with_server_move();
    let cross = RemoteTransferPlan::new(OperationKind::Copy, &source, &other, remote_io, remote_io)
        .unwrap();
    let same = RemoteTransferPlan::new(OperationKind::Copy, &source, &source, remote_io, remote_io)
        .unwrap();

    assert_eq!(cross.strategy(), RemoteTransferStrategy::Streamed);
    assert_eq!(same.strategy(), RemoteTransferStrategy::ServerSideCopy);
}

#[test]
fn server_side_copy_requires_destination_specific_support() {
    let provider = snapshot("remote", &[]);
    let source_io = RemoteTransferCapabilities::readable().with_server_copy();
    let destination_io = RemoteTransferCapabilities::writable();

    let plan = RemoteTransferPlan::new(
        OperationKind::Copy,
        &provider,
        &provider,
        source_io,
        destination_io,
    )
    .unwrap();

    assert_eq!(plan.strategy(), RemoteTransferStrategy::Streamed);
}

#[test]
fn server_side_move_requires_proven_atomic_rename() {
    let provider = snapshot("remote", &[]);
    let remote_io = RemoteTransferCapabilities::readable()
        .with_write()
        .with_server_move();

    let plan = RemoteTransferPlan::new(
        OperationKind::Move,
        &provider,
        &provider,
        remote_io,
        remote_io,
    )
    .unwrap();

    assert_eq!(plan.strategy(), RemoteTransferStrategy::Streamed);
}

#[test]
fn proven_atomic_move_and_durability_avoid_false_warnings() {
    let provider = snapshot("remote", &[CapabilityKind::AtomicRename]);
    let remote_io = RemoteTransferCapabilities::readable()
        .with_write()
        .with_server_move()
        .with_atomic_publish()
        .with_crash_durability();

    let plan = RemoteTransferPlan::new(
        OperationKind::Move,
        &provider,
        &provider,
        remote_io,
        remote_io,
    )
    .unwrap();

    assert_eq!(plan.strategy(), RemoteTransferStrategy::ServerSideMove);
    assert!(!plan.gaps().contains(&RemoteTransferGap::AtomicPublication));
    assert!(!plan.gaps().contains(&RemoteTransferGap::CrashDurability));
}

#[test]
fn resume_needs_verified_identity_and_range_write_on_both_ends() {
    let source = snapshot("local", &[]);
    let destination = snapshot("remote", &[]);
    let readable = RemoteTransferCapabilities::readable()
        .with_stable_identity()
        .with_range_read();
    let writable = RemoteTransferCapabilities::writable()
        .with_range_write()
        .with_stable_identity();
    let safe = RemoteTransferPlan::new(
        OperationKind::Copy,
        &source,
        &destination,
        readable,
        writable,
    )
    .unwrap();
    let unsafe_write = RemoteTransferPlan::new(
        OperationKind::Copy,
        &source,
        &destination,
        readable,
        RemoteTransferCapabilities::writable(),
    )
    .unwrap();

    assert_eq!(safe.resume_policy(), ResumePolicy::VerifiedRange);
    assert_eq!(unsafe_write.resume_policy(), ResumePolicy::RestartOnly);
}

#[test]
fn unknown_or_missing_write_support_rejects_preflight() {
    let source = snapshot("local", &[]);
    let destination = snapshot("remote", &[]);
    let result = RemoteTransferPlan::new(
        OperationKind::Copy,
        &source,
        &destination,
        RemoteTransferCapabilities::readable(),
        RemoteTransferCapabilities::default(),
    );
    assert!(result.is_err());
}
