use super::budget::{
    ArchiveBudget, ArchiveMemoryLease, ArchiveOperationAccounting, ArchiveOperationError,
    ArchiveOperationLimits, map_io,
};
use super::create::{
    ArchiveCleanupIntent, ArchiveOperationOutcome, ArchivePublicationPaths, append_archive_phase,
    cleanup_path, cleanup_phase, deletion_path, local_path, map_errno, path_identity_with_controls,
    publication_phase, publication_quarantine_path, publish_staging, remove_owned_journaled,
    remove_owned_journaled_by_identity, staging_path, sync_parent,
};
use super::format::{ArchiveCopyContext, RawEntryKind, copy_files_in_order, open_scanner};
use super::io::BoundedWriter;
use super::store::{ArchiveError, ArchiveLimits, ArchivePasswordProvider, DecodeCounterState};
use super::{ArchiveFormat, ArchivePath};
use musheen_core::{CancellationToken, ProviderId};
use musheen_ops::{
    AnsweredItem, ArchiveCleanupKind, ArchiveCodec, ArchiveConflictPolicy, ArchiveEventPhase,
    ArchiveOperationPlan, ArchivePathIdentity, EventGeneration, ExtractMerge, JobId, Journal,
    JournalPhase, JournalStorage,
};
use nix::libc::O_NOFOLLOW;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
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
    begin_commit: &mut dyn FnMut() -> Result<(), ArchiveOperationError>,
    accounting: &ArchiveOperationAccounting,
) -> Result<ArchiveOperationOutcome, ArchiveOperationError> {
    report_phase(ArchiveEventPhase::Preflight)?;
    cancellation.wait_if_paused()?;
    let source = local_path(
        plan.sources()
            .first()
            .ok_or(ArchiveOperationError::InvalidArchive)?,
    )?;
    let destination = local_path(plan.destination())?;
    // A merge needs a real folder at the destination. Anything else follows
    // the plan's conflict policy for the destination as a whole, and a merge
    // plan replaces such an item only while it is the one the user answered
    // about.
    let merge = match (plan.merge(), std::fs::symlink_metadata(&destination)) {
        (Some(merge), Ok(metadata)) if metadata.is_dir() => Some(merge),
        (Some(merge), Ok(metadata)) => {
            if !merge
                .whole()
                .is_some_and(|item| item.is(metadata.dev(), metadata.ino()))
            {
                return Err(ArchiveOperationError::Conflict);
            }
            None
        }
        _ => None,
    };
    if merge.is_none() && destination.exists() {
        match plan.conflict_policy() {
            ArchiveConflictPolicy::Fail => return Err(ArchiveOperationError::Conflict),
            ArchiveConflictPolicy::Skip => return Ok(ArchiveOperationOutcome::Skipped),
            ArchiveConflictPolicy::Replace => {}
        }
    }
    let parent = destination
        .parent()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive destination needs a parent",
        ))?;
    let operation_limits = extraction_limits(limits, parent)?;
    let mut budget = ArchiveBudget::with_accounting(operation_limits.clone(), accounting.clone())
        .with_identity_cancellation(cancellation.clone());
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
    // The archive is read where it is, never copied. Its identity is checked
    // again just before publishing, so nothing is published when it changed
    // during the run.
    let source_file = open_archive_source(&source)?;
    let source_before = SourceIdentity::of(&source_file)?;
    let source_bytes = source_before.size;
    let format = archive_format(plan.codec());
    let decode_limits = decode_limits(&operation_limits);
    let entries = collect_extract_entries(
        &source_file,
        source_bytes,
        format,
        &decode_limits,
        passwords,
        cancellation,
        &mut budget,
        &counters,
    )?;
    let staging = staging_path(plan, job_id, generation)?;
    let rollback = cleanup_path(&staging)?;
    let destination_before = if merge.is_some() {
        folder_identity(&destination)?
    } else {
        path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?
    };
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
    let mut transaction_started = false;
    let mut stage_root = None;
    let mut result = (|| {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&staging)
            .map_err(|error| map_io(&error))?;
        sync_parent(&staging)?;
        stage_root = path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?;
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
        for entry in entries
            .iter()
            .filter(|entry| matches!(entry.kind, ExtractEntryKind::Directory))
        {
            cancellation.wait_if_paused()?;
            let _path_memory = actual_budget.reserve_memory(
                u64::try_from(entry.path.len().saturating_add(256)).unwrap_or(u64::MAX),
            )?;
            actual_budget.charge_temporary(4 * 1_024)?;
            let output = output_path(&staging, &entry.path, limits.max_path_bytes)?;
            std::fs::create_dir_all(&output).map_err(|error| map_io(&error))?;
            std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| map_io(&error))?;
        }
        // Every file is decoded once, in archive order, in one pass.
        let mut files = entries
            .iter()
            .filter(|entry| matches!(entry.kind, ExtractEntryKind::File))
            .map(|entry| (entry.ordinal, entry))
            .collect::<Vec<_>>();
        files.sort_by_key(|(ordinal, _)| *ordinal);
        let _file_index_memory = actual_budget
            .reserve_memory(u64::try_from(files.len().saturating_mul(24)).unwrap_or(u64::MAX))?;
        let mut written = vec![false; files.len()];
        let mut failure = None;
        let position = |ordinal: u64| files.binary_search_by_key(&ordinal, |(key, _)| *key);
        let copied = copy_files_in_order(
            &source_file,
            format,
            &ArchiveCopyContext {
                passwords,
                limits: &decode_limits,
                counters: &counters,
                cancellation,
                compressed_size: None,
            },
            &|ordinal| position(ordinal).is_ok(),
            &mut |ordinal, contents| {
                let Ok(index) = position(ordinal) else {
                    return Ok(());
                };
                if written[index] {
                    return Err(ArchiveError::InvalidArchive);
                }
                written[index] = true;
                extract_file(
                    files[index].1,
                    contents,
                    &staging,
                    limits.max_path_bytes,
                    &decode_limits,
                    source_bytes,
                    &mut actual_budget,
                    cancellation,
                )
                .map_err(|error| {
                    failure = Some(error);
                    ArchiveError::Io
                })
            },
        );
        // A visit that fails returns `ArchiveError::Io` and leaves its own
        // error in `failure`. A reader that fails keeps its own error, which
        // names the cause the visit saw only as a failed read.
        match (copied, failure) {
            (Err(error), _) if error != ArchiveError::Io => return Err(error.into()),
            (_, Some(error)) => return Err(error),
            (copied, None) => copied.map_err(ArchiveOperationError::from)?,
        }
        if written.contains(&false) {
            return Err(ArchiveOperationError::InvalidArchive);
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
        let staging_before_publish =
            path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?
                .ok_or(ArchiveOperationError::Conflict)?;
        // The last check before publishing: the archive is still the one
        // that was listed and decoded.
        if SourceIdentity::of(&source_file)? != source_before {
            return Err(ArchiveOperationError::SourceChanged);
        }
        begin_commit()?;
        transaction_started = true;
        if let Some(merge) = merge {
            // The merge moves staged entries into the folder one by one. What
            // it does not move, including the items it replaced, stays staged
            // and is removed whether or not the merge finished; a merge error
            // is returned after that.
            let merged = merge_staging(&staging, &destination, merge, cancellation);
            published = merged.is_ok();
            sync_parent(&destination)?;
            let destination_after = folder_identity(&destination)?;
            if let Some(leftover) = folder_identity(&staging)? {
                let owned = stage_root.is_some_and(|created| {
                    created.device() == leftover.device() && created.inode() == leftover.inode()
                });
                if !owned {
                    return Err(ArchiveOperationError::RecoveryRequired);
                }
                let cleanup_cancellation = CancellationToken::new();
                let cleanup_budget = budget
                    .next_phase()
                    .with_identity_cancellation(cleanup_cancellation.clone());
                let stage_deletion = deletion_path(&staging)?;
                let cleanup = ArchiveCleanupIntent::new(
                    ArchiveCleanupKind::PrepublishStage,
                    &staging,
                    &stage_deletion,
                    Some(leftover),
                );
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    cleanup_phase(JournalPhase::PrepublishStageCleanupPlanned, cleanup),
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&cleanup_budget),
                )?;
                let mut quarantined = || {
                    append_archive_phase(
                        journal,
                        job_id,
                        generation,
                        cleanup_phase(JournalPhase::PrepublishStageCleanupQuarantined, cleanup),
                        plan,
                        &staging,
                        destination_before,
                        destination_after,
                        Some(&cleanup_budget),
                    )
                };
                remove_owned_journaled_by_identity(
                    &staging,
                    &stage_deletion,
                    leftover,
                    &cleanup_budget,
                    &cleanup_cancellation,
                    &mut quarantined,
                )
                .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
            }
            let phases: &[JournalPhase] = if merged.is_ok() {
                &[JournalPhase::StagingCleaned, JournalPhase::Completed]
            } else {
                &[JournalPhase::RolledBack]
            };
            for phase in phases {
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    *phase,
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&budget),
                )?;
            }
            merged?;
            return Ok(ArchiveOperationOutcome::Published);
        }
        let outcome = {
            let rollback_quarantine = deletion_path(&rollback)?;
            let payload_quarantine = publication_quarantine_path(&staging)?;
            let publication = ArchivePublicationPaths::new(
                &staging,
                &rollback,
                &rollback_quarantine,
                &payload_quarantine,
                destination_before,
            );
            let mut publication_checkpoint = |phase, destination_after, paths| {
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    publication_phase(phase, paths),
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&budget),
                )
            };
            publish_staging(
                publication,
                &destination,
                plan.conflict_policy(),
                staging_before_publish,
                destination_before,
                Some(&budget),
                Some(cancellation),
                &mut publication_checkpoint,
            )?
        };
        if outcome == ArchiveOperationOutcome::Skipped {
            if path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?
                != Some(staging_before_publish)
            {
                return Err(ArchiveOperationError::Conflict);
            }
            let stage_deletion = deletion_path(&staging)?;
            let cleanup = ArchiveCleanupIntent::new(
                ArchiveCleanupKind::PrepublishStage,
                &staging,
                &stage_deletion,
                Some(staging_before_publish),
            );
            append_archive_phase(
                journal,
                job_id,
                generation,
                cleanup_phase(JournalPhase::PrepublishStageCleanupPlanned, cleanup),
                plan,
                &staging,
                destination_before,
                None,
                Some(&budget),
            )?;
            {
                let mut quarantined = || {
                    append_archive_phase(
                        journal,
                        job_id,
                        generation,
                        cleanup_phase(JournalPhase::PrepublishStageCleanupQuarantined, cleanup),
                        plan,
                        &staging,
                        destination_before,
                        None,
                        Some(&budget),
                    )
                };
                remove_owned_journaled(
                    &staging,
                    &stage_deletion,
                    Some(staging_before_publish),
                    &budget,
                    cancellation,
                    &mut quarantined,
                )
                .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
            }
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
        let destination_after =
            path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?;
        let rollback_deletion = deletion_path(&rollback)?;
        let cleanup = ArchiveCleanupIntent::new(
            ArchiveCleanupKind::PublishedDestination,
            &rollback,
            &rollback_deletion,
            destination_before,
        );
        append_archive_phase(
            journal,
            job_id,
            generation,
            cleanup_phase(JournalPhase::PublishedDestinationCleanupPlanned, cleanup),
            plan,
            &staging,
            destination_before,
            destination_after,
            Some(&budget),
        )?;
        report_phase(ArchiveEventPhase::Cleaning)?;
        if path_identity_with_controls(&rollback, Some(&budget), Some(cancellation))?
            != destination_before
        {
            return Err(ArchiveOperationError::RecoveryRequired);
        }
        {
            let mut quarantined = || {
                append_archive_phase(
                    journal,
                    job_id,
                    generation,
                    cleanup_phase(
                        JournalPhase::PublishedDestinationCleanupQuarantined,
                        cleanup,
                    ),
                    plan,
                    &staging,
                    destination_before,
                    destination_after,
                    Some(&budget),
                )
            };
            remove_owned_journaled(
                &rollback,
                &rollback_deletion,
                destination_before,
                &budget,
                cancellation,
                &mut quarantined,
            )
            .map_err(|_| ArchiveOperationError::RecoveryRequired)?;
        }
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

    if !matches!(result, Err(ArchiveOperationError::RecoveryRequired))
        && result.is_err()
        && !published
        && !transaction_started
        && staging.exists()
    {
        let cleanup_cancellation = CancellationToken::new();
        let cleanup_budget = budget
            .next_phase()
            .with_identity_cancellation(cleanup_cancellation.clone());
        let stage_deletion = deletion_path(&staging)?;
        let cleanup_result = path_identity_with_controls(
            &staging,
            Some(&cleanup_budget),
            Some(&cleanup_cancellation),
        )
        .and_then(|cleanup_identity| {
            let cleanup_owned =
                stage_root
                    .zip(cleanup_identity)
                    .is_some_and(|(created, current)| {
                        created.device() == current.device() && created.inode() == current.inode()
                    });
            if !cleanup_owned {
                return Err(ArchiveOperationError::RecoveryRequired);
            }
            let cleanup = ArchiveCleanupIntent::new(
                ArchiveCleanupKind::PrepublishStage,
                &staging,
                &stage_deletion,
                cleanup_identity,
            );
            append_archive_phase(
                journal,
                job_id,
                generation,
                cleanup_phase(JournalPhase::PrepublishStageCleanupPlanned, cleanup),
                plan,
                &staging,
                destination_before,
                None,
                Some(&cleanup_budget),
            )
            .and_then(|()| {
                let mut quarantined = || {
                    append_archive_phase(
                        journal,
                        job_id,
                        generation,
                        cleanup_phase(JournalPhase::PrepublishStageCleanupQuarantined, cleanup),
                        plan,
                        &staging,
                        destination_before,
                        None,
                        Some(&cleanup_budget),
                    )
                };
                remove_owned_journaled(
                    &staging,
                    &stage_deletion,
                    cleanup_identity,
                    &cleanup_budget,
                    &cleanup_cancellation,
                    &mut quarantined,
                )
            })
        });
        if cleanup_result.is_ok() {
            if append_archive_phase(
                journal,
                job_id,
                generation,
                JournalPhase::RolledBack,
                plan,
                &staging,
                destination_before,
                None,
                Some(&cleanup_budget),
            )
            .is_err()
            {
                result = Err(ArchiveOperationError::RecoveryRequired);
            }
        } else {
            result = Err(ArchiveOperationError::RecoveryRequired);
        }
    }
    result
}

/// Moves staged entries into the existing destination folder. An entry the
/// folder lacks moves in whole; folders on both sides merge entry by entry;
/// any other collision is replaced only when `merge` names its path and the
/// item there is still the one the user answered about, and otherwise stays
/// staged for cleanup. A replaced item leaves the folder in the same step
/// that the entry arrives, and waits in the staging folder for its cleanup.
/// Every step works relative to opened folders and never follows a symlink
/// in the destination.
fn merge_staging(
    staging: &Path,
    destination: &Path,
    merge: &ExtractMerge,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let staged = open_directory(staging)?;
    let target = open_directory(destination)?;
    merge_directory(&staged, &target, &mut Vec::new(), merge, cancellation, 0)
}

fn open_directory(path: &Path) -> Result<File, ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, open};
    open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(map_errno)
}

fn open_child_directory(
    parent: &File,
    name: &std::ffi::CStr,
) -> Result<File, ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, openat};
    openat(
        parent,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(map_errno)
}

fn merge_directory(
    staged: &File,
    target: &File,
    relative: &mut Vec<u8>,
    merge: &ExtractMerge,
    cancellation: &CancellationToken,
    depth: usize,
) -> Result<(), ArchiveOperationError> {
    use rustix::fs::{AtFlags, Dir, FileType, RenameFlags, renameat_with, statat};
    use rustix::io::Errno;
    const MAX_MERGE_DEPTH: usize = 4_096;
    if depth > MAX_MERGE_DEPTH {
        return Err(ArchiveOperationError::UnsafePath(
            "archive folders nest too deep",
        ));
    }
    let mut names = Vec::new();
    for entry in Dir::read_from(staged).map_err(map_errno)? {
        let entry = entry.map_err(map_errno)?;
        let name = entry.file_name();
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            names.push(name.to_owned());
        }
    }
    names.sort();
    let is_directory = |mode| FileType::from_raw_mode(mode) == FileType::Directory;
    for name in names {
        cancellation.wait_if_paused()?;
        let mark = relative.len();
        if !relative.is_empty() {
            relative.push(b'/');
        }
        relative.extend_from_slice(name.to_bytes());
        let staged_is_directory = is_directory(
            statat(staged, &name, AtFlags::SYMLINK_NOFOLLOW)
                .map_err(map_errno)?
                .st_mode,
        );
        let moved = match statat(target, &name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(Errno::NOENT) => {
                renameat_with(staged, &name, target, &name, RenameFlags::NOREPLACE)
            }
            Err(error) => return Err(map_errno(error)),
            Ok(existing) if is_directory(existing.st_mode) && staged_is_directory => {
                let staged_child = open_child_directory(staged, &name)?;
                let target_child = open_child_directory(target, &name)?;
                merge_directory(
                    &staged_child,
                    &target_child,
                    relative,
                    merge,
                    cancellation,
                    depth.saturating_add(1),
                )?;
                Ok(())
            }
            Ok(existing) if !merge.replaces(relative, existing.st_dev, existing.st_ino) => Ok(()),
            // An item of the other kind trades places with the entry.
            Ok(existing) if is_directory(existing.st_mode) || staged_is_directory => {
                exchange_entry(staged, target, &name)
            }
            // A file over a file or a link: one rename replaces it in place.
            Ok(_) => renameat_with(staged, &name, target, &name, RenameFlags::empty()),
        };
        match moved {
            // An item that appeared after the question stays; the staged
            // entry is cleaned up with the rest of the staging.
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => return Err(map_errno(error)),
        }
        relative.truncate(mark);
    }
    target.sync_all().map_err(|error| map_io(&error))
}

/// Puts the staged entry `name` in place of the existing item of the other
/// kind, leaving that item in the staging folder for cleanup. One exchange
/// does it where the filesystem allows; elsewhere the item first moves
/// aside into the staging folder, and moves back if the entry cannot take
/// its place.
fn exchange_entry(
    staged: &File,
    target: &File,
    name: &std::ffi::CStr,
) -> Result<(), rustix::io::Errno> {
    use rustix::fs::{RenameFlags, renameat_with};
    use rustix::io::Errno;
    match renameat_with(staged, name, target, name, RenameFlags::EXCHANGE) {
        Err(Errno::INVAL | Errno::NOSYS | Errno::OPNOTSUPP) => {}
        other => return other,
    }
    let aside = (0_u32..)
        .map(|attempt| {
            std::ffi::CString::new(format!(".musheen-replaced-{attempt}"))
                .expect("the aside name has no NUL byte")
        })
        .find_map(|aside| {
            match renameat_with(target, name, staged, &aside, RenameFlags::NOREPLACE) {
                Err(Errno::EXIST) => None,
                moved => Some(moved.map(|()| aside)),
            }
        })
        .expect("an unused aside name exists")?;
    renameat_with(staged, name, target, name, RenameFlags::NOREPLACE).inspect_err(|_| {
        let _ = renameat_with(staged, &aside, target, name, RenameFlags::NOREPLACE);
    })
}

/// A folder's device, inode and kind, without reading what it holds. A merge
/// enters a folder the user keeps, which may hold links, devices, unreadable
/// files or more data than an identity digest may read.
fn folder_identity(path: &Path) -> Result<Option<ArchivePathIdentity>, ArchiveOperationError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(ArchivePathIdentity::new(
            metadata.dev(),
            metadata.ino(),
            0,
            0,
            0,
            metadata.is_dir(),
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(map_io(&error)),
    }
}

/// What an extraction would meet at its destination folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExtractDestination {
    /// The folder does not exist; the extraction publishes it in one step.
    Absent,
    /// An item other than a folder has the folder's name.
    NotAFolder(AnsweredItem),
    /// The folder exists; the items that collide with archive entries.
    Folder(Vec<ExtractCollision>),
}

/// An existing item an archive entry would replace, by its path relative to
/// the destination folder with `/` between components, and the item as it is
/// now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtractCollision {
    pub path: Vec<u8>,
    pub item: AnsweredItem,
}

/// Lists the collisions an extraction would meet, reading the archive's
/// entries as the extraction itself does and never following a symlink in
/// the destination. A folder that meets a folder merges and is no collision;
/// the entries under a colliding item follow that item's answer.
pub fn extract_destination(
    plan: &ArchiveOperationPlan,
    limits: &ArchiveOperationLimits,
    passwords: &dyn ArchivePasswordProvider,
    cancellation: &CancellationToken,
) -> Result<ExtractDestination, ArchiveOperationError> {
    let destination = local_path(plan.destination())?;
    match std::fs::symlink_metadata(&destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(ExtractDestination::Absent);
        }
        Err(error) => return Err(map_io(&error)),
        Ok(metadata) if !metadata.is_dir() => {
            return Ok(ExtractDestination::NotAFolder(AnsweredItem {
                device: metadata.dev(),
                inode: metadata.ino(),
                folder: false,
            }));
        }
        Ok(_) => {}
    }
    let source = local_path(
        plan.sources()
            .first()
            .ok_or(ArchiveOperationError::InvalidArchive)?,
    )?;
    let parent = destination
        .parent()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive destination needs a parent",
        ))?;
    let operation_limits = extraction_limits(limits, parent)?;
    let mut budget = ArchiveBudget::new(operation_limits.clone());
    let counters = DecodeCounterState::with_memory_budget(budget.shared_memory());
    let source_file = open_archive_source(&source)?;
    let source_bytes = source_file
        .metadata()
        .map_err(|error| map_io(&error))?
        .len();
    let entries = collect_extract_entries(
        &source_file,
        source_bytes,
        archive_format(plan.codec()),
        &decode_limits(&operation_limits),
        passwords,
        cancellation,
        &mut budget,
        &counters,
    )?;
    let mut collisions: Vec<ExtractCollision> = Vec::new();
    let mut collided = std::collections::HashSet::new();
    for entry in &entries {
        let relative = ArchivePath::normalize_bytes(&entry.path, limits.max_path_bytes)?;
        let components = relative.split(|byte| *byte == b'/').collect::<Vec<_>>();
        let mut prefix = Vec::new();
        for (index, component) in components.iter().enumerate() {
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(component);
            if collided.contains(&prefix) {
                break;
            }
            let entry_is_folder =
                index + 1 < components.len() || matches!(entry.kind, ExtractEntryKind::Directory);
            let existing = match std::fs::symlink_metadata(
                destination.join(OsString::from_vec(prefix.clone())),
            ) {
                Ok(existing) => existing,
                Err(error) if error.kind() == io::ErrorKind::NotFound => break,
                Err(error) => return Err(map_io(&error)),
            };
            if existing.is_dir() && entry_is_folder {
                continue;
            }
            collided.insert(prefix.clone());
            collisions.push(ExtractCollision {
                path: prefix.clone(),
                item: AnsweredItem {
                    device: existing.dev(),
                    inode: existing.ino(),
                    folder: existing.is_dir(),
                },
            });
            break;
        }
    }
    Ok(ExtractDestination::Folder(collisions))
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

/// The limits one extraction runs under: the operation's limits with the free
/// space beside the destination, and no time limit on its identity checks.
/// The entry, size, ratio, path, memory and temporary-space limits bound an
/// extraction, and Cancel stops it (OPS-034).
fn extraction_limits(
    limits: &ArchiveOperationLimits,
    parent: &Path,
) -> Result<ArchiveOperationLimits, ArchiveOperationError> {
    Ok(ArchiveOperationLimits {
        max_identity_millis: u64::MAX,
        ..limits.for_staging(parent)?
    })
}

/// What must stay the same about the archive while it is extracted from
/// where it is. Any write changes its size, modification or change time.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SourceIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

impl SourceIdentity {
    fn of(file: &File) -> Result<Self, ArchiveOperationError> {
        let metadata = file.metadata().map_err(|error| map_io(&error))?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

/// Writes one archive file into staging from its decoded bytes.
#[allow(clippy::too_many_arguments)]
fn extract_file(
    entry: &ExtractEntry,
    contents: &mut dyn Read,
    staging: &Path,
    max_path_bytes: usize,
    limits: &ArchiveLimits,
    source_bytes: u64,
    budget: &mut ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    cancellation.wait_if_paused()?;
    let _path_memory = budget
        .reserve_memory(u64::try_from(entry.path.len().saturating_add(256)).unwrap_or(u64::MAX))?;
    budget.charge_temporary(4 * 1_024)?;
    let output = output_path(staging, &entry.path, max_path_bytes)?;
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
        budget,
        source_bytes,
        error: None,
    };
    let mut bounded = BoundedWriter::new(
        &mut writer,
        limits.max_nested_archive_bytes,
        limits.max_expanded_bytes,
        entry
            .compressed_size
            .unwrap_or(source_bytes)
            .saturating_mul(limits.max_compression_ratio),
        cancellation.clone(),
        limits.max_elapsed,
    );
    let copied = io::copy(contents, &mut bounded);
    let limit = bounded.take_error();
    if let Some(error) = writer.error.take() {
        return Err(error);
    }
    if let Some(error) = limit {
        return Err(error.into());
    }
    if copied.is_err() {
        cancellation.wait_if_paused()?;
        return Err(ArchiveOperationError::InvalidArchive);
    }
    writer.inner.sync_all().map_err(|error| map_io(&error))
}

fn decode_limits(limits: &ArchiveOperationLimits) -> ArchiveLimits {
    ArchiveLimits {
        max_entries: usize::try_from(limits.max_entries).unwrap_or(usize::MAX),
        max_path_bytes: limits.max_path_bytes,
        max_metadata_bytes: usize::try_from(limits.max_memory_bytes).unwrap_or(usize::MAX),
        max_expanded_bytes: limits.max_expanded_bytes,
        max_compression_ratio: limits.max_compression_ratio,
        max_elapsed: Duration::MAX,
        // Extraction never opens an archive inside the archive.
        max_nested_archives: 0,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_cost_extraction_has_no_time_limit() {
        // The entry, size, ratio, path, memory and temporary-space limits
        // bound an extraction, and Cancel stops it (OPS-034).
        let limits = extraction_limits(&ArchiveOperationLimits::default(), &std::env::temp_dir())
            .expect("extraction limits");
        assert_eq!(limits.max_identity_millis, u64::MAX);
        assert_eq!(decode_limits(&limits).max_elapsed, Duration::MAX);
    }
}
