use musheen_core::StorePath;
use musheen_ops::{
    AclChange, AclEntry, AclQualifier, MetadataChange, MetadataEntry, MetadataEntryKind,
    MetadataPlan, MetadataProvider, MetadataScope, ModeEdit, MutationError, ResolvedMetadataChange,
};

#[derive(Default)]
struct RecordingProvider {
    preview: Vec<MetadataEntry>,
    applied: Vec<(StorePath, ResolvedMetadataChange)>,
}

impl MetadataProvider for RecordingProvider {
    fn preview(
        &mut self,
        _root: &StorePath,
        _expected_identity: &[u8],
        _scope: MetadataScope,
        _change: &MetadataChange,
    ) -> Result<Vec<MetadataEntry>, MutationError> {
        Ok(self.preview.clone())
    }

    fn apply_metadata(
        &mut self,
        entry: &MetadataEntry,
        change: &ResolvedMetadataChange,
    ) -> Result<(), MutationError> {
        self.applied.push((entry.path().clone(), change.clone()));
        Ok(())
    }
}

#[test]
fn recursive_metadata_requires_review_and_keeps_file_and_directory_modes_separate() {
    let change = MetadataChange::new()
        .with_file_mode(0o640)
        .with_directory_mode(0o750);
    let unreviewed = MetadataScope::recursive(false, false);
    let mut provider = RecordingProvider::default();
    assert_eq!(
        MetadataPlan::preflight(
            &mut provider,
            local("/root"),
            b"root".to_vec(),
            unreviewed,
            change.clone(),
        ),
        Err(MutationError::ScopeNotReviewed)
    );

    provider.preview = vec![
        MetadataEntry::new(
            local("/root"),
            b"root".to_vec(),
            MetadataEntryKind::Directory,
            false,
        ),
        MetadataEntry::new(
            local("/root/file"),
            b"file".to_vec(),
            MetadataEntryKind::File,
            false,
        ),
    ];
    let plan = MetadataPlan::preflight(
        &mut provider,
        local("/root"),
        b"root".to_vec(),
        MetadataScope::recursive(false, true),
        change,
    )
    .unwrap();
    plan.execute(&mut provider).unwrap();

    assert_eq!(provider.applied[0].1.mode(), Some(0o750));
    assert_eq!(provider.applied[1].1.mode(), Some(0o640));
}

#[test]
fn metadata_plan_surfaces_privilege_and_carries_acl_changes() {
    let acl = AclChange::Replace(vec![AclEntry::new(
        AclQualifier::User(1000),
        true,
        false,
        false,
    )]);
    let change = MetadataChange::new()
        .with_owner(0)
        .with_access_acl(acl.clone());
    let mut provider = RecordingProvider {
        preview: vec![MetadataEntry::new(
            local("/root"),
            b"root".to_vec(),
            MetadataEntryKind::Directory,
            true,
        )],
        applied: Vec::new(),
    };
    let plan = MetadataPlan::preflight(
        &mut provider,
        local("/root"),
        b"root".to_vec(),
        MetadataScope::Single,
        change,
    )
    .unwrap();

    assert!(plan.requires_privilege());
    plan.execute(&mut provider).unwrap();
    assert_eq!(provider.applied[0].1.access_acl(), Some(&acl));
}

#[test]
fn empty_or_invalid_metadata_changes_never_reach_the_provider() {
    let mut provider = RecordingProvider::default();
    assert_eq!(
        MetadataPlan::preflight(
            &mut provider,
            local("/root"),
            b"root".to_vec(),
            MetadataScope::Single,
            MetadataChange::new(),
        ),
        Err(MutationError::NoChanges)
    );
    assert_eq!(
        MetadataPlan::preflight(
            &mut provider,
            local("/root"),
            b"root".to_vec(),
            MetadataScope::Single,
            MetadataChange::new().with_file_mode(0o10_000),
        ),
        Err(MutationError::InvalidMetadata)
    );

    provider.preview = vec![MetadataEntry::new(
        local("/root/link"),
        b"link".to_vec(),
        MetadataEntryKind::SymbolicLink,
        false,
    )];
    assert_eq!(
        MetadataPlan::preflight(
            &mut provider,
            local("/root/link"),
            b"link".to_vec(),
            MetadataScope::Single,
            MetadataChange::new().with_access_acl(AclChange::Remove),
        ),
        Err(MutationError::InvalidMetadata)
    );
}

#[test]
fn mode_edits_apply_to_each_entry_and_skip_entries_they_leave_as_they_are() {
    // Group: Can View; Others: No Access.
    let edit = ModeEdit {
        file_clear: 0o067,
        file_set: 0o040,
        directory_clear: 0o077,
        directory_set: 0o050,
        ..ModeEdit::default()
    };
    assert_eq!(edit.apply(MetadataEntryKind::File, 0o755), 0o750);
    assert_eq!(edit.apply(MetadataEntryKind::File, 0o600), 0o640);
    assert_eq!(edit.apply(MetadataEntryKind::Directory, 0o777), 0o750);
    assert_eq!(edit.apply(MetadataEntryKind::SymbolicLink, 0o777), 0o777);

    let executable = ModeEdit {
        file_execute: Some(true),
        ..ModeEdit::default()
    };
    assert_eq!(executable.apply(MetadataEntryKind::File, 0o640), 0o750);
    assert_eq!(executable.apply(MetadataEntryKind::Directory, 0o700), 0o700);
    let not_executable = ModeEdit {
        file_execute: Some(false),
        ..ModeEdit::default()
    };
    assert_eq!(
        not_executable.apply(MetadataEntryKind::File, 0o4755),
        0o4644
    );
    let sticky = ModeEdit {
        bits_set: 0o1000,
        bits_clear: 0o002,
        ..ModeEdit::default()
    };
    assert_eq!(sticky.apply(MetadataEntryKind::Directory, 0o777), 0o1775);
    assert!(ModeEdit::default().is_empty());

    let mut provider = RecordingProvider {
        preview: vec![
            MetadataEntry::new(local("/a"), b"a".to_vec(), MetadataEntryKind::File, false)
                .with_current_mode(0o640),
            MetadataEntry::new(local("/b"), b"b".to_vec(), MetadataEntryKind::File, false)
                .with_current_mode(0o600),
        ],
        ..RecordingProvider::default()
    };
    let plan = MetadataPlan::preflight(
        &mut provider,
        local("/a"),
        b"a".to_vec(),
        MetadataScope::Single,
        MetadataChange::new().with_mode_edit(edit),
    )
    .unwrap();
    plan.execute(&mut provider).unwrap();
    assert_eq!(provider.applied.len(), 1, "/a already has the mode");
    assert_eq!(provider.applied[0].0, local("/b"));
    assert_eq!(provider.applied[0].1.mode(), Some(0o640));

    provider.preview.truncate(1);
    provider.applied.clear();
    let plan = MetadataPlan::preflight(
        &mut provider,
        local("/a"),
        b"a".to_vec(),
        MetadataScope::Single,
        MetadataChange::new().with_mode_edit(edit),
    )
    .expect("an item the edit leaves as it is does not fail");
    plan.execute(&mut provider).unwrap();
    assert!(provider.applied.is_empty());
}

fn local(path: &str) -> StorePath {
    StorePath::from_unix_path(path)
}
