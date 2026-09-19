use musheen_core::{ResourceLimits, StorePath};
use musheen_desktop::PropertySnapshot;
use musheen_ops::{
    ApplyScope, ConflictChoice, ConflictDecision, ConflictDecisionJournal, ConflictPolicies,
    JobState, MutationError,
};
use musheen_ui::{
    DropAction, DropError, FileDragPayload, LocalOperationQueue, PropertiesDialogModel,
};
use std::fs;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn run_ready(queue: &mut LocalOperationQueue) {
    let ready = queue.start_ready().expect("ready operations start");
    assert!(!ready.is_empty());
    for operation in ready {
        let id = operation.id();
        let result = operation.execute();
        queue
            .finish(id, result)
            .expect("operation reaches a terminal state");
    }
}

#[derive(Default)]
struct ConflictJournal;

impl ConflictDecisionJournal for ConflictJournal {
    fn persist_decision(&mut self, _decision: &ConflictDecision) -> Result<(), MutationError> {
        Ok(())
    }
}

#[test]
fn sidebar_and_content_drops_use_one_copy_move_queue() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("copy-source.txt");
    let move_source = temporary.path().join("move-source.txt");
    let destination = temporary.path().join("destination");
    fs::write(&source, b"copy me").unwrap();
    fs::write(&move_source, b"move me").unwrap();
    fs::create_dir(&destination).unwrap();

    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    let copy = FileDragPayload::new(
        vec![StorePath::from_unix_path(source.as_os_str())],
        DropAction::Copy,
    )
    .unwrap();
    assert!(queue.can_accept(&copy, &StorePath::from_unix_path(&destination)));
    let copy_jobs = queue
        .submit_drop(copy, StorePath::from_unix_path(&destination))
        .unwrap();
    assert_eq!(copy_jobs.len(), 1);
    assert_eq!(queue.state(copy_jobs[0]), Some(JobState::Queued));
    run_ready(&mut queue);
    assert_eq!(
        fs::read(destination.join("copy-source.txt")).unwrap(),
        b"copy me"
    );
    assert!(source.exists());

    let move_payload = FileDragPayload::new(
        vec![StorePath::from_unix_path(move_source.as_os_str())],
        DropAction::Move,
    )
    .unwrap();
    let move_jobs = queue
        .submit_drop(move_payload, StorePath::from_unix_path(&destination))
        .unwrap();
    run_ready(&mut queue);
    assert_eq!(queue.state(move_jobs[0]), Some(JobState::Completed));
    assert_eq!(
        fs::read(destination.join("move-source.txt")).unwrap(),
        b"move me"
    );
    assert!(!move_source.exists());
}

#[test]
fn resolved_copy_conflicts_execute_the_exact_visible_choice() {
    for choice in [
        ConflictChoice::Skip,
        ConflictChoice::KeepBoth,
        ConflictChoice::Replace,
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let source_parent = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::create_dir(&source_parent).unwrap();
        fs::create_dir(&destination).unwrap();
        let source = source_parent.join("same.txt");
        let existing = destination.join("same.txt");
        fs::write(&source, b"incoming").unwrap();
        fs::write(&existing, b"existing").unwrap();
        let payload = FileDragPayload::new(
            vec![StorePath::from_unix_path(source.as_os_str())],
            DropAction::Copy,
        )
        .unwrap();
        let target = StorePath::from_unix_path(destination.as_os_str());
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());

        assert!(queue.can_accept(&payload, &target));
        let conflicts = queue.conflicts_for_drop(&payload, &target).unwrap();
        assert_eq!(conflicts.len(), 1);
        let decision = ConflictPolicies::default()
            .decide(
                &conflicts[0],
                choice,
                ApplyScope::ThisConflict,
                &mut ConflictJournal,
            )
            .unwrap();
        let jobs = queue
            .submit_drop_resolved(payload, target, vec![decision])
            .unwrap();
        assert_eq!(jobs.len(), 1);
        run_ready(&mut queue);

        match choice {
            ConflictChoice::Skip => {
                assert_eq!(fs::read(&existing).unwrap(), b"existing");
                assert_eq!(fs::read(&source).unwrap(), b"incoming");
            }
            ConflictChoice::KeepBoth => {
                assert_eq!(fs::read(&existing).unwrap(), b"existing");
                assert!(
                    fs::read_dir(&destination)
                        .unwrap()
                        .filter_map(Result::ok)
                        .any(|entry| entry.path() != existing
                            && fs::read(entry.path()).ok().as_deref() == Some(b"incoming"))
                );
            }
            ConflictChoice::Replace => {
                assert_eq!(fs::read(&existing).unwrap(), b"incoming");
                assert_eq!(fs::read(&source).unwrap(), b"incoming");
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn resolved_directory_copy_merge_preserves_disjoint_children() {
    let temporary = tempfile::tempdir().unwrap();
    let source_parent = temporary.path().join("source");
    let destination = temporary.path().join("destination");
    let source = source_parent.join("folder");
    let existing = destination.join("folder");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&existing).unwrap();
    fs::write(source.join("incoming.txt"), b"incoming").unwrap();
    fs::write(existing.join("existing.txt"), b"existing").unwrap();
    let payload = FileDragPayload::new(
        vec![StorePath::from_unix_path(source.as_os_str())],
        DropAction::Copy,
    )
    .unwrap();
    let target = StorePath::from_unix_path(destination.as_os_str());
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    let conflicts = queue.conflicts_for_drop(&payload, &target).unwrap();
    let decision = ConflictPolicies::default()
        .decide(
            &conflicts[0],
            ConflictChoice::MergeDirectory,
            ApplyScope::ThisConflict,
            &mut ConflictJournal,
        )
        .unwrap();

    queue
        .submit_drop_resolved(payload, target, vec![decision])
        .unwrap();
    run_ready(&mut queue);

    assert_eq!(
        fs::read(existing.join("incoming.txt")).unwrap(),
        b"incoming"
    );
    assert_eq!(
        fs::read(existing.join("existing.txt")).unwrap(),
        b"existing"
    );
    assert!(source.exists());
}

#[test]
fn resolved_conflict_revalidates_both_identities_before_mutating() {
    let temporary = tempfile::tempdir().unwrap();
    let source_parent = temporary.path().join("source");
    let destination = temporary.path().join("destination");
    fs::create_dir(&source_parent).unwrap();
    fs::create_dir(&destination).unwrap();
    let source = source_parent.join("same.txt");
    let existing = destination.join("same.txt");
    fs::write(&source, b"incoming").unwrap();
    fs::write(&existing, b"original occupant").unwrap();
    let payload = FileDragPayload::new(
        vec![StorePath::from_unix_path(source.as_os_str())],
        DropAction::Copy,
    )
    .unwrap();
    let target = StorePath::from_unix_path(destination.as_os_str());
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    let conflict = queue
        .conflicts_for_drop(&payload, &target)
        .unwrap()
        .remove(0);
    let decision = ConflictPolicies::default()
        .decide(
            &conflict,
            ConflictChoice::Replace,
            ApplyScope::ThisConflict,
            &mut ConflictJournal,
        )
        .unwrap();
    let job = queue
        .submit_drop_resolved(payload, target, vec![decision])
        .unwrap()[0];
    fs::remove_file(&existing).unwrap();
    fs::write(&existing, b"changed after decision").unwrap();

    let ready = queue.start_ready().unwrap().remove(0);
    let result = ready.execute();
    assert!(result.is_err());
    queue.finish(job, result).unwrap();

    assert_eq!(fs::read(&existing).unwrap(), b"changed after decision");
    assert_eq!(fs::read(&source).unwrap(), b"incoming");
}

#[test]
fn resolved_move_replace_and_merge_remove_only_the_selected_source() {
    let replace_root = tempfile::tempdir().unwrap();
    let replace_source_parent = replace_root.path().join("source");
    let replace_destination = replace_root.path().join("destination");
    fs::create_dir(&replace_source_parent).unwrap();
    fs::create_dir(&replace_destination).unwrap();
    let replace_source = replace_source_parent.join("same.txt");
    let replace_target = replace_destination.join("same.txt");
    fs::write(&replace_source, b"incoming").unwrap();
    fs::write(&replace_target, b"existing").unwrap();
    let payload = FileDragPayload::new(
        vec![StorePath::from_unix_path(replace_source.as_os_str())],
        DropAction::Move,
    )
    .unwrap();
    let target = StorePath::from_unix_path(replace_destination.as_os_str());
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    let conflict = queue
        .conflicts_for_drop(&payload, &target)
        .unwrap()
        .remove(0);
    let decision = ConflictPolicies::default()
        .decide(
            &conflict,
            ConflictChoice::Replace,
            ApplyScope::ThisConflict,
            &mut ConflictJournal,
        )
        .unwrap();
    queue
        .submit_drop_resolved(payload, target, vec![decision])
        .unwrap();
    run_ready(&mut queue);
    assert_eq!(fs::read(&replace_target).unwrap(), b"incoming");
    assert!(!replace_source.exists());

    let merge_root = tempfile::tempdir().unwrap();
    let merge_source_parent = merge_root.path().join("source");
    let merge_destination = merge_root.path().join("destination");
    let merge_source = merge_source_parent.join("folder");
    let merge_target = merge_destination.join("folder");
    fs::create_dir_all(&merge_source).unwrap();
    fs::create_dir_all(&merge_target).unwrap();
    fs::write(merge_source.join("incoming.txt"), b"incoming").unwrap();
    fs::write(merge_target.join("existing.txt"), b"existing").unwrap();
    let payload = FileDragPayload::new(
        vec![StorePath::from_unix_path(merge_source.as_os_str())],
        DropAction::Move,
    )
    .unwrap();
    let target = StorePath::from_unix_path(merge_destination.as_os_str());
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    let conflict = queue
        .conflicts_for_drop(&payload, &target)
        .unwrap()
        .remove(0);
    let decision = ConflictPolicies::default()
        .decide(
            &conflict,
            ConflictChoice::MergeDirectory,
            ApplyScope::ThisConflict,
            &mut ConflictJournal,
        )
        .unwrap();
    queue
        .submit_drop_resolved(payload, target, vec![decision])
        .unwrap();
    run_ready(&mut queue);
    assert_eq!(
        fs::read(merge_target.join("incoming.txt")).unwrap(),
        b"incoming"
    );
    assert_eq!(
        fs::read(merge_target.join("existing.txt")).unwrap(),
        b"existing"
    );
    assert!(!merge_source.exists());
}

#[test]
fn unsupported_targets_are_rejected_before_any_job_is_queued() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source.txt");
    let non_directory = temporary.path().join("not-a-directory");
    let read_only = temporary.path().join("read-only");
    fs::write(&source, b"source").unwrap();
    fs::write(&non_directory, b"target").unwrap();
    fs::create_dir(&read_only).unwrap();
    fs::set_permissions(&read_only, fs::Permissions::from_mode(0o555)).unwrap();

    let payload = FileDragPayload::new(
        vec![StorePath::from_unix_path(source.as_os_str())],
        DropAction::Copy,
    )
    .unwrap();
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    for target in [&non_directory, &read_only] {
        let target = StorePath::from_unix_path(target.as_os_str());
        assert!(!queue.can_accept(&payload, &target));
        assert!(matches!(
            queue.submit_drop(payload.clone(), target),
            Err(DropError::UnsupportedTarget(_))
        ));
    }
    assert_eq!(queue.job_count(), 0);
}

#[test]
fn an_invalid_batch_never_partially_enters_the_queue() {
    let temporary = tempfile::tempdir().unwrap();
    let first_parent = temporary.path().join("first");
    let second_parent = temporary.path().join("second");
    let destination = temporary.path().join("destination");
    fs::create_dir(&first_parent).unwrap();
    fs::create_dir(&second_parent).unwrap();
    fs::create_dir(&destination).unwrap();
    let first = first_parent.join("same.txt");
    let second = second_parent.join("same.txt");
    fs::write(&first, b"first").unwrap();
    fs::write(&second, b"second").unwrap();

    let payload = FileDragPayload::new(
        vec![
            StorePath::from_unix_path(first.as_os_str()),
            StorePath::from_unix_path(second.as_os_str()),
        ],
        DropAction::Copy,
    )
    .unwrap();
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    assert!(matches!(
        queue.submit_drop(payload, StorePath::from_unix_path(destination.as_os_str()),),
        Err(DropError::DuplicateDestination(_))
    ));
    assert_eq!(queue.job_count(), 0);
}

#[cfg(unix)]
#[test]
fn drop_paths_remain_lossless_for_non_utf8_names() {
    let temporary = tempfile::tempdir().unwrap();
    let name = std::ffi::OsString::from_vec(vec![b'n', b'o', b'n', b'-', 0xff]);
    let source = temporary.path().join(&name);
    let destination = temporary.path().join("destination");
    fs::write(&source, b"opaque").unwrap();
    fs::create_dir(&destination).unwrap();

    let payload = FileDragPayload::new(
        vec![StorePath::from_unix_path(source.as_os_str())],
        DropAction::Copy,
    )
    .unwrap();
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    queue
        .submit_drop(payload, StorePath::from_unix_path(destination.as_os_str()))
        .unwrap();
    run_ready(&mut queue);
    assert_eq!(fs::read(destination.join(name)).unwrap(), b"opaque");
}

#[cfg(unix)]
#[test]
fn reviewed_properties_changes_execute_through_the_operation_queue() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("selected");
    fs::write(&path, b"data").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let snapshot = PropertySnapshot::load(std::slice::from_ref(&path)).unwrap();
    let mut model = PropertiesDialogModel::new(snapshot);
    model.permissions_mut().set_file_mode(0o600);
    model.permissions_mut().set_recursive(false);
    assert!(!model.apply_visible());
    model.permissions_mut().review_recursive_scope();
    assert!(model.apply_visible());

    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
    let jobs = model.submit_permissions(&mut queue).unwrap();
    assert_eq!(jobs.len(), 1);
    run_ready(&mut queue);
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o7777,
        0o600
    );
}

#[test]
fn operation_controls_preserve_retryable_work_and_distinguish_cancellation() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source.txt");
    let second = temporary.path().join("second.txt");
    let target = temporary.path().join("target");
    fs::write(&source, b"source").unwrap();
    fs::write(&second, b"second").unwrap();
    fs::create_dir(&target).unwrap();
    let mut queue = LocalOperationQueue::new(&ResourceLimits::default());

    let cancelled = queue
        .submit_drop(
            FileDragPayload::new(
                vec![StorePath::from_unix_path(source.as_os_str())],
                DropAction::Copy,
            )
            .unwrap(),
            StorePath::from_unix_path(target.as_os_str()),
        )
        .unwrap()[0];
    queue.start_ready().unwrap();
    queue.pause(cancelled).unwrap();
    assert_eq!(queue.state(cancelled), Some(JobState::Paused));
    queue.resume(cancelled).unwrap();
    queue.cancel(cancelled).unwrap();
    queue
        .finish(cancelled, Err("cancelled by user".into()))
        .unwrap();
    assert_eq!(queue.state(cancelled), Some(JobState::Cancelled));

    let failed = queue
        .submit_drop(
            FileDragPayload::new(
                vec![StorePath::from_unix_path(second.as_os_str())],
                DropAction::Copy,
            )
            .unwrap(),
            StorePath::from_unix_path(target.as_os_str()),
        )
        .unwrap()[0];
    let first_generation = queue.start_ready().unwrap()[0].generation();
    queue
        .finish(failed, Err("temporary failure".into()))
        .unwrap();
    queue.retry(failed).unwrap();
    let retried = queue.start_ready().unwrap();
    assert_eq!(retried[0].id(), failed);
    assert_eq!(retried[0].generation().get(), first_generation.get() + 1);
}
