use musheen_core::{ResourceLimits, StorePath};
use musheen_desktop::StatusStore;
use musheen_ops::{
    ApplyScope, ConflictChoice, ConflictItemKind, ConflictRecord, EventGeneration, JobId,
    OperationKind, TrashReceipt,
};
use musheen_ui::{
    ConfirmationDefault, ConflictDialogModel, DestructiveConfirmation, DropAction, FileDragPayload,
    OperationHub, OperationStatus, RecoveryAction, StatusCenterModel, TrashItem, TrashSurfaceModel,
};
use std::fs;

#[test]
fn partial_success_keeps_actionable_failures_and_dismissal_survives_restart() {
    let id = JobId::new(41).unwrap();
    let location = StorePath::from_unix_path("/home/user/Documents");
    let failed_item = StorePath::from_unix_path("/home/user/Documents/locked.txt");
    let mut center = StatusCenterModel::default();
    center
        .register(
            id,
            EventGeneration::new(0),
            OperationKind::Copy,
            location,
            Some(2),
        )
        .unwrap();
    center.mark_running(id).unwrap();
    center.record_item_success(id).unwrap();
    center
        .record_failure(
            id,
            failed_item.clone(),
            "permission denied",
            [RecoveryAction::RetryFailed, RecoveryAction::ViewLocation],
        )
        .unwrap();
    center.complete(id).unwrap();

    let entry = center.entry(id).unwrap();
    assert_eq!(entry.status(), OperationStatus::PartialSuccess);
    assert_eq!(entry.completed_items(), 1);
    assert_eq!(entry.failures().len(), 1);
    let message = entry.failures()[0].message(entry.kind());
    assert!(message.contains("Copy"));
    assert!(message.contains("locked.txt"));
    assert!(message.contains("permission denied"));
    assert!(message.contains("Retry failed item"));

    center.dismiss(id).unwrap();
    assert!(center.visible_entries().is_empty());
    assert_eq!(center.history().len(), 1);
    let restored = StatusCenterModel::from_json(&center.to_json().unwrap()).unwrap();
    assert!(restored.visible_entries().is_empty());
    assert!(restored.entry(id).unwrap().dismissed());
    assert_eq!(restored.retry_targets(id).unwrap(), vec![failed_item]);
}

#[test]
fn destructive_confirmations_name_the_risk_and_never_default_to_destruction() {
    let confirmation = DestructiveConfirmation::new(
        "Empty Trash",
        "3 items",
        "cannot be undone",
        StorePath::from_unix_path("trash:///"),
    );

    assert_eq!(confirmation.default_action(), ConfirmationDefault::Cancel);
    let message = confirmation.message();
    assert!(message.contains("Empty Trash"));
    assert!(message.contains("3 items"));
    assert!(message.contains("cannot be undone"));
    assert!(message.contains("trash:///"));
}

#[test]
fn trash_surface_preserves_original_location_time_and_exact_empty_scope() {
    let receipt = TrashReceipt::new(
        StorePath::from_unix_path("/home/user/Documents/old.txt"),
        b"trash-id".to_vec(),
    );
    let surface = TrashSurfaceModel::new(vec![TrashItem::new(receipt.clone(), 1_726_742_400)]);

    assert_eq!(surface.items()[0].receipt(), &receipt);
    assert_eq!(surface.items()[0].deleted_at_unix_seconds(), 1_726_742_400);
    assert_eq!(surface.restore_receipt(0).unwrap(), &receipt);
    let challenge = surface.empty_challenge();
    assert!(challenge.confirm(0, true).is_err());
    assert!(challenge.confirm(1, false).is_err());
    assert_eq!(challenge.confirm(1, true).unwrap().item_count(), 1);
}

#[test]
fn conflict_dialog_exposes_only_compatible_choices_and_defaults_to_skip() {
    let file = ConflictRecord::new(
        OperationKind::Copy,
        StorePath::from_unix_path("/source/file.txt"),
        vec![1],
        ConflictItemKind::File,
        StorePath::from_unix_path("/destination/file.txt"),
        vec![2],
        ConflictItemKind::File,
    )
    .unwrap();
    let mut dialog = ConflictDialogModel::new(file);

    assert_eq!(dialog.choice(), ConflictChoice::Skip);
    assert!(dialog.choices().contains(&ConflictChoice::Replace));
    assert!(dialog.choices().contains(&ConflictChoice::KeepBoth));
    assert!(!dialog.choices().contains(&ConflictChoice::MergeDirectory));
    dialog.select(ConflictChoice::Replace).unwrap();
    dialog.set_apply_to_remaining(true);
    assert_eq!(
        dialog.decision(),
        (ConflictChoice::Replace, ApplyScope::CompatibleRemaining)
    );

    let directory = ConflictRecord::new(
        OperationKind::Move,
        StorePath::from_unix_path("/source/photos"),
        vec![3],
        ConflictItemKind::Directory,
        StorePath::from_unix_path("/destination/photos"),
        vec![4],
        ConflictItemKind::Directory,
    )
    .unwrap();
    let mut directory_dialog = ConflictDialogModel::new(directory);
    assert!(directory_dialog.destructive_warning().is_none());
    directory_dialog
        .select(ConflictChoice::ReplaceTree)
        .unwrap();
    let warning = directory_dialog.destructive_warning().unwrap();
    assert!(warning.contains("Replace existing folder tree"));
    assert!(warning.contains("/destination/photos"));
    assert!(warning.contains("removed before publication"));
    assert!(warning.contains("cannot be undone"));
}

#[test]
fn operation_hub_restores_interrupted_work_and_persisted_dismissals() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let source_one = temporary.path().join("one.txt");
    let source_two = temporary.path().join("two.txt");
    let source_three = temporary.path().join("three.txt");
    let destination = temporary.path().join("destination");
    fs::write(&source_one, b"one").expect("first source writes");
    fs::write(&source_two, b"two").expect("second source writes");
    fs::write(&source_three, b"three").expect("third source writes");
    fs::create_dir(&destination).expect("destination directory creates");
    let store = StatusStore::at(temporary.path().join("operations.json"));
    let limits = ResourceLimits::default();
    let hub = OperationHub::with_status_store(&limits, store.clone()).expect("status opens");
    let initial_revision = hub.status_revision();
    let first = hub
        .submit_drop(
            FileDragPayload::new(
                vec![StorePath::from_unix_path(source_one.into_os_string())],
                DropAction::Copy,
            )
            .unwrap(),
            StorePath::from_unix_path(destination.as_os_str()),
        )
        .unwrap()[0];
    let second = hub
        .submit_drop(
            FileDragPayload::new(
                vec![StorePath::from_unix_path(source_two.into_os_string())],
                DropAction::Copy,
            )
            .unwrap(),
            StorePath::from_unix_path(destination.as_os_str()),
        )
        .unwrap()[0];
    hub.cancel(second).unwrap();
    hub.dismiss(second).unwrap();
    assert!(hub.status_revision() > initial_revision);
    drop(hub);

    let restored = OperationHub::with_status_store(&limits, store).expect("status restores");
    let status = restored.status();
    let status = status.lock().expect("status lock is available");
    assert_eq!(
        status.entry(first).unwrap().status(),
        OperationStatus::Interrupted
    );
    assert!(
        status.entry(first).unwrap().failures()[0]
            .message(OperationKind::Copy)
            .contains("run it again")
    );
    assert_eq!(
        status.entry(second).unwrap().status(),
        OperationStatus::Cancelled
    );
    assert!(status.entry(second).unwrap().dismissed());
    drop(status);
    assert!(!restored.can_retry(first));

    let new_id = restored
        .submit_drop(
            FileDragPayload::new(
                vec![StorePath::from_unix_path(source_three.into_os_string())],
                DropAction::Copy,
            )
            .unwrap(),
            StorePath::from_unix_path(destination.as_os_str()),
        )
        .unwrap()[0];
    assert!(new_id > second);
    assert_eq!(
        restored
            .status()
            .lock()
            .unwrap()
            .entry(new_id)
            .unwrap()
            .status(),
        OperationStatus::Pending
    );
}

#[test]
fn recoverable_and_needs_attention_histories_round_trip_with_safe_actions() {
    let recoverable = JobId::new(51).unwrap();
    let attention = JobId::new(52).unwrap();
    let location = StorePath::from_unix_path("/home/user/Documents");
    let staging = StorePath::from_unix_path("/home/user/Documents/.musheen-stage-v1-51-0");
    let mut center = StatusCenterModel::default();
    for id in [recoverable, attention] {
        center
            .register(
                id,
                EventGeneration::new(0),
                OperationKind::Move,
                location.clone(),
                Some(1),
            )
            .unwrap();
        center.mark_running(id).unwrap();
    }
    assert!(
        center
            .record_recoverable_failure(
                recoverable,
                location.clone(),
                StorePath::from_unix_path("/home/user/Documents/not-app-staging"),
                "invalid staging",
            )
            .is_err()
    );
    center
        .record_recoverable_failure(
            recoverable,
            location.clone(),
            staging.clone(),
            "staging data is available",
        )
        .unwrap();
    center.mark_recoverable(recoverable).unwrap();
    center
        .record_failure(
            attention,
            location,
            "publication state is unknown",
            [RecoveryAction::ViewLocation],
        )
        .unwrap();
    center.mark_needs_attention(attention).unwrap();

    let document = center.to_json().unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&document).unwrap();
    for entry in legacy["entries"].as_array_mut().unwrap() {
        if entry["id"] == recoverable.get() {
            entry["failures"][0]
                .as_object_mut()
                .unwrap()
                .remove("recovery_staging");
        }
    }
    let migrated = StatusCenterModel::from_json(&serde_json::to_vec(&legacy).unwrap()).unwrap();
    assert_eq!(
        migrated.entry(recoverable).unwrap().status(),
        OperationStatus::NeedsAttention
    );
    assert_eq!(
        migrated.entry(recoverable).unwrap().failures()[0].actions(),
        &[RecoveryAction::ViewLocation]
    );

    let mut restored = StatusCenterModel::from_json(&document).unwrap();
    let mut missing_staging = StatusCenterModel::from_json(&document).unwrap();
    assert!(missing_staging.reconcile_recovery_staging(|_| false, |_| false));
    assert_eq!(
        missing_staging.entry(recoverable).unwrap().status(),
        OperationStatus::NeedsAttention
    );
    let mut restarted = StatusCenterModel::from_json(&document).unwrap();
    assert!(restarted.reconcile_recovery_staging(|_| true, |_| false));
    let restarted_failure = &restarted.entry(recoverable).unwrap().failures()[0];
    assert_eq!(
        restarted.entry(recoverable).unwrap().status(),
        OperationStatus::Recoverable
    );
    assert_eq!(
        restarted_failure.actions(),
        &[RecoveryAction::DiscardStaging, RecoveryAction::ViewLocation]
    );
    assert_eq!(
        restored.entry(recoverable).unwrap().status(),
        OperationStatus::Recoverable
    );
    assert_eq!(
        restored.entry(recoverable).unwrap().failures()[0].recovery_staging(),
        Some(&staging)
    );
    assert_eq!(
        restored.entry(attention).unwrap().status(),
        OperationStatus::NeedsAttention
    );
    assert!(
        restored
            .mark_retry_pending(recoverable, EventGeneration::new(1))
            .is_err(),
        "recoverable staging must be resumed or discarded, never blindly retried"
    );

    restored
        .mark_recovery_retry_pending(recoverable, EventGeneration::new(1))
        .unwrap();
    assert_eq!(
        restored.entry(recoverable).unwrap().status(),
        OperationStatus::Pending
    );
    assert!(restored.entry(recoverable).unwrap().failures().is_empty());

    let mut discarded = StatusCenterModel::from_json(&document).unwrap();
    discarded.mark_staging_discarded(recoverable, true).unwrap();
    let failure = &discarded.entry(recoverable).unwrap().failures()[0];
    assert_eq!(
        discarded.entry(recoverable).unwrap().status(),
        OperationStatus::Failed
    );
    assert!(failure.recovery_staging().is_none());
    assert_eq!(
        failure.actions(),
        &[RecoveryAction::RetryFailed, RecoveryAction::ViewLocation]
    );

    let mut restart_discard = StatusCenterModel::from_json(&document).unwrap();
    restart_discard.reconcile_recovery_staging(|_| true, |_| false);
    restart_discard
        .mark_staging_discarded(recoverable, false)
        .unwrap();
    let failure = &restart_discard.entry(recoverable).unwrap().failures()[0];
    assert_eq!(
        restart_discard.entry(recoverable).unwrap().status(),
        OperationStatus::NeedsAttention
    );
    assert_eq!(failure.actions(), &[RecoveryAction::ViewLocation]);
}
