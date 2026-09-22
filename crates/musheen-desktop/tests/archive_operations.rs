use musheen_core::{
    CancellationToken, CapabilityMatrix, CapabilityState, ProviderId, ResourceLimits, StorePath,
};
use musheen_desktop::{
    ArchiveBudget, ArchiveError, ArchiveOperationAccounting, ArchiveOperationError,
    ArchiveOperationLimits, ArchiveOperationOutcome, ArchivePassword, ArchivePasswordProvider,
    FileJournalStorage, PasswordRequest, execute_scheduled_archive_operation,
    execute_scheduled_archive_operation_with_accounting, recover_archive_operations,
};
use musheen_ops::{
    ArchiveCheckpoint, ArchiveCodec, ArchiveConflictPolicy, ArchiveOperationPlan,
    ArchivePathIdentity, CorruptSource, Durability, EventGeneration, JobId, Journal, JournalPhase,
    JournalStorage, ProviderLimits, ProviderSnapshot, Scheduler,
};
use std::io::{self, Cursor, Write};
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
    ArchivePathIdentity::new(
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.is_dir(),
    )
}

fn run(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(ArchiveOperationOutcome, Vec<JournalPhase>), ArchiveOperationError> {
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    let mut scheduler = Scheduler::new(&ResourceLimits::default());
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
        execute_scheduled_archive_operation(&mut scheduler, &job, limits, passwords, &mut journal)?;
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
    let mut scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler
        .enqueue_archive(plan.clone(), provider())
        .expect("archive queues");
    let job = scheduler
        .start_ready()
        .expect("archive starts")
        .pop()
        .expect("archive job");
    execute_scheduled_archive_operation_with_accounting(
        &mut scheduler,
        &job,
        limits,
        &Passwords("unused"),
        &mut journal,
        accounting,
    )
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
    let mut scheduler = Scheduler::new(&ResourceLimits::default());
    let id = scheduler.enqueue_archive(plan, provider()).expect("queues");
    let job = scheduler.start_ready().expect("starts").pop().expect("job");
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal");

    execute_scheduled_archive_operation(
        &mut scheduler,
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
fn journal_failures_clean_unpublished_staging_and_retain_replace_recovery_data() {
    for fail_append_at in 1..=7 {
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
        let mut scheduler = Scheduler::new(&ResourceLimits::default());
        let id = scheduler.enqueue_archive(plan, provider()).expect("queues");
        let job = scheduler.start_ready().expect("starts").pop().expect("job");
        assert_eq!(job.id(), id);
        let result = execute_scheduled_archive_operation(
            &mut scheduler,
            &job,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &mut journal,
        );
        assert!(matches!(result, Err(ArchiveOperationError::Journal)));
        let staging = root
            .path()
            .join(format!(".musheen-stage-v1-{}-0", id.get()));
        if fail_append_at < 5 {
            assert_eq!(
                std::fs::read(&destination).expect("old destination remains"),
                b"old destination"
            );
            assert!(!staging.exists());
        } else {
            assert_ne!(
                std::fs::read(&destination).expect("published archive"),
                b"old destination"
            );
            if fail_append_at == 5 {
                assert_eq!(
                    std::fs::read(&staging).expect("old destination retained for recovery"),
                    b"old destination"
                );
            } else {
                assert!(!staging.exists());
            }
            assert_ne!(
                journal.records().last().map(|record| record.phase()),
                Some(JournalPhase::RolledBack)
            );
        }

        let storage = journal.into_storage();
        let mut reopened = Journal::open(storage).expect("journal reopens after crash point");
        recover_archive_operations(&mut reopened)
            .unwrap_or_else(|error| panic!("crash point {fail_append_at} recovers: {error:?}"));
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
    }
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
        let staging = root
            .path()
            .join(format!(".musheen-stage-v1-{job_number}-0"));
        if phase == JournalPhase::StagingCreated {
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
            ArchiveCheckpoint::new(plan, local(&staging), staging_identity, None, None);
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
        if phase == JournalPhase::StagingCreated {
            std::fs::write(&staging, b"partially encoded archive")
                .expect("codec writes before crash");
        }

        let storage = FileJournalStorage::at(&journal_dir).expect("reopened storage");
        let mut reopened = Journal::open(storage).expect("journal reopens");
        let outcomes = recover_archive_operations(&mut reopened).expect("recovery succeeds");
        assert_eq!(outcomes.len(), 1);
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
fn recovery_recleans_stage_that_reappears_after_staging_cleaned_checkpoint() {
    let root = tempdir().expect("temporary root");
    let journal_dir = root.path().join("journal");
    let source = root.path().join("source.txt");
    let destination = root.path().join("recovered.zip");
    let staging = root.path().join(".musheen-stage-v1-73-0");
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
    );
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
    assert_eq!(
        recover_archive_operations(&mut reopened).expect("recovery succeeds"),
        vec![musheen_desktop::ArchiveRecoveryOutcome::Completed]
    );
    assert!(!staging.exists());
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
fn extraction_rejects_traversal_links_special_entries_and_nested_archives_before_writing() {
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

    let mut inner_zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    inner_zip
        .start_file("payload.txt", zip::write::SimpleFileOptions::default())
        .expect("inner entry");
    inner_zip.write_all(b"nested payload").expect("inner data");
    let inner_bytes = inner_zip.finish().expect("inner zip finish").into_inner();
    let nested = fixtures.join("nested.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(
        "no-archive-suffix.bin",
        zip::write::SimpleFileOptions::default(),
    )
    .expect("nested entry");
    zip.write_all(&inner_bytes).expect("nested bytes");
    std::fs::write(&nested, zip.finish().expect("zip finish").into_inner())
        .expect("nested fixture");
    let output = root.path().join("nested-output");
    let plan = ArchiveOperationPlan::extract(
        local(&nested),
        local(&output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("nested plan");
    let limits = ArchiveOperationLimits {
        max_nesting: 0,
        ..ArchiveOperationLimits::default()
    };
    assert!(matches!(
        run(
            &plan,
            &limits,
            &Passwords("unused"),
            &CancellationToken::new(),
        ),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "archive nesting",
            ..
        })
    ));
    assert!(!output.exists());

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
        assert!(matches!(
            run(
                &plan,
                &limits,
                &Passwords("unused"),
                &CancellationToken::new(),
            ),
            Err(ArchiveOperationError::LimitExceeded {
                resource: "archive nesting",
                ..
            })
        ));
        assert!(!output.exists());

        let accepted_output = root.path().join(format!("{label}-accepted"));
        let accepted_plan = ArchiveOperationPlan::extract(
            local(&source),
            local(&accepted_output),
            ArchiveCodec::Zip,
            ArchiveConflictPolicy::Fail,
            false,
        )
        .expect("accepted nested content plan");
        run(
            &accepted_plan,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
        )
        .expect("valid nested content is accepted");
        assert_eq!(
            std::fs::read(accepted_output.join("opaque.bin")).expect("extracted nested payload"),
            nested_bytes
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
    let next_phase = combined.next_phase();
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

    let mut inner = zip::ZipWriter::new(Cursor::new(Vec::new()));
    inner
        .start_file("leaf", zip::write::SimpleFileOptions::default())
        .expect("leaf entry");
    inner.write_all(b"leaf").expect("leaf payload");
    let inner = inner.finish().expect("inner zip").into_inner();
    let nested = root.path().join("nested.zip");
    let mut outer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    outer
        .start_file("opaque.bin", zip::write::SimpleFileOptions::default())
        .expect("outer entry");
    outer.write_all(&inner).expect("outer payload");
    std::fs::write(&nested, outer.finish().expect("outer zip").into_inner())
        .expect("nested fixture");

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
            "archive nesting",
            nested.as_path(),
            ArchiveOperationLimits {
                max_nesting: 0,
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
    };
    let combined_output = root.path().join("combined-output");
    let combined_plan = ArchiveOperationPlan::extract(
        local(&nested),
        local(&combined_output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("combined plan");
    let accounting = ArchiveOperationAccounting::default();
    assert!(matches!(
        run_accounted(&combined_plan, &combined, &accounting),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "temporary bytes",
            ..
        })
    ));
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
    assert!(counters.max_nesting >= 1, "nesting was not exercised");
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
fn nested_codec_allocations_share_one_live_memory_ceiling() {
    let root = tempdir().expect("temporary root");
    let long_name = format!("{}x", "nested/".repeat(120));
    let mut nested_bytes = b"leaf".to_vec();
    for _ in 0..8 {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(&long_name, zip::write::SimpleFileOptions::default())
            .expect("nested entry");
        zip.write_all(&nested_bytes).expect("nested bytes");
        nested_bytes = zip.finish().expect("nested zip finish").into_inner();
    }
    let source = root.path().join("shared-memory.zip");
    std::fs::write(&source, nested_bytes).expect("nested fixture");
    let output = root.path().join("shared-memory-output");
    let plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&output),
        ArchiveCodec::Zip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("extract plan");
    let limits = ArchiveOperationLimits {
        max_memory_bytes: 20 * 1024,
        max_nesting: 10,
        ..ArchiveOperationLimits::default()
    };
    let result = run(
        &plan,
        &limits,
        &Passwords("unused"),
        &CancellationToken::new(),
    );
    assert!(
        matches!(
            result,
            Err(ArchiveOperationError::LimitExceeded {
                resource: "memory bytes",
                ..
            })
        ),
        "shared nested allocation did not trip memory: {result:?}"
    );
    assert!(!output.exists());
}

#[test]
fn nested_gzip_and_zstd_workspaces_share_the_operation_memory_ceiling() {
    let root = tempdir().expect("temporary root");
    let mut leaf_tar = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut leaf_tar);
        let mut header = tar::Header::new_gnu();
        header.set_size(4);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "leaf", &b"leaf"[..])
            .expect("leaf tar entry");
        tar.finish().expect("leaf tar finish");
    }
    let nested_zstd = zstd::stream::encode_all(&leaf_tar[..], 0).expect("zstd nested tar");
    let zstd_source = root.path().join("standalone.tar.zst");
    std::fs::write(&zstd_source, &nested_zstd).expect("standalone zstd fixture");
    let mut outer_tar = Vec::new();
    {
        let mut tar = tar::Builder::new(&mut outer_tar);
        let mut header = tar::Header::new_gnu();
        header.set_size(nested_zstd.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "opaque.bin", &nested_zstd[..])
            .expect("nested zstd entry");
        tar.finish().expect("outer tar finish");
    }
    let source = root.path().join("nested.tar.gz");
    let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut encoder = encoder;
    encoder.write_all(&outer_tar).expect("gzip outer tar");
    std::fs::write(&source, encoder.finish().expect("gzip finish")).expect("gzip fixture");

    let first_output = root.path().join("workspace-a");
    let first_plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&first_output),
        ArchiveCodec::TarGzip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("first extract plan");
    let accounting = ArchiveOperationAccounting::default();
    run_accounted(&first_plan, &ArchiveOperationLimits::default(), &accounting)
        .expect("nested gzip/zstd extraction");
    let peak = accounting.counters().peak_memory_bytes;

    let zstd_output = root.path().join("standalone-zstd");
    let zstd_plan = ArchiveOperationPlan::extract(
        local(&zstd_source),
        local(&zstd_output),
        ArchiveCodec::TarZstd,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("standalone zstd plan");
    let zstd_accounting = ArchiveOperationAccounting::default();
    run_accounted(
        &zstd_plan,
        &ArchiveOperationLimits::default(),
        &zstd_accounting,
    )
    .expect("standalone zstd extraction");
    let zstd_peak = zstd_accounting.counters().peak_memory_bytes;
    assert!(
        peak > zstd_peak + 32 * 1024,
        "nested codec workspaces did not overlap: nested={peak}, zstd={zstd_peak}"
    );
    assert_eq!(accounting.counters().memory_bytes, 0);
    assert_eq!(zstd_accounting.counters().memory_bytes, 0);

    let second_output = root.path().join("workspace-b");
    let second_plan = ArchiveOperationPlan::extract(
        local(&source),
        local(&second_output),
        ArchiveCodec::TarGzip,
        ArchiveConflictPolicy::Fail,
        false,
    )
    .expect("second extract plan");
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
fn production_expansion_ratio_and_nesting_limits_reject_real_bombs() {
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
    assert!(matches!(
        run(
            &nesting_plan,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
        ),
        Err(ArchiveOperationError::LimitExceeded {
            resource: "archive nesting",
            ..
        })
    ));
}
