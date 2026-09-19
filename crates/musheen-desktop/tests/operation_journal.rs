use musheen_desktop::FileJournalStorage;
use musheen_ops::{Durability, EventGeneration, JobId, Journal, JournalPhase};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

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
