use musheen_core::StorePath;
use musheen_desktop::{FileBatchRenameJournal, FileJournalStorage};
use musheen_ops::{
    BatchRenamePlan, CreateKind, Durability, EventGeneration, JobId, Journal, JournalPhase,
    MutationError, MutationProvider, RenameMapping,
};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::Command;
use std::time::{Duration, Instant};

const FORCED_TERMINATION_ROOT: &str = "MUSHEEN_FORCED_TERMINATION_ROOT";
const FORCED_TERMINATION_PHASE: &str = "MUSHEEN_FORCED_TERMINATION_PHASE";

#[test]
fn file_storage_round_trips_snapshot_and_journal_tail() {
    let root = tempfile::tempdir().unwrap();
    let storage = FileJournalStorage::at(root.path()).unwrap();
    let mut journal = Journal::open(storage).unwrap();
    append(&mut journal, EventGeneration::new(0), JournalPhase::Planned);
    journal.compact().unwrap();
    append(
        &mut journal,
        EventGeneration::new(1),
        JournalPhase::StagingCreated,
    );
    drop(journal);

    let recovered = Journal::open(FileJournalStorage::at(root.path()).unwrap()).unwrap();

    assert_eq!(recovered.records().len(), 2);
    assert_eq!(recovered.records()[1].phase(), JournalPhase::StagingCreated);
    assert_eq!(
        std::fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(root.path().join("operations.journal"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn file_storage_quarantines_and_repairs_a_corrupt_tail() {
    let root = tempfile::tempdir().unwrap();
    let storage = FileJournalStorage::at(root.path()).unwrap();
    let journal_path = storage.journal_path().to_path_buf();
    let mut journal = Journal::open(storage).unwrap();
    append(&mut journal, EventGeneration::new(0), JournalPhase::Planned);
    drop(journal);
    OpenOptions::new()
        .append(true)
        .open(&journal_path)
        .unwrap()
        .write_all(b"corrupt\n")
        .unwrap();

    let mut recovered = Journal::open(FileJournalStorage::at(root.path()).unwrap()).unwrap();
    assert_eq!(recovered.records().len(), 1);
    assert_eq!(recovered.quarantined_records(), 1);
    append(
        &mut recovered,
        EventGeneration::new(1),
        JournalPhase::StagingCreated,
    );
    drop(recovered);

    let reopened = Journal::open(FileJournalStorage::at(root.path()).unwrap()).unwrap();
    assert_eq!(reopened.records().len(), 2);
    assert_eq!(reopened.quarantined_records(), 0);
    assert_eq!(
        std::fs::read_dir(root.path().join("quarantine"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn batch_rename_journal_durably_records_plan_and_completed_steps() {
    #[derive(Default)]
    struct Provider {
        entries: HashMap<StorePath, Box<[u8]>>,
    }
    impl MutationProvider for Provider {
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
            Ok(self.entries.get(path).cloned())
        }

        fn create(&mut self, _path: &StorePath, _kind: CreateKind) -> Result<(), MutationError> {
            Err(MutationError::Unsupported)
        }

        fn rename_no_replace(
            &mut self,
            source: &StorePath,
            destination: &StorePath,
            expected_identity: &[u8],
        ) -> Result<(), MutationError> {
            if self.entries.get(source).map(Box::as_ref) != Some(expected_identity) {
                return Err(MutationError::SourceChanged);
            }
            let identity = self.entries.remove(source).ok_or(MutationError::Missing)?;
            self.entries.insert(destination.clone(), identity);
            Ok(())
        }
    }

    let root = tempfile::tempdir().unwrap();
    let source = StorePath::from_unix_path("/work/a");
    let mut provider = Provider::default();
    provider
        .entries
        .insert(source.clone(), b"a".to_vec().into());
    let plan = BatchRenamePlan::preflight(
        &mut provider,
        vec![RenameMapping::new(
            source,
            OsString::from("b"),
            b"a".to_vec(),
        )],
    )
    .unwrap();
    let mut journal = FileBatchRenameJournal::at(root.path(), 7).unwrap();

    plan.execute(&mut provider, &mut journal).unwrap();

    let recovery = FileBatchRenameJournal::at(root.path(), 7)
        .unwrap()
        .recovery()
        .unwrap()
        .unwrap();
    assert_eq!(recovery.steps().len(), 1);
    assert_eq!(recovery.completed_steps(), &[0]);
    journal.finish().unwrap();
}

#[test]
fn file_journal_recovers_every_phase_after_forced_process_termination() {
    let phases = [
        JournalPhase::Planned,
        JournalPhase::StagingCreated,
        JournalPhase::DataCopied,
        JournalPhase::MetadataApplied,
        JournalPhase::DestinationPublished,
        JournalPhase::SourceRemoved,
        JournalPhase::StagingCleaned,
    ];

    for (phase_index, expected_phase) in phases.into_iter().enumerate() {
        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "forced_termination_child",
                "--ignored",
                "--test-threads=1",
            ])
            .env(FORCED_TERMINATION_ROOT, root.path())
            .env(FORCED_TERMINATION_PHASE, phase_index.to_string())
            .spawn()
            .unwrap();
        let ready = root.path().join("ready");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "child exited before phase {expected_phase:?} became durable"
            );
            assert!(
                Instant::now() < deadline,
                "child did not make phase {expected_phase:?} durable"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());

        let recovered = Journal::open(FileJournalStorage::at(root.path()).unwrap()).unwrap();
        assert_eq!(recovered.records().len(), phase_index + 1);
        assert_eq!(recovered.records()[phase_index].phase(), expected_phase);
    }
}

#[test]
#[ignore = "spawned by file_journal_recovers_every_phase_after_forced_process_termination"]
fn forced_termination_child() {
    let Some(root) = std::env::var_os(FORCED_TERMINATION_ROOT) else {
        return;
    };
    let phase_index = std::env::var(FORCED_TERMINATION_PHASE)
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let phases = [
        JournalPhase::Planned,
        JournalPhase::StagingCreated,
        JournalPhase::DataCopied,
        JournalPhase::MetadataApplied,
        JournalPhase::DestinationPublished,
        JournalPhase::SourceRemoved,
        JournalPhase::StagingCleaned,
    ];
    let mut journal = Journal::open(FileJournalStorage::at(&root).unwrap()).unwrap();
    for (generation, phase) in phases.into_iter().take(phase_index + 1).enumerate() {
        append(&mut journal, EventGeneration::new(generation as u64), phase);
    }
    std::fs::write(std::path::Path::new(&root).join("ready"), b"ready").unwrap();
    loop {
        std::thread::park();
    }
}

fn append(
    journal: &mut Journal<FileJournalStorage>,
    generation: EventGeneration,
    phase: JournalPhase,
) {
    journal
        .append(
            JobId::new(1).unwrap(),
            generation,
            phase,
            Durability::CrashDurable,
        )
        .unwrap();
}
