use musheen_core::StorePath;
use musheen_ops::{
    AclChange, AclEntry, AclQualifier, MetadataChange, MetadataEntry, MetadataEntryKind,
    MetadataPlan, MetadataProvider, MetadataScope, MutationError, ResolvedMetadataChange,
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

fn local(path: &str) -> StorePath {
    StorePath::from_unix_path(path)
}
