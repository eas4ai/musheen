use musheen_core::StorePath;
use musheen_ops::{
    CorruptSource, Durability, EventGeneration, JobId, Journal, JournalPhase, JournalStorage,
    RecoveryContext, RecoveryDecision, StagingPath, StorageAction, decide_recovery,
};
use std::io;

#[derive(Clone, Copy, Debug)]
enum CrashTiming {
    Before,
    After,
}

struct CrashStorage {
    journal: Vec<u8>,
    durable_journal: Vec<u8>,
    snapshot: Vec<u8>,
    durable_snapshot: Vec<u8>,
    temporary_snapshot: Vec<u8>,
    journal_entry_durable: bool,
    fail_at: Option<(StorageAction, CrashTiming)>,
}

impl CrashStorage {
    fn empty() -> Self {
        Self {
            journal: Vec::new(),
            durable_journal: Vec::new(),
            snapshot: Vec::new(),
            durable_snapshot: Vec::new(),
            temporary_snapshot: Vec::new(),
            journal_entry_durable: false,
            fail_at: None,
        }
    }

    fn fail_at(&mut self, action: StorageAction, timing: CrashTiming) {
        self.fail_at = Some((action, timing));
    }

    fn before(&mut self, action: StorageAction) -> io::Result<()> {
        if matches!(self.fail_at, Some((target, CrashTiming::Before)) if target == action) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "simulated crash",
            ));
        }
        Ok(())
    }

    fn after(&mut self, action: StorageAction) -> io::Result<()> {
        if matches!(self.fail_at, Some((target, CrashTiming::After)) if target == action) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "simulated crash",
            ));
        }
        Ok(())
    }

    fn restart(mut self) -> Self {
        if self.journal_entry_durable {
            self.journal.clone_from(&self.durable_journal);
        } else {
            self.journal.clear();
        }
        self.snapshot.clone_from(&self.durable_snapshot);
        self.temporary_snapshot.clear();
        self.fail_at = None;
        self
    }
}

impl JournalStorage for CrashStorage {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.snapshot.clone())
    }

    fn read_journal(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.journal.clone())
    }

    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.before(StorageAction::AppendJournal)?;
        self.journal.extend_from_slice(bytes);
        self.after(StorageAction::AppendJournal)
    }

    fn sync_journal(&mut self) -> io::Result<()> {
        self.before(StorageAction::SyncJournal)?;
        self.durable_journal.clone_from(&self.journal);
        self.after(StorageAction::SyncJournal)
    }

    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.before(StorageAction::WriteSnapshotTemporary)?;
        self.temporary_snapshot = bytes.to_vec();
        self.after(StorageAction::WriteSnapshotTemporary)
    }

    fn sync_snapshot_temporary(&mut self) -> io::Result<()> {
        self.before(StorageAction::SyncSnapshotTemporary)?;
        self.after(StorageAction::SyncSnapshotTemporary)
    }

    fn publish_snapshot(&mut self) -> io::Result<()> {
        self.before(StorageAction::PublishSnapshot)?;
        self.snapshot.clone_from(&self.temporary_snapshot);
        self.after(StorageAction::PublishSnapshot)
    }

    fn sync_parent(&mut self) -> io::Result<()> {
        self.before(StorageAction::SyncParent)?;
        self.durable_snapshot.clone_from(&self.snapshot);
        self.journal_entry_durable = true;
        self.after(StorageAction::SyncParent)
    }

    fn reset_journal(&mut self) -> io::Result<()> {
        self.before(StorageAction::ResetJournal)?;
        self.journal.clear();
        self.after(StorageAction::ResetJournal)
    }

    fn quarantine(
        &mut self,
        source: CorruptSource,
        valid_prefix: &[u8],
        _corrupt_suffix: &[u8],
    ) -> io::Result<()> {
        match source {
            CorruptSource::Snapshot => {
                self.snapshot = valid_prefix.to_vec();
                self.durable_snapshot = valid_prefix.to_vec();
            }
            CorruptSource::Journal => {
                self.journal = valid_prefix.to_vec();
                self.durable_journal = valid_prefix.to_vec();
                self.journal_entry_durable = true;
            }
        }
        Ok(())
    }
}

fn append_phase(journal: &mut Journal<CrashStorage>, phase: JournalPhase) {
    journal
        .append(
            JobId::new(7).unwrap(),
            EventGeneration::new(0),
            phase,
            Durability::CrashDurable,
        )
        .unwrap();
}

#[test]
fn every_crash_phase_resolves_to_resume_rollback_or_ask() {
    let safe = RecoveryContext {
        continuation_verified: true,
        staging_owned: true,
        destination_verified: true,
        source_identity_current: true,
    };
    for (phase, expected) in [
        (JournalPhase::Planned, RecoveryDecision::Rollback),
        (JournalPhase::StagingCreated, RecoveryDecision::Rollback),
        (JournalPhase::DataCopied, RecoveryDecision::Rollback),
        (JournalPhase::MetadataApplied, RecoveryDecision::Rollback),
        (JournalPhase::DestinationPublished, RecoveryDecision::Resume),
        (JournalPhase::SourceRemoved, RecoveryDecision::Resume),
        (JournalPhase::StagingCleaned, RecoveryDecision::Resume),
    ] {
        assert_eq!(decide_recovery(phase, safe), expected, "phase {phase:?}");
    }
}

#[test]
fn ambiguous_or_unowned_recovery_never_guesses() {
    let unsafe_context = RecoveryContext {
        continuation_verified: false,
        staging_owned: false,
        destination_verified: false,
        source_identity_current: false,
    };
    for phase in [
        JournalPhase::StagingCreated,
        JournalPhase::DataCopied,
        JournalPhase::MetadataApplied,
        JournalPhase::DestinationPublished,
        JournalPhase::SourceRemoved,
    ] {
        assert_eq!(
            decide_recovery(phase, unsafe_context),
            RecoveryDecision::Ask,
            "phase {phase:?}"
        );
    }
}

#[test]
fn source_removal_without_a_verified_destination_needs_attention() {
    let unverified_destination = RecoveryContext {
        continuation_verified: true,
        staging_owned: false,
        destination_verified: false,
        source_identity_current: false,
    };

    for phase in [JournalPhase::SourceRemoved, JournalPhase::StagingCleaned] {
        assert_eq!(
            decide_recovery(phase, unverified_destination),
            RecoveryDecision::Ask,
            "phase {phase:?}"
        );
    }
}

#[test]
fn staging_names_are_destination_siblings_and_cleanup_is_owner_scoped() {
    let destination = StorePath::from_unix_path("/volume/folder/report.txt");
    let staging = StagingPath::for_destination(
        &destination,
        JobId::new(42).unwrap(),
        EventGeneration::new(3),
    )
    .expect("a local destination has a sibling staging path");

    assert_eq!(
        staging.path().as_unix_path().unwrap().parent(),
        destination.as_unix_path().unwrap().parent()
    );
    assert!(staging.is_app_owned());
    let nonce_staging = StagingPath::for_destination_with_nonce(
        &destination,
        JobId::new(42).unwrap(),
        EventGeneration::new(3),
        [0x5a; 16],
    )
    .expect("nonce-bearing staging path");
    assert!(nonce_staging.is_app_owned());
    assert_eq!(StagingPath::nonce(nonce_staging.path()), Some([0x5a; 16]));
    for malformed in [
        "/volume/folder/.musheen-stage-v1-42-3-short",
        "/volume/folder/.musheen-stage-v1-42-3-zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
        "/volume/folder/.musheen-stage-v1-42-3-5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a-extra",
    ] {
        assert!(!StagingPath::is_owned_path(&StorePath::from_unix_path(
            malformed
        )));
    }
    assert!(!StagingPath::is_owned_path(&StorePath::from_unix_path(
        "/volume/folder/.musheen-stage-user-data"
    )));
    assert!(!StagingPath::is_owned_path(&StorePath::from_unix_bytes(
        b"/volume/folder/.musheen-stage-v1-42-3\xff".to_vec()
    )));
}

#[test]
fn journal_append_and_fsync_crashes_recover_only_durable_prefixes() {
    let phases = [
        JournalPhase::Planned,
        JournalPhase::StagingCreated,
        JournalPhase::DataCopied,
        JournalPhase::MetadataApplied,
        JournalPhase::DestinationPublished,
        JournalPhase::SourceRemoved,
        JournalPhase::StagingCleaned,
    ];

    for action in [
        StorageAction::AppendJournal,
        StorageAction::SyncJournal,
        StorageAction::SyncParent,
    ] {
        for timing in [CrashTiming::Before, CrashTiming::After] {
            for (phase_index, phase) in phases.iter().copied().enumerate() {
                let mut journal = Journal::open(CrashStorage::empty()).unwrap();
                for stable_phase in phases.iter().copied().take(phase_index) {
                    append_phase(&mut journal, stable_phase);
                }
                let mut storage = journal.into_storage();
                storage.fail_at(action, timing);
                let mut interrupted = Journal::open(storage).unwrap();

                assert!(
                    interrupted
                        .append(
                            JobId::new(7).unwrap(),
                            EventGeneration::new(0),
                            phase,
                            Durability::CrashDurable,
                        )
                        .is_err(),
                    "{action:?} {timing:?} {phase:?} must interrupt"
                );
                let recovered = Journal::open(interrupted.into_storage().restart()).unwrap();
                let includes_attempted = match action {
                    StorageAction::SyncJournal => {
                        phase_index > 0 && matches!(timing, CrashTiming::After)
                    }
                    StorageAction::SyncParent => {
                        phase_index > 0 || matches!(timing, CrashTiming::After)
                    }
                    _ => false,
                };
                assert_eq!(
                    recovered.records().len(),
                    phase_index + usize::from(includes_attempted),
                    "{action:?} {timing:?} {phase:?}"
                );
            }
        }
    }
}

#[test]
fn snapshot_compaction_survives_every_storage_crash_boundary() {
    for action in [
        StorageAction::WriteSnapshotTemporary,
        StorageAction::SyncSnapshotTemporary,
        StorageAction::PublishSnapshot,
        StorageAction::SyncParent,
        StorageAction::ResetJournal,
        StorageAction::SyncJournal,
    ] {
        for timing in [CrashTiming::Before, CrashTiming::After] {
            let mut journal = Journal::open(CrashStorage::empty()).unwrap();
            append_phase(&mut journal, JournalPhase::Planned);
            let mut storage = journal.into_storage();
            storage.fail_at(action, timing);
            let mut interrupted = Journal::open(storage).unwrap();

            assert!(
                interrupted.compact().is_err(),
                "{action:?} {timing:?} must interrupt"
            );
            let recovered = Journal::open(interrupted.into_storage().restart()).unwrap();
            assert_eq!(
                recovered.records().len(),
                1,
                "{action:?} {timing:?} lost the durable record"
            );
            assert_eq!(recovered.records()[0].phase(), JournalPhase::Planned);
        }
    }
}

#[test]
fn same_and_cross_filesystem_action_crashes_never_guess() {
    for relation in ["same-filesystem", "cross-filesystem"] {
        for (label, phase, context, expected) in [
            (
                "before metadata",
                JournalPhase::DataCopied,
                RecoveryContext {
                    continuation_verified: true,
                    staging_owned: true,
                    destination_verified: false,
                    source_identity_current: true,
                },
                RecoveryDecision::Rollback,
            ),
            (
                "after metadata",
                JournalPhase::DataCopied,
                RecoveryContext {
                    continuation_verified: true,
                    staging_owned: true,
                    destination_verified: false,
                    source_identity_current: true,
                },
                RecoveryDecision::Rollback,
            ),
            (
                "before destination publish",
                JournalPhase::MetadataApplied,
                RecoveryContext {
                    continuation_verified: true,
                    staging_owned: true,
                    destination_verified: false,
                    source_identity_current: true,
                },
                RecoveryDecision::Rollback,
            ),
            (
                "after destination publish",
                JournalPhase::MetadataApplied,
                RecoveryContext {
                    continuation_verified: false,
                    staging_owned: false,
                    destination_verified: true,
                    source_identity_current: true,
                },
                RecoveryDecision::Ask,
            ),
            (
                "before source removal",
                JournalPhase::DestinationPublished,
                RecoveryContext {
                    continuation_verified: true,
                    staging_owned: false,
                    destination_verified: true,
                    source_identity_current: true,
                },
                RecoveryDecision::Resume,
            ),
            (
                "after source removal",
                JournalPhase::DestinationPublished,
                RecoveryContext {
                    continuation_verified: false,
                    staging_owned: false,
                    destination_verified: true,
                    source_identity_current: false,
                },
                RecoveryDecision::Ask,
            ),
            (
                "before staging cleanup",
                JournalPhase::SourceRemoved,
                RecoveryContext {
                    continuation_verified: true,
                    staging_owned: true,
                    destination_verified: true,
                    source_identity_current: false,
                },
                RecoveryDecision::Resume,
            ),
            (
                "after staging cleanup",
                JournalPhase::SourceRemoved,
                RecoveryContext {
                    continuation_verified: true,
                    staging_owned: false,
                    destination_verified: true,
                    source_identity_current: false,
                },
                RecoveryDecision::Resume,
            ),
        ] {
            assert_eq!(
                decide_recovery(phase, context),
                expected,
                "{relation}: {label}"
            );
        }
    }
}
