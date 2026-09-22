use musheen_core::StorePath;
use musheen_ops::{
    ArchiveCheckpoint, ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase,
    ArchiveOperationPlan, ArchivePathIdentity, CorruptSource, Durability, EventGeneration,
    JobEvent, JobId, Journal, JournalPhase, JournalStorage, RecoveryContext, RecoveryDecision,
    decide_recovery,
};
use std::io;
use std::sync::Arc;

#[derive(Default)]
struct PersistentMemoryStorage {
    snapshot: Vec<u8>,
    journal: Vec<u8>,
    temporary: Vec<u8>,
}

impl JournalStorage for PersistentMemoryStorage {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.snapshot.clone())
    }
    fn read_journal(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.journal.clone())
    }
    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.journal.extend_from_slice(bytes);
        Ok(())
    }
    fn sync_journal(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.temporary = bytes.to_vec();
        Ok(())
    }
    fn sync_snapshot_temporary(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn publish_snapshot(&mut self) -> io::Result<()> {
        self.snapshot.clone_from(&self.temporary);
        Ok(())
    }
    fn sync_parent(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn reset_journal(&mut self) -> io::Result<()> {
        self.journal.clear();
        Ok(())
    }
    fn quarantine(
        &mut self,
        _source: CorruptSource,
        _valid_prefix: &[u8],
        _corrupt_suffix: &[u8],
    ) -> io::Result<()> {
        Ok(())
    }
}

fn path(value: &str) -> StorePath {
    StorePath::from_unix_path(value)
}

#[test]
fn archive_plans_are_typed_and_reject_invalid_shapes() {
    let sources = vec![path("/data/one"), path("/data/two")];
    let create = ArchiveOperationPlan::create(
        sources.clone(),
        path("/data/out.zip"),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("valid create plan");
    assert_eq!(create.sources(), sources.as_slice());
    assert!(!create.encrypted());

    assert!(
        ArchiveOperationPlan::create(
            Vec::new(),
            path("/data/out.zip"),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .is_err()
    );
    assert!(
        ArchiveOperationPlan::extract(
            path("/data/in.zip"),
            path("/data/out"),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .is_ok()
    );
    assert!(
        ArchiveOperationPlan::extract(
            path("/data/in.zip"),
            path("/data/in.zip"),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Replace,
            false,
        )
        .is_err()
    );
}

#[test]
fn archive_events_report_recoverable_phases() {
    let event = JobEvent::archive_phase(
        JobId::new(7).expect("job id"),
        EventGeneration::new(2),
        99,
        ArchiveEventPhase::Publishing,
    );
    assert_eq!(
        event.archive_phase_value(),
        Some(ArchiveEventPhase::Publishing)
    );
    assert!(event.state().is_none());
    assert!(event.progress_value().is_none());
}

#[test]
fn restart_recovery_never_auto_publishes_unfinished_archive_staging() {
    let owned = RecoveryContext {
        continuation_verified: false,
        staging_owned: true,
        destination_verified: false,
        source_identity_current: true,
    };
    for phase in [
        JournalPhase::Planned,
        JournalPhase::StagingCreated,
        JournalPhase::DataCopied,
        JournalPhase::MetadataApplied,
    ] {
        assert_eq!(decide_recovery(phase, owned), RecoveryDecision::Rollback);
    }

    let published = RecoveryContext {
        continuation_verified: true,
        staging_owned: true,
        destination_verified: true,
        source_identity_current: true,
    };
    assert_eq!(
        decide_recovery(JournalPhase::DestinationPublished, published),
        RecoveryDecision::Resume
    );
    assert_eq!(
        decide_recovery(JournalPhase::Completed, published),
        RecoveryDecision::NoAction
    );
}

#[test]
fn archive_checkpoint_survives_a_fresh_journal_instance() {
    let plan = ArchiveOperationPlan::extract(
        path("/data/in.zip"),
        path("/data/out"),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("archive plan");
    let identity = ArchivePathIdentity::new(1, 2, 3, 4, 5, true);
    let checkpoint = ArchiveCheckpoint::new(
        plan.clone(),
        path("/data/.musheen-stage-v1-9-0"),
        Some(identity),
        None,
        Some(identity),
    );
    let mut first = Journal::open(PersistentMemoryStorage::default()).expect("journal opens");
    first
        .append_archive(
            JobId::new(9).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::DestinationPublished,
            Durability::CrashDurable,
            checkpoint,
        )
        .expect("checkpoint persists");
    let storage = first.into_storage();

    let reopened = Journal::open(storage).expect("fresh journal instance reopens");
    let recovered = reopened.records()[0]
        .archive_checkpoint()
        .expect("archive checkpoint survives");
    assert_eq!(recovered.plan(), &plan);
    assert_eq!(recovered.staging_identity(), Some(identity));
    assert_eq!(recovered.destination_after(), Some(identity));
}

#[test]
fn archive_checkpoint_reuses_one_plan_in_memory_and_on_disk_across_phases() {
    let sources = (0..64)
        .map(|index| {
            path(&format!(
                "/data/{index:03}-{}",
                "long-source-name".repeat(12)
            ))
        })
        .collect();
    let plan = ArchiveOperationPlan::create(
        sources,
        path("/data/out.zip"),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("archive plan");
    let staging = path("/data/.musheen-stage-v1-10-0-5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a");
    let job = JobId::new(10).expect("job id");
    let generation = EventGeneration::new(0);
    let mut journal = Journal::open(PersistentMemoryStorage::default()).expect("journal opens");
    for phase in [
        JournalPhase::Planned,
        JournalPhase::StagingCreated,
        JournalPhase::DataCopied,
        JournalPhase::MetadataApplied,
    ] {
        journal
            .append_archive(
                job,
                generation,
                phase,
                Durability::CrashDurable,
                ArchiveCheckpoint::new(plan.clone(), staging.clone(), None, None, None)
                    .with_staging_nonce([0x5a; 16]),
            )
            .expect("checkpoint persists");
    }
    let records = journal.records();
    let first_plan = records[0]
        .archive_checkpoint()
        .expect("first checkpoint")
        .shared_plan();
    for record in &records[1..] {
        assert!(Arc::ptr_eq(
            &first_plan,
            &record
                .archive_checkpoint()
                .expect("checkpoint")
                .shared_plan()
        ));
    }
    let full_plan_bytes = serde_json::to_vec(&plan).expect("plan serializes").len();
    assert!(
        journal.storage().journal.len() < full_plan_bytes * 2,
        "the durable journal must store the plan once, not once per phase"
    );

    let reopened = Journal::open(journal.into_storage()).expect("journal reopens");
    assert_eq!(reopened.records().len(), 4);
    let reopened_plan = reopened.records()[0]
        .archive_checkpoint()
        .expect("checkpoint")
        .shared_plan();
    for record in &reopened.records()[1..] {
        assert!(Arc::ptr_eq(
            &reopened_plan,
            &record
                .archive_checkpoint()
                .expect("checkpoint")
                .shared_plan()
        ));
    }
}
