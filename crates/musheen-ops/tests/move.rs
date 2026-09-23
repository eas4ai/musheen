mod support;

use musheen_core::CancellationToken;
use musheen_ops::{
    MetadataKind, MetadataReport, MoveStrategy, OperationFailure, ProviderError, SourceState,
    complete_move_after_metadata_review, execute_move,
};
use support::{Action, RecordingProvider, destination, request};

#[test]
fn same_filesystem_move_prefers_atomic_rename() {
    let mut provider = RecordingProvider::regular();
    provider.capabilities.atomic_rename = true;
    provider.atomic_move_ok = true;

    let outcome =
        execute_move(&mut provider, &request("atomic"), &CancellationToken::new()).unwrap();

    assert_eq!(outcome.strategy(), MoveStrategy::AtomicRename);
    assert_eq!(provider.actions, [Action::AtomicMove]);
}

#[test]
fn cross_filesystem_move_prepares_removal_before_publication() {
    let mut provider = RecordingProvider::regular();

    let outcome = execute_move(
        &mut provider,
        &request("cross-device"),
        &CancellationToken::new(),
    )
    .unwrap();

    assert_eq!(outcome.strategy(), MoveStrategy::VerifiedCopy);
    let prepare = provider
        .actions
        .iter()
        .position(|action| action == &Action::PrepareSourceRemoval)
        .unwrap();
    let verify = provider
        .actions
        .iter()
        .position(|action| action == &Action::Verify)
        .unwrap();
    let publish = provider
        .actions
        .iter()
        .position(|action| action == &Action::Publish)
        .unwrap();
    let remove = provider
        .actions
        .iter()
        .position(|action| action == &Action::RemoveSource)
        .unwrap();
    assert!(prepare < verify);
    assert!(verify < publish);
    assert!(publish < remove);
}

#[test]
fn failed_verification_or_cancellation_never_removes_the_source() {
    let mut corrupt = RecordingProvider::regular();
    corrupt.verify_ok = false;
    assert!(execute_move(&mut corrupt, &request("corrupt"), &CancellationToken::new(),).is_err());
    assert!(!corrupt.actions.contains(&Action::RemoveSource));

    let mut cancelled = RecordingProvider::regular();
    cancelled.cancel_after_publish = true;
    let token = CancellationToken::new();
    let failure = execute_move(&mut cancelled, &request("cancelled"), &token).unwrap_err();
    assert!(failure.destination_published());
    assert!(failure.source_retained());
    assert!(failure.destination_can_be_removed_for_rollback());
    assert!(!cancelled.actions.contains(&Action::RemoveSource));
}

#[test]
fn source_removal_preparation_failure_never_publishes() {
    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::PrepareSourceRemoval);

    let failure = execute_move(
        &mut provider,
        &request("unremovable-source"),
        &CancellationToken::new(),
    )
    .unwrap_err();

    assert!(failure.source_retained());
    assert!(!failure.destination_published());
    assert!(provider.actions.contains(&Action::Cleanup));
    assert!(!provider.actions.contains(&Action::Publish));
}

#[test]
fn source_removal_failure_keeps_the_verified_destination_and_recovery_context() {
    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::RemoveSource);

    let failure = execute_move(
        &mut provider,
        &request("retained"),
        &CancellationToken::new(),
    )
    .unwrap_err();

    assert!(failure.destination_published());
    assert!(failure.source_retained());
    assert_eq!(failure.destination(), &destination("retained"));
}

#[test]
fn ambiguous_move_outcomes_never_allow_destructive_rollback() {
    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::RemoveSource);
    provider.fail_with = ProviderError::SourceRemovalUnknown;

    let failure = move_failure(&mut provider, "ambiguous-removal");

    assert!(failure.destination_published());
    assert_eq!(failure.source_state(), SourceState::Unknown);
    assert!(!failure.destination_can_be_removed_for_rollback());

    let mut provider = RecordingProvider::regular();
    provider.capabilities.atomic_rename = true;
    provider.fail_action = Some(Action::AtomicMove);
    provider.fail_with = ProviderError::AtomicMoveUnknown;

    let failure = move_failure(&mut provider, "ambiguous-atomic");

    assert_eq!(failure.source_state(), SourceState::Unknown);
    assert!(!failure.destination_can_be_removed_for_rollback());
}

#[test]
fn partial_source_removal_preserves_the_verified_destination() {
    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::RemoveSource);
    provider.fail_with = ProviderError::SourcePartiallyRemoved;

    let failure = move_failure(&mut provider, "partial-removal");

    assert_eq!(failure.source_state(), SourceState::PartiallyRemoved);
    assert!(failure.destination_published());
    assert!(!failure.destination_can_be_removed_for_rollback());
}

#[test]
fn metadata_loss_requires_review_before_source_removal() {
    let mut provider = RecordingProvider::regular();
    provider.metadata =
        MetadataReport::with_skipped([MetadataKind::Ownership, MetadataKind::AccessControlList]);

    let outcome = execute_move(
        &mut provider,
        &request("metadata-loss"),
        &CancellationToken::new(),
    )
    .unwrap();

    assert_eq!(outcome.metadata(), &provider.metadata);
    assert_eq!(
        outcome
            .metadata_review()
            .expect("incomplete metadata requires a decision")
            .metadata(),
        &provider.metadata
    );
    let prepare = provider
        .actions
        .iter()
        .position(|action| action == &Action::PrepareSourceRemoval)
        .unwrap();
    let verify = provider
        .actions
        .iter()
        .position(|action| action == &Action::Verify)
        .unwrap();
    let publish = provider
        .actions
        .iter()
        .position(|action| action == &Action::Publish)
        .unwrap();
    assert!(prepare < verify);
    assert!(verify < publish);
    assert!(!provider.actions.contains(&Action::RemoveSource));
}

#[test]
fn confirmed_metadata_loss_removes_the_unchanged_source() {
    let mut provider = RecordingProvider::regular();
    provider.metadata = MetadataReport::with_skipped([MetadataKind::Ownership]);
    let outcome = execute_move(
        &mut provider,
        &request("metadata-confirmed"),
        &CancellationToken::new(),
    )
    .unwrap();
    let review = outcome
        .into_metadata_review()
        .expect("incomplete metadata requires a decision");
    provider.actions.clear();

    let completed =
        complete_move_after_metadata_review(&mut provider, review, &CancellationToken::new())
            .unwrap();

    assert!(completed.metadata_review().is_none());
    assert_eq!(completed.metadata(), &provider.metadata);
    assert_eq!(provider.actions, [Action::Verify, Action::RemoveSource]);
}

#[test]
fn confirmed_metadata_loss_reverifies_the_published_destination_before_removal() {
    let mut provider = RecordingProvider::regular();
    provider.metadata = MetadataReport::with_skipped([MetadataKind::Ownership]);
    let review = execute_move(
        &mut provider,
        &request("metadata-destination-changed"),
        &CancellationToken::new(),
    )
    .unwrap()
    .into_metadata_review()
    .expect("incomplete metadata requires a decision");
    provider.verify_ok = false;
    provider.actions.clear();

    let failure =
        complete_move_after_metadata_review(&mut provider, review, &CancellationToken::new())
            .unwrap_err();

    assert!(failure.destination_published());
    assert!(failure.source_retained());
    assert_eq!(provider.actions, [Action::Verify]);
}

fn move_failure(provider: &mut RecordingProvider, name: &str) -> OperationFailure {
    execute_move(provider, &request(name), &CancellationToken::new()).unwrap_err()
}
