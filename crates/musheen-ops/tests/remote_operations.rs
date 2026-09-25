#[allow(dead_code)]
mod support;

use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState,
    ProviderId,
};
use musheen_ops::{
    CopyRequest, CopySession, EventGeneration, JobId, OperationKind, ProviderLimits,
    ProviderSnapshot, RemoteTransferCapabilities, RemoteTransferGap, RemoteTransferPlan,
    RemoteTransferStrategy, ResumePolicy, StagingPath,
};
use support::RecordingProvider;

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

#[test]
fn remote_staging_is_a_unique_sibling_and_owned_by_its_job() {
    let provider = ProviderId::new("remote").unwrap();
    let destination =
        musheen_core::StorePath::from_provider_key(provider, b"/projects/report.txt".to_vec())
            .unwrap();
    let job = JobId::new(19).unwrap();
    let generation = EventGeneration::new(2);
    let nonce = [0xab; 16];

    let staging =
        StagingPath::for_slash_key_destination_with_nonce(&destination, job, generation, nonce)
            .unwrap();
    let (_, key) = staging.path().provider_key().unwrap();
    assert!(key.starts_with(b"/projects/.musheen-stage-v1-19-2-"));
    assert!(staging.is_app_owned());
    assert!(staging.is_sibling_of(&destination));
    assert!(
        !staging.is_sibling_of(
            &musheen_core::StorePath::from_provider_key(
                ProviderId::new("remote").unwrap(),
                b"/other/report.txt".to_vec(),
            )
            .unwrap()
        )
    );
    assert!(StagingPath::is_for_destination(
        staging.path(),
        &destination,
        job,
        generation,
        nonce
    ));
    assert!(!StagingPath::is_for_destination(
        staging.path(),
        &destination,
        JobId::new(20).unwrap(),
        generation,
        nonce
    ));
}

#[test]
fn opaque_or_traversing_keys_cannot_use_slash_staging() {
    let provider = ProviderId::new("remote").unwrap();
    for key in [
        b"opaque".as_slice(),
        b"/".as_slice(),
        b"/projects/../report".as_slice(),
        b"/projects//report".as_slice(),
    ] {
        let destination =
            musheen_core::StorePath::from_provider_key(provider.clone(), key.to_vec()).unwrap();
        assert!(
            StagingPath::for_slash_key_destination_with_nonce(
                &destination,
                JobId::new(1).unwrap(),
                EventGeneration::new(0),
                [0; 16]
            )
            .is_err()
        );
    }
}

#[test]
fn copy_session_uses_remote_sibling_staging_for_slash_key_provider() {
    let provider = ProviderId::new("remote").unwrap();
    let source =
        musheen_core::StorePath::from_provider_key(provider.clone(), b"/source.txt".to_vec())
            .unwrap();
    let destination =
        musheen_core::StorePath::from_provider_key(provider, b"/target.txt".to_vec()).unwrap();
    let request = CopyRequest::new(
        JobId::new(3).unwrap(),
        EventGeneration::new(0),
        source,
        destination,
    );
    let mut backend = RecordingProvider::regular();

    let outcome = CopySession::default().execute(&mut backend, &request, &CancellationToken::new());

    assert!(outcome.is_ok());
}
