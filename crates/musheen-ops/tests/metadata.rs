use musheen_core::StorePath;
use musheen_ops::{
    AclChange, AclEdit, AclEditStep, AclEntry, AclQualifier, MetadataChange, MetadataEntry,
    MetadataEntryKind, MetadataPlan, MetadataProvider, MetadataScope, ModeEdit, ModeStep,
    MutationError, ResolvedMetadataChange, executes_where_readable, mode_after_ownership_change,
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

fn access(shift: u32, file_bits: u32, folder_bits: u32) -> ModeStep {
    ModeStep::Access {
        shift,
        file_bits,
        folder_bits,
    }
}

#[test]
fn mode_edits_apply_steps_in_order_to_each_entry() {
    // Group: Can View; Others: No Access.
    let edit = ModeEdit::new(vec![access(3, 0o4, 0o5), access(0, 0, 0)]);
    assert_eq!(edit.apply(MetadataEntryKind::File, 0o755), 0o750);
    assert_eq!(edit.apply(MetadataEntryKind::File, 0o600), 0o640);
    assert_eq!(edit.apply(MetadataEntryKind::File, 0o644), 0o640);
    assert_eq!(edit.apply(MetadataEntryKind::Directory, 0o777), 0o750);
    assert_eq!(edit.apply(MetadataEntryKind::SymbolicLink, 0o777), 0o777);
    assert!(ModeEdit::default().is_empty());

    // Execute follows read while every reader executes, and stays otherwise.
    assert_eq!(edit.apply(MetadataEntryKind::File, 0o700), 0o750);
    assert_eq!(edit.apply(MetadataEntryKind::File, 0o744), 0o740);

    let executable = ModeEdit::new(vec![ModeStep::Executable(true)]);
    assert_eq!(executable.apply(MetadataEntryKind::File, 0o640), 0o750);
    assert_eq!(executable.apply(MetadataEntryKind::Directory, 0o700), 0o700);
    let not_executable = ModeEdit::new(vec![ModeStep::Executable(false)]);
    assert_eq!(
        not_executable.apply(MetadataEntryKind::File, 0o4755),
        0o4644
    );

    // A later step wins over an earlier one.
    let others_read = ModeStep::Bit {
        mask: 0o004,
        on: true,
        files: true,
        folders: true,
    };
    let latest = ModeEdit::new(vec![others_read, access(0, 0, 0)]);
    assert_eq!(latest.apply(MetadataEntryKind::File, 0o600), 0o600);
    let owner_execute = ModeStep::Bit {
        mask: 0o100,
        on: true,
        files: true,
        folders: true,
    };
    let cleared = ModeEdit::new(vec![owner_execute, ModeStep::Executable(false)]);
    assert_eq!(cleared.apply(MetadataEntryKind::File, 0o644), 0o644);

    // A bit reaches only the kinds it was chosen on.
    let setgid = ModeEdit::new(vec![ModeStep::Bit {
        mask: 0o2000,
        on: true,
        files: false,
        folders: true,
    }]);
    assert_eq!(setgid.apply(MetadataEntryKind::Directory, 0o755), 0o2755);
    assert_eq!(setgid.apply(MetadataEntryKind::File, 0o755), 0o755);

    assert!(executes_where_readable(0o755));
    assert!(!executes_where_readable(0o744));
    assert!(!executes_where_readable(0o644));
    assert_eq!(
        mode_after_ownership_change(MetadataEntryKind::File, 0o6755),
        0o755
    );
    assert_eq!(
        mode_after_ownership_change(MetadataEntryKind::File, 0o6745),
        0o2745
    );
    assert_eq!(
        mode_after_ownership_change(MetadataEntryKind::Directory, 0o2775),
        0o2775
    );
}

#[test]
fn mode_edits_and_groups_skip_entries_they_leave_as_they_are() {
    let edit = ModeEdit::new(vec![access(3, 0o4, 0o5), access(0, 0, 0)]);
    let entry = |path: &str, mode: u32, group: u32| {
        MetadataEntry::new(
            local(path),
            path.as_bytes().to_vec(),
            MetadataEntryKind::File,
            false,
        )
        .with_current_mode(mode)
        .with_current_group(group)
    };
    let mut provider = RecordingProvider {
        preview: vec![entry("/a", 0o640, 10), entry("/b", 0o600, 10)],
        ..RecordingProvider::default()
    };
    let plan = MetadataPlan::preflight(
        &mut provider,
        local("/a"),
        b"/a".to_vec(),
        MetadataScope::Single,
        MetadataChange::new().with_mode_edit(edit),
    )
    .unwrap();
    plan.execute(&mut provider).unwrap();
    assert_eq!(provider.applied.len(), 1, "/a already has the mode");
    assert_eq!(provider.applied[0].0, local("/b"));
    let resolved = &provider.applied[0].1;
    assert_eq!(resolved.mode(), None, "no fixed mode");
    assert_eq!(resolved.mode_for(0o600), Some(0o640));
    assert_eq!(
        resolved.mode_for(0o666),
        Some(0o640),
        "the edit applies to the mode found when applying"
    );
    assert_eq!(resolved.mode_for(0o640), None);

    // A group change skips an entry already in the group, and does not fail.
    provider.preview = vec![entry("/a", 0o640, 10), entry("/b", 0o4755, 20)];
    provider.applied.clear();
    let plan = MetadataPlan::preflight(
        &mut provider,
        local("/a"),
        b"/a".to_vec(),
        MetadataScope::Single,
        MetadataChange::new().with_group(20),
    )
    .unwrap();
    plan.execute(&mut provider).unwrap();
    assert_eq!(provider.applied.len(), 1, "/b is already in group 20");
    assert_eq!(provider.applied[0].0, local("/a"));
    assert_eq!(provider.applied[0].1.group_for(10), Some(20));
    assert_eq!(provider.applied[0].1.group_for(20), None);

    provider.preview = vec![entry("/b", 0o4755, 20)];
    provider.applied.clear();
    MetadataPlan::preflight(
        &mut provider,
        local("/b"),
        b"/b".to_vec(),
        MetadataScope::Single,
        MetadataChange::new().with_group(20),
    )
    .expect("an item already in the group does not fail")
    .execute(&mut provider)
    .unwrap();
    assert!(provider.applied.is_empty());
}

fn local(path: &str) -> StorePath {
    StorePath::from_unix_path(path)
}

fn acl(entries: &[(AclQualifier, &str)]) -> Vec<AclEntry> {
    entries
        .iter()
        .map(|(qualifier, rights)| {
            AclEntry::new(
                qualifier.clone(),
                rights.contains('r'),
                rights.contains('w'),
                rights.contains('x'),
            )
        })
        .collect()
}

#[test]
fn acl_edit_changes_named_entries_and_sets_the_mask_as_setfacl_does() {
    let file = acl(&[
        (AclQualifier::Owner, "rw"),
        (AclQualifier::User(7), "r"),
        (AclQualifier::OwningGroup, "r"),
        (AclQualifier::Group(9), "rx"),
        (AclQualifier::Mask, "r"),
        (AclQualifier::Other, ""),
    ]);
    let edit = AclEdit::new(vec![
        AclEditStep::Set(AclEntry::new(AclQualifier::User(8), true, true, false)),
        AclEditStep::Remove(AclQualifier::User(7)),
    ]);
    assert_eq!(
        edit.apply(&file, &[], false),
        acl(&[
            (AclQualifier::Owner, "rw"),
            (AclQualifier::User(8), "rw"),
            (AclQualifier::OwningGroup, "r"),
            (AclQualifier::Group(9), "rx"),
            (AclQualifier::Mask, "rwx"),
            (AclQualifier::Other, ""),
        ]),
        "the group entry the edit did not touch stays, and the mask is the union"
    );

    // An edit that leaves the named entries as they are keeps the mask.
    let same = AclEdit::new(vec![AclEditStep::Set(AclEntry::new(
        AclQualifier::User(7),
        true,
        false,
        false,
    ))]);
    assert_eq!(same.apply(&file, &[], false), file);

    // Removing the last named entry keeps the mask, now the owning
    // group's rights.
    let minimal = acl(&[
        (AclQualifier::Owner, "rw"),
        (AclQualifier::User(7), "rw"),
        (AclQualifier::OwningGroup, "r"),
        (AclQualifier::Mask, "rw"),
        (AclQualifier::Other, ""),
    ]);
    let remove = AclEdit::new(vec![AclEditStep::Remove(AclQualifier::User(7))]);
    assert_eq!(
        remove.apply(&minimal, &[], false),
        acl(&[
            (AclQualifier::Owner, "rw"),
            (AclQualifier::OwningGroup, "r"),
            (AclQualifier::Mask, "r"),
            (AclQualifier::Other, ""),
        ])
    );
}

#[test]
fn acl_edit_starts_default_entries_from_the_access_acl() {
    let access = acl(&[
        (AclQualifier::Owner, "rwx"),
        (AclQualifier::OwningGroup, "rx"),
        (AclQualifier::Other, ""),
    ]);
    let edit = AclEdit::new(vec![AclEditStep::Set(AclEntry::new(
        AclQualifier::Group(9),
        true,
        false,
        true,
    ))]);
    assert_eq!(
        edit.apply(&[], &access, true),
        acl(&[
            (AclQualifier::Owner, "rwx"),
            (AclQualifier::OwningGroup, "rx"),
            (AclQualifier::Group(9), "rx"),
            (AclQualifier::Mask, "rx"),
            (AclQualifier::Other, ""),
        ])
    );
    let remove = AclEdit::new(vec![AclEditStep::Remove(AclQualifier::Group(9))]);
    assert!(
        remove.apply(&[], &access, true).is_empty(),
        "removing from an empty list adds nothing"
    );
}

#[test]
fn acl_edit_gives_contents_execute_only_where_an_execute_bit_is() {
    let edit = AclEdit::new(vec![AclEditStep::Set(AclEntry::new(
        AclQualifier::User(8),
        true,
        true,
        true,
    ))]);
    let change = MetadataChange::new()
        .with_access_acl(AclChange::Edit(edit.clone()))
        .with_default_acl(AclChange::Edit(edit));
    let mut provider = RecordingProvider {
        preview: vec![
            MetadataEntry::new(
                local("/root/file"),
                b"file".to_vec(),
                MetadataEntryKind::File,
                false,
            )
            .as_contents(),
            MetadataEntry::new(
                local("/root"),
                b"root".to_vec(),
                MetadataEntryKind::Directory,
                false,
            ),
        ],
        ..RecordingProvider::default()
    };
    let plan = MetadataPlan::preflight(
        &mut provider,
        local("/root"),
        b"root".to_vec(),
        MetadataScope::recursive(false, true),
        change,
    )
    .unwrap();
    plan.execute(&mut provider).unwrap();
    let file = acl(&[
        (AclQualifier::Owner, "rw"),
        (AclQualifier::OwningGroup, "r"),
        (AclQualifier::Other, "r"),
    ]);
    let edited = |change: &ResolvedMetadataChange, executable: bool| match change.access_acl() {
        Some(AclChange::Edit(edit)) => edit
            .apply(&file, &[], executable)
            .into_iter()
            .find(|entry| *entry.qualifier() == AclQualifier::User(8)),
        other => panic!("an ACL edit, not {other:?}"),
    };
    let (_, contents) = &provider.applied[0];
    assert!(
        contents.default_acl().is_none(),
        "a file has no default ACL"
    );
    assert_eq!(
        edited(contents, false),
        Some(AclEntry::new(AclQualifier::User(8), true, true, false)),
        "a file inside the folder without an execute bit does not take execute"
    );
    assert_eq!(
        edited(contents, true),
        Some(AclEntry::new(AclQualifier::User(8), true, true, true))
    );
    let (_, selected) = &provider.applied[1];
    assert!(selected.default_acl().is_some());
    assert_eq!(
        edited(selected, false),
        Some(AclEntry::new(AclQualifier::User(8), true, true, true)),
        "a selected item takes the rights as chosen"
    );
}
