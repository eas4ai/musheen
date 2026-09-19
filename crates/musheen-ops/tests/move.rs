mod support;

use musheen_core::CancellationToken;
use musheen_ops::{MoveStrategy, ProviderError, SourceState, execute_move};
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
fn cross_filesystem_move_verifies_and_publishes_before_source_removal() {
    let mut provider = RecordingProvider::regular();

    let outcome = execute_move(
        &mut provider,
        &request("cross-device"),
        &CancellationToken::new(),
    )
    .unwrap();

    assert_eq!(outcome.strategy(), MoveStrategy::VerifiedCopy);
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
    assert!(!cancelled.actions.contains(&Action::RemoveSource));
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
fn ambiguous_source_removal_never_claims_the_source_still_exists() {
    let mut provider = RecordingProvider::regular();
    provider.fail_action = Some(Action::RemoveSource);
    provider.fail_with = ProviderError::SourceRemovalUnknown;

    let failure = execute_move(
        &mut provider,
        &request("ambiguous-removal"),
        &CancellationToken::new(),
    )
    .unwrap_err();

    assert!(failure.destination_published());
    assert_eq!(failure.source_state(), SourceState::Unknown);
}
