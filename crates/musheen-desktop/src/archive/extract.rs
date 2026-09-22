use super::budget::{
    ArchiveBudget, ArchiveMemoryLease, ArchiveOperationAccounting, ArchiveOperationError,
    ArchiveOperationLimits, map_io,
};
use super::create::{
    ArchiveOperationOutcome, append_archive_phase, file_identity, local_path, path_identity,
    publish_staging, remove_owned, staging_path, sync_parent,
};
use super::format::{ArchiveCopyContext, RawEntryKind, copy_entry, open_scanner};
use super::store::{ArchiveError, ArchiveLimits, ArchivePasswordProvider, DecodeCounterState};
use super::{ArchiveFormat, ArchivePath};
use super::{tar_codec, zip_codec};
use musheen_core::{CancellationToken, ProviderId};
use musheen_ops::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase, ArchiveOperationPlan, EventGeneration,
    JobId, Journal, JournalPhase, JournalStorage,
};
use nix::libc::O_NOFOLLOW;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

struct ExtractEntry {
    path: Vec<u8>,
    kind: ExtractEntryKind,
    compressed_size: Option<u64>,
    ordinal: u64,
    _memory: ArchiveMemoryLease,
}

#[derive(Clone, Copy)]
enum ExtractEntryKind {
    Directory,
    File,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_extract<S: JournalStorage>(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
    report_phase: &mut dyn FnMut(ArchiveEventPhase) -> Result<(), ArchiveOperationError>,
    accounting: &ArchiveOperationAccounting,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    report_phase(ArchiveEventPhase::Preflight)?;
    cancellation.wait_if_paused()?;
    let mut budget = ArchiveBudget::with_accounting(limits.clone(), accounting.clone());
    let plan_path_bytes = plan
        .sources()
        .iter()
        .chain(std::iter::once(plan.destination()))
        .map(|path| {
            path.as_unix_path()
                .map_or(0, |value| value.as_os_str().len())
        })
        .sum::<usize>();
    let _plan_paths_memory =
        budget.reserve_memory(u64::try_from(plan_path_bytes).unwrap_or(u64::MAX))?;
    let counters = DecodeCounterState::with_memory_budget(budget.shared_memory());
    let source = local_path(
        plan.sources()
            .first()
            .ok_or(ArchiveOperationError::InvalidArchive)?,
    )?;
    let destination = local_path(plan.destination())?;
    if destination.exists() {
        match plan.conflict_policy() {
            ArchiveConflictPolicy::Fail => return Err(ArchiveOperationError::Conflict),
            ArchiveConflictPolicy::Skip => return Ok(ArchiveOperationOutcome::Skipped),
            ArchiveConflictPolicy::Replace => {}
        }
    }
    let source_file = open_archive_source(&source)?;
    let source_identity = file_identity(&source_file)?;
    let source_bytes = source_file
        .metadata()
        .map_err(|error| map_io(&error))?
        .len();
    let format = archive_format(plan.codec());
    let decode_limits = decode_limits(limits);
    let entries = collect_extract_entries(
        &source_file,
        source_bytes,
        format,
        &decode_limits,
        passwords,
        cancellation,
        &mut budget,
        &counters,
        destination
            .parent()
            .ok_or(ArchiveOperationError::UnsafePath(
                "archive destination needs a parent",
            ))?,
    )?;
    let staging = staging_path(plan, job_id, generation)?;
    let destination_before = path_identity(&destination)?;
    append_archive_phase(
        journal,
        job_id,
        generation,
        JournalPhase::Planned,
        plan,
        &staging,
        destination_before,
        None,
        Some(&budget),
    )?;
    let mut published = false;
    let result = (|| {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(|error| map_io(&error))?;
        sync_parent(&staging)?;
        report_phase(ArchiveEventPhase::Staging)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::StagingCreated,
            plan,
            &staging,
            destination_before,
            None,
            Some(&budget),
        )?;
        report_phase(ArchiveEventPhase::Decoding)?;
        let mut actual_budget = budget.next_phase();
        for entry in &entries {
            cancellation.wait_if_paused()?;
            let _path_memory = actual_budget.reserve_memory(
                u64::try_from(entry.path.len().saturating_add(256)).unwrap_or(u64::MAX),
            )?;
            actual_budget.charge_temporary(4 * 1_024)?;
            let output = output_path(&staging, &entry.path, limits.max_path_bytes)?;
            match entry.kind {
                ExtractEntryKind::Directory => {
                    std::fs::create_dir_all(&output).map_err(|error| map_io(&error))?;
                    std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700))
                        .map_err(|error| map_io(&error))?;
                }
                ExtractEntryKind::File => {
                    let parent = output.parent().ok_or(ArchiveOperationError::UnsafePath(
                        "archive entry needs a parent",
                    ))?;
                    std::fs::create_dir_all(parent).map_err(|error| map_io(&error))?;
                    let file = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .custom_flags(O_NOFOLLOW)
                        .mode(0o600)
                        .open(&output)
                        .map_err(|error| map_io(&error))?;
                    let mut writer = BudgetWriter {
                        inner: file,
                        budget: &mut actual_budget,
                        source_bytes,
                        error: None,
                    };
                    let copy_result = copy_entry(
                        &source_file,
                        format,
                        entry.ordinal,
                        &mut writer,
                        ArchiveCopyContext {
                            passwords,
                            limits: &decode_limits,
                            counters: &counters,
                            cancellation,
                            compressed_size: entry.compressed_size,
                        },
                    );
                    if let Some(error) = writer.error.take() {
                        return Err(error);
                    }
                    copy_result.map_err(ArchiveOperationError::from)?;
                    writer.inner.sync_all().map_err(|error| map_io(&error))?;
                }
            }
        }
        sync_tree(&staging)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::DataCopied,
            plan,
            &staging,
            destination_before,
            None,
            Some(&budget),
        )?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::MetadataApplied,
            plan,
            &staging,
            destination_before,
            None,
            Some(&budget),
        )?;
        report_phase(ArchiveEventPhase::Publishing)?;
        cancellation.wait_if_paused()?;
        if path_identity(&source)? != Some(source_identity)
            || file_identity(&source_file)? != source_identity
        {
            return Err(ArchiveOperationError::Conflict);
        }
        let staging_before_publish = path_identity(&staging)?;
        let outcome = publish_staging(
            &staging,
            &destination,
            plan.conflict_policy(),
            destination_before,
        )?;
        if outcome == ArchiveOperationOutcome::Skipped {
            if path_identity(&staging)? != staging_before_publish {
                return Err(ArchiveOperationError::Conflict);
            }
            remove_owned(&staging)?;
            append_archive_phase(
                journal,
                job_id,
                generation,
                JournalPhase::RolledBack,
                plan,
                &staging,
                destination_before,
                None,
                Some(&budget),
            )?;
            return Ok(outcome);
        }
        published = true;
        sync_parent(&destination)?;
        let destination_after = path_identity(&destination)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::DestinationPublished,
            plan,
            &staging,
            destination_before,
            destination_after,
            Some(&budget),
        )?;
        report_phase(ArchiveEventPhase::Cleaning)?;
        if path_identity(&staging)? != destination_before {
            return Err(ArchiveOperationError::Conflict);
        }
        remove_owned(&staging)?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::StagingCleaned,
            plan,
            &staging,
            destination_before,
            destination_after,
            Some(&budget),
        )?;
        append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::Completed,
            plan,
            &staging,
            destination_before,
            destination_after,
            Some(&budget),
        )?;
        Ok(ArchiveOperationOutcome::Published)
    })();

    if result.is_err() && !published && staging.exists() {
        let _ = remove_owned(&staging);
        let _ = append_archive_phase(
            journal,
            job_id,
            generation,
            JournalPhase::RolledBack,
            plan,
            &staging,
            destination_before,
            None,
            Some(&budget),
        );
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn collect_extract_entries(
    source: &File,
    source_bytes: u64,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    budget: &mut ArchiveBudget,
    counters: &std::sync::Arc<DecodeCounterState>,
    temporary_root: &Path,
) -> Result<Vec<ExtractEntry>, ArchiveOperationError> {
    let provider = ProviderId::new("archive-extract").map_err(|_| ArchiveOperationError::Io)?;
    let mut scanner = open_scanner(source, provider, format, limits, counters, passwords)?;
    let mut entries = Vec::new();
    let mut total_expanded = 0_u64;
    let mut total_compressed = 0_u64;
    let mut all_file_sizes_known = true;
    while let Some(raw) = scanner.next_entry(cancellation)? {
        budget.charge_entry()?;
        budget.check_path(&raw.path)?;
        let memory = budget.reserve_memory(
            u64::try_from(
                raw.path
                    .len()
                    .saturating_add(std::mem::size_of::<ExtractEntry>()),
            )
            .unwrap_or(u64::MAX),
        )?;
        entries
            .try_reserve_exact(1)
            .map_err(|_| ArchiveOperationError::Io)?;
        let kind = match raw.kind {
            RawEntryKind::Directory => ExtractEntryKind::Directory,
            RawEntryKind::RegularFile => ExtractEntryKind::File,
            RawEntryKind::SymbolicLink | RawEntryKind::HardLink | RawEntryKind::Other => {
                return Err(ArchiveOperationError::UnsupportedFileType);
            }
        };
        let size = raw.size.unwrap_or(0);
        total_expanded =
            total_expanded
                .checked_add(size)
                .ok_or(ArchiveOperationError::LimitExceeded {
                    resource: "expanded bytes",
                    value: u64::MAX,
                    maximum: limits.max_expanded_bytes,
                })?;
        budget.check_expanded(total_expanded)?;
        if matches!(kind, ExtractEntryKind::File) {
            if let Some(compressed_size) = raw.compressed_size {
                total_compressed = total_compressed.checked_add(compressed_size).ok_or(
                    ArchiveOperationError::LimitExceeded {
                        resource: "compressed bytes",
                        value: u64::MAX,
                        maximum: u64::MAX,
                    },
                )?;
            } else {
                all_file_sizes_known = false;
            }
        }
        if matches!(kind, ExtractEntryKind::File) {
            inspect_nested_entry(
                source,
                format,
                raw.ordinal,
                raw.compressed_size,
                limits,
                passwords,
                cancellation,
                budget,
                counters,
                temporary_root,
                1,
            )?;
        }
        entries.push(ExtractEntry {
            path: raw.path,
            kind,
            compressed_size: raw.compressed_size,
            ordinal: raw.ordinal,
            _memory: memory,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    if entries.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err(ArchiveOperationError::InvalidArchive);
    }
    let compressed_budget = if all_file_sizes_known {
        total_compressed.max(1)
    } else {
        source_bytes.max(1)
    };
    budget.charge_expanded(total_expanded, compressed_budget)?;
    Ok(entries)
}

#[allow(clippy::too_many_arguments)]
fn inspect_nested_entry(
    source: &File,
    format: ArchiveFormat,
    ordinal: u64,
    compressed_size: Option<u64>,
    limits: &ArchiveLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    budget: &mut ArchiveBudget,
    counters: &std::sync::Arc<DecodeCounterState>,
    temporary_root: &Path,
    depth: usize,
) -> Result<(), ArchiveOperationError> {
    let temporary =
        tempfile::NamedTempFile::new_in(temporary_root).map_err(|error| map_io(&error))?;
    let error = std::rc::Rc::new(std::cell::RefCell::new(None));
    let mut writer = NestedBudgetWriter {
        inner: temporary.reopen().map_err(|error| map_io(&error))?,
        budget,
        error: std::rc::Rc::clone(&error),
    };
    let copy_result = copy_entry(
        source,
        format,
        ordinal,
        &mut writer,
        ArchiveCopyContext {
            passwords,
            limits,
            counters,
            cancellation,
            compressed_size,
        },
    );
    if let Some(error) = error.borrow_mut().take() {
        return Err(error);
    }
    copy_result.map_err(ArchiveOperationError::from)?;
    writer.inner.flush().map_err(|error| map_io(&error))?;
    let NestedBudgetWriter { mut inner, .. } = writer;
    inner
        .seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let Some(nested_format) = detect_archive_format(&mut inner, limits, counters)? else {
        return Ok(());
    };
    budget.check_nesting(depth)?;
    scan_nested_archive(
        &inner,
        nested_format,
        limits,
        passwords,
        cancellation,
        budget,
        counters,
        temporary_root,
        depth,
    )
}

#[allow(clippy::too_many_arguments)]
fn scan_nested_archive(
    source: &File,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    budget: &mut ArchiveBudget,
    counters: &std::sync::Arc<DecodeCounterState>,
    temporary_root: &Path,
    depth: usize,
) -> Result<(), ArchiveOperationError> {
    let provider = ProviderId::new("archive-nested").map_err(|_| ArchiveOperationError::Io)?;
    let mut scanner = open_scanner(source, provider, format, limits, counters, passwords)?;
    let source_bytes = source
        .metadata()
        .map_err(|error| map_io(&error))?
        .len()
        .max(1);
    let mut expanded = 0_u64;
    let mut compressed = 0_u64;
    while let Some(raw) = scanner.next_entry(cancellation)? {
        budget.charge_entry()?;
        budget.check_path(&raw.path)?;
        match raw.kind {
            RawEntryKind::Directory => {}
            RawEntryKind::RegularFile => {
                let size = raw.size.unwrap_or(0);
                expanded = expanded.saturating_add(size);
                compressed = compressed.saturating_add(raw.compressed_size.unwrap_or(0));
                inspect_nested_entry(
                    source,
                    format,
                    raw.ordinal,
                    raw.compressed_size,
                    limits,
                    passwords,
                    cancellation,
                    budget,
                    counters,
                    temporary_root,
                    depth.saturating_add(1),
                )?;
            }
            RawEntryKind::SymbolicLink | RawEntryKind::HardLink | RawEntryKind::Other => {
                return Err(ArchiveOperationError::UnsupportedFileType);
            }
        }
    }
    budget.check_compression_ratio(expanded, compressed.max(source_bytes))?;
    budget.charge_expanded_bytes(expanded)
}

fn detect_archive_format(
    file: &mut File,
    limits: &ArchiveLimits,
    counters: &std::sync::Arc<DecodeCounterState>,
) -> Result<Option<ArchiveFormat>, ArchiveOperationError> {
    match zip_codec::preflight(file, limits, counters) {
        Ok(_) => {
            file.seek(SeekFrom::Start(0))
                .map_err(|error| map_io(&error))?;
            return Ok(Some(ArchiveFormat::Zip));
        }
        Err(ArchiveError::InvalidArchive) => {}
        Err(error) => return Err(error.into()),
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let mut prefix = [0_u8; 1_024];
    let count = file.read(&mut prefix).map_err(|error| map_io(&error))?;
    file.seek(SeekFrom::Start(0))
        .map_err(|error| map_io(&error))?;
    let bytes = &prefix[..count];
    let format = if bytes.starts_with(b"7z\xBC\xAF\x27\x1C") {
        Some(ArchiveFormat::SevenZip)
    } else if bytes.starts_with(&[0x1f, 0x8b]) {
        Some(ArchiveFormat::TarGzip)
    } else if bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd])
        || bytes.get(..4).is_some_and(|magic| {
            (0x184d_2a50..=0x184d_2a5f).contains(&u32::from_le_bytes([
                magic[0], magic[1], magic[2], magic[3],
            ]))
        })
    {
        Some(ArchiveFormat::TarZstd)
    } else if count == prefix.len()
        && (prefix.iter().all(|byte| *byte == 0)
            || tar_codec::valid_tar_checksum(
                prefix[..512]
                    .try_into()
                    .map_err(|_| ArchiveOperationError::InvalidArchive)?,
            ))
    {
        Some(ArchiveFormat::Tar)
    } else {
        None
    };
    Ok(format)
}

struct NestedBudgetWriter<'a> {
    inner: File,
    budget: &'a mut ArchiveBudget,
    error: std::rc::Rc<std::cell::RefCell<Option<ArchiveOperationError>>>,
}

impl Write for NestedBudgetWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if let Err(error) = self.budget.charge_temporary(count) {
            *self.error.borrow_mut() = Some(error);
            return Err(io::Error::other(
                "nested archive temporary-space budget exceeded",
            ));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn output_path(
    root: &Path,
    raw: &[u8],
    max_path_bytes: usize,
) -> Result<PathBuf, ArchiveOperationError> {
    let normalized = ArchivePath::normalize_bytes(raw, max_path_bytes)?;
    if normalized.is_empty() {
        return Err(ArchiveOperationError::UnsafePath(
            "archive entries need a file name",
        ));
    }
    let relative = PathBuf::from(OsString::from_vec(normalized));
    let output = root.join(relative);
    if !output.starts_with(root) {
        return Err(ArchiveOperationError::UnsafePath(
            "archive entry escaped the staging root",
        ));
    }
    Ok(output)
}

fn open_archive_source(path: &Path) -> Result<File, ArchiveOperationError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(path)
        .map_err(|error| map_io(&error))?;
    if !file.metadata().map_err(|error| map_io(&error))?.is_file() {
        return Err(ArchiveOperationError::UnsupportedFileType);
    }
    Ok(file)
}

fn archive_format(codec: ArchiveCodec) -> ArchiveFormat {
    match codec {
        ArchiveCodec::Zip => ArchiveFormat::Zip,
        ArchiveCodec::Tar => ArchiveFormat::Tar,
        ArchiveCodec::TarGzip => ArchiveFormat::TarGzip,
        ArchiveCodec::TarZstd => ArchiveFormat::TarZstd,
        ArchiveCodec::SevenZip => ArchiveFormat::SevenZip,
    }
}

fn decode_limits(limits: &ArchiveOperationLimits) -> ArchiveLimits {
    ArchiveLimits {
        max_entries: usize::try_from(limits.max_entries).unwrap_or(usize::MAX),
        max_path_bytes: limits.max_path_bytes,
        max_metadata_bytes: usize::try_from(limits.max_memory_bytes).unwrap_or(usize::MAX),
        max_expanded_bytes: limits.max_expanded_bytes,
        max_compression_ratio: limits.max_compression_ratio,
        max_elapsed: Duration::from_secs(30),
        max_nested_archives: limits.max_nesting,
        max_nested_archive_bytes: limits.max_temporary_bytes,
    }
}

struct BudgetWriter<'a> {
    inner: File,
    budget: &'a mut ArchiveBudget,
    source_bytes: u64,
    error: Option<ArchiveOperationError>,
}

impl Write for BudgetWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if let Err(error) = self.budget.check_stage_write(count) {
            self.error = Some(map_io(&error));
            return Err(error);
        }
        if let Err(error) = self
            .budget
            .charge_expanded(count, self.source_bytes.max(1))
            .and_then(|()| self.budget.charge_temporary(count))
        {
            self.error = Some(error);
            return Err(io::Error::other("archive operation budget exceeded"));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn sync_tree(root: &Path) -> Result<(), ArchiveOperationError> {
    let mut directories = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.map_err(|_| ArchiveOperationError::Io)?;
        if entry.file_type().is_dir() {
            directories.push(entry.path().to_path_buf());
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        File::open(directory)
            .and_then(|file| file.sync_all())
            .map_err(|error| map_io(&error))?;
    }
    Ok(())
}
