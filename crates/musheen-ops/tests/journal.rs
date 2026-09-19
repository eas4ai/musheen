use musheen_ops::{
    CorruptSource, Durability, EventGeneration, JobId, Journal, JournalPhase, JournalStorage,
    StorageAction,
};
use std::io;

#[derive(Default)]
struct RecordingStorage {
    journal: Vec<u8>,
    snapshot: Vec<u8>,
    temporary_snapshot: Vec<u8>,
    quarantine: Vec<(CorruptSource, Vec<u8>)>,
    actions: Vec<StorageAction>,
}

impl JournalStorage for RecordingStorage {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>> {
        self.actions.push(StorageAction::ReadSnapshot);
        Ok(self.snapshot.clone())
    }

    fn read_journal(&mut self) -> io::Result<Vec<u8>> {
        self.actions.push(StorageAction::ReadJournal);
        Ok(self.journal.clone())
    }

    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.actions.push(StorageAction::AppendJournal);
        self.journal.extend_from_slice(bytes);
        Ok(())
    }

    fn sync_journal(&mut self) -> io::Result<()> {
        self.actions.push(StorageAction::SyncJournal);
        Ok(())
    }

    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.actions.push(StorageAction::WriteSnapshotTemporary);
        self.temporary_snapshot = bytes.to_vec();
        Ok(())
    }

    fn sync_snapshot_temporary(&mut self) -> io::Result<()> {
        self.actions.push(StorageAction::SyncSnapshotTemporary);
        Ok(())
    }

    fn publish_snapshot(&mut self) -> io::Result<()> {
        self.actions.push(StorageAction::PublishSnapshot);
        self.snapshot = std::mem::take(&mut self.temporary_snapshot);
        Ok(())
    }

    fn sync_parent(&mut self) -> io::Result<()> {
        self.actions.push(StorageAction::SyncParent);
        Ok(())
    }

    fn reset_journal(&mut self) -> io::Result<()> {
        self.actions.push(StorageAction::ResetJournal);
        self.journal.clear();
        Ok(())
    }

    fn quarantine(
        &mut self,
        source: CorruptSource,
        valid_prefix: &[u8],
        corrupt_suffix: &[u8],
    ) -> io::Result<()> {
        self.actions.push(StorageAction::Quarantine);
        self.quarantine.push((source, corrupt_suffix.to_vec()));
        match source {
            CorruptSource::Snapshot => self.snapshot = valid_prefix.to_vec(),
            CorruptSource::Journal => self.journal = valid_prefix.to_vec(),
        }
        Ok(())
    }
}

#[test]
fn appends_are_versioned_checksummed_and_synced_before_success() {
    let storage = RecordingStorage::default();
    let mut journal = Journal::open(storage).expect("empty storage opens");
    let record = journal
        .append(
            JobId::new(1).unwrap(),
            EventGeneration::new(0),
            JournalPhase::Planned,
            Durability::CrashDurable,
        )
        .expect("record appends durably");

    assert_eq!(record.schema_version(), 1);
    assert_eq!(record.sequence(), 1);
    assert_eq!(
        journal.storage().actions,
        [
            StorageAction::ReadSnapshot,
            StorageAction::ReadJournal,
            StorageAction::AppendJournal,
            StorageAction::SyncJournal,
            StorageAction::SyncParent,
        ]
    );
    assert!(journal.storage().journal.ends_with(b"\n"));
    assert!(
        !journal
            .storage()
            .journal
            .windows(3)
            .any(|bytes| bytes == b"\0\0\0")
    );
}

#[test]
fn compaction_publishes_only_a_synced_complete_snapshot() {
    let mut journal = Journal::open(RecordingStorage::default()).unwrap();
    journal
        .append(
            JobId::new(1).unwrap(),
            EventGeneration::new(0),
            JournalPhase::Planned,
            Durability::BestEffort("directory sync unavailable".into()),
        )
        .unwrap();
    journal.compact().expect("snapshot compacts atomically");

    assert_eq!(
        &journal.storage().actions[5..],
        [
            StorageAction::WriteSnapshotTemporary,
            StorageAction::SyncSnapshotTemporary,
            StorageAction::PublishSnapshot,
            StorageAction::SyncParent,
            StorageAction::ResetJournal,
            StorageAction::SyncJournal,
        ]
    );
    assert!(!journal.storage().snapshot.is_empty());
    assert!(journal.storage().journal.is_empty());
}

#[test]
fn restart_combines_a_compacted_snapshot_with_the_journal_tail() {
    let mut journal = Journal::open(RecordingStorage::default()).unwrap();
    journal
        .append(
            JobId::new(1).unwrap(),
            EventGeneration::new(0),
            JournalPhase::Planned,
            Durability::CrashDurable,
        )
        .unwrap();
    journal.compact().unwrap();
    journal
        .append(
            JobId::new(1).unwrap(),
            EventGeneration::new(1),
            JournalPhase::StagingCreated,
            Durability::CrashDurable,
        )
        .unwrap();

    let reopened = Journal::open(journal.into_storage()).expect("snapshot and tail reopen");

    assert_eq!(reopened.records().len(), 2);
    assert_eq!(reopened.records()[0].sequence(), 1);
    assert_eq!(reopened.records()[1].sequence(), 2);
    assert_eq!(reopened.records()[1].phase(), JournalPhase::StagingCreated);
}

#[test]
fn restart_ignores_journal_records_already_present_in_the_snapshot() {
    let mut journal = Journal::open(RecordingStorage::default()).unwrap();
    journal
        .append(
            JobId::new(1).unwrap(),
            EventGeneration::new(0),
            JournalPhase::Planned,
            Durability::CrashDurable,
        )
        .unwrap();
    journal.compact().unwrap();
    let mut storage = journal.into_storage();
    storage.journal = storage.snapshot.clone();

    let reopened = Journal::open(storage).expect("pre-reset duplicate is recoverable");

    assert_eq!(reopened.records().len(), 1);
    assert_eq!(reopened.records()[0].sequence(), 1);
}

#[test]
fn corrupt_records_are_quarantined_without_discarding_valid_prefixes() {
    let mut valid = Journal::open(RecordingStorage::default()).unwrap();
    valid
        .append(
            JobId::new(9).unwrap(),
            EventGeneration::new(0),
            JournalPhase::Planned,
            Durability::CrashDurable,
        )
        .unwrap();
    let mut storage = valid.into_storage();
    storage
        .journal
        .extend_from_slice(b"{\"checksum\":\"bad\",\"payload\":{}}\n");

    let mut reopened = Journal::open(storage).expect("valid prefix remains recoverable");
    assert_eq!(reopened.records().len(), 1);
    assert_eq!(reopened.quarantined_records(), 1);
    assert_eq!(reopened.storage().quarantine.len(), 1);

    reopened
        .append(
            JobId::new(9).unwrap(),
            EventGeneration::new(1),
            JournalPhase::StagingCreated,
            Durability::CrashDurable,
        )
        .expect("append follows the repaired valid prefix");
    let recovered = Journal::open(reopened.into_storage()).expect("repaired journal reopens");
    assert_eq!(recovered.records().len(), 2);
    assert_eq!(recovered.quarantined_records(), 0);
    assert_eq!(recovered.storage().quarantine.len(), 1);
}
