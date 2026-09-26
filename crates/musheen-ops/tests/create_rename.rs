use musheen_core::StorePath;
use musheen_ops::{
    BatchRenameJournal, BatchRenamePlan, BatchRenameStep, CreateKind, CreateRequest, MutationError,
    MutationProvider, NameError, RenameMapping, RenameRequest, execute_create, execute_rename,
    validate_local_name,
};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

struct RecordingProvider {
    entries: HashMap<StorePath, Box<[u8]>>,
    actions: Vec<(StorePath, StorePath)>,
    creates: Vec<(StorePath, CreateKind)>,
    create_allowed: bool,
    rename_allowed: bool,
}

impl Default for RecordingProvider {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            actions: Vec::new(),
            creates: Vec::new(),
            create_allowed: true,
            rename_allowed: true,
        }
    }
}

#[derive(Default)]
struct RecordingJournal {
    plan: Vec<BatchRenameStep>,
    completed: Vec<usize>,
}

impl BatchRenameJournal for RecordingJournal {
    fn persist_plan(&mut self, steps: &[BatchRenameStep]) -> Result<(), MutationError> {
        self.plan = steps.to_vec();
        Ok(())
    }

    fn persist_completed_step(&mut self, index: usize) -> Result<(), MutationError> {
        self.completed.push(index);
        Ok(())
    }
}

impl RecordingProvider {
    fn add(&mut self, path: &str, identity: &[u8]) {
        self.entries.insert(local(path), identity.into());
    }
}

impl MutationProvider for RecordingProvider {
    fn allows_create(
        &mut self,
        _parent: &StorePath,
        _kind: CreateKind,
    ) -> Result<bool, MutationError> {
        Ok(self.create_allowed)
    }

    fn allows_rename(&mut self, _source: &StorePath) -> Result<bool, MutationError> {
        Ok(self.rename_allowed)
    }

    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
        Ok(self.entries.get(path).cloned())
    }

    fn create(&mut self, path: &StorePath, kind: CreateKind) -> Result<(), MutationError> {
        if self.entries.contains_key(path) {
            return Err(MutationError::Conflict);
        }
        self.entries
            .insert(path.clone(), b"created".to_vec().into());
        self.creates.push((path.clone(), kind));
        Ok(())
    }

    fn rename_no_replace(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
        expected_identity: &[u8],
    ) -> Result<(), MutationError> {
        let Some(identity) = self.entries.get(source) else {
            return Err(MutationError::Missing);
        };
        if identity.as_ref() != expected_identity {
            return Err(MutationError::SourceChanged);
        }
        if self.entries.contains_key(destination) {
            return Err(MutationError::Conflict);
        }
        let identity = self.entries.remove(source).unwrap();
        self.entries.insert(destination.clone(), identity);
        self.actions.push((source.clone(), destination.clone()));
        Ok(())
    }
}

#[test]
fn local_name_validation_accepts_lossless_names_and_rejects_path_syntax() {
    for invalid in [
        OsStr::new(""),
        OsStr::new("."),
        OsStr::new(".."),
        OsStr::new("a/b"),
    ] {
        assert!(validate_local_name(invalid).is_err());
    }
    assert_eq!(
        validate_local_name(OsStr::from_bytes(b"nul\0name")),
        Err(NameError::ContainsNul)
    );
    let non_utf8 = OsString::from_vec(vec![b'n', 0xff]);
    assert!(validate_local_name(&non_utf8).is_ok());
}

#[test]
fn create_validates_the_name_and_refuses_conflicts_before_mutation() {
    let parent = local("/work");
    let mut provider = RecordingProvider::default();
    provider.add("/work/taken", b"taken");

    let invalid = CreateRequest::new(
        parent.clone(),
        OsString::from("../escape"),
        CreateKind::File,
    );
    assert_eq!(
        execute_create(&mut provider, &invalid),
        Err(MutationError::InvalidName)
    );
    assert!(provider.creates.is_empty());

    provider.create_allowed = false;
    let unsupported =
        CreateRequest::new(parent.clone(), OsString::from("blocked"), CreateKind::File);
    assert_eq!(
        execute_create(&mut provider, &unsupported),
        Err(MutationError::Unsupported)
    );
    provider.create_allowed = true;

    let escaping_parent = CreateRequest::new(
        local("/work/../outside"),
        OsString::from("file"),
        CreateKind::File,
    );
    assert_eq!(
        execute_create(&mut provider, &escaping_parent),
        Err(MutationError::InvalidScope)
    );
    assert!(provider.creates.is_empty());

    let conflict = CreateRequest::new(
        parent.clone(),
        OsString::from("taken"),
        CreateKind::Directory,
    );
    assert_eq!(
        execute_create(&mut provider, &conflict),
        Err(MutationError::Conflict)
    );
    assert!(provider.creates.is_empty());

    let valid = CreateRequest::new(
        parent,
        OsString::from_vec(vec![b'n', 0xff]),
        CreateKind::File,
    );
    execute_create(&mut provider, &valid).unwrap();
    assert_eq!(provider.creates.len(), 1);
}

#[test]
fn rename_revalidates_identity_and_never_replaces_a_destination() {
    let mut provider = RecordingProvider::default();
    provider.add("/work/source", b"source-id");
    provider.add("/work/taken", b"taken-id");

    let conflict = RenameRequest::new(
        local("/work/source"),
        OsString::from("taken"),
        b"source-id".to_vec(),
    );
    assert_eq!(
        execute_rename(&mut provider, &conflict),
        Err(MutationError::Conflict)
    );
    assert!(provider.actions.is_empty());

    let stale = RenameRequest::new(
        local("/work/source"),
        OsString::from("renamed"),
        b"old-id".to_vec(),
    );
    assert_eq!(
        execute_rename(&mut provider, &stale),
        Err(MutationError::SourceChanged)
    );
    assert!(provider.actions.is_empty());

    let valid = RenameRequest::new(
        local("/work/source"),
        OsString::from("renamed"),
        b"source-id".to_vec(),
    );
    execute_rename(&mut provider, &valid).unwrap();
    assert!(provider.entries.contains_key(&local("/work/renamed")));
}

#[test]
fn batch_rename_preflights_collisions_and_uses_temporary_names_for_cycles() {
    let mut collision_provider = RecordingProvider::default();
    collision_provider.add("/work/a", b"a");
    collision_provider.add("/work/b", b"b");
    collision_provider.add("/work/occupied", b"occupied");
    let collision = BatchRenamePlan::preflight(
        &mut collision_provider,
        vec![
            RenameMapping::new(local("/work/a"), OsString::from("same"), b"a".to_vec()),
            RenameMapping::new(local("/work/b"), OsString::from("same"), b"b".to_vec()),
        ],
    );
    assert_eq!(collision, Err(MutationError::BatchCollision));
    assert!(collision_provider.actions.is_empty());

    let occupied = BatchRenamePlan::preflight(
        &mut collision_provider,
        vec![RenameMapping::new(
            local("/work/a"),
            OsString::from("occupied"),
            b"a".to_vec(),
        )],
    );
    assert_eq!(occupied, Err(MutationError::Conflict));
    assert!(collision_provider.actions.is_empty());

    let mut cycle_provider = RecordingProvider::default();
    cycle_provider.add("/work/a", b"a");
    cycle_provider.add("/work/b", b"b");
    let plan = BatchRenamePlan::preflight(
        &mut cycle_provider,
        vec![
            RenameMapping::new(local("/work/a"), OsString::from("b"), b"a".to_vec()),
            RenameMapping::new(local("/work/b"), OsString::from("a"), b"b".to_vec()),
        ],
    )
    .unwrap();
    let mut journal = RecordingJournal::default();
    plan.execute(&mut cycle_provider, &mut journal).unwrap();

    assert_eq!(cycle_provider.entries[&local("/work/a")].as_ref(), b"b");
    assert_eq!(cycle_provider.entries[&local("/work/b")].as_ref(), b"a");
    assert!(cycle_provider.actions.len() >= 3);
    assert!(journal.plan.iter().any(BatchRenameStep::is_temporary));
    assert_eq!(journal.completed.len(), journal.plan.len());
}

#[test]
fn batch_rename_persists_the_complete_plan_before_the_first_mutation() {
    struct FailingJournal;
    impl BatchRenameJournal for FailingJournal {
        fn persist_plan(&mut self, _steps: &[BatchRenameStep]) -> Result<(), MutationError> {
            Err(MutationError::Provider("journal unavailable".into()))
        }

        fn persist_completed_step(&mut self, _index: usize) -> Result<(), MutationError> {
            Ok(())
        }
    }

    let mut provider = RecordingProvider::default();
    provider.add("/work/a", b"a");
    let plan = BatchRenamePlan::preflight(
        &mut provider,
        vec![RenameMapping::new(
            local("/work/a"),
            OsString::from("b"),
            b"a".to_vec(),
        )],
    )
    .unwrap();

    assert_eq!(
        plan.execute(&mut provider, &mut FailingJournal),
        Err(MutationError::Provider("journal unavailable".into()))
    );
    assert!(provider.actions.is_empty());
}

#[test]
fn rename_allows_provider_reported_case_aliases_but_blocks_normalization_collisions() {
    struct AliasProvider {
        source: StorePath,
        destination_alias: StorePath,
        destination_identity: Box<[u8]>,
        renamed: bool,
    }

    impl MutationProvider for AliasProvider {
        fn allows_create(
            &mut self,
            _parent: &StorePath,
            _kind: CreateKind,
        ) -> Result<bool, MutationError> {
            Ok(true)
        }

        fn allows_rename(&mut self, _source: &StorePath) -> Result<bool, MutationError> {
            Ok(true)
        }

        fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
            if path == &self.source {
                Ok(Some(b"source".to_vec().into()))
            } else if path == &self.destination_alias {
                Ok(Some(self.destination_identity.clone()))
            } else {
                Ok(None)
            }
        }

        fn create(&mut self, _path: &StorePath, _kind: CreateKind) -> Result<(), MutationError> {
            Err(MutationError::Unsupported)
        }

        fn rename_no_replace(
            &mut self,
            _source: &StorePath,
            _destination: &StorePath,
            _expected_identity: &[u8],
        ) -> Result<(), MutationError> {
            self.renamed = true;
            Ok(())
        }
    }

    let mut case_alias = AliasProvider {
        source: local("/work/Name"),
        destination_alias: local("/work/name"),
        destination_identity: b"source".to_vec().into(),
        renamed: false,
    };
    let request = RenameRequest::new(
        local("/work/Name"),
        OsString::from("name"),
        b"source".to_vec(),
    );
    execute_rename(&mut case_alias, &request).unwrap();
    assert!(case_alias.renamed);

    let mut normalization_collision = AliasProvider {
        source: local("/work/source"),
        destination_alias: StorePath::from_unix_path(OsString::from("/work/é")),
        destination_identity: b"other".to_vec().into(),
        renamed: false,
    };
    let request = RenameRequest::new(
        local("/work/source"),
        OsString::from("é"),
        b"source".to_vec(),
    );
    assert_eq!(
        execute_rename(&mut normalization_collision, &request),
        Err(MutationError::Conflict)
    );
    assert!(!normalization_collision.renamed);
}

fn local(path: &str) -> StorePath {
    StorePath::from_unix_path(path)
}
