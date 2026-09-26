use musheen_core::StorePath;
use musheen_ops::{
    DeleteProvider, DeleteTarget, MutationError, PermanentDeleteRequest, TrashReceipt,
    execute_delete, execute_permanent_delete, execute_restore,
};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
struct RecordingProvider {
    entries: HashMap<StorePath, Box<[u8]>>,
    trash_supported: bool,
    fail_trash: HashSet<StorePath>,
    trashed: Vec<StorePath>,
    permanently_deleted: Vec<StorePath>,
    restored: Vec<StorePath>,
}

impl DeleteProvider for RecordingProvider {
    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
        Ok(self.entries.get(path).cloned())
    }

    fn supports_trash(&mut self, _path: &StorePath) -> Result<bool, MutationError> {
        Ok(self.trash_supported)
    }

    fn supports_permanent_delete(&mut self, _path: &StorePath) -> Result<bool, MutationError> {
        Ok(true)
    }

    fn move_to_trash(&mut self, target: &DeleteTarget) -> Result<TrashReceipt, MutationError> {
        if self.fail_trash.contains(target.path()) {
            return Err(MutationError::Provider("trash failed".into()));
        }
        self.trashed.push(target.path().clone());
        self.entries.remove(target.path());
        Ok(TrashReceipt::new(
            target.path().clone(),
            target.path().unix_bytes().unwrap().to_vec(),
        ))
    }

    fn restore_no_replace(&mut self, receipt: &TrashReceipt) -> Result<(), MutationError> {
        if self.entries.contains_key(receipt.original_path()) {
            return Err(MutationError::Conflict);
        }
        self.restored.push(receipt.original_path().clone());
        self.entries
            .insert(receipt.original_path().clone(), b"restored".to_vec().into());
        Ok(())
    }

    fn permanently_delete(&mut self, target: &DeleteTarget) -> Result<(), MutationError> {
        self.permanently_deleted.push(target.path().clone());
        self.entries.remove(target.path());
        Ok(())
    }
}

#[test]
fn normal_delete_refuses_without_trash_and_never_falls_back() {
    let target = DeleteTarget::new(local("/work/file"), b"id".to_vec());
    let mut provider = RecordingProvider::default();
    provider
        .entries
        .insert(target.path().clone(), b"id".to_vec().into());

    assert_eq!(
        execute_delete(&mut provider, vec![target]),
        Err(MutationError::TrashUnsupported)
    );
    assert!(provider.trashed.is_empty());
    assert!(provider.permanently_deleted.is_empty());
}

#[test]
fn trash_reports_partial_failure_and_receipts_can_restore_without_replacement() {
    let first = DeleteTarget::new(local("/work/a"), b"a".to_vec());
    let second = DeleteTarget::new(local("/work/b"), b"b".to_vec());
    let mut provider = RecordingProvider {
        trash_supported: true,
        ..RecordingProvider::default()
    };
    provider
        .entries
        .insert(first.path().clone(), b"a".to_vec().into());
    provider
        .entries
        .insert(second.path().clone(), b"b".to_vec().into());
    provider.fail_trash.insert(second.path().clone());

    let outcome = execute_delete(&mut provider, vec![first, second]).unwrap();
    assert_eq!(outcome.trashed().len(), 1);
    assert_eq!(outcome.failures().len(), 1);

    execute_restore(&mut provider, &outcome.trashed()[0]).unwrap();
    assert_eq!(provider.restored, vec![local("/work/a")]);
    assert!(provider.permanently_deleted.is_empty());
}

#[test]
fn permanent_delete_requires_a_confirmation_bound_to_exact_scope() {
    let target = DeleteTarget::new(local("/work/file"), b"id".to_vec());
    let request = PermanentDeleteRequest::new(local("/work"), vec![target.clone()]).unwrap();
    let challenge = request.challenge();
    let confirmation = challenge.confirm(1, &local("/work"), true).unwrap();
    let changed_request = PermanentDeleteRequest::new(
        local("/work"),
        vec![DeleteTarget::new(local("/work/other"), b"id".to_vec())],
    )
    .unwrap();
    let mut provider = RecordingProvider::default();
    provider
        .entries
        .insert(target.path().clone(), b"id".to_vec().into());

    assert_eq!(
        execute_permanent_delete(&mut provider, &changed_request, &confirmation),
        Err(MutationError::ConfirmationRequired)
    );
    assert!(provider.permanently_deleted.is_empty());

    execute_permanent_delete(&mut provider, &request, &confirmation).unwrap();
    assert_eq!(provider.permanently_deleted, vec![local("/work/file")]);
}

#[test]
fn delete_rejects_duplicate_and_parent_traversal_scopes_before_mutation() {
    let target = DeleteTarget::new(local("/work/file"), b"id".to_vec());
    let mut provider = RecordingProvider {
        trash_supported: true,
        ..RecordingProvider::default()
    };
    provider
        .entries
        .insert(target.path().clone(), b"id".to_vec().into());
    assert_eq!(
        execute_delete(&mut provider, vec![target.clone(), target]),
        Err(MutationError::BatchCollision)
    );
    assert!(provider.trashed.is_empty());

    let escaping = DeleteTarget::new(local("/work/../outside"), b"outside".to_vec());
    assert_eq!(
        PermanentDeleteRequest::new(local("/work"), vec![escaping]),
        Err(MutationError::InvalidScope)
    );
    let nested = DeleteTarget::new(local("/work/link/outside"), b"outside".to_vec());
    assert_eq!(
        PermanentDeleteRequest::new(local("/work"), vec![nested]),
        Err(MutationError::InvalidScope)
    );
}

fn local(path: &str) -> StorePath {
    StorePath::from_unix_path(path)
}
