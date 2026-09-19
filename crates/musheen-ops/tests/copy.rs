mod support;

use musheen_core::CancellationToken;
use musheen_ops::{
    CopyCapabilities, CopyOptions, CopySession, CopyStrategy, EntryKind, EntrySnapshot,
    FailureKind, MetadataKind, MetadataReport, ProviderError, PublicationState,
};
use support::{Action, RecordingProvider, request};

#[test]
fn strategy_selection_prefers_hard_link_then_reflink_sparse_and_streaming() {
    let cases = [
        (true, true, true, CopyStrategy::Reflink),
        (false, true, true, CopyStrategy::Sparse),
        (false, false, true, CopyStrategy::Streamed),
    ];
    for (reflink_ok, sparse_ok, sparse_source, expected) in cases {
        let mut provider = RecordingProvider::regular();
        provider.capabilities = CopyCapabilities {
            reflink: true,
            sparse: true,
            ..CopyCapabilities::default()
        };
        provider.reflink_ok = reflink_ok;
        provider.sparse_ok = sparse_ok;
        if sparse_source {
            provider.initial =
                EntrySnapshot::new(b"source".to_vec(), EntryKind::RegularFile, 8, 2, 1);
        }

        let outcome = CopySession::default()
            .execute(&mut provider, &request("result"), &CancellationToken::new())
            .unwrap();

        assert_eq!(outcome.strategy(), expected);
    }
}

#[test]
fn repeated_identity_uses_a_hard_link_when_supported() {
    let mut provider = RecordingProvider::regular();
    provider.capabilities.hard_links = true;
    provider.hard_link_ok = true;
    let mut session = CopySession::default();
    session
        .execute(&mut provider, &request("first"), &CancellationToken::new())
        .unwrap();
    provider.actions.clear();

    let second = session
        .execute(&mut provider, &request("second"), &CancellationToken::new())
        .unwrap();

    assert_eq!(second.strategy(), CopyStrategy::HardLink);
    assert!(provider.actions.contains(&Action::HardLink));
    assert!(!provider.actions.contains(&Action::Stream));
}

#[test]
fn symlinks_are_copied_as_links_unless_following_is_explicit() {
    let mut provider = RecordingProvider::regular();
    provider.initial = EntrySnapshot::new(b"link".to_vec(), EntryKind::SymbolicLink, 0, 0, 1);
    provider.followed = Some(EntrySnapshot::new(
        b"target".to_vec(),
        EntryKind::RegularFile,
        8,
        8,
        1,
    ));
    let mut session = CopySession::default();
    let link = session
        .execute(&mut provider, &request("link"), &CancellationToken::new())
        .unwrap();
    assert_eq!(link.strategy(), CopyStrategy::SymbolicLink);

    provider.actions.clear();
    let followed = session
        .execute(
            &mut provider,
            &request("target").with_options(CopyOptions::default().follow_links(true)),
            &CancellationToken::new(),
        )
        .unwrap();
    assert_eq!(followed.strategy(), CopyStrategy::Streamed);
    assert!(
        provider
            .actions
            .contains(&Action::Inspect { follow_links: true })
    );
}

#[test]
fn unsupported_special_files_are_refused_before_staging() {
    for kind in [
        EntryKind::BlockDevice,
        EntryKind::CharacterDevice,
        EntryKind::Fifo,
        EntryKind::Socket,
    ] {
        let mut provider = RecordingProvider::regular();
        provider.initial = EntrySnapshot::new(b"special".to_vec(), kind, 0, 0, 1);
        let failure = CopySession::default()
            .execute(
                &mut provider,
                &request("special"),
                &CancellationToken::new(),
            )
            .unwrap_err();

        assert_eq!(failure.kind(), &FailureKind::UnsupportedSpecialFile(kind));
        assert!(!provider.actions.contains(&Action::CreateStaging));
    }
}

#[test]
fn metadata_losses_are_reported_in_the_outcome() {
    let mut provider = RecordingProvider::regular();
    provider.metadata = MetadataReport::with_skipped([
        MetadataKind::Ownership,
        MetadataKind::ExtendedAttributes,
        MetadataKind::AccessControlList,
    ]);

    let outcome = CopySession::default()
        .execute(
            &mut provider,
            &request("metadata"),
            &CancellationToken::new(),
        )
        .unwrap();

    assert_eq!(outcome.metadata(), &provider.metadata);
}

#[test]
fn strategy_fallbacks_report_unpreserved_layout_and_link_relationships() {
    let mut sparse = RecordingProvider::regular();
    sparse.initial = EntrySnapshot::new(b"sparse".to_vec(), EntryKind::RegularFile, 32, 2, 1);
    let sparse_outcome = CopySession::default()
        .execute(
            &mut sparse,
            &request("sparse-fallback"),
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(
        sparse_outcome
            .metadata()
            .skipped()
            .contains(&MetadataKind::SparseLayout)
    );

    let mut linked = RecordingProvider::regular();
    let mut session = CopySession::default();
    session
        .execute(
            &mut linked,
            &request("linked-first"),
            &CancellationToken::new(),
        )
        .unwrap();
    let second = session
        .execute(
            &mut linked,
            &request("linked-second"),
            &CancellationToken::new(),
        )
        .unwrap();
    assert!(
        second
            .metadata()
            .skipped()
            .contains(&MetadataKind::HardLinkRelationship)
    );
}

#[test]
fn failures_and_cancellation_remove_unpublished_staging() {
    for (action, error) in [
        (Action::Stream, ProviderError::OutOfSpace),
        (Action::Metadata, ProviderError::PermissionDenied),
        (
            Action::Stream,
            ProviderError::ShortWrite {
                expected: 8,
                written: 3,
            },
        ),
    ] {
        let mut provider = RecordingProvider::regular();
        provider.fail_action = Some(action);
        provider.fail_with = error.clone();
        let failure = CopySession::default()
            .execute(
                &mut provider,
                &request("failure"),
                &CancellationToken::new(),
            )
            .unwrap_err();
        assert_eq!(failure.kind(), &FailureKind::Provider(error));
        assert!(provider.actions.contains(&Action::Cleanup));
        assert!(!provider.actions.contains(&Action::Publish));
    }

    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::Stream);
    provider.fail_with = ProviderError::Cancelled;
    let failure = CopySession::default()
        .execute(
            &mut provider,
            &request("cancelled"),
            &CancellationToken::new(),
        )
        .unwrap_err();
    assert_eq!(failure.kind(), &FailureKind::Cancelled);
    assert!(provider.actions.contains(&Action::Cleanup));
}

#[test]
fn partial_staging_creation_is_cleaned_before_failure_returns() {
    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::CreateStaging);

    let failure = CopySession::default()
        .execute(
            &mut provider,
            &request("partial-stage"),
            &CancellationToken::new(),
        )
        .unwrap_err();

    assert!(matches!(failure.kind(), FailureKind::Provider(_)));
    assert!(provider.actions.contains(&Action::Cleanup));
}

#[test]
fn an_existing_staging_path_is_never_deleted_as_failed_partial_work() {
    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::CreateStaging);
    provider.fail_with = ProviderError::StagingExists;

    let failure = CopySession::default()
        .execute(
            &mut provider,
            &request("occupied-stage"),
            &CancellationToken::new(),
        )
        .unwrap_err();

    assert_eq!(
        failure.kind(),
        &FailureKind::Provider(ProviderError::StagingExists)
    );
    assert!(!provider.actions.contains(&Action::Cleanup));
}

#[test]
fn cleanup_and_publication_uncertainty_return_recovery_context() {
    let mut retained = RecordingProvider::regular();
    retained.fail_action = Some(Action::Stream);
    retained.cleanup_fails = true;
    let failure = CopySession::default()
        .execute(
            &mut retained,
            &request("retained-stage"),
            &CancellationToken::new(),
        )
        .unwrap_err();
    assert!(failure.staging_retained().is_some());
    assert_eq!(failure.publication_state(), PublicationState::NotPublished);

    let mut ambiguous = RecordingProvider::regular();
    ambiguous.fail_action = Some(Action::Publish);
    ambiguous.fail_with = ProviderError::PublishUnknown;
    let failure = CopySession::default()
        .execute(
            &mut ambiguous,
            &request("unknown-publication"),
            &CancellationToken::new(),
        )
        .unwrap_err();
    assert_eq!(failure.publication_state(), PublicationState::Unknown);
    assert!(failure.source_retained());
}

#[test]
fn source_changes_and_failed_verification_never_publish() {
    let mut changed = RecordingProvider::regular();
    changed.after = Some(EntrySnapshot::new(
        b"replacement".to_vec(),
        EntryKind::RegularFile,
        8,
        8,
        1,
    ));
    let failure = CopySession::default()
        .execute(&mut changed, &request("changed"), &CancellationToken::new())
        .unwrap_err();
    assert_eq!(failure.kind(), &FailureKind::SourceChanged);
    assert!(!changed.actions.contains(&Action::Publish));

    let mut corrupt = RecordingProvider::regular();
    corrupt.verify_ok = false;
    let failure = CopySession::default()
        .execute(&mut corrupt, &request("corrupt"), &CancellationToken::new())
        .unwrap_err();
    assert_eq!(failure.kind(), &FailureKind::VerificationFailed);
    assert!(!corrupt.actions.contains(&Action::Publish));
}

#[test]
fn recursive_copy_refuses_nested_mounts_unless_included() {
    let mut provider = RecordingProvider::regular();
    provider.initial = EntrySnapshot::new(b"directory".to_vec(), EntryKind::Directory, 0, 0, 1);
    provider.fail_action = Some(Action::Directory);
    provider.fail_with = ProviderError::NestedMount;

    let failure = CopySession::default()
        .execute(
            &mut provider,
            &request("directory"),
            &CancellationToken::new(),
        )
        .unwrap_err();

    assert_eq!(
        failure.kind(),
        &FailureKind::Provider(ProviderError::NestedMount)
    );
    assert!(provider.actions.contains(&Action::Cleanup));
}
