use musheen_core::StorePath;
use musheen_desktop::ConflictDecisionStore;
use musheen_ops::{
    ApplyScope, ConflictChoice, ConflictItemKind, ConflictPolicies, ConflictRecord, OperationKind,
};
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn conflict_decisions_are_durable_private_and_identity_bound() {
    let temporary = tempfile::tempdir().expect("temporary directory is available");
    let path = temporary.path().join("state/conflicts.journal");
    let mut journal = ConflictDecisionStore::at(&path).expect("journal opens");
    let conflict = ConflictRecord::new(
        OperationKind::Restore,
        StorePath::from_unix_path("/trash/old.txt"),
        vec![1, 2, 3],
        ConflictItemKind::File,
        StorePath::from_unix_path("/home/user/old.txt"),
        vec![9, 8, 7],
        ConflictItemKind::File,
    )
    .unwrap();

    ConflictPolicies::default()
        .decide(
            &conflict,
            ConflictChoice::KeepBoth,
            ApplyScope::ThisConflict,
            &mut journal,
        )
        .expect("decision persists before use");

    let document = fs::read_to_string(&path).expect("journal is readable");
    assert!(document.contains("keep_both"));
    assert!(document.contains("[1,2,3]"));
    assert!(document.contains("[9,8,7]"));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
