use musheen_core::StorePath;
use musheen_ops::{
    ApplyScope, ConflictChoice, ConflictDecision, ConflictDecisionJournal, ConflictError,
    ConflictItemKind, ConflictPolicies, ConflictRecord, MutationError, OperationKind,
};

#[derive(Default)]
struct RecordingJournal(Vec<ConflictDecision>);

impl ConflictDecisionJournal for RecordingJournal {
    fn persist_decision(&mut self, decision: &ConflictDecision) -> Result<(), MutationError> {
        self.0.push(decision.clone());
        Ok(())
    }
}

fn file_conflict(name: &str, source_identity: u8, destination_identity: u8) -> ConflictRecord {
    ConflictRecord::new(
        OperationKind::Copy,
        StorePath::from_unix_path(format!("/source/{name}")),
        vec![source_identity],
        ConflictItemKind::File,
        StorePath::from_unix_path(format!("/destination/{name}")),
        vec![destination_identity],
        ConflictItemKind::File,
    )
    .unwrap()
}

#[test]
fn apply_to_all_is_compatible_only_and_each_decision_is_identity_bound() {
    let first = file_conflict("one", 1, 11);
    let second = file_conflict("two", 2, 22);
    let directory = ConflictRecord::new(
        OperationKind::Copy,
        StorePath::from_unix_path("/source/folder"),
        vec![3],
        ConflictItemKind::Directory,
        StorePath::from_unix_path("/destination/folder"),
        vec![33],
        ConflictItemKind::Directory,
    )
    .unwrap();
    let mut policies = ConflictPolicies::default();
    let mut journal = RecordingJournal::default();

    policies
        .decide(
            &first,
            ConflictChoice::Replace,
            ApplyScope::CompatibleRemaining,
            &mut journal,
        )
        .unwrap();
    assert_eq!(
        policies
            .resolve_saved(&second, &[2], &[22], &mut journal)
            .unwrap(),
        Some(ConflictChoice::Replace)
    );
    assert_eq!(
        policies
            .resolve_saved(&directory, &[3], &[33], &mut journal)
            .unwrap(),
        None
    );
    assert_eq!(journal.0.len(), 2);
    assert_eq!(journal.0[1].source_identity(), &[2]);
    assert_eq!(journal.0[1].destination_identity(), &[22]);
}

#[test]
fn stale_destination_and_invalid_choices_are_refused_before_journaling() {
    let file = file_conflict("file", 7, 8);
    let mut policies = ConflictPolicies::default();
    let mut journal = RecordingJournal::default();
    policies
        .decide(
            &file,
            ConflictChoice::KeepBoth,
            ApplyScope::CompatibleRemaining,
            &mut journal,
        )
        .unwrap();

    assert_eq!(
        policies.resolve_saved(&file, &[7], &[9], &mut journal),
        Err(ConflictError::StaleDestination)
    );
    assert_eq!(journal.0.len(), 1);

    assert_eq!(
        policies.decide(
            &file,
            ConflictChoice::MergeDirectory,
            ApplyScope::ThisConflict,
            &mut journal,
        ),
        Err(ConflictError::InvalidChoice)
    );
    assert_eq!(journal.0.len(), 1);
}
