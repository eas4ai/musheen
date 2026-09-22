use musheen_core::{CancellationToken, StorePath};
use musheen_desktop::{
    ArchiveBudget, ArchiveError, ArchiveOperationError, ArchiveOperationLimits,
    ArchiveOperationOutcome, ArchivePassword, ArchivePasswordProvider, PasswordRequest,
    execute_archive_operation,
};
use musheen_ops::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveOperationPlan, CorruptSource, EventGeneration,
    JobId, Journal, JournalPhase, JournalStorage,
};
use std::io::{self, Cursor, Write};
use std::os::unix::fs::symlink;
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

fn run(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<(ArchiveOperationOutcome, Vec<JournalPhase>), ArchiveOperationError> {
    let mut journal = Journal::open(MemoryJournal::default()).expect("journal opens");
    let outcome = execute_archive_operation(
        plan,
        limits,
        passwords,
        cancellation,
        &mut journal,
        JobId::new(41).expect("job id"),
        EventGeneration::new(0),
    )?;
    let phases = journal
        .records()
        .iter()
        .map(|record| record.phase())
        .collect();
    Ok((outcome, phases))
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
fn conflict_cancellation_bad_password_and_cleanup_are_safe() {
    let root = tempdir().expect("temporary root");
    let source = root.path().join("source.txt");
    let archive = root.path().join("data.zip");
    std::fs::write(&source, b"new").expect("source");
    std::fs::write(&archive, b"old").expect("existing destination");

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
fn journal_failures_clean_unpublished_staging_and_retain_replace_recovery_data() {
    for fail_append_at in 1..=5 {
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
        let result = execute_archive_operation(
            &plan,
            &ArchiveOperationLimits::default(),
            &Passwords("unused"),
            &CancellationToken::new(),
            &mut journal,
            JobId::new(41).expect("job id"),
            EventGeneration::new(0),
        );
        assert!(matches!(result, Err(ArchiveOperationError::Journal)));
        let staging = root.path().join(".musheen-stage-v1-41-0");
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
            assert_eq!(
                std::fs::read(&staging).expect("old destination retained for recovery"),
                b"old destination"
            );
            assert_ne!(
                journal.records().last().map(|record| record.phase()),
                Some(JournalPhase::RolledBack)
            );
        }
    }
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

    let nested = fixtures.join("nested.zip");
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file("nested.zip", zip::write::SimpleFileOptions::default())
        .expect("nested entry");
    zip.write_all(b"PK\x03\x04").expect("nested bytes");
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
    let _lease = combined.reserve_memory(8).expect("memory");
    combined.charge_temporary(9).expect("temporary");
    assert_eq!(combined.counters().entries, 1);
    assert!(combined.charge_entry().is_err());
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
