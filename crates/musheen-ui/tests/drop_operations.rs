use musheen_core::{ResourceLimits, StorePath};
use musheen_desktop::PropertySnapshot;
use musheen_ops::JobState;
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
