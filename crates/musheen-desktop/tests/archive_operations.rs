use musheen_core::{
    CancellationToken, CapabilityMatrix, CapabilityState, ProviderId, ResourceLimits, StorePath,
};
use musheen_desktop::{
    ArchiveBudget, ArchiveError, ArchiveOperationAccounting, ArchiveOperationError,
    ArchiveOperationLimits, ArchiveOperationOutcome, ArchivePassword, ArchivePasswordProvider,
    ArchiveRecoveryAction, FileJournalStorage, PasswordRequest, apply_archive_recovery,
    apply_archive_recovery_with_accounting, apply_archive_recovery_with_cancellation,
    execute_archive_plan, execute_scheduled_archive_operation,
    execute_scheduled_archive_operation_with_accounting, recover_archive_operations,
    recover_archive_operations_with_cancellation,
};
use musheen_ops::{
    ArchiveCheckpoint, ArchiveCleanupKind, ArchiveCodec, ArchiveConflictPolicy,
    ArchiveOperationPlan, ArchivePathIdentity, CorruptSource, Durability, EventGeneration,
    ExtractMerge, JobId, Journal, JournalPhase, JournalStorage, ProviderLimits, ProviderSnapshot,
    Scheduler,
};
use std::io::{self, Cursor, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::Path;
use tempfile::tempdir;

#[derive(Default)]
struct MemoryJournal {
    snapshot: Vec<u8>,
    journal: Vec<u8>,
    temporary: Vec<u8>,
    fail_append_at: Option<usize>,
    append_count: usize,
}

struct HookJournal {
    inner: MemoryJournal,
    hook_at: usize,
    hook: Option<Box<dyn FnOnce()>>,
}

struct TwoHookJournal {
    inner: MemoryJournal,
    first: Option<Box<dyn FnOnce()>>,
    second: Option<Box<dyn FnOnce()>>,
}

impl JournalStorage for TwoHookJournal {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>> {
        self.inner.read_snapshot()
    }
    fn read_journal(&mut self) -> io::Result<Vec<u8>> {
        self.inner.read_journal()
    }
    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.append_journal(bytes)?;
        match self.inner.append_count {
            2 => {
                if let Some(hook) = self.first.take() {
                    hook();
                }
            }
            4 => {
                if let Some(hook) = self.second.take() {
                    hook();
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn sync_journal(&mut self) -> io::Result<()> {
        self.inner.sync_journal()
    }
    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_snapshot_temporary(bytes)
    }
    fn sync_snapshot_temporary(&mut self) -> io::Result<()> {
        self.inner.sync_snapshot_temporary()
    }
    fn publish_snapshot(&mut self) -> io::Result<()> {
        self.inner.publish_snapshot()
    }
    fn sync_parent(&mut self) -> io::Result<()> {
        self.inner.sync_parent()
    }
    fn reset_journal(&mut self) -> io::Result<()> {
        self.inner.reset_journal()
    }
    fn quarantine(
        &mut self,
        source: CorruptSource,
        valid_prefix: &[u8],
        corrupt_suffix: &[u8],
    ) -> io::Result<()> {
        self.inner.quarantine(source, valid_prefix, corrupt_suffix)
    }
}

impl JournalStorage for HookJournal {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>> {
        self.inner.read_snapshot()
    }

    fn read_journal(&mut self) -> io::Result<Vec<u8>> {
        self.inner.read_journal()
    }

    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.append_journal(bytes)?;
        if self.inner.append_count == self.hook_at
            && let Some(hook) = self.hook.take()
        {
            hook();
        }
        Ok(())
    }

    fn sync_journal(&mut self) -> io::Result<()> {
        self.inner.sync_journal()
    }

    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.inner.write_snapshot_temporary(bytes)
    }

    fn sync_snapshot_temporary(&mut self) -> io::Result<()> {
        self.inner.sync_snapshot_temporary()
    }

    fn publish_snapshot(&mut self) -> io::Result<()> {
        self.inner.publish_snapshot()
    }

    fn sync_parent(&mut self) -> io::Result<()> {
        self.inner.sync_parent()
    }

    fn reset_journal(&mut self) -> io::Result<()> {
        self.inner.reset_journal()
    }

    fn quarantine(
        &mut self,
        source: CorruptSource,
        valid_prefix: &[u8],
        corrupt_suffix: &[u8],
    ) -> io::Result<()> {
        self.inner.quarantine(source, valid_prefix, corrupt_suffix)
    }
}

impl JournalStorage for MemoryJournal {
    fn read_snapshot(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.snapshot.clone())
    }

    fn read_journal(&mut self) -> io::Result<Vec<u8>> {
        Ok(self.journal.clone())
    }

    fn append_journal(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.append_count += 1;
        if self.fail_append_at == Some(self.append_count) {
            return Err(io::Error::other("injected journal failure"));
        }
        self.journal.extend_from_slice(bytes);
        Ok(())
    }

    fn sync_journal(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn write_snapshot_temporary(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.temporary.clear();
        self.temporary.extend_from_slice(bytes);
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

struct Passwords(&'static str);

impl ArchivePasswordProvider for Passwords {
    fn request_password(
        &self,
        _request: &PasswordRequest,
    ) -> Result<Option<ArchivePassword>, ArchiveError> {
        Ok(Some(ArchivePassword::new(self.0.as_bytes().to_vec())))
    }
}

fn local(path: &Path) -> StorePath {
    StorePath::from_unix_path(path.as_os_str())
}

fn provider() -> ProviderSnapshot {
    ProviderSnapshot::new(
        ProviderId::new("archive-tests").expect("provider id"),
        CapabilityMatrix::new(|_| CapabilityState::Supported),
        ProviderLimits::unbounded(),
    )
}

fn identity(path: &Path) -> ArchivePathIdentity {
    let metadata = std::fs::symlink_metadata(path).expect("path identity");
    let mut hasher = blake3::Hasher::new();
    hash_test_path(path, &metadata, None, &mut hasher);
    ArchivePathIdentity::new(
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.is_dir(),
    )
    .with_content_digest(*hasher.finalize().as_bytes())
}

/// The identity digest from names and metadata, walked as the implementation
/// walks it: each item, then its children in name order. Every item below the
/// root adds its name, device, inode, and modification and change times.
fn hash_test_path(
    path: &Path,
    metadata: &std::fs::Metadata,
    name: Option<&[u8]>,
    hasher: &mut blake3::Hasher,
) {
    hasher.update(&metadata.mode().to_le_bytes());
    hasher.update(&metadata.len().to_le_bytes());
    if let Some(name) = name {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name);
        for value in [
            metadata.dev(),
            metadata.ino(),
            metadata.mtime() as u64,
            metadata.mtime_nsec() as u64,
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
        ] {
            hasher.update(&value.to_le_bytes());
        }
    }
    if metadata.is_file() {
        return;
    }
    let mut children = std::fs::read_dir(path)
        .expect("identity directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("identity children");
    children.sort_by(|left, right| {
        left.file_name()
            .as_bytes()
            .cmp(right.file_name().as_bytes())
    });
    for child in children {
        let metadata = std::fs::symlink_metadata(child.path()).expect("child identity");
        hash_test_path(
            &child.path(),
            &metadata,
            Some(child.file_name().as_bytes()),
            hasher,
        );
    }
}

const TEST_STAGE_NONCE: [u8; 16] = [0x5a; 16];

fn test_staging(root: &Path, job: u64) -> std::path::PathBuf {
    root.join(format!(".musheen-stage-v1-{job}-0-{}", "5a".repeat(16)))
}

fn live_staging(root: &Path) -> std::path::PathBuf {
    std::fs::read_dir(root)
        .expect("staging parent")
        .map(|entry| entry.expect("staging entry").path())
        .find(|path| {
            path.file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b".musheen-stage-v1-"))
        })
        .expect("live archive staging path")
}

fn run(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(ArchiveOperationOutcome, Vec<JournalPhase>), ArchiveOperationError> {
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    let id = scheduler
        .enqueue_archive(plan.clone(), provider())
        .expect("archive queues");
    let job = scheduler
        .start_ready()
        .expect("archive starts")
        .pop()
        .expect("archive job");
    if cancellation.is_cancelled() {
        scheduler.cancel(id).expect("archive cancels");
    }
    let outcome =
        execute_scheduled_archive_operation(&scheduler, &job, limits, passwords, &mut journal)?;
    let phases = journal
        .records()
        .iter()
        .map(|record| record.phase())
        .collect();
    Ok((outcome, phases))
}

fn run_accounted(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    accounting: &ArchiveOperationAccounting,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan.clone(), provider())
        .expect("archive queues");
    let job = scheduler
        .start_ready()
        .expect("archive starts")
        .pop()
        .expect("archive job");
    execute_scheduled_archive_operation_with_accounting(
        &scheduler,
        &job,
        limits,
        &Passwords("unused"),
        &mut journal,
        accounting,
    )
}

fn approved_recovery_action(phase: JournalPhase) -> ArchiveRecoveryAction {
    match phase {
        JournalPhase::DataCopied
        | JournalPhase::MetadataApplied
        | JournalPhase::DestinationQuarantinePlanned
        | JournalPhase::DestinationQuarantined
        | JournalPhase::StagePublishPlanned
        | JournalPhase::PrepublishStageCleanupPlanned
        | JournalPhase::PublishedDestinationCleanupPlanned
        | JournalPhase::DestinationPublished
        | JournalPhase::PublishRollbackPlanned
        | JournalPhase::PublishedPayloadQuarantined
        | JournalPhase::DestinationRestorePlanned
        | JournalPhase::DestinationRestored
        | JournalPhase::StageRestorePlanned
        | JournalPhase::PrepublishStageCleanupQuarantined
        | JournalPhase::PublishedDestinationCleanupQuarantined
        | JournalPhase::RecoveryRequired
        | JournalPhase::StagingCleaned => ArchiveRecoveryAction::Resume,
        JournalPhase::Planned | JournalPhase::StagingCreated => ArchiveRecoveryAction::Rollback,
        JournalPhase::SourceRemoved | JournalPhase::Completed | JournalPhase::RolledBack => {
            panic!("phase {phase:?} does not need archive recovery")
        }
    }
}

#[test]
fn zip_tar_and_seven_zip_round_trip_with_supported_encryption() {
    let root = tempdir().expect("temporary root");
    let input = root.path().join("input");
    std::fs::create_dir(&input).expect("input directory");
    std::fs::write(input.join("hello.txt"), b"archive payload").expect("input file");

    for (index, codec, encrypted, suffix) in [
        (1, ArchiveCodec::Zip, false, "zip"),
        (2, ArchiveCodec::Zip, true, "aes.zip"),
        (3, ArchiveCodec::TarGzip, false, "tar.gz"),
        (4, ArchiveCodec::TarZstd, false, "tar.zst"),
        (5, ArchiveCodec::SevenZip, false, "7z"),
        (6, ArchiveCodec::SevenZip, true, "aes.7z"),
    ] {
        let archive = root.path().join(format!("round-{index}.{suffix}"));
        let output = root.path().join(format!("output-{index}"));
        let create = ArchiveOperationPlan::create(
            vec![local(&input)],
            local(&archive),
            codec,
            ArchiveConflictPolicy::Fail,
            encrypted,
        )
        .expect("create plan");
        let (outcome, phases) = run(
            &create,
            &ArchiveOperationLimits::default(),
            &Passwords("correct horse"),
            &CancellationToken::new(),
        )
        .expect("archive creation");
        assert_eq!(outcome, ArchiveOperationOutcome::Published);
        assert_eq!(phases.last(), Some(&JournalPhase::Completed));

        let extract = ArchiveOperationPlan::extract(
            local(&archive),
            local(&output),
            codec,
            ArchiveConflictPolicy::Fail,
            encrypted,
        )
        .expect("extract plan");
        run(
            &extract,
            &ArchiveOperationLimits::default(),
            &Passwords("correct horse"),
            &CancellationToken::new(),
        )
        .expect("archive extraction");
        assert_eq!(
            std::fs::read(output.join("input/hello.txt")).expect("extracted payload"),
            b"archive payload"
        );
    }
}

#[test]
fn scheduled_executor_emits_archive_phases_through_the_job_event_stream() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("data.zip");
    std::fs::write(&source, b"payload").expect("source");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("archive plan");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    let id = scheduler.enqueue_archive(plan, provider()).expect("queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal");

    execute_scheduled_archive_operation(
        &scheduler,
        &job,
        &ArchiveOperationLimits::default(),
        &Passwords("unused"),
        &mut journal,
    )
    .expect("archive completes");

    assert_eq!(
        scheduler
            .events()
            .iter()
            .filter_map(|event| event.archive_phase_value())
            .collect::<Vec<_>>(),
        vec![
            musheen_ops::ArchiveEventPhase::Preflight,
            musheen_ops::ArchiveEventPhase::Staging,
            musheen_ops::ArchiveEventPhase::Encoding,
            musheen_ops::ArchiveEventPhase::Publishing,
            musheen_ops::ArchiveEventPhase::Cleaning,
        ]
    );
    assert_eq!(scheduler.state(id), Some(musheen_ops::JobState::Completed));
}

#[test]
fn direct_archive_executor_publishes_for_an_external_scheduler() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("direct.zip");
    std::fs::write(&source, b"payload").expect("source");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("archive plan");
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal");

    let outcome = execute_archive_plan(
        &plan,
        &ArchiveOperationLimits::default(),
        &Passwords("unused"),
        &CancellationToken::new(),
        &mut journal,
        JobId::new(71).unwrap(),
        EventGeneration::new(3),
    )
    .expect("archive completes");

    assert_eq!(outcome, ArchiveOperationOutcome::Published);
    assert!(destination.is_file());
}

#[test]
fn conflict_cancellation_bad_password_and_cleanup_are_safe() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let archive = root.path().join("data.zip");
    std::fs::write(&source, b"new").expect("source");
    std::fs::write(&archive, b"old").expect("existing destination");

    let fail = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&archive),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("fail plan");
    assert!(matches!(
        run(
            &fail,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
        ),
        Err(ArchiveOperationError::Conflict)
    ));

    let skip = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&archive),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Skip,
        false,
    )
    .expect("skip plan");
    assert_eq!(
        run(
            &skip,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
        )
        .expect("skip succeeds")
        .0,
        ArchiveOperationOutcome::Skipped
    );
    assert_eq!(std::fs::read(&archive).expect("old archive"), b"old");

    let replace = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&archive),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        true,
    )
    .expect("replace plan");
    run(
        &replace,
        &ArchiveOperationLimits::default(),
        &Passwords("right"),
        &CancellationToken::new(),
    )
    .expect("replace succeeds");

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let cancelled_target = root.path().join("cancelled.zip");
    let cancelled_plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&cancelled_target),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("cancel plan");
    assert!(matches!(
        run(
            &cancelled_plan,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &cancelled,
        ),
        Err(ArchiveOperationError::Cancelled)
    ));
    assert!(!cancelled_target.exists());

    let output = root.path().join("bad-password-output");
    let extract = ArchiveOperationPlan::extract(
        local(&archive),
        local(&output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        true,
    )
    .expect("extract plan");
    assert!(matches!(
        run(
            &extract,
            &ArchiveOperationLimits::default(),
            &Passwords("wrong"),
            &CancellationToken::new(),
        ),
        Err(ArchiveOperationError::InvalidPassword)
    ));
    assert!(!output.exists());
    assert!(
        std::fs::read_dir(root.path())
            .expect("root listing")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".musheen-stage-v1-"))
    );
}

#[test]
fn replace_aborts_when_destination_changes_at_the_publish_boundary() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("data.zip");
    std::fs::write(&source, b"new archive payload").expect("source");
    std::fs::write(&destination, b"original destination").expect("destination");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("replace plan");
    let raced_destination = destination.clone();
    let storage = HookJournal {
        inner: MemoryJournal::default(),
        hook_at: 4,
        hook: Some(Box::new(move || {
            std::fs::write(&raced_destination, b"concurrent replacement")
                .expect("concurrent write");
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");

    assert!(matches!(
        execute_scheduled_archive_operation(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
        ),
        Err(ArchiveOperationError::Conflict)
    ));
    assert_eq!(
        std::fs::read(&destination).expect("concurrent destination remains"),
        b"concurrent replacement"
    );
}

#[test]
fn publication_rejects_a_symlink_swapped_over_the_owned_stage() {
    for extract in [false, true] {
        let root = tempdir().expect("temporary root");
        let source = root
            .path()
            .join(if extract { "source.zip" } else { "source.txt" });
        let destination = root
            .path()
            .join(if extract { "output" } else { "data.zip" });
        if extract {
            std::fs::write(
                &source,
                zip_bytes(zip::CompressionMethod::Stored, b"payload"),
            )
            .expect("ZIP source");
        } else {
            std::fs::write(&source, b"payload").expect("source");
        }
        let plan = if extract {
            ArchiveOperationPlan::extract(
                local(&source),
                local(&destination),
                ArchiveCodec::Zip,
                ArchiveConflictPolicy::Fail,
                false,
            )
        } else {
            ArchiveOperationPlan::create(
                vec![local(&source)],
                local(&destination),
                ArchiveCodec::Zip,
                ArchiveConflictPolicy::Fail,
                false,
            )
        }
        .expect("archive plan");
        let stage_parent = root.path().to_path_buf();
        let moved = root.path().join("captured-owned-stage");
        let storage = HookJournal {
            inner: MemoryJournal::default(),
            hook_at: 4,
            hook: Some(Box::new(move || {
                let staging = live_staging(&stage_parent);
                std::fs::rename(&staging, &moved).expect("move owned stage aside");
                symlink("/tmp", &staging).expect("replace stage with symlink");
            })),
        };
        let mut journal = Journal::open(storage).expect("journal opens");
        let scheduler = Scheduler::new(&ResourceLimits::default());
        scheduler
            .enqueue_archive(plan, provider())
            .expect("archive queues");
        let job = scheduler.start_ready().expect("starts").pop().expect("job");

        assert!(matches!(
            execute_scheduled_archive_operation(
                &scheduler,
                &job,
                &ArchiveOperationLimits::default(),
                &Passwords("unused"),
                &mut journal,
            ),
            Err(ArchiveOperationError::RecoveryRequired)
                | Err(ArchiveOperationError::UnsupportedFileType)
        ));
        assert!(!destination.exists());
        assert!(
            std::fs::symlink_metadata(live_staging(root.path()))
                .expect("foreign symlink retained")
                .file_type()
                .is_symlink()
        );
    }
}

#[test]
fn cleanup_swap_retains_foreign_stage_and_never_journals_staging_cleaned() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("data.zip");
    std::fs::write(&source, b"replacement archive").expect("source");
    std::fs::write(&destination, b"old destination").expect("destination");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("replace plan");
    let stage_parent = root.path().to_path_buf();
    let displaced = root.path().join("displaced-destination");
    let storage = HookJournal {
        inner: MemoryJournal::default(),
        hook_at: 7,
        hook: Some(Box::new(move || {
            let staging = live_staging(&stage_parent);
            std::fs::rename(&staging, &displaced).expect("retain displaced destination");
            std::fs::write(&staging, b"foreign replacement").expect("foreign stage");
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");

    assert!(matches!(
        execute_scheduled_archive_operation(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
        ),
        Err(ArchiveOperationError::RecoveryRequired)
    ));
    assert_eq!(
        std::fs::read(live_staging(root.path())).expect("foreign stage remains"),
        b"foreign replacement"
    );
    assert!(matches!(
        journal.records().last().map(|record| record.phase()),
        Some(
            JournalPhase::DestinationQuarantinePlanned
                | JournalPhase::DestinationQuarantined
                | JournalPhase::StagePublishPlanned
                | JournalPhase::PublishRollbackPlanned
                | JournalPhase::PublishedPayloadQuarantined
                | JournalPhase::DestinationRestorePlanned
                | JournalPhase::DestinationRestored
                | JournalPhase::PrepublishStageCleanupPlanned
                | JournalPhase::PublishedDestinationCleanupPlanned
        )
    ));
}

#[test]
fn nested_staging_directory_swap_to_symlink_is_never_followed_or_published() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.zip");
    let destination = root.path().join("output");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).expect("outside directory");
    std::fs::write(outside.join("secret"), b"outside").expect("outside file");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("nested/file", zip::write::SimpleFileOptions::default())
        .expect("nested entry");
    zip.write_all(b"inside").expect("nested contents");
    std::fs::write(&source, zip.finish().expect("zip finish").into_inner()).expect("zip fixture");
    let plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("extract plan");
    let stage_parent = root.path().to_path_buf();
    let displaced = root.path().join("displaced-nested");
    let outside_for_hook = outside.clone();
    let storage = HookJournal {
        inner: MemoryJournal::default(),
        hook_at: 4,
        hook: Some(Box::new(move || {
            let stage = live_staging(&stage_parent);
            std::fs::rename(stage.join("nested"), &displaced).expect("move nested stage");
            symlink(&outside_for_hook, stage.join("nested")).expect("nested symlink swap");
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");

    assert!(matches!(
        execute_scheduled_archive_operation(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
        ),
        Err(ArchiveOperationError::UnsupportedFileType)
            | Err(ArchiveOperationError::RecoveryRequired)
    ));
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read(outside.join("secret")).expect("outside remains"),
        b"outside"
    );
}

#[test]
fn failed_cleanup_is_recovery_needed_and_not_a_terminal_rollback() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("data.zip");
    std::fs::write(&source, b"replacement archive").expect("source");
    std::fs::write(&destination, b"old destination").expect("destination");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("replace plan");
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    let accounting = ArchiveOperationAccounting::default();
    accounting.inject_cleanup_error(5);
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");
    let result = execute_scheduled_archive_operation_with_accounting(
        &scheduler,
        &job,
        &ArchiveOperationLimits::default(),
        &Passwords("unused"),
        &mut journal,
        &accounting,
    );

    assert!(matches!(
        result,
        Err(ArchiveOperationError::RecoveryRequired)
    ));
    assert_eq!(
        journal.records().last().map(|record| record.phase()),
        Some(JournalPhase::PublishedDestinationCleanupPlanned)
    );
    assert!(live_staging(root.path()).exists());
}

#[test]
fn post_publish_rollback_survives_stage_recreation_and_reports_injected_failure() {
    for inject_rollback_failure in [false, true] {
        let root = tempdir().expect("temporary root");
        let source = root.path().join("source.txt");
        let destination = root.path().join("data.zip");
        std::fs::write(&source, b"replacement archive").expect("source");
        std::fs::write(&destination, b"old destination").expect("destination");
        let plan = ArchiveOperationPlan::create(
            vec![local(&source)],
            local(&destination),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Replace,
            false,
        )
        .expect("replace plan");
        let accounting = ArchiveOperationAccounting::default();
        accounting.inject_post_publish_validation_failure();
        if inject_rollback_failure {
            accounting.inject_rollback_error(5);
        } else {
            accounting.inject_stage_recreation_during_rollback();
        }
        let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
        let scheduler = Scheduler::new(&ResourceLimits::default());
        scheduler
            .enqueue_archive(plan, provider())
            .expect("archive queues");
        let job = scheduler.start_ready().expect("starts").pop().expect("job");

        assert!(matches!(
            execute_scheduled_archive_operation_with_accounting(
                &scheduler,
                &job,
                &ArchiveOperationLimits::default(),
                &Passwords("unused"),
                &mut journal,
                &accounting,
            ),
            Err(ArchiveOperationError::RecoveryRequired)
        ));
        let final_phase = journal.records().last().map(|record| record.phase());
        assert!(
            matches!(
                final_phase,
                Some(
                    JournalPhase::DestinationPublished
                        | JournalPhase::StagePublishPlanned
                        | JournalPhase::PublishRollbackPlanned
                        | JournalPhase::PublishedPayloadQuarantined
                        | JournalPhase::DestinationRestorePlanned
                        | JournalPhase::DestinationRestored
                )
            ),
            "unexpected durable rollback phase: {final_phase:?}"
        );
        let checkpoint = journal.records()[0]
            .archive_checkpoint()
            .expect("checkpoint");
        let staging = checkpoint.staging().as_unix_path().expect("local stage");
        let cleanup = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
        let published_quarantine = checkpoint
            .publication_quarantine()
            .and_then(StorePath::as_unix_path)
            .expect("published quarantine");
        if inject_rollback_failure {
            assert_eq!(
                std::fs::read(&cleanup).expect("old destination is quarantined"),
                b"old destination"
            );
            assert_ne!(
                std::fs::read(&destination).expect("new archive remains discoverable"),
                b"old destination"
            );
        } else {
            assert_eq!(
                std::fs::read(&destination).expect("old destination restored"),
                b"old destination"
            );
            assert_eq!(
                std::fs::read(staging).expect("foreign stage remains"),
                b"foreign stage replacement"
            );
            assert!(
                published_quarantine.exists(),
                "published payload remains in its durable quarantine"
            );
            assert!(!cleanup.exists(), "old destination was restored");
        }
        let request = recover_archive_operations(&journal)
            .expect("recovery scan")
            .pop()
            .expect("recovery request");
        let action = if inject_rollback_failure {
            ArchiveRecoveryAction::Resume
        } else {
            ArchiveRecoveryAction::Rollback
        };
        apply_archive_recovery(&mut journal, &request, action)
            .expect("explicit recovery action succeeds");
        assert!(matches!(
            journal.records().last().map(|record| record.phase()),
            Some(JournalPhase::Completed | JournalPhase::RolledBack)
        ));
    }
}

#[test]
fn destination_identity_walk_is_budgeted_and_honors_midwalk_pause_cancel() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("existing-tree");
    std::fs::write(&source, b"payload").expect("source");
    std::fs::create_dir(&destination).expect("destination tree");
    for index in 0..2_000 {
        std::fs::write(destination.join(format!("entry-{index:04}")), b"").expect("tree entry");
    }
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("replace plan");
    let low_accounting = ArchiveOperationAccounting::default();
    let low_limits = ArchiveOperationLimits {
        max_memory_bytes: 32 * 1_024,
        ..ArchiveOperationLimits::default()
    };
    assert!(matches!(
        run_accounted(&plan, &low_limits, &low_accounting),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "memory bytes",
            ..
        })
    ));
    assert!(destination.is_dir());

    let accounting = ArchiveOperationAccounting::default();
    let worker_accounting = accounting.clone();
    let worker_plan = plan.clone();
    let worker_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_done_signal = std::sync::Arc::clone(&worker_done);
    let (control_sender, control_receiver) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let scheduler = Scheduler::new(&ResourceLimits::default());
        let id = scheduler
            .enqueue_archive(worker_plan, provider())
            .expect("archive queues");
        let job = scheduler.start_ready().expect("starts").pop().expect("job");
        control_sender
            .send((scheduler.clone(), id))
            .expect("send controls");
        let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
        let result = execute_scheduled_archive_operation_with_accounting(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
            &worker_accounting,
        );
        worker_done_signal.store(true, std::sync::atomic::Ordering::Release);
        result
    });
    let (scheduler, id) = control_receiver.recv().expect("receive controls");
    while accounting.counters().memory_bytes < 100 * 1_024
        && !worker_done.load(std::sync::atomic::Ordering::Acquire)
    {
        std::thread::yield_now();
    }
    assert!(!worker_done.load(std::sync::atomic::Ordering::Acquire));
    scheduler.pause(id).expect("pause during identity walk");
    assert_eq!(scheduler.state(id), Some(musheen_ops::JobState::Paused));
    scheduler.cancel(id).expect("cancel paused identity walk");
    assert!(matches!(
        worker.join().expect("worker joins"),
        Err(ArchiveOperationError::Cancelled)
    ));
    assert_eq!(scheduler.state(id), Some(musheen_ops::JobState::Cancelled));
    assert!(destination.is_dir());
}

#[test]
fn scheduler_pause_and_cancel_stop_archive_before_publication() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("data.zip");
    std::fs::write(&source, vec![b'x'; 1024 * 1024]).expect("source");
    std::fs::write(&destination, b"original destination").expect("destination");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("replace plan");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    let id = scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");
    let pause_control = scheduler.clone();
    let storage = HookJournal {
        inner: MemoryJournal::default(),
        hook_at: 4,
        hook: Some(Box::new(move || {
            pause_control.pause(id).expect("worker pauses");
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");
    let cancel_control = scheduler.clone();
    let cancel_destination = destination.clone();
    let canceller = std::thread::spawn(move || {
        while cancel_control.state(id) != Some(musheen_ops::JobState::Paused) {
            std::thread::yield_now();
        }
        assert_eq!(
            std::fs::read(&cancel_destination).expect("destination before cancel"),
            b"original destination"
        );
        cancel_control.cancel(id).expect("paused job cancels");
    });

    let result = execute_scheduled_archive_operation(
        &scheduler,
        &job,
        &ArchiveOperationLimits::default(),
        &Passwords("unused"),
        &mut journal,
    );
    assert!(
        matches!(result, Err(ArchiveOperationError::Cancelled)),
        "unexpected cancellation result: {result:?}"
    );
    canceller.join().expect("canceller finishes");
    assert_eq!(scheduler.state(id), Some(musheen_ops::JobState::Cancelled));
    assert_eq!(
        std::fs::read(&destination).expect("destination after cancel"),
        b"original destination"
    );
}

#[test]
fn cancellation_after_commit_admission_is_rejected_and_publication_completes() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("data.zip");
    std::fs::write(&source, b"committed payload").expect("source");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("create plan");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    let id = scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");
    let cancel_control = scheduler.clone();
    let storage = HookJournal {
        inner: MemoryJournal::default(),
        hook_at: 6,
        hook: Some(Box::new(move || {
            assert!(matches!(
                cancel_control.cancel(id),
                Err(musheen_ops::SchedulerError::CommitInProgress(job_id)) if job_id == id
            ));
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");

    assert_eq!(
        execute_scheduled_archive_operation(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
        )
        .expect("commit completes"),
        ArchiveOperationOutcome::Published
    );
    assert_eq!(scheduler.state(id), Some(musheen_ops::JobState::Completed));
    assert!(destination.exists());
}

#[test]
fn extraction_remains_bound_to_snapshot_after_source_rewrite() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.zip");
    let destination = root.path().join("output");
    std::fs::write(
        &source,
        zip_bytes(zip::CompressionMethod::Stored, b"payload"),
    )
    .expect("ZIP fixture");
    let plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("extract plan");
    let rewritten_source = source.clone();
    let storage = HookJournal {
        inner: MemoryJournal::default(),
        hook_at: 4,
        hook: Some(Box::new(move || {
            let length = std::fs::metadata(&rewritten_source)
                .expect("source metadata")
                .len() as usize;
            std::fs::write(&rewritten_source, vec![0x7f; length]).expect("source rewrite");
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");

    assert_eq!(
        execute_scheduled_archive_operation(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
        )
        .expect("owned source snapshot remains valid"),
        ArchiveOperationOutcome::Published
    );
    assert_eq!(
        std::fs::read(destination.join("x")).expect("snapshot payload"),
        b"payload"
    );
    assert_eq!(
        journal.records().last().map(|record| record.phase()),
        Some(JournalPhase::Completed)
    );
}

#[test]
fn extraction_publishes_nothing_when_the_source_is_rewritten_during_decode() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.zip");
    let destination = root.path().join("output");
    let original = zip_bytes(zip::CompressionMethod::Stored, b"stable snapshot payload");
    std::fs::write(&source, &original).expect("ZIP fixture");
    let plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("extract plan");
    let mutate = source.clone();
    let restore = source.clone();
    let storage = TwoHookJournal {
        inner: MemoryJournal::default(),
        first: Some(Box::new(move || {
            std::fs::write(&mutate, vec![0x7f; original.len()]).expect("transient rewrite");
        })),
        second: Some(Box::new(move || {
            std::fs::write(
                &restore,
                zip_bytes(zip::CompressionMethod::Stored, b"stable snapshot payload"),
            )
            .expect("restore source");
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");

    // The archive is read where it is (OPS-034), so a rewrite during the run
    // fails it, even one that restores the bytes, and nothing is published.
    let result = execute_scheduled_archive_operation(
        &scheduler,
        &job,
        &ArchiveOperationLimits::default(),
        &Passwords("unused"),
        &mut journal,
    );
    assert!(result.is_err(), "{result:?}");
    assert!(!destination.exists());
}

#[test]
fn kernel_enospc_is_mapped_at_the_archive_io_boundary() {
    let mut full = std::fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .expect("Linux /dev/full is available");
    let error = full
        .write_all(b"archive bytes")
        .expect_err("/dev/full returns ENOSPC");
    assert_eq!(error.raw_os_error(), Some(28));
    assert_eq!(
        ArchiveOperationError::from_io_error(&error),
        ArchiveOperationError::NoSpace
    );
}

#[test]
fn stage_write_enospc_is_journaled_and_cleans_the_unpublished_archive() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("data.zip");
    std::fs::write(&source, b"payload").expect("source");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("archive plan");
    let accounting = ArchiveOperationAccounting::default();
    accounting.inject_stage_write_error_after(0, 28);
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");

    assert_eq!(
        execute_scheduled_archive_operation_with_accounting(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
            &accounting,
        )
        .expect_err("stage write fails"),
        ArchiveOperationError::NoSpace
    );
    assert!(!destination.exists());
    assert_eq!(
        journal.records().last().map(|record| record.phase()),
        Some(JournalPhase::RolledBack)
    );
    assert!(
        std::fs::read_dir(root.path())
            .expect("root listing")
            .all(|entry| !entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .starts_with(".musheen-stage-v1-"))
    );
}

#[test]
fn journal_failures_clean_unpublished_staging_and_retain_replace_recovery_data() {
    let mut observed_success_after_all_boundaries = false;
    for fail_append_at in 1..=24 {
        let root = tempdir().expect("temporary root");
        let source = root.path().join("source.txt");
        let destination = root.path().join("data.zip");
        std::fs::write(&source, b"new payload").expect("source");
        std::fs::write(&destination, b"old destination").expect("old destination");
        let plan = ArchiveOperationPlan::create(
            vec![local(&source)],
            local(&destination),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Replace,
            false,
        )
        .expect("replace plan");
        let mut journal = Journal::open(MemoryJournal {
            fail_append_at: Some(fail_append_at),
            ..MemoryJournal::default()
        })
        .expect("journal opens");
        let scheduler = Scheduler::new(&ResourceLimits::default());
        let id = scheduler.enqueue_archive(plan, provider()).expect("queues");
        let job = scheduler.start_ready().expect("starts").pop().expect("job");
        assert_eq!(job.id(), id);
        let result = execute_scheduled_archive_operation(
            &scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
        );
        if result == Ok(ArchiveOperationOutcome::Published) {
            observed_success_after_all_boundaries = true;
            break;
        }
        assert!(
            matches!(
                result,
                Err(ArchiveOperationError::Journal | ArchiveOperationError::RecoveryRequired)
            ),
            "journal boundary {fail_append_at} returned {result:?}"
        );
        let staging = journal
            .records()
            .iter()
            .find_map(|record| record.archive_checkpoint())
            .and_then(|checkpoint| checkpoint.staging().as_unix_path())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.path().join("stage-was-never-recorded"));
        let cleanup = journal
            .records()
            .iter()
            .find_map(|record| record.archive_checkpoint())
            .and_then(|checkpoint| checkpoint.cleanup())
            .and_then(StorePath::as_unix_path)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| root.path().join("cleanup-was-never-recorded"));
        let storage = journal.into_storage();
        let mut reopened = Journal::open(storage).expect("journal reopens after crash point");
        let requests = recover_archive_operations(&reopened)
            .unwrap_or_else(|error| panic!("crash point {fail_append_at} scans: {error:?}"));
        for request in requests {
            let action = approved_recovery_action(request.phase());
            apply_archive_recovery(&mut reopened, &request, action)
                .unwrap_or_else(|error| panic!("crash point {fail_append_at} recovers: {error:?}"));
        }
        if fail_append_at == 1 {
            assert!(reopened.records().is_empty());
        } else {
            assert!(
                matches!(
                    reopened.records().last().map(|record| record.phase()),
                    Some(JournalPhase::RolledBack | JournalPhase::Completed)
                ),
                "journal append crash point {fail_append_at} did not reach a terminal phase"
            );
        }
        assert!(
            !staging.exists(),
            "boundary {fail_append_at}: stage cleaned"
        );
        assert!(
            !cleanup.exists(),
            "boundary {fail_append_at}: rollback cleaned (phase {:?}, bytes {:?})",
            reopened.records().last().map(|record| record.phase()),
            std::fs::read(&cleanup).ok()
        );
        assert!(
            destination.exists(),
            "boundary {fail_append_at}: destination exists"
        );
    }
    assert!(
        observed_success_after_all_boundaries,
        "the failure sweep did not pass the final journal boundary"
    );
}

#[test]
fn reopened_file_journal_rolls_back_or_completes_real_archive_checkpoints() {
    let root = tempdir().expect("temporary root");
    let journal_dir = root.path().join("journal");
    let source = root.path().join("source.txt");
    std::fs::write(&source, b"payload").expect("source");

    for (job_number, phase, should_publish) in [
        (69, JournalPhase::Planned, false),
        (70, JournalPhase::StagingCreated, false),
        (71, JournalPhase::DataCopied, true),
        (72, JournalPhase::MetadataApplied, true),
    ] {
        let destination = root.path().join(format!("recovered-{job_number}.zip"));
        let staging = test_staging(root.path(), job_number);
        if phase == JournalPhase::Planned {
            // A Planned checkpoint precedes creation of the owned stage.
        } else if phase == JournalPhase::StagingCreated {
            std::fs::write(&staging, b"").expect("empty stage");
        } else {
            std::fs::write(&staging, format!("staged-{job_number}")).expect("staged archive");
        }
        let plan = ArchiveOperationPlan::create(
            vec![local(&source)],
            local(&destination),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("recovery plan");
        let staging_identity = (phase != JournalPhase::Planned).then(|| identity(&staging));
        let checkpoint =
            ArchiveCheckpoint::new(plan, local(&staging), staging_identity, None, None)
                .with_staging_nonce(TEST_STAGE_NONCE);
        let job_id = JobId::new(job_number).expect("job id");
        {
            let storage = FileJournalStorage::at(&journal_dir).expect("file journal storage");
            let mut journal = Journal::open(storage).expect("file journal opens");
            journal
                .append_archive(
                    job_id,
                    EventGeneration::new(0),
                    phase,
                    Durability::CrashDurable,
                    checkpoint,
                )
                .expect("checkpoint is durable");
        }
        let storage = FileJournalStorage::at(&journal_dir).expect("reopened storage");
        let mut reopened = Journal::open(storage).expect("journal reopens");
        let requests = recover_archive_operations(&reopened).expect("recovery scan succeeds");
        assert_eq!(requests.len(), 1);
        assert_eq!(
            staging.exists(),
            phase != JournalPhase::Planned,
            "scan does not mutate staging"
        );
        assert!(!destination.exists(), "scan does not publish");
        let action = approved_recovery_action(requests[0].phase());
        apply_archive_recovery(&mut reopened, &requests[0], action)
            .expect("approved recovery succeeds");
        assert_eq!(destination.exists(), should_publish);
        assert!(!staging.exists());
        assert_eq!(
            reopened.records().last().map(|record| record.phase()),
            Some(if should_publish {
                JournalPhase::Completed
            } else {
                JournalPhase::RolledBack
            })
        );
    }
}

#[test]
fn recovery_never_publishes_a_merged_extraction_over_the_folder_it_merges_into() {
    let root = tempdir().expect("temporary root");
    let archive = root.path().join("Archive.zip");
    std::fs::write(&archive, b"archive").expect("archive");
    let destination = root.path().join("Archive");
    std::fs::create_dir(&destination).expect("existing folder");
    std::fs::write(destination.join("keep.txt"), b"keep").expect("kept item");
    let staging = test_staging(root.path(), 76);
    std::fs::create_dir(&staging).expect("staging folder");
    std::fs::write(staging.join("a.txt"), b"new a").expect("staged entry");
    let plan = ArchiveOperationPlan::extract(
        local(&archive),
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .and_then(|plan| plan.with_merge(ExtractMerge::default()))
    .expect("merge plan");
    let checkpoint = ArchiveCheckpoint::new(
        plan,
        local(&staging),
        Some(identity(&staging)),
        Some(identity(&destination)),
        None,
    )
    .with_staging_nonce(TEST_STAGE_NONCE);
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal");
    journal
        .append_archive(
            JobId::new(76).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::MetadataApplied,
            Durability::CrashDurable,
            checkpoint,
        )
        .expect("checkpoint");
    let requests = recover_archive_operations(&journal).expect("recovery scan");
    assert_eq!(requests.len(), 1);

    assert!(
        apply_archive_recovery(&mut journal, &requests[0], ArchiveRecoveryAction::Resume).is_err(),
        "a merge interrupted before it ends is never published as a whole folder"
    );
    assert_eq!(
        std::fs::read(destination.join("keep.txt")).expect("the folder keeps its items"),
        b"keep"
    );
    assert!(!destination.join("a.txt").exists());
}

#[test]
fn recovery_requires_explicit_consent_before_data_copied_mutation() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("recovered.zip");
    let staging = test_staging(root.path(), 74);
    std::fs::write(&source, b"source").expect("source");
    std::fs::write(&staging, b"complete staged archive").expect("staging");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("recovery plan");
    let checkpoint =
        ArchiveCheckpoint::new(plan, local(&staging), Some(identity(&staging)), None, None)
            .with_staging_nonce(TEST_STAGE_NONCE);
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    journal
        .append_archive(
            JobId::new(74).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::DataCopied,
            Durability::CrashDurable,
            checkpoint,
        )
        .expect("checkpoint persists");

    let requests = recover_archive_operations(&journal).expect("recovery scan succeeds");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].phase(), JournalPhase::DataCopied);
    assert!(staging.exists(), "a scan must not remove staged data");
    assert!(!destination.exists(), "a scan must not publish staged data");
    assert_eq!(journal.records().len(), 1, "a scan must not append records");

    assert_eq!(
        apply_archive_recovery(&mut journal, &requests[0], ArchiveRecoveryAction::Resume,)
            .expect("approved recovery resumes"),
        musheen_desktop::ArchiveRecoveryOutcome::Completed
    );
    assert!(destination.exists());
    assert!(!staging.exists());
}

#[test]
fn planned_recovery_never_deletes_an_object_that_appears_at_the_stage_path() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("recovered.zip");
    let staging = test_staging(root.path(), 75);
    std::fs::write(&source, b"source").expect("source");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("recovery plan");
    let checkpoint = ArchiveCheckpoint::new(plan, local(&staging), None, None, None)
        .with_staging_nonce(TEST_STAGE_NONCE);
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    journal
        .append_archive(
            JobId::new(75).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::Planned,
            Durability::CrashDurable,
            checkpoint,
        )
        .expect("checkpoint persists");
    std::fs::write(&staging, b"unrelated replacement").expect("replacement");

    let requests = recover_archive_operations(&journal).expect("recovery scan succeeds");
    apply_archive_recovery(&mut journal, &requests[0], ArchiveRecoveryAction::Rollback)
        .expect("approved rollback records completion");
    assert_eq!(
        std::fs::read(&staging).expect("replacement remains"),
        b"unrelated replacement"
    );
}

#[test]
fn recovery_refuses_to_delete_a_staging_tree_after_child_mutation() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.zip");
    let destination = root.path().join("output");
    let staging = test_staging(root.path(), 76);
    std::fs::write(&source, b"source").expect("source");
    std::fs::create_dir(&staging).expect("staging directory");
    let child = staging.join("x");
    std::fs::write(&child, b"first").expect("staged child");
    let plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("recovery plan");
    let checkpoint =
        ArchiveCheckpoint::new(plan, local(&staging), Some(identity(&staging)), None, None)
            .with_staging_nonce(TEST_STAGE_NONCE);
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    journal
        .append_archive(
            JobId::new(76).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::StagingCreated,
            Durability::CrashDurable,
            checkpoint,
        )
        .expect("checkpoint persists");
    std::fs::write(&child, b"other").expect("child mutation");

    let requests = recover_archive_operations(&journal).expect("recovery scan succeeds");
    assert_eq!(
        requests[0].recommended(),
        musheen_ops::RecoveryDecision::Ask
    );
    assert!(matches!(
        apply_archive_recovery(&mut journal, &requests[0], ArchiveRecoveryAction::Rollback,),
        Err(ArchiveOperationError::UnsafePath(_))
    ));
    assert_eq!(
        std::fs::read(&child).expect("mutated child remains"),
        b"other"
    );
}

#[test]
fn recovery_identity_walk_uses_persisted_memory_limit_and_cancellation() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.zip");
    let destination = root.path().join("output");
    let staging = test_staging(root.path(), 77);
    std::fs::write(&source, b"source").expect("source");
    std::fs::create_dir(&staging).expect("staging directory");
    for index in 0..128 {
        std::fs::write(
            staging.join(format!("entry-{index:04}-{}", "x".repeat(96))),
            b"x",
        )
        .expect("staging child");
    }
    let plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("recovery plan");
    let checkpoint =
        ArchiveCheckpoint::new(plan, local(&staging), Some(identity(&staging)), None, None)
            .with_staging_nonce(TEST_STAGE_NONCE)
            .with_identity_memory_limit(4 * 1_024);
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    journal
        .append_archive(
            JobId::new(77).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::StagingCreated,
            Durability::CrashDurable,
            checkpoint,
        )
        .expect("checkpoint persists");

    assert!(matches!(
        recover_archive_operations(&journal),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "memory bytes",
            ..
        })
    ));

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        recover_archive_operations_with_cancellation(&journal, &cancellation),
        Err(ArchiveOperationError::Cancelled)
    ));

    let normal_checkpoint = ArchiveCheckpoint::new(
        journal.records()[0]
            .archive_checkpoint()
            .expect("archive checkpoint")
            .plan()
            .clone(),
        local(&staging),
        Some(identity(&staging)),
        None,
        None,
    )
    .with_staging_nonce(TEST_STAGE_NONCE);
    let mut normal = Journal::open(MemoryJournal::default()).expect("normal journal");
    normal
        .append_archive(
            JobId::new(77).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::StagingCreated,
            Durability::CrashDurable,
            normal_checkpoint,
        )
        .expect("normal checkpoint persists");
    let request = recover_archive_operations(&normal)
        .expect("normal recovery scan")
        .pop()
        .expect("recovery request");
    assert!(matches!(
        apply_archive_recovery_with_cancellation(
            &mut normal,
            &request,
            ArchiveRecoveryAction::Rollback,
            &cancellation,
        ),
        Err(ArchiveOperationError::Cancelled)
    ));
    assert!(
        staging.exists(),
        "cancelled recovery must not mutate staging"
    );
}

#[test]
fn prepublish_cleanup_recovers_every_topology_without_touching_foreign_collisions() {
    for (crash_point, phase) in [
        (
            "planned-before-rename",
            JournalPhase::PrepublishStageCleanupPlanned,
        ),
        (
            "planned-after-rename",
            JournalPhase::PrepublishStageCleanupPlanned,
        ),
        (
            "quarantined-before-delete",
            JournalPhase::PrepublishStageCleanupQuarantined,
        ),
        (
            "quarantined-after-delete",
            JournalPhase::PrepublishStageCleanupQuarantined,
        ),
    ] {
        for destination_exists in [false, true] {
            for action in [
                ArchiveRecoveryAction::Resume,
                ArchiveRecoveryAction::Rollback,
            ] {
                let root = tempdir().expect("temporary root");
                let source = root.path().join("source.txt");
                let destination = root.path().join("published.zip");
                let staging = test_staging(root.path(), 79);
                let rollback = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
                let published =
                    Path::new(&format!("{}.published", staging.display())).to_path_buf();
                let deletion = Path::new(&format!("{}.delete", staging.display())).to_path_buf();
                std::fs::write(&source, b"source").expect("source");
                std::fs::write(&staging, b"owned stage").expect("stage");
                std::fs::write(&rollback, b"foreign rollback").expect("foreign rollback");
                std::fs::write(&published, b"foreign published").expect("foreign published");
                if destination_exists {
                    std::fs::write(&destination, b"existing destination").expect("destination");
                }
                let staging_identity = identity(&staging);
                let destination_before = destination_exists.then(|| identity(&destination));
                match crash_point {
                    "planned-before-rename" => {}
                    "planned-after-rename" | "quarantined-before-delete" => {
                        std::fs::rename(&staging, &deletion).expect("stage quarantine");
                    }
                    "quarantined-after-delete" => {
                        std::fs::rename(&staging, &deletion).expect("stage quarantine");
                        std::fs::remove_file(&deletion).expect("stage delete");
                    }
                    _ => unreachable!(),
                }
                let plan = ArchiveOperationPlan::create(
                    vec![local(&source)],
                    local(&destination),
                    ArchiveCodec::Zip,
                    ArchiveConflictPolicy::Replace,
                    false,
                )
                .expect("recovery plan");
                let checkpoint = ArchiveCheckpoint::new(
                    plan,
                    local(&staging),
                    Some(staging_identity),
                    destination_before,
                    None,
                )
                .with_staging_nonce(TEST_STAGE_NONCE)
                .with_cleanup_intent(
                    ArchiveCleanupKind::PrepublishStage,
                    local(&staging),
                    local(&deletion),
                    Some(staging_identity),
                )
                .with_publication_quarantine(local(&published));
                let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
                journal
                    .append_archive(
                        JobId::new(79).expect("job id"),
                        EventGeneration::new(0),
                        phase,
                        Durability::CrashDurable,
                        checkpoint,
                    )
                    .expect("cleanup intent persists");

                let request = recover_archive_operations(&journal)
                    .expect("recovery scans")
                    .pop()
                    .expect("recovery request");
                apply_archive_recovery(&mut journal, &request, action).unwrap_or_else(|error| {
                    panic!(
                        "{crash_point} destination={destination_exists} {action:?} recovers: {error:?}"
                    )
                });
                assert!(!staging.exists(), "{crash_point}: stage removed");
                assert!(!deletion.exists(), "{crash_point}: quarantine removed");
                assert_eq!(
                    std::fs::read(&rollback).expect("rollback remains"),
                    b"foreign rollback"
                );
                assert_eq!(
                    std::fs::read(&published).expect("published remains"),
                    b"foreign published"
                );
                assert_eq!(destination.exists(), destination_exists);
                assert_eq!(
                    journal.records().last().map(|record| record.phase()),
                    Some(JournalPhase::RolledBack)
                );
            }
        }
    }
}

#[test]
fn prepublish_cleanup_rejects_an_occupied_exact_quarantine_for_both_actions() {
    for action in [
        ArchiveRecoveryAction::Resume,
        ArchiveRecoveryAction::Rollback,
    ] {
        let root = tempdir().expect("temporary root");
        let source = root.path().join("source.txt");
        let destination = root.path().join("archive.zip");
        let staging = test_staging(root.path(), 81);
        let deletion = Path::new(&format!("{}.delete", staging.display())).to_path_buf();
        std::fs::write(&source, b"source").expect("source");
        std::fs::write(&staging, b"owned stage").expect("stage");
        std::fs::write(&deletion, b"foreign quarantine").expect("foreign quarantine");
        let staging_identity = identity(&staging);
        let plan = ArchiveOperationPlan::create(
            vec![local(&source)],
            local(&destination),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Replace,
            false,
        )
        .expect("archive plan");
        let checkpoint =
            ArchiveCheckpoint::new(plan, local(&staging), Some(staging_identity), None, None)
                .with_staging_nonce(TEST_STAGE_NONCE)
                .with_cleanup_intent(
                    ArchiveCleanupKind::PrepublishStage,
                    local(&staging),
                    local(&deletion),
                    Some(staging_identity),
                );
        let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
        journal
            .append_archive(
                JobId::new(81).expect("job id"),
                EventGeneration::new(0),
                JournalPhase::PrepublishStageCleanupPlanned,
                Durability::CrashDurable,
                checkpoint,
            )
            .expect("checkpoint persists");

        let request = recover_archive_operations(&journal)
            .expect("recovery scans")
            .pop()
            .expect("recovery request");
        assert!(apply_archive_recovery(&mut journal, &request, action).is_err());
        assert_eq!(
            std::fs::read(&staging).expect("stage remains"),
            b"owned stage"
        );
        assert_eq!(
            std::fs::read(&deletion).expect("foreign quarantine remains"),
            b"foreign quarantine"
        );
        assert_eq!(
            journal.records().last().map(|record| record.phase()),
            Some(JournalPhase::PrepublishStageCleanupPlanned)
        );
    }
}

#[test]
fn published_destination_cleanup_has_distinct_resume_and_rollback_semantics() {
    for (crash_point, phase) in [
        (
            "planned-before-rename",
            JournalPhase::PublishedDestinationCleanupPlanned,
        ),
        (
            "planned-after-rename",
            JournalPhase::PublishedDestinationCleanupPlanned,
        ),
        (
            "quarantined-before-delete",
            JournalPhase::PublishedDestinationCleanupQuarantined,
        ),
        (
            "quarantined-after-delete",
            JournalPhase::PublishedDestinationCleanupQuarantined,
        ),
    ] {
        for action in [
            ArchiveRecoveryAction::Resume,
            ArchiveRecoveryAction::Rollback,
        ] {
            let root = tempdir().expect("temporary root");
            let source = root.path().join("source.txt");
            let destination = root.path().join("archive.zip");
            let staging = test_staging(root.path(), 82);
            let rollback = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
            let deletion = Path::new(&format!("{}.delete", rollback.display())).to_path_buf();
            let published = Path::new(&format!("{}.published", staging.display())).to_path_buf();
            std::fs::write(&source, b"source").expect("source");
            std::fs::write(&destination, b"new archive").expect("new destination");
            std::fs::write(&rollback, b"old archive").expect("old destination");
            let new_identity = identity(&destination);
            let old_identity = identity(&rollback);
            match crash_point {
                "planned-before-rename" => {}
                "planned-after-rename" | "quarantined-before-delete" => {
                    std::fs::rename(&rollback, &deletion).expect("cleanup quarantine");
                }
                "quarantined-after-delete" => {
                    std::fs::remove_file(&rollback).expect("cleanup delete");
                }
                _ => unreachable!(),
            }
            let plan = ArchiveOperationPlan::create(
                vec![local(&source)],
                local(&destination),
                ArchiveCodec::Zip,
                ArchiveConflictPolicy::Replace,
                false,
            )
            .expect("archive plan");
            let checkpoint = ArchiveCheckpoint::new(
                plan,
                local(&staging),
                Some(new_identity),
                Some(old_identity),
                Some(new_identity),
            )
            .with_staging_nonce(TEST_STAGE_NONCE)
            .with_cleanup_intent(
                ArchiveCleanupKind::PublishedDestination,
                local(&rollback),
                local(&deletion),
                Some(old_identity),
            )
            .with_publication_quarantine(local(&published));
            let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
            journal
                .append_archive(
                    JobId::new(82).expect("job id"),
                    EventGeneration::new(0),
                    phase,
                    Durability::CrashDurable,
                    checkpoint,
                )
                .expect("checkpoint persists");
            let request = recover_archive_operations(&journal)
                .expect("recovery scans")
                .pop()
                .expect("recovery request");

            if crash_point == "quarantined-after-delete"
                && action == ArchiveRecoveryAction::Rollback
            {
                assert!(apply_archive_recovery(&mut journal, &request, action).is_err());
                assert_eq!(
                    std::fs::read(&destination).expect("new destination remains"),
                    b"new archive"
                );
                assert!(!published.exists(), "new payload was not moved");
                continue;
            }
            apply_archive_recovery(&mut journal, &request, action)
                .unwrap_or_else(|error| panic!("{crash_point} {action:?} recovers: {error:?}"));
            let (contents, terminal) = match action {
                ArchiveRecoveryAction::Resume => {
                    (b"new archive".as_slice(), JournalPhase::Completed)
                }
                ArchiveRecoveryAction::Rollback => {
                    (b"old archive".as_slice(), JournalPhase::RolledBack)
                }
            };
            assert_eq!(std::fs::read(&destination).expect("destination"), contents);
            assert_eq!(
                journal.records().last().map(|record| record.phase()),
                Some(terminal)
            );
        }
    }
}

#[test]
fn cleanup_rollback_subphases_recover_old_destination_from_exact_quarantine() {
    #[derive(Clone, Copy, Debug)]
    enum Topology {
        NewPublished,
        PayloadQuarantined,
        OldRestored,
    }

    let cases = [
        (JournalPhase::PublishRollbackPlanned, Topology::NewPublished),
        (
            JournalPhase::PublishRollbackPlanned,
            Topology::PayloadQuarantined,
        ),
        (
            JournalPhase::PublishedPayloadQuarantined,
            Topology::PayloadQuarantined,
        ),
        (
            JournalPhase::DestinationRestorePlanned,
            Topology::PayloadQuarantined,
        ),
        (
            JournalPhase::DestinationRestorePlanned,
            Topology::OldRestored,
        ),
        (JournalPhase::DestinationRestored, Topology::OldRestored),
    ];

    for (case_index, (phase, topology)) in cases.into_iter().enumerate() {
        for action in [
            ArchiveRecoveryAction::Resume,
            ArchiveRecoveryAction::Rollback,
        ] {
            let root = tempdir().expect("temporary root");
            let source = root.path().join("source.txt");
            let destination = root.path().join("archive.zip");
            let staging = test_staging(root.path(), 820 + case_index as u64);
            let rollback = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
            let deletion = Path::new(&format!("{}.delete", rollback.display())).to_path_buf();
            let published = Path::new(&format!("{}.published", staging.display())).to_path_buf();
            std::fs::write(&source, b"source").expect("source");
            std::fs::write(&staging, b"foreign stage").expect("foreign stage");
            std::fs::write(&deletion, b"old archive").expect("old cleanup quarantine");
            std::fs::write(&destination, b"new archive").expect("new destination");
            let old_identity = identity(&deletion);
            let new_identity = identity(&destination);
            match topology {
                Topology::NewPublished => {}
                Topology::PayloadQuarantined => {
                    std::fs::rename(&destination, &published).expect("payload quarantine");
                }
                Topology::OldRestored => {
                    std::fs::rename(&destination, &published).expect("payload quarantine");
                    std::fs::rename(&deletion, &destination).expect("restore old destination");
                }
            }
            let plan = ArchiveOperationPlan::create(
                vec![local(&source)],
                local(&destination),
                ArchiveCodec::Zip,
                ArchiveConflictPolicy::Replace,
                false,
            )
            .expect("archive plan");
            let checkpoint = ArchiveCheckpoint::new(
                plan,
                local(&staging),
                Some(new_identity),
                Some(old_identity),
                None,
            )
            .with_staging_nonce(TEST_STAGE_NONCE)
            .with_cleanup_intent(
                ArchiveCleanupKind::PublishedDestination,
                local(&rollback),
                local(&deletion),
                Some(old_identity),
            )
            .with_publication_quarantine(local(&published));
            let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
            journal
                .append_archive(
                    JobId::new(820 + case_index as u64).expect("job id"),
                    EventGeneration::new(0),
                    phase,
                    Durability::CrashDurable,
                    checkpoint,
                )
                .expect("checkpoint persists");
            let request = recover_archive_operations(&journal)
                .expect("recovery scans")
                .pop()
                .expect("recovery request");

            apply_archive_recovery(&mut journal, &request, action).unwrap_or_else(|error| {
                panic!("{phase:?} {topology:?} {action:?} recovers: {error:?}")
            });

            let (expected, terminal) = match action {
                ArchiveRecoveryAction::Resume => {
                    (b"new archive".as_slice(), JournalPhase::Completed)
                }
                ArchiveRecoveryAction::Rollback => {
                    (b"old archive".as_slice(), JournalPhase::RolledBack)
                }
            };
            assert_eq!(
                std::fs::read(&destination).expect("destination exists"),
                expected,
                "{phase:?} {topology:?} {action:?}"
            );
            assert_eq!(
                std::fs::read(&staging).expect("foreign stage remains"),
                b"foreign stage"
            );
            assert!(!rollback.exists(), "rollback source is absent");
            assert!(!deletion.exists(), "rollback quarantine is consumed");
            assert!(!published.exists(), "payload quarantine is consumed");
            assert_eq!(
                journal.records().last().map(|record| record.phase()),
                Some(terminal)
            );
        }
    }
}

#[test]
fn recovery_handles_published_payload_with_a_foreign_recreated_stage_for_both_actions() {
    for action in [
        ArchiveRecoveryAction::Resume,
        ArchiveRecoveryAction::Rollback,
    ] {
        let root = tempdir().expect("temporary root");
        let source = root.path().join("source.txt");
        let destination = root.path().join("archive.zip");
        let staging = test_staging(root.path(), 80);
        let rollback = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
        let rollback_deletion = Path::new(&format!("{}.delete", rollback.display())).to_path_buf();
        let published = Path::new(&format!("{}.published", staging.display())).to_path_buf();
        std::fs::write(&source, b"source").expect("source");
        std::fs::write(&destination, b"old archive").expect("destination");
        std::fs::write(&published, b"new archive").expect("published quarantine");
        let old_identity = identity(&destination);
        let new_identity = identity(&published);
        std::fs::write(&staging, b"foreign stage").expect("foreign stage");
        let plan = ArchiveOperationPlan::create(
            vec![local(&source)],
            local(&destination),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Replace,
            false,
        )
        .expect("archive plan");
        let checkpoint = ArchiveCheckpoint::new(
            plan,
            local(&staging),
            Some(new_identity),
            Some(old_identity),
            Some(old_identity),
        )
        .with_staging_nonce(TEST_STAGE_NONCE)
        .with_cleanup_intent(
            ArchiveCleanupKind::PublishedDestination,
            local(&rollback),
            local(&rollback_deletion),
            Some(old_identity),
        )
        .with_publication_quarantine(local(&published));
        let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
        journal
            .append_archive(
                JobId::new(80).expect("job id"),
                EventGeneration::new(0),
                JournalPhase::DestinationRestored,
                Durability::CrashDurable,
                checkpoint,
            )
            .expect("checkpoint persists");

        let request = recover_archive_operations(&journal)
            .expect("recovery scans")
            .pop()
            .expect("recovery request");
        apply_archive_recovery(&mut journal, &request, action)
            .unwrap_or_else(|error| panic!("{action:?} recovers: {error:?}"));

        assert_eq!(
            std::fs::read(&staging).expect("foreign stage remains"),
            b"foreign stage"
        );
        assert!(!published.exists(), "owned quarantine is consumed");
        assert!(!rollback.exists(), "rollback source is consumed");
        assert!(
            !rollback_deletion.exists(),
            "rollback quarantine is consumed"
        );
        let (expected_contents, expected_phase) = match action {
            ArchiveRecoveryAction::Resume => (b"new archive".as_slice(), JournalPhase::Completed),
            ArchiveRecoveryAction::Rollback => {
                (b"old archive".as_slice(), JournalPhase::RolledBack)
            }
        };
        assert_eq!(
            std::fs::read(&destination).expect("destination"),
            expected_contents
        );
        assert_eq!(
            journal.records().last().map(|record| record.phase()),
            Some(expected_phase)
        );
    }
}

#[test]
fn foreign_stage_recovery_uses_the_persisted_payload_quarantine_for_validation_rollback() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let destination = root.path().join("archive.zip");
    let staging = test_staging(root.path(), 880);
    let rollback = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
    let rollback_deletion = Path::new(&format!("{}.delete", rollback.display())).to_path_buf();
    let published = Path::new(&format!("{}.published", staging.display())).to_path_buf();
    let derived_nested = Path::new(&format!("{}.published", published.display())).to_path_buf();
    std::fs::write(&source, b"source").expect("source");
    std::fs::write(&destination, b"old archive").expect("old destination");
    std::fs::write(&published, b"new archive").expect("owned payload quarantine");
    let old_identity = identity(&destination);
    let new_identity = identity(&published);
    std::fs::write(&staging, b"foreign stage").expect("foreign stage");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Replace,
        false,
    )
    .expect("archive plan");
    let checkpoint = ArchiveCheckpoint::new(
        plan,
        local(&staging),
        Some(new_identity),
        Some(old_identity),
        Some(old_identity),
    )
    .with_staging_nonce(TEST_STAGE_NONCE)
    .with_cleanup_intent(
        ArchiveCleanupKind::PublishedDestination,
        local(&rollback),
        local(&rollback_deletion),
        Some(old_identity),
    )
    .with_publication_quarantine(local(&published));
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    journal
        .append_archive(
            JobId::new(880).expect("job id"),
            EventGeneration::new(0),
            JournalPhase::DestinationRestored,
            Durability::CrashDurable,
            checkpoint,
        )
        .expect("checkpoint persists");
    let request = recover_archive_operations(&journal)
        .expect("recovery scans")
        .pop()
        .expect("recovery request");
    let accounting = ArchiveOperationAccounting::default();
    accounting.inject_post_publish_validation_failure();

    assert!(matches!(
        apply_archive_recovery_with_accounting(
            &mut journal,
            &request,
            ArchiveRecoveryAction::Resume,
            &CancellationToken::new(),
            &accounting,
        ),
        Err(ArchiveOperationError::RecoveryRequired)
    ));
    assert!(!derived_nested.exists(), "no unjournaled nested quarantine");
    assert_eq!(
        std::fs::read(&staging).expect("foreign stage remains"),
        b"foreign stage"
    );
    assert_eq!(
        std::fs::read(&destination).expect("old destination restored"),
        b"old archive"
    );
    assert_eq!(
        std::fs::read(&published).expect("owned payload remains recoverable"),
        b"new archive"
    );

    let request = recover_archive_operations(&journal)
        .expect("recovery rescans")
        .pop()
        .expect("recovery request");
    assert_eq!(
        apply_archive_recovery(&mut journal, &request, ArchiveRecoveryAction::Resume)
            .expect("owned payload resumes from the exact quarantine"),
        musheen_desktop::ArchiveRecoveryOutcome::Completed
    );
    assert_eq!(
        std::fs::read(&destination).expect("new destination published"),
        b"new archive"
    );
    assert_eq!(
        std::fs::read(&staging).expect("foreign stage remains"),
        b"foreign stage"
    );
    assert!(!published.exists(), "owned quarantine is consumed");
    assert!(!derived_nested.exists(), "derived quarantine never appears");
}

#[test]
fn foreign_stage_publication_recovers_every_validation_rollback_checkpoint() {
    for fail_append_at in 2..=8 {
        for action in [
            ArchiveRecoveryAction::Resume,
            ArchiveRecoveryAction::Rollback,
        ] {
            let root = tempdir().expect("temporary root");
            let source = root.path().join("source.txt");
            let destination = root.path().join("archive.zip");
            let staging = test_staging(root.path(), 900 + fail_append_at as u64);
            let rollback = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
            let rollback_deletion =
                Path::new(&format!("{}.delete", rollback.display())).to_path_buf();
            let published = Path::new(&format!("{}.published", staging.display())).to_path_buf();
            let derived_nested =
                Path::new(&format!("{}.published", published.display())).to_path_buf();
            std::fs::write(&source, b"source").expect("source");
            std::fs::write(&destination, b"old archive").expect("old destination");
            std::fs::write(&published, b"new archive").expect("owned payload quarantine");
            let old_identity = identity(&destination);
            let new_identity = identity(&published);
            std::fs::write(&staging, b"foreign stage").expect("foreign stage");
            let plan = ArchiveOperationPlan::create(
                vec![local(&source)],
                local(&destination),
                ArchiveCodec::Zip,
                ArchiveConflictPolicy::Replace,
                false,
            )
            .expect("archive plan");
            let checkpoint = ArchiveCheckpoint::new(
                plan,
                local(&staging),
                Some(new_identity),
                Some(old_identity),
                Some(old_identity),
            )
            .with_staging_nonce(TEST_STAGE_NONCE)
            .with_cleanup_intent(
                ArchiveCleanupKind::PublishedDestination,
                local(&rollback),
                local(&rollback_deletion),
                Some(old_identity),
            )
            .with_publication_quarantine(local(&published));
            let storage = MemoryJournal {
                fail_append_at: Some(fail_append_at),
                ..MemoryJournal::default()
            };
            let mut journal = Journal::open(storage).expect("journal opens");
            journal
                .append_archive(
                    JobId::new(900 + fail_append_at as u64).expect("job id"),
                    EventGeneration::new(0),
                    JournalPhase::DestinationRestored,
                    Durability::CrashDurable,
                    checkpoint,
                )
                .expect("checkpoint persists");
            let request = recover_archive_operations(&journal)
                .expect("recovery scans")
                .pop()
                .expect("recovery request");
            let accounting = ArchiveOperationAccounting::default();
            accounting.inject_post_publish_validation_failure();
            assert!(
                apply_archive_recovery_with_accounting(
                    &mut journal,
                    &request,
                    ArchiveRecoveryAction::Resume,
                    &CancellationToken::new(),
                    &accounting,
                )
                .is_err()
            );

            let mut storage = journal.into_storage();
            storage.fail_append_at = None;
            let mut reopened = Journal::open(storage).expect("journal reopens");
            let request = recover_archive_operations(&reopened)
                .unwrap_or_else(|error| panic!("append {fail_append_at} scans: {error:?}"))
                .pop()
                .expect("recovery request");
            apply_archive_recovery(&mut reopened, &request, action).unwrap_or_else(|error| {
                panic!("append {fail_append_at} {action:?} recovers: {error:?}")
            });

            let expected = match action {
                ArchiveRecoveryAction::Resume => b"new archive".as_slice(),
                ArchiveRecoveryAction::Rollback => b"old archive".as_slice(),
            };
            assert_eq!(
                std::fs::read(&destination).expect("destination exists"),
                expected,
                "append {fail_append_at} {action:?}"
            );
            assert_eq!(
                std::fs::read(&staging).expect("foreign stage remains"),
                b"foreign stage"
            );
            assert!(!rollback.exists(), "rollback is consumed");
            assert!(
                !rollback_deletion.exists(),
                "rollback quarantine is consumed"
            );
            assert!(!published.exists(), "payload quarantine is consumed");
            assert!(!derived_nested.exists(), "derived quarantine never appears");
            assert!(matches!(
                reopened.records().last().map(|record| record.phase()),
                Some(JournalPhase::Completed | JournalPhase::RolledBack)
            ));
        }
    }
}

#[test]
fn publication_state_machine_recovers_every_durable_crash_topology() {
    #[derive(Clone, Copy, Debug)]
    enum Topology {
        Original,
        DestinationQuarantined,
        StagePublished,
        PayloadQuarantined,
        DestinationRestored,
        StageRestored,
    }

    let cases = [
        (
            JournalPhase::DestinationQuarantinePlanned,
            Topology::Original,
        ),
        (
            JournalPhase::DestinationQuarantinePlanned,
            Topology::DestinationQuarantined,
        ),
        (
            JournalPhase::DestinationQuarantined,
            Topology::DestinationQuarantined,
        ),
        (
            JournalPhase::StagePublishPlanned,
            Topology::DestinationQuarantined,
        ),
        (JournalPhase::StagePublishPlanned, Topology::StagePublished),
        (
            JournalPhase::PublishRollbackPlanned,
            Topology::StagePublished,
        ),
        (
            JournalPhase::PublishRollbackPlanned,
            Topology::PayloadQuarantined,
        ),
        (
            JournalPhase::PublishedPayloadQuarantined,
            Topology::PayloadQuarantined,
        ),
        (
            JournalPhase::DestinationRestorePlanned,
            Topology::PayloadQuarantined,
        ),
        (
            JournalPhase::DestinationRestorePlanned,
            Topology::DestinationRestored,
        ),
        (
            JournalPhase::DestinationRestored,
            Topology::DestinationRestored,
        ),
        (
            JournalPhase::StageRestorePlanned,
            Topology::DestinationRestored,
        ),
        (JournalPhase::StageRestorePlanned, Topology::StageRestored),
    ];

    for (case_index, (phase, topology)) in cases.into_iter().enumerate() {
        for action in [
            ArchiveRecoveryAction::Resume,
            ArchiveRecoveryAction::Rollback,
        ] {
            let root = tempdir().expect("temporary root");
            let source = root.path().join("source.txt");
            let destination = root.path().join("archive.zip");
            let staging = test_staging(root.path(), 200 + case_index as u64);
            let rollback = Path::new(&format!("{}.rollback", staging.display())).to_path_buf();
            let published = Path::new(&format!("{}.published", staging.display())).to_path_buf();
            let rollback_deletion =
                Path::new(&format!("{}.delete", rollback.display())).to_path_buf();
            let stage_deletion = Path::new(&format!("{}.delete", staging.display())).to_path_buf();
            std::fs::write(&source, b"source").expect("source");
            std::fs::write(&staging, b"new archive").expect("stage");
            std::fs::write(&destination, b"old archive").expect("destination");
            let staged_identity = identity(&staging);
            let old_identity = identity(&destination);
            match topology {
                Topology::Original => {}
                Topology::DestinationQuarantined => {
                    std::fs::rename(&destination, &rollback).expect("quarantine destination");
                }
                Topology::StagePublished => {
                    std::fs::rename(&destination, &rollback).expect("quarantine destination");
                    std::fs::rename(&staging, &destination).expect("publish stage");
                }
                Topology::PayloadQuarantined => {
                    std::fs::rename(&destination, &rollback).expect("quarantine destination");
                    std::fs::rename(&staging, &published).expect("quarantine payload");
                }
                Topology::DestinationRestored => {
                    std::fs::rename(&staging, &published).expect("quarantine payload");
                }
                Topology::StageRestored => {}
            }
            let plan = ArchiveOperationPlan::create(
                vec![local(&source)],
                local(&destination),
                ArchiveCodec::Zip,
                ArchiveConflictPolicy::Replace,
                false,
            )
            .expect("archive plan");
            let checkpoint = ArchiveCheckpoint::new(
                plan,
                local(&staging),
                Some(staged_identity),
                Some(old_identity),
                None,
            )
            .with_staging_nonce(TEST_STAGE_NONCE)
            .with_cleanup_intent(
                ArchiveCleanupKind::PublishedDestination,
                local(&rollback),
                local(&rollback_deletion),
                Some(old_identity),
            )
            .with_stage_deletion(local(&stage_deletion))
            .with_publication_quarantine(local(&published));
            let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
            journal
                .append_archive(
                    JobId::new(200 + case_index as u64).expect("job id"),
                    EventGeneration::new(0),
                    phase,
                    Durability::CrashDurable,
                    checkpoint,
                )
                .expect("checkpoint persists");
            let request = recover_archive_operations(&journal)
                .unwrap_or_else(|error| panic!("{phase:?} {topology:?} scans: {error:?}"))
                .pop()
                .expect("recovery request");

            apply_archive_recovery(&mut journal, &request, action).unwrap_or_else(|error| {
                panic!("{phase:?} {topology:?} {action:?} recovers: {error:?}")
            });

            let expected = match action {
                ArchiveRecoveryAction::Resume => b"new archive".as_slice(),
                ArchiveRecoveryAction::Rollback => b"old archive".as_slice(),
            };
            assert_eq!(
                std::fs::read(&destination).expect("destination exists"),
                expected,
                "{phase:?} {topology:?} {action:?}"
            );
            assert!(!rollback.exists(), "rollback cleaned");
            assert!(!published.exists(), "published quarantine cleaned");
            assert!(!stage_deletion.exists(), "stage deletion cleaned");
            assert!(!rollback_deletion.exists(), "rollback deletion cleaned");
            assert!(matches!(
                journal.records().last().map(|record| record.phase()),
                Some(JournalPhase::Completed | JournalPhase::RolledBack)
            ));
        }
    }
}

#[test]
fn recovery_never_deletes_a_replacement_after_staging_cleaned_checkpoint() {
    let root = tempdir().expect("temporary root");
    let journal_dir = root.path().join("journal");
    let source = root.path().join("source.txt");
    let destination = root.path().join("recovered.zip");
    let staging = test_staging(root.path(), 73);
    std::fs::write(&source, b"source").expect("source");
    std::fs::write(&destination, b"published archive").expect("destination");
    let plan = ArchiveOperationPlan::create(
        vec![local(&source)],
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("recovery plan");
    let checkpoint = ArchiveCheckpoint::new(
        plan,
        local(&staging),
        None,
        None,
        Some(identity(&destination)),
    )
    .with_staging_nonce(TEST_STAGE_NONCE);
    {
        let storage = FileJournalStorage::at(&journal_dir).expect("file journal storage");
        let mut journal = Journal::open(storage).expect("file journal opens");
        journal
            .append_archive(
                JobId::new(73).expect("job id"),
                EventGeneration::new(0),
                JournalPhase::StagingCleaned,
                Durability::CrashDurable,
                checkpoint,
            )
            .expect("clean checkpoint is durable");
    }
    std::fs::write(&staging, b"reappeared stale stage").expect("reappeared stage");

    let storage = FileJournalStorage::at(&journal_dir).expect("reopened storage");
    let mut reopened = Journal::open(storage).expect("journal reopens");
    let requests = recover_archive_operations(&reopened).expect("recovery scan succeeds");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        apply_archive_recovery(&mut reopened, &requests[0], ArchiveRecoveryAction::Resume,)
            .expect("approved recovery succeeds"),
        musheen_desktop::ArchiveRecoveryOutcome::Completed
    );
    assert_eq!(
        std::fs::read(&staging).expect("unowned replacement remains"),
        b"reappeared stale stage"
    );
    assert_eq!(
        std::fs::read(&destination).expect("destination remains"),
        b"published archive"
    );
    assert_eq!(
        reopened.records().last().map(|record| record.phase()),
        Some(JournalPhase::Completed)
    );
}

#[test]
fn creation_rejects_symlinks_special_files_and_temporary_space_exhaustion() {
    let root = tempdir().expect("temporary root");
    let regular = root.path().join("regular");
    let link = root.path().join("link");
    std::fs::write(&regular, b"payload").expect("regular file");
    symlink(&regular, &link).expect("symlink");

    let archive = root.path().join("unsafe.zip");
    for source in [&link, Path::new("/dev/null")] {
        let plan = ArchiveOperationPlan::create(
            vec![local(source)],
            local(&archive),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("plan");
        assert!(matches!(
            run(
                &plan,
                &ArchiveOperationLimits::default(),
                &Passwords("unused"),
                &CancellationToken::new(),
            ),
            Err(ArchiveOperationError::UnsupportedFileType)
        ));
    }

    let limits = ArchiveOperationLimits {
        max_temporary_bytes: 1,
        ..ArchiveOperationLimits::default()
    };
    let plan = ArchiveOperationPlan::create(
        vec![local(&regular)],
        local(&archive),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("space plan");
    assert!(matches!(
        run(
            &plan,
            &limits,
            &Passwords("unused"),
            &CancellationToken::new(),
        ),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "temporary bytes",
            ..
        })
    ));
    assert!(!archive.exists());
}

#[test]
fn extraction_rejects_traversal_links_and_special_entries_and_writes_nested_archives_as_files() {
    let root = tempdir().expect("temporary root");
    let fixtures = root.path().join("fixtures");
    std::fs::create_dir(&fixtures).expect("fixture directory");

    let traversal = fixtures.join("traversal.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("../escape", zip::write::SimpleFileOptions::default())
        .expect("traversal entry");
    zip.write_all(b"escape").expect("traversal data");
    std::fs::write(&traversal, zip.finish().expect("zip finish").into_inner())
        .expect("traversal fixture");

    let symlink_archive = fixtures.join("symlink.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "link",
        zip::write::SimpleFileOptions::default().unix_permissions(0o120_777),
    )
    .expect("symlink entry");
    zip.write_all(b"../../escape").expect("symlink target");
    let mut symlink_bytes = zip.finish().expect("zip finish").into_inner();
    let central = symlink_bytes
        .windows(4)
        .position(|window| window == b"PK\x01\x02")
        .expect("central header");
    symlink_bytes[central + 38..central + 42].copy_from_slice(&(0o120_777_u32 << 16).to_le_bytes());
    std::fs::write(&symlink_archive, symlink_bytes).expect("symlink fixture");

    let special_archive = fixtures.join("special.tar");
    let mut tar_bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Fifo);
        header.set_size(0);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "pipe", io::empty())
            .expect("special entry");
        tar.finish().expect("tar finish");
    }
    std::fs::write(&special_archive, tar_bytes).expect("special fixture");

    for (index, source, codec) in [
        (1, traversal, ArchiveCodec::Zip),
        (2, symlink_archive, ArchiveCodec::Zip),
        (3, special_archive, ArchiveCodec::Tar),
    ] {
        let output = root.path().join(format!("unsafe-output-{index}"));
        let plan = ArchiveOperationPlan::extract(
            local(&source),
            local(&output),
            codec,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("extract plan");
        assert!(
            run(
                &plan,
                &ArchiveOperationLimits::default(),
                &Passwords("unused"),
                &CancellationToken::new(),
            )
            .is_err(),
            "unsafe fixture {index} was accepted"
        );
        assert!(!output.exists());
    }

    // An archive inside the archive is extracted as a file and never opened,
    // even when no nesting is allowed (OPS-034).
    let limits = ArchiveOperationLimits {
        max_nesting: 0,
        ..ArchiveOperationLimits::default()
    };
    let mut inner_zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    inner_zip
        .start_file("payload.txt", zip::write::SimpleFileOptions::default())
        .expect("inner entry");
    inner_zip.write_all(b"nested payload").expect("inner data");
    let inner_bytes = inner_zip.finish().expect("inner zip finish").into_inner();

    let mut v7_bytes = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut v7_bytes);
        let mut header = tar::Header::new_old();
        header.set_size(4);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "leaf", &b"leaf"[..])
            .expect("V7 tar entry");
        tar.finish().expect("V7 tar finish");
    }
    assert_ne!(v7_bytes.get(257..262), Some(&b"ustar"[..]));

    let mut prefixed_zip = b"MZ\x90\0self-extracting-stub".to_vec();
    prefixed_zip.extend_from_slice(&inner_bytes);
    let mut skippable_zstd = 0x184d_2a50_u32.to_le_bytes().to_vec();
    skippable_zstd.extend_from_slice(&4_u32.to_le_bytes());
    skippable_zstd.extend_from_slice(b"skip");
    skippable_zstd
        .extend_from_slice(&zstd::stream::encode_all(&v7_bytes[..], 0).expect("zstd V7 tar"));
    for (label, nested_bytes) in [
        ("zip", inner_bytes),
        ("v7-tar", v7_bytes),
        ("prefixed-zip", prefixed_zip),
        ("skippable-zstd", skippable_zstd),
    ] {
        let source = fixtures.join(format!("{label}-outer.zip"));
        let mut outer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        outer
            .start_file("opaque.bin", zip::write::SimpleFileOptions::default())
            .expect("nested content entry");
        outer.write_all(&nested_bytes).expect("nested content");
        std::fs::write(
            &source,
            outer.finish().expect("outer zip finish").into_inner(),
        )
        .expect("nested content fixture");
        let output = root.path().join(format!("{label}-output"));
        let plan = ArchiveOperationPlan::extract(
            local(&source),
            local(&output),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("nested content plan");
        run(
            &plan,
            &limits,
            &Passwords("unused"),
            &CancellationToken::new(),
        )
        .expect("an archive inside the archive is extracted as a file");
        assert_eq!(
            std::fs::read(output.join("opaque.bin")).expect("extracted nested archive"),
            nested_bytes,
            "{label}"
        );
    }
}

#[test]
fn every_archive_budget_trips_independently_and_in_combination() {
    let tiny = ArchiveOperationLimits {
        max_entries: 1,
        max_expanded_bytes: 10,
        max_compression_ratio: 2,
        max_nesting: 1,
        max_path_bytes: 4,
        max_memory_bytes: 8,
        max_temporary_bytes: 9,
        max_identity_millis: 30_000,
    };

    let mut budget = ArchiveBudget::new(tiny.clone());
    budget.charge_entry().expect("first entry");
    assert!(budget.charge_entry().is_err());

    assert!(
        ArchiveBudget::new(tiny.clone())
            .check_path(b"12345")
            .is_err()
    );
    assert!(
        ArchiveBudget::new(tiny.clone())
            .charge_expanded(11, 10)
            .is_err()
    );
    assert!(
        ArchiveBudget::new(tiny.clone())
            .charge_expanded(5, 2)
            .is_err()
    );
    assert!(ArchiveBudget::new(tiny.clone()).check_nesting(2).is_err());
    assert!(ArchiveBudget::new(tiny.clone()).reserve_memory(9).is_err());
    assert!(
        ArchiveBudget::new(tiny.clone())
            .charge_temporary(10)
            .is_err()
    );

    let mut combined = ArchiveBudget::new(tiny);
    combined.charge_entry().expect("entry");
    combined.check_path(b"1234").expect("path");
    combined.charge_expanded(8, 4).expect("expansion");
    combined.check_nesting(1).expect("nesting");
    let lease = combined.reserve_memory(8).expect("memory");
    combined.charge_temporary(9).expect("temporary");
    assert_eq!(
        combined.counters(),
        musheen_desktop::ArchiveBudgetCounters {
            entries: 1,
            expanded_bytes: 8,
            compressed_bytes: 4,
            compression_ratio_checks: 1,
            temporary_bytes: 9,
            memory_bytes: 8,
            peak_memory_bytes: 8,
            max_nesting: 1,
            max_path_bytes: 4,
        }
    );
    drop(lease);
    assert_eq!(combined.counters().memory_bytes, 0);
    let lease = combined.reserve_memory(8).expect("phase-one allocation");
    let mut next_phase = combined.next_phase();
    assert!(matches!(
        next_phase.charge_temporary(1),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "temporary bytes",
            value: 10,
            maximum: 9,
        })
    ));
    assert!(matches!(
        next_phase.reserve_memory(1),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "memory bytes",
            value: 9,
            maximum: 8,
        })
    ));
    drop(lease);
    assert!(combined.charge_entry().is_err());
}

#[test]
fn every_ceiling_rejects_a_real_archive_operation_before_publication() {
    let root = tempdir().expect("temporary root");
    let simple = root.path().join("simple.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "a-very-long-entry-name.txt",
        zip::write::SimpleFileOptions::default(),
    )
    .expect("simple entry");
    zip.write_all(&vec![0_u8; 4_096]).expect("simple payload");
    std::fs::write(&simple, zip.finish().expect("simple zip").into_inner())
        .expect("simple fixture");

    let pair = root.path().join("pair.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for name in ["one.txt", "two.txt"] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .expect("pair entry");
        zip.write_all(b"pair payload").expect("pair payload");
    }
    std::fs::write(&pair, zip.finish().expect("pair zip").into_inner()).expect("pair fixture");

    let cases = [
        (
            "archive entries",
            simple.as_path(),
            ArchiveOperationLimits {
                max_entries: 0,
                ..ArchiveOperationLimits::default()
            },
        ),
        (
            "expanded bytes",
            simple.as_path(),
            ArchiveOperationLimits {
                max_expanded_bytes: 1,
                ..ArchiveOperationLimits::default()
            },
        ),
        (
            "compression ratio",
            simple.as_path(),
            ArchiveOperationLimits {
                max_compression_ratio: 0,
                ..ArchiveOperationLimits::default()
            },
        ),
        (
            "path bytes",
            simple.as_path(),
            ArchiveOperationLimits {
                max_path_bytes: 4,
                ..ArchiveOperationLimits::default()
            },
        ),
        (
            "memory bytes",
            simple.as_path(),
            ArchiveOperationLimits {
                max_memory_bytes: 1,
                ..ArchiveOperationLimits::default()
            },
        ),
        (
            "temporary bytes",
            simple.as_path(),
            ArchiveOperationLimits {
                max_temporary_bytes: 1,
                ..ArchiveOperationLimits::default()
            },
        ),
    ];

    for (index, (resource, source, limits)) in cases.into_iter().enumerate() {
        let output = root.path().join(format!("ceiling-output-{index}"));
        let plan = ArchiveOperationPlan::extract(
            local(source),
            local(&output),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("ceiling plan");
        let result = run(
            &plan,
            &limits,
            &Passwords("unused"),
            &CancellationToken::new(),
        );
        assert!(
            matches!(
                &result,
                Err(ArchiveOperationError::LimitExceeded { resource: actual, .. }) if *actual == resource
            ),
            "{resource} did not reject the archive operation: {result:?}"
        );
        assert!(!output.exists());
    }

    let combined = ArchiveOperationLimits {
        max_entries: 10,
        max_expanded_bytes: 100_000,
        max_compression_ratio: 1_000,
        max_nesting: 4,
        max_path_bytes: 100,
        max_memory_bytes: 1024 * 1024,
        max_temporary_bytes: 4_200,
        max_identity_millis: 30_000,
    };
    let combined_output = root.path().join("combined-output");
    let combined_plan = ArchiveOperationPlan::extract(
        local(&pair),
        local(&combined_output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("combined plan");
    let accounting = ArchiveOperationAccounting::default();
    let combined_result = run_accounted(&combined_plan, &combined, &accounting);
    assert!(
        matches!(
            combined_result,
            Err(ArchiveOperationError::LimitExceeded {
                resource: "temporary bytes",
                ..
            })
        ),
        "unexpected combined budget result: {combined_result:?}"
    );
    let counters = accounting.counters();
    assert!(counters.entries >= 2, "entry accounting was not exercised");
    assert!(
        counters.expanded_bytes > 0,
        "expanded-byte accounting was not exercised"
    );
    assert!(
        counters.compressed_bytes > 0 && counters.compression_ratio_checks > 0,
        "compression-ratio accounting was not exercised"
    );
    assert!(
        counters.max_path_bytes >= 4,
        "path accounting was not exercised"
    );
    assert!(
        counters.peak_memory_bytes > 0,
        "memory accounting was not exercised"
    );
    assert!(
        counters.temporary_bytes > combined.max_temporary_bytes,
        "the intended later temporary-space ceiling was not reached: {counters:?}"
    );
    assert!(!combined_output.exists());
}

#[test]
fn production_archive_ceiling_values_trip_without_large_allocations() {
    let limits = ArchiveOperationLimits::default();
    assert_eq!(limits.max_entries, 100_000);
    assert_eq!(limits.max_expanded_bytes, 20 * 1_024 * 1_024 * 1_024);
    assert_eq!(limits.max_compression_ratio, 1_000);
    assert_eq!(limits.max_nesting, 8);
    assert_eq!(limits.max_path_bytes, 4_096);
    assert_eq!(limits.max_memory_bytes, 512 * 1_024 * 1_024);

    let mut entries = ArchiveBudget::new(limits.clone());
    for _ in 0..limits.max_entries {
        entries.charge_entry().expect("within entry ceiling");
    }
    assert!(entries.charge_entry().is_err());
    assert!(
        ArchiveBudget::new(limits.clone())
            .charge_expanded(limits.max_expanded_bytes + 1, limits.max_expanded_bytes + 1)
            .is_err()
    );
    assert!(
        ArchiveBudget::new(limits.clone())
            .charge_expanded(1_001, 1)
            .is_err()
    );
    assert!(
        ArchiveBudget::new(limits.clone())
            .check_nesting(limits.max_nesting + 1)
            .is_err()
    );
    assert!(
        ArchiveBudget::new(limits.clone())
            .check_path(&vec![b'x'; limits.max_path_bytes + 1])
            .is_err()
    );
    assert!(
        ArchiveBudget::new(limits.clone())
            .reserve_memory(limits.max_memory_bytes + 1)
            .is_err()
    );
    assert!(
        ArchiveBudget::new(limits.clone())
            .charge_temporary(limits.max_temporary_bytes + 1)
            .is_err()
    );
}

#[test]
fn default_archive_staging_budget_does_not_exceed_ten_gib() {
    assert_eq!(
        ArchiveOperationLimits::default().max_temporary_bytes,
        10 * 1_024 * 1_024 * 1_024
    );
}

#[test]
fn checkpoint_plan_serialization_respects_the_global_memory_limit() {
    let root = tempdir().expect("temporary root");
    let mut sources = Vec::new();
    for index in 0..48 {
        let source = root
            .path()
            .join(format!("{index:03}-{}", "long-name".repeat(18)));
        std::fs::write(&source, b"x").expect("source");
        sources.push(local(&source));
    }
    let destination = root.path().join("many-sources.zip");
    let plan = ArchiveOperationPlan::create(
        sources,
        local(&destination),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("archive plan");
    let limits = ArchiveOperationLimits {
        max_memory_bytes: 96 * 1024,
        ..ArchiveOperationLimits::default()
    };
    let accounting = ArchiveOperationAccounting::default();
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");

    assert!(matches!(
        execute_scheduled_archive_operation_with_accounting(
            &scheduler,
            &job,
            &limits,
            &Passwords("unused"),
            &mut journal,
            &accounting,
        ),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "memory bytes",
            ..
        })
    ));
    assert!(journal.records().is_empty());
    assert!(!destination.exists());
    assert_eq!(accounting.counters().memory_bytes, 0);
}

fn zip_bytes(method: zip::CompressionMethod, payload: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "x",
        zip::write::SimpleFileOptions::default().compression_method(method),
    )
    .expect("ZIP entry");
    zip.write_all(payload).expect("ZIP payload");
    zip.finish().expect("ZIP finish").into_inner()
}

fn zip_bytes_with_unix_file_type(mode: u32) -> Vec<u8> {
    let mut bytes = zip_bytes(zip::CompressionMethod::Stored, b"special");
    let central = bytes
        .windows(4)
        .position(|window| window == b"PK\x01\x02")
        .expect("central directory");
    bytes[central + 38..central + 42].copy_from_slice(&(mode << 16).to_le_bytes());
    bytes
}

#[test]
fn zip_extraction_rejects_unix_special_file_types_before_writing() {
    let root = tempdir().expect("temporary root");
    for (name, mode) in [
        ("fifo", 0o010_000),
        ("character", 0o020_000),
        ("block", 0o060_000),
        ("socket", 0o140_000),
    ] {
        let source = root.path().join(format!("{name}.zip"));
        let output = root.path().join(format!("{name}-output"));
        std::fs::write(&source, zip_bytes_with_unix_file_type(mode)).expect("ZIP fixture");
        let plan = ArchiveOperationPlan::extract(
            local(&source),
            local(&output),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("extract plan");
        assert!(matches!(
            run(
                &plan,
                &ArchiveOperationLimits::default(),
                &Passwords("unused"),
                &CancellationToken::new(),
            ),
            Err(ArchiveOperationError::UnsupportedFileType)
        ));
        assert!(!output.exists());
    }
}

fn extraction_peak(source: &Path, output: &Path) -> u64 {
    let plan = ArchiveOperationPlan::extract(
        local(source),
        local(output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("extract plan");
    let accounting = ArchiveOperationAccounting::default();
    run_accounted(&plan, &ArchiveOperationLimits::default(), &accounting).expect("ZIP extraction");
    assert_eq!(accounting.counters().memory_bytes, 0);
    accounting.counters().peak_memory_bytes
}

#[test]
fn zip_deflate_extraction_charges_decoder_workspace_before_allocation() {
    let root = tempdir().expect("temporary root");
    let stored = root.path().join("s.zip");
    let deflate = root.path().join("d.zip");
    std::fs::write(&stored, zip_bytes(zip::CompressionMethod::Stored, b"leaf"))
        .expect("stored fixture");
    std::fs::write(
        &deflate,
        zip_bytes(zip::CompressionMethod::Deflated, b"leaf"),
    )
    .expect("deflate fixture");

    let stored_peak = extraction_peak(&stored, &root.path().join("s-out-a"));
    let deflate_peak = extraction_peak(&deflate, &root.path().join("d-out-a"));
    assert!(
        deflate_peak > stored_peak + 16 * 1_024,
        "deflate decoder workspace was not charged: stored={stored_peak}, deflate={deflate_peak}"
    );

    let output = root.path().join("d-out-b");
    let plan = ArchiveOperationPlan::extract(
        local(&deflate),
        local(&output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("limited extract plan");
    let accounting = ArchiveOperationAccounting::default();
    let limits = ArchiveOperationLimits {
        max_memory_bytes: deflate_peak - 1,
        ..ArchiveOperationLimits::default()
    };
    let result = run_accounted(&plan, &limits, &accounting);
    assert!(
        matches!(
            result,
            Err(ArchiveOperationError::LimitExceeded {
                resource: "memory bytes",
                ..
            })
        ),
        "unexpected workspace result: {result:?}"
    );
    assert_eq!(accounting.counters().memory_bytes, 0);
    assert!(!output.exists());
}

#[test]
fn zip_zstd_extraction_charges_decoder_workspace_before_allocation() {
    let root = tempdir().expect("temporary root");
    let stored = root.path().join("s.zip");
    let zstd = root.path().join("z.zip");
    std::fs::write(&stored, zip_bytes(zip::CompressionMethod::Stored, b"leaf"))
        .expect("stored fixture");
    std::fs::write(&zstd, zip_bytes(zip::CompressionMethod::Zstd, b"leaf")).expect("zstd fixture");

    let stored_peak = extraction_peak(&stored, &root.path().join("s-out-a"));
    let zstd_peak = extraction_peak(&zstd, &root.path().join("z-out-a"));
    assert!(
        zstd_peak > stored_peak + 64 * 1_024,
        "zstd decoder workspace was not charged: stored={stored_peak}, zstd={zstd_peak}"
    );

    let output = root.path().join("z-out-b");
    let plan = ArchiveOperationPlan::extract(
        local(&zstd),
        local(&output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("limited extract plan");
    let accounting = ArchiveOperationAccounting::default();
    let limits = ArchiveOperationLimits {
        max_memory_bytes: zstd_peak - 1,
        ..ArchiveOperationLimits::default()
    };
    let result = run_accounted(&plan, &limits, &accounting);
    assert!(
        matches!(
            result,
            Err(ArchiveOperationError::LimitExceeded {
                resource: "memory bytes",
                ..
            })
        ),
        "unexpected workspace result: {result:?}"
    );
    assert_eq!(accounting.counters().memory_bytes, 0);
    assert!(!output.exists());
}

#[test]
fn archive_creation_charges_codec_workspaces() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("input");
    std::fs::write(&source, b"small payload").expect("source");

    for (label, codec) in [
        ("zip", ArchiveCodec::Zip),
        ("7z", ArchiveCodec::SevenZip),
        ("tar-gzip", ArchiveCodec::TarGzip),
        ("tar-zstd", ArchiveCodec::TarZstd),
    ] {
        let first_output = root.path().join(format!("{label}-a.bin"));
        let first_plan = ArchiveOperationPlan::create(
            vec![local(&source)],
            local(&first_output),
            codec,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("first creation plan");
        let accounting = ArchiveOperationAccounting::default();
        run_accounted(&first_plan, &ArchiveOperationLimits::default(), &accounting)
            .expect("archive creation");
        let peak = accounting.counters().peak_memory_bytes;
        assert!(
            peak > 64 * 1024,
            "{label} codec workspace was not charged: {peak}"
        );
        assert_eq!(accounting.counters().memory_bytes, 0);

        let second_output = root.path().join(format!("{label}-b.bin"));
        let second_plan = ArchiveOperationPlan::create(
            vec![local(&source)],
            local(&second_output),
            codec,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("second creation plan");
        let limits = ArchiveOperationLimits {
            max_memory_bytes: peak - 1,
            ..ArchiveOperationLimits::default()
        };
        let failed_accounting = ArchiveOperationAccounting::default();
        assert!(matches!(
            run_accounted(&second_plan, &limits, &failed_accounting),
            Err(ArchiveOperationError::LimitExceeded {
                resource: "memory bytes",
                ..
            })
        ));
        assert_eq!(failed_accounting.counters().memory_bytes, 0);
        assert!(!second_output.exists());
    }
}

#[test]
fn production_limits_reject_real_bombs_and_a_nesting_bomb_is_extracted_as_a_file() {
    let root = tempdir().expect("temporary root");

    let declared_huge = root.path().join("declared-huge.tar");
    let mut header = tar::Header::new_gnu();
    header.set_path("huge.bin").expect("tar path");
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o600);
    header.set_size(20 * 1_024 * 1_024 * 1_024 + 1);
    header.set_cksum();
    std::fs::write(&declared_huge, header.as_bytes()).expect("huge tar header");
    let huge_output = root.path().join("huge-output");
    let huge_plan = ArchiveOperationPlan::extract(
        local(&declared_huge),
        local(&huge_output),
        ArchiveCodec::Tar,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("huge plan");
    assert!(matches!(
        run(
            &huge_plan,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
        ),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "expanded bytes",
            ..
        })
    ));

    let ratio_bomb = root.path().join("ratio.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "zeros.bin",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )
    .expect("ratio entry");
    zip.write_all(&vec![0_u8; 8 * 1_024 * 1_024])
        .expect("ratio payload");
    std::fs::write(&ratio_bomb, zip.finish().expect("ratio zip").into_inner())
        .expect("ratio fixture");
    let ratio_output = root.path().join("ratio-output");
    let ratio_plan = ArchiveOperationPlan::extract(
        local(&ratio_bomb),
        local(&ratio_output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("ratio plan");
    assert!(matches!(
        run(
            &ratio_plan,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
        ),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "compression ratio",
            ..
        })
    ));

    let mut nested_bytes = b"leaf".to_vec();
    for depth in 0..=9 {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            format!("opaque-{depth}.bin"),
            zip::write::SimpleFileOptions::default(),
        )
        .expect("nested entry");
        zip.write_all(&nested_bytes).expect("nested payload");
        nested_bytes = zip.finish().expect("nested zip").into_inner();
    }
    let nesting_bomb = root.path().join("nesting.zip");
    std::fs::write(&nesting_bomb, nested_bytes).expect("nesting fixture");
    let nesting_output = root.path().join("nesting-output");
    let nesting_plan = ArchiveOperationPlan::extract(
        local(&nesting_bomb),
        local(&nesting_output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("nesting plan");
    // Extraction never opens an archive inside the archive (OPS-034).
    let inner_bytes =
        zip::ZipArchive::new(Cursor::new(std::fs::read(&nesting_bomb).expect("bomb")))
            .expect("outer archive")
            .by_name("opaque-9.bin")
            .map(|mut entry| {
                let mut bytes = Vec::new();
                std::io::Read::read_to_end(&mut entry, &mut bytes).expect("inner bytes");
                bytes
            })
            .expect("outer entry");
    run(
        &nesting_plan,
        &ArchiveOperationLimits::default(),
        &Passwords("unused"),
        &CancellationToken::new(),
    )
    .expect("a nesting bomb is extracted as a file");
    assert_eq!(
        std::fs::read(nesting_output.join("opaque-9.bin")).expect("extracted inner archive"),
        inner_bytes
    );
}

/// Names the one measured extraction or collision check that
/// `extract_cost_child` runs: mode, codec, source and destination, one per
/// line.
const EXTRACT_COST_REQUEST: &str = "MUSHEEN_EXTRACT_COST_REQUEST";
const MIB: u64 = 1_024 * 1_024;

/// The bytes this process has read and written so far.
fn process_io() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/self/io").expect("process I/O counters");
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|value| value.trim().parse::<u64>().ok())
            .expect("process I/O field")
    };
    (field("rchar:"), field("wchar:"))
}

fn extract_cost_codec(name: &str) -> ArchiveCodec {
    match name {
        "zip" => ArchiveCodec::Zip,
        "tar.gz" => ArchiveCodec::TarGzip,
        "tar.zst" => ArchiveCodec::TarZstd,
        "7z" => ArchiveCodec::SevenZip,
        other => panic!("unknown codec {other}"),
    }
}

/// Runs one measured extraction or collision check when
/// `extract_cost_reads_the_archive_at_most_twice_and_writes_only_its_files`
/// starts this test binary as a child, and does nothing otherwise. Alone in
/// its own process, /proc/self/io counts only that work.
#[test]
fn extract_cost_child() {
    let Ok(request) = std::env::var(EXTRACT_COST_REQUEST) else {
        return;
    };
    let lines = request.lines().collect::<Vec<_>>();
    let [mode, codec, source, destination] = lines.as_slice() else {
        panic!("malformed request {request:?}");
    };
    let plan = ArchiveOperationPlan::extract(
        local(Path::new(source)),
        local(Path::new(destination)),
        extract_cost_codec(codec),
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("extract plan");
    // An archive inside the archive is never opened, so no nesting is needed.
    let limits = ArchiveOperationLimits {
        max_nesting: 0,
        ..ArchiveOperationLimits::default()
    };
    let before = process_io();
    match *mode {
        "extract" => {
            run(
                &plan,
                &limits,
                &Passwords("unused"),
                &CancellationToken::new(),
            )
            .expect("extraction");
        }
        "check" => {
            musheen_desktop::extract_destination(
                &plan,
                &limits,
                &Passwords("unused"),
                &CancellationToken::new(),
            )
            .expect("collision check");
        }
        other => panic!("unknown mode {other}"),
    }
    let after = process_io();
    println!(
        "EXTRACT_COST read={} written={}",
        after.0 - before.0,
        after.1 - before.1
    );
}

/// Runs `extract_cost_child` in a child copy of this test binary and returns
/// the bytes it read and wrote during the measured work.
fn measured_in_child(mode: &str, codec: &str, source: &Path, destination: &Path) -> (u64, u64) {
    let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "extract_cost_child",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            EXTRACT_COST_REQUEST,
            format!(
                "{mode}\n{codec}\n{}\n{}",
                source.display(),
                destination.display()
            ),
        )
        .output()
        .expect("child test runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{codec} {mode} failed: {stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let line = stdout
        .lines()
        .find_map(|line| line.split_once("EXTRACT_COST ").map(|(_, rest)| rest))
        .unwrap_or_else(|| panic!("no measurement in {stdout}"));
    let value = |name: &str| {
        line.split_whitespace()
            .find_map(|field| field.strip_prefix(name))
            .and_then(|value| value.parse::<u64>().ok())
            .expect("measurement field")
    };
    (value("read="), value("written="))
}

/// About 2.5 MiB of files that do not compress, and one file that is itself
/// a ZIP archive. Returns the folder and the bytes of its files.
fn extract_cost_input(root: &Path) -> (std::path::PathBuf, u64) {
    let input = root.join("input");
    let mut state = 0x9e37_79b9_7f4a_7c15_u64;
    let mut total = 0;
    for index in 0..200_usize {
        let folder = input.join(format!("folder-{}", index % 10));
        std::fs::create_dir_all(&folder).expect("input folder");
        let bytes = (0..12_000 + index * 7)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state.to_le_bytes()[0]
            })
            .collect::<Vec<_>>();
        total += bytes.len() as u64;
        std::fs::write(folder.join(format!("file-{index}.bin")), &bytes).expect("input file");
    }
    let inner = zip_bytes(
        zip::CompressionMethod::Deflated,
        b"an archive inside the archive",
    );
    total += inner.len() as u64;
    std::fs::write(input.join("inner.zip"), &inner).expect("inner archive");
    (input, total)
}

#[test]
fn extract_cost_reads_the_archive_at_most_twice_and_writes_only_its_files() {
    let root = tempdir().expect("temporary root");
    let (input, file_bytes) = extract_cost_input(root.path());
    for (codec, name) in [
        (ArchiveCodec::Zip, "zip"),
        (ArchiveCodec::TarGzip, "tar.gz"),
        (ArchiveCodec::TarZstd, "tar.zst"),
        (ArchiveCodec::SevenZip, "7z"),
    ] {
        let archive = root.path().join(format!("fixture.{name}"));
        let create = ArchiveOperationPlan::create(
            vec![local(&input)],
            local(&archive),
            codec,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("create plan");
        run(
            &create,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
        )
        .expect("fixture archive");
        let archive_bytes = std::fs::metadata(&archive).expect("fixture archive").len();
        let output = root.path().join(format!("output-{name}"));

        let (read, written) = measured_in_child("extract", name, &archive, &output);
        assert!(
            read <= 2 * archive_bytes + MIB,
            "{name}: extraction read {read} bytes of a {archive_bytes}-byte archive"
        );
        assert!(
            written <= file_bytes + MIB,
            "{name}: extraction wrote {written} bytes for {file_bytes} bytes of files"
        );
        for file in ["inner.zip", "folder-3/file-123.bin"] {
            assert_eq!(
                std::fs::read(output.join("input").join(file)).expect("extracted file"),
                std::fs::read(input.join(file)).expect("input file"),
                "{name}: {file} is extracted as it was archived"
            );
        }

        let (read, written) = measured_in_child("check", name, &archive, &output);
        assert!(
            read <= archive_bytes + MIB,
            "{name}: the collision check read {read} bytes of a {archive_bytes}-byte archive"
        );
        assert!(
            written <= MIB,
            "{name}: the collision check wrote {written} bytes"
        );
    }
}

#[test]
fn extract_cost_publishes_nothing_when_the_archive_changes_during_the_run() {
    let root = tempdir().expect("temporary root");
    let archive = root.path().join("fixture.tar");
    let mut tar_bytes = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_bytes);
        let mut header = tar::Header::new_gnu();
        header.set_size(4_096);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, "data.txt", &[b'a'; 4_096][..])
            .expect("tar entry");
        builder.finish().expect("tar finish");
    }
    std::fs::write(&archive, &tar_bytes).expect("fixture archive");
    let output = root.path().join("output");
    let plan = ArchiveOperationPlan::extract(
        local(&archive),
        local(&output),
        ArchiveCodec::Tar,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("extract plan");
    let changed = archive.clone();
    let storage = HookJournal {
        inner: MemoryJournal::default(),
        hook_at: 2,
        // Rewrite the file's bytes in place, at the same size and with the
        // same modification time, so the archive still decodes.
        hook: Some(Box::new(move || {
            use std::os::unix::fs::FileExt;
            // Some file systems keep coarse timestamps; let the clock move
            // past the fixture's change time first.
            std::thread::sleep(std::time::Duration::from_millis(20));
            let modified = std::fs::metadata(&changed)
                .and_then(|metadata| metadata.modified())
                .expect("archive time");
            let mut bytes = std::fs::read(&changed).expect("archive bytes");
            let start = bytes
                .windows(4_096)
                .position(|window| window.iter().all(|byte| *byte == b'a'))
                .expect("file bytes");
            bytes[start..start + 4_096].fill(b'b');
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&changed)
                .expect("archive opens");
            file.write_all_at(&bytes, 0).expect("archive rewritten");
            file.set_modified(modified).expect("archive time restored");
        })),
    };
    let mut journal = Journal::open(storage).expect("journal opens");
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan, provider())
        .expect("archive queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");

    let result = execute_scheduled_archive_operation(
        &scheduler,
        &job,
        &ArchiveOperationLimits::default(),
        &Passwords("unused"),
        &mut journal,
    );
    assert!(
        matches!(result, Err(ArchiveOperationError::Conflict)),
        "an archive changed during the run is refused: {result:?}"
    );
    assert!(!output.exists(), "nothing is published");
    assert!(
        std::fs::read_dir(root.path())
            .expect("archive folder")
            .all(|entry| !entry
                .expect("folder entry")
                .file_name()
                .as_bytes()
                .starts_with(b".musheen-stage-v1-")),
        "the staging folder is cleaned"
    );
}
