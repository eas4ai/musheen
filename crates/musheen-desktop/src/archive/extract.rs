use super::budget::{
    ArchiveBudget, ArchiveMemoryLease, ArchiveOperationError, ArchiveOperationLimits, map_io,
};
use super::create::{
    ArchiveOperationOutcome, append_phase, local_path, publish_staging, remove_owned, staging_path,
    sync_parent,
};
use super::format::{ArchiveCopyContext, RawEntryKind, copy_entry, open_scanner};
use super::store::{ArchiveLimits, ArchivePasswordProvider, DecodeCounterState};
use super::{ArchiveFormat, ArchivePath};
use musheen_core::{CancellationToken, ProviderId};
use musheen_ops::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveOperationPlan, EventGeneration, JobId, Journal,
    JournalPhase, JournalStorage,
};
use nix::libc::O_NOFOLLOW;
use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
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

pub(crate) fn execute_extract<S: JournalStorage>(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
    journal: &mut Journal<S>,
    job_id: JobId,
    generation: EventGeneration,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    cancellation.check()?;
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
    let source_bytes = source_file
        .metadata()
        .map_err(|error| map_io(&error))?
        .len();
    let format = archive_format(plan.codec());
    let mut preflight_limits = limits.clone();
    preflight_limits.max_memory_bytes /= 2;
    let decode_limits = decode_limits(&preflight_limits);
    let mut budget = ArchiveBudget::new(preflight_limits);
    let entries = collect_extract_entries(
        &source_file,
        source_bytes,
        format,
        &decode_limits,
        passwords,
        cancellation,
        &mut budget,
    )?;
    append_phase(journal, job_id, generation, JournalPhase::Planned)?;

    let staging = staging_path(plan, job_id, generation)?;
    let mut published = false;
    let result = (|| {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(|error| map_io(&error))?;
        append_phase(journal, job_id, generation, JournalPhase::StagingCreated)?;
        let counters = DecodeCounterState::new();
        let mut actual_budget = ArchiveBudget::new(limits.clone());
        for entry in &entries {
            cancellation.check()?;
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
        append_phase(journal, job_id, generation, JournalPhase::DataCopied)?;
        append_phase(journal, job_id, generation, JournalPhase::MetadataApplied)?;
        let outcome = publish_staging(&staging, &destination, plan.conflict_policy())?;
        if outcome == ArchiveOperationOutcome::Skipped {
            remove_owned(&staging)?;
            append_phase(journal, job_id, generation, JournalPhase::RolledBack)?;
            return Ok(outcome);
        }
        published = true;
        sync_parent(&destination)?;
        append_phase(
            journal,
            job_id,
            generation,
            JournalPhase::DestinationPublished,
        )?;
        remove_owned(&staging)?;
        append_phase(journal, job_id, generation, JournalPhase::StagingCleaned)?;
        append_phase(journal, job_id, generation, JournalPhase::Completed)?;
        Ok(ArchiveOperationOutcome::Published)
    })();

    if result.is_err() && !published && staging.exists() {
        let _ = remove_owned(&staging);
        let _ = append_phase(journal, job_id, generation, JournalPhase::RolledBack);
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
) -> Result<Vec<ExtractEntry>, ArchiveOperationError> {
    let counters = DecodeCounterState::new();
    let provider = ProviderId::new("archive-extract").map_err(|_| ArchiveOperationError::Io)?;
    let mut scanner = open_scanner(source, provider, format, limits, &counters, passwords)?;
    let mut entries = Vec::new();
    let mut seen = HashSet::new();
    let mut total_expanded = 0_u64;
    let mut total_compressed = 0_u64;
    let mut all_file_sizes_known = true;
    while let Some(raw) = scanner.next_entry(cancellation)? {
        budget.charge_entry()?;
        budget.check_path(&raw.path)?;
        if !seen.insert(raw.path.clone()) {
            return Err(ArchiveOperationError::InvalidArchive);
        }
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
        budget.charge_temporary(size)?;
        if looks_like_archive(&raw.path) {
            budget.check_nesting(1)?;
        }
        let memory = budget.reserve_memory(
            u64::try_from(raw.path.len().saturating_add(128)).unwrap_or(u64::MAX),
        )?;
        entries.push(ExtractEntry {
            path: raw.path,
            kind,
            compressed_size: raw.compressed_size,
            ordinal: raw.ordinal,
            _memory: memory,
        });
    }
    let compressed_budget = if all_file_sizes_known {
        total_compressed.max(1)
    } else {
        source_bytes.max(1)
    };
    budget.charge_expanded(total_expanded, compressed_budget)?;
    Ok(entries)
}

fn looks_like_archive(path: &[u8]) -> bool {
    let lower = path.iter().map(u8::to_ascii_lowercase).collect::<Vec<_>>();
    [b".zip".as_slice(), b".7z", b".tar", b".tgz", b".tzst"]
        .iter()
        .any(|suffix| lower.ends_with(suffix))
        || lower.ends_with(b".tar.gz")
        || lower.ends_with(b".tar.zst")
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
