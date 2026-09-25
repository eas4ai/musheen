use crate::operation::{remove_path, sync_parent};
use crate::{LocalStore, TrashDisposition, TrashEnvironment};
use musheen_core::{CancellationToken, CapabilityKind, CapabilityState, StorePath};
use musheen_ops::{
    AclChange, AclEntry, AclQualifier, ConflictChoice, ConflictDecision, CopyRequest, CopySession,
    CreateKind, DeleteProvider, DeleteTarget, LinkProvider, MetadataEntry, MetadataEntryKind,
    MetadataProvider, MetadataScope, MoveMetadataReview, MutationError, MutationProvider,
    OperationFailure, OperationKind, ResolvedMetadataChange, StagingPath, TrashReceipt,
    execute_move,
};
use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, PosixACL, Qualifier};
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, CWD, Gid, Mode, OFlags, RenameFlags, StatxFlags, Uid, chownat, fchmod, fchown, fsync,
    linkat, mkdirat, open, openat, renameat_with, statx, symlinkat, unlinkat,
};
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use walkdir::WalkDir;

static TRASH_LOCK: Mutex<()> = Mutex::new(());
static NEXT_RESTORE_NAME: AtomicU64 = AtomicU64::new(1);
#[cfg(test)]
static TRASH_LISTINGS: AtomicU64 = AtomicU64::new(0);

/// How many times this process has listed the trash. Tests use it to show
/// that remembering and checking an undo never lists the trash.
#[cfg(test)]
pub(crate) fn trash_listings() -> u64 {
    TRASH_LISTINGS.load(Ordering::Relaxed)
}

const REPLACE_BACKUP_PREFIX: &str = ".musheen-replace-backup-v1-";
const REPLACE_JOURNAL_PREFIX: &str = ".musheen-replace-journal-v1-";
const REPLACE_MERGING_PREFIX: &str = ".musheen-replace-merging-v1-";
const REPLACE_MERGING_MARKER: &[u8] = b"musheen-replacement-merging-v1\n";
const REPLACE_PUBLISHED_PREFIX: &str = ".musheen-replace-published-v1-";
const REPLACE_PUBLISHED_MARKER: &[u8] = b"musheen-replacement-published-v1\n";

#[derive(Debug)]
struct ReplacementJournal {
    version: u8,
    job_id: u64,
    generation: u64,
    nonce: [u8; 16],
    destination: Vec<u8>,
    backup: Vec<u8>,
    staging: Vec<u8>,
}

struct ReplacementTransaction {
    backup: PathBuf,
    journal: PathBuf,
    merging: PathBuf,
    published: PathBuf,
}

#[derive(Debug)]
pub(crate) enum ResolvedTransferFailure {
    Transfer(OperationFailure),
    Failed(Box<str>),
    NeedsAttention(Box<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedTransferOutcome {
    Skipped,
    Completed {
        path: StorePath,
        metadata_review: Option<Box<MoveMetadataReview>>,
    },
}

impl From<OperationFailure> for ResolvedTransferFailure {
    fn from(error: OperationFailure) -> Self {
        Self::Transfer(error)
    }
}

impl From<Box<str>> for ResolvedTransferFailure {
    fn from(message: Box<str>) -> Self {
        Self::Failed(message)
    }
}

impl fmt::Display for ResolvedTransferFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transfer(error) => error.fmt(formatter),
            Self::Failed(message) | Self::NeedsAttention(message) => formatter.write_str(message),
        }
    }
}

fn resolved_move_aside_failure(error: MutationError) -> ResolvedTransferFailure {
    match error {
        MutationError::RecoveryRequired(message) => {
            ResolvedTransferFailure::NeedsAttention(message)
        }
        error => ResolvedTransferFailure::Failed(error.to_string().into()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalTrashEntry {
    receipt: TrashReceipt,
    deleted_at_unix_seconds: i64,
    kind: musheen_ops::ConflictItemKind,
    restorable: bool,
}

impl LocalTrashEntry {
    #[must_use]
    pub const fn new(
        receipt: TrashReceipt,
        deleted_at_unix_seconds: i64,
        kind: musheen_ops::ConflictItemKind,
        restorable: bool,
    ) -> Self {
        Self {
            receipt,
            deleted_at_unix_seconds,
            kind,
            restorable,
        }
    }

    #[must_use]
    pub const fn receipt(&self) -> &TrashReceipt {
        &self.receipt
    }

    #[must_use]
    pub const fn deleted_at_unix_seconds(&self) -> i64 {
        self.deleted_at_unix_seconds
    }

    #[must_use]
    pub const fn kind(&self) -> musheen_ops::ConflictItemKind {
        self.kind
    }

    /// Whether the entry's data is still in Trash. An entry whose data is
    /// missing or unreadable is listed so it can be purged, but it cannot be
    /// restored.
    #[must_use]
    pub const fn restorable(&self) -> bool {
        self.restorable
    }
}

impl LocalStore {
    pub fn recover_replacements_at(&mut self, location: &StorePath) -> Result<(), MutationError> {
        let location = location.as_unix_path().ok_or(MutationError::Unsupported)?;
        let parent = location.parent().ok_or(MutationError::InvalidScope)?;
        let entries = match fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(map_io_error(error)),
        };
        let mut journals = Vec::new();
        for entry in entries {
            let path = entry.map_err(map_io_error)?.path();
            if path.file_name().is_some_and(|name| {
                name.as_bytes()
                    .starts_with(REPLACE_JOURNAL_PREFIX.as_bytes())
            }) {
                journals.push(path);
            }
        }
        for journal_path in journals {
            recover_replacement_journal(&journal_path)?;
        }
        Ok(())
    }

    pub fn cleanup_stale_staging_at(
        &mut self,
        location: &StorePath,
        protected: &[StorePath],
    ) -> Result<usize, MutationError> {
        let location = location.as_unix_path().ok_or(MutationError::Unsupported)?;
        let parent = location.parent().ok_or(MutationError::InvalidScope)?;
        let entries = match fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(map_io_error(error)),
        };
        let mut removed = 0_usize;
        for entry in entries {
            let path = entry.map_err(map_io_error)?.path();
            let staging = StorePath::from_unix_path(path.as_os_str());
            if !StagingPath::is_owned_path(&staging)
                || protected.iter().any(|protected| protected == &staging)
            {
                continue;
            }
            remove_path(&path)
                .map_err(|error| MutationError::Provider(error.to_string().into()))?;
            removed = removed.saturating_add(1);
        }
        if removed > 0 {
            sync_parent(location).map_err(|error| {
                MutationError::RecoveryRequired(
                    format!(
                        "stale staging was removed, but the directory update could not be made \
                         durable: {error}"
                    )
                    .into(),
                )
            })?;
        }
        Ok(removed)
    }

    #[must_use]
    pub fn recovery_staging_available(&self, staging: &StorePath) -> bool {
        StagingPath::is_owned_path(staging)
            && staging
                .as_unix_path()
                .is_some_and(|path| fs::symlink_metadata(path).is_ok())
    }

    pub fn discard_recovery_staging(&mut self, staging: &StorePath) -> Result<(), MutationError> {
        if !StagingPath::is_owned_path(staging) {
            return Err(MutationError::InvalidScope);
        }
        let staging = staging.as_unix_path().ok_or(MutationError::Unsupported)?;
        remove_path(staging).map_err(|error| MutationError::Provider(error.to_string().into()))?;
        sync_parent(staging).map_err(|error| {
            MutationError::RecoveryRequired(
                format!(
                    "recovery staging was removed, but the directory update could not be made \
                     durable: {error}"
                )
                .into(),
            )
        })
    }

    pub fn conflict_item_kind(
        &self,
        path: &StorePath,
    ) -> Result<musheen_ops::ConflictItemKind, MutationError> {
        let path = path.as_unix_path().ok_or(MutationError::Unsupported)?;
        let metadata = fs::symlink_metadata(path).map_err(map_io_error)?;
        Ok(if metadata.file_type().is_symlink() {
            musheen_ops::ConflictItemKind::SymbolicLink
        } else if metadata.is_dir() {
            musheen_ops::ConflictItemKind::Directory
        } else {
            musheen_ops::ConflictItemKind::File
        })
    }

    pub fn list_trash(&mut self) -> Result<Vec<LocalTrashEntry>, MutationError> {
        let _guard = TRASH_LOCK
            .lock()
            .map_err(|_| MutationError::Provider("trash lock was poisoned".into()))?;
        #[cfg(test)]
        TRASH_LISTINGS.fetch_add(1, Ordering::Relaxed);
        let listed = trash::os_limited::list().map_err(map_trash_error)?;
        let listed_ids = listed
            .iter()
            .map(|item| item.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let mut entries = listed
            .into_iter()
            .map(|item| {
                // An entry whose data is missing or unreadable stays in the
                // listing as unrestorable; it never hides the other entries.
                let (kind, restorable) = match fs::symlink_metadata(trash_payload_path(&item)?) {
                    Ok(metadata) => (trash_payload_kind(&metadata), true),
                    Err(_) => (musheen_ops::ConflictItemKind::File, false),
                };
                Ok(LocalTrashEntry {
                    receipt: TrashReceipt::new(
                        StorePath::from_unix_path(item.original_path().into_os_string()),
                        item.id.as_bytes().to_vec(),
                    ),
                    deleted_at_unix_seconds: item.time_deleted,
                    kind,
                    restorable,
                })
            })
            .collect::<Result<Vec<_>, MutationError>>()?;
        entries.extend(unreadable_trash_entries(&listed_ids));
        entries.sort_by_key(|entry| std::cmp::Reverse(entry.deleted_at_unix_seconds));
        Ok(entries)
    }

    pub fn purge_trash(&mut self, receipts: &[TrashReceipt]) -> Result<(), MutationError> {
        if receipts.is_empty() {
            return Err(MutationError::InvalidScope);
        }
        let _guard = TRASH_LOCK
            .lock()
            .map_err(|_| MutationError::Provider("trash lock was poisoned".into()))?;
        let listed_ids = trash::os_limited::list()
            .map_err(map_trash_error)?
            .into_iter()
            .map(|item| item.id)
            .collect::<std::collections::HashSet<_>>();
        let unreadable_ids = unreadable_trash_entries(&listed_ids)
            .into_iter()
            .map(|entry| OsString::from_vec(entry.receipt.provider_reference().to_vec()))
            .collect::<std::collections::HashSet<_>>();
        let mut requested = std::collections::HashSet::with_capacity(receipts.len());
        let mut info_files = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            if !requested.insert(receipt.provider_reference().to_vec()) {
                return Err(MutationError::BatchCollision);
            }
            let id = OsString::from_vec(receipt.provider_reference().to_vec());
            if !listed_ids.contains(&id) && !unreadable_ids.contains(&id) {
                return Err(MutationError::Missing);
            }
            info_files.push(PathBuf::from(id));
        }
        purge_each(&info_files, |info| purge_trash_record(info))
    }

    pub fn resolve_restore_conflict(
        &mut self,
        receipt: &TrashReceipt,
        decision: &ConflictDecision,
    ) -> Result<(), MutationError> {
        if decision.operation() != OperationKind::Restore
            || decision.destination() != receipt.original_path()
            || decision.source_identity() != receipt.provider_reference()
        {
            return Err(MutationError::InvalidScope);
        }
        let _guard = TRASH_LOCK
            .lock()
            .map_err(|_| MutationError::Provider("trash lock was poisoned".into()))?;
        let source_id = OsString::from_vec(receipt.provider_reference().to_vec());
        let source_item = trash::os_limited::list()
            .map_err(map_trash_error)?
            .into_iter()
            .find(|item| item.id == source_id)
            .ok_or(MutationError::Missing)?;
        let current = MutationProvider::identity(self, receipt.original_path())?
            .ok_or(MutationError::Missing)?;
        if current.as_ref() != decision.destination_identity() {
            return Err(MutationError::SourceChanged);
        }
        match decision.choice() {
            ConflictChoice::Skip => Ok(()),
            ConflictChoice::KeepBoth => {
                self.restore_after_moving_destination(receipt, RestoreDisposition::KeepBoth)
            }
            ConflictChoice::Replace | ConflictChoice::ReplaceTree => {
                self.restore_after_moving_destination(receipt, RestoreDisposition::Replace)
            }
            ConflictChoice::MergeDirectory => merge_restored_directory(
                &source_item,
                receipt
                    .original_path()
                    .as_unix_path()
                    .ok_or(MutationError::Unsupported)?,
            ),
        }
    }

    fn restore_after_moving_destination(
        &mut self,
        receipt: &TrashReceipt,
        disposition: RestoreDisposition,
    ) -> Result<(), MutationError> {
        let destination = receipt
            .original_path()
            .as_unix_path()
            .ok_or(MutationError::Unsupported)?;
        let moved = move_destination_aside(destination, disposition)?;
        if let Err(error) = restore_receipt_no_replace(receipt) {
            renameat_with(CWD, &moved, CWD, destination, RenameFlags::NOREPLACE).map_err(
                |rollback| {
                    MutationError::RecoveryRequired(
                        format!(
                            "restore failed ({error}); the original destination remains at {} \
                             because rollback failed: {rollback}",
                            moved.display()
                        )
                        .into(),
                    )
                },
            )?;
            sync_parent(destination).map_err(|rollback| {
                MutationError::RecoveryRequired(
                    format!(
                        "restore failed ({error}); the original destination was restored, but \
                         rollback could not be made durable: {rollback}"
                    )
                    .into(),
                )
            })?;
            return Err(error);
        }
        sync_parent(destination).map_err(|error| {
            MutationError::RecoveryRequired(
                format!(
                    "the trashed item was restored at {}, but the directory update could not be \
                     made durable: {error}; the previous destination remains at {}",
                    destination.display(),
                    moved.display()
                )
                .into(),
            )
        })?;
        if disposition == RestoreDisposition::Replace {
            remove_path(&moved).map_err(|error| {
                MutationError::RecoveryRequired(
                    format!(
                        "the trashed item was restored, but the previous destination remains at \
                         {}: {error}",
                        moved.display()
                    )
                    .into(),
                )
            })?;
            sync_parent(&moved).map_err(|error| {
                MutationError::RecoveryRequired(
                    format!(
                        "the trashed item was restored, but removing the previous destination \
                         could not be made durable: {error}"
                    )
                    .into(),
                )
            })?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RestoreDisposition {
    KeepBoth,
    Replace,
}

fn move_destination_aside(
    destination: &Path,
    disposition: RestoreDisposition,
) -> Result<PathBuf, MutationError> {
    move_destination_aside_with_sync(destination, disposition, sync_parent)
}

fn move_destination_aside_with_sync(
    destination: &Path,
    disposition: RestoreDisposition,
    mut sync: impl FnMut(&Path) -> std::io::Result<()>,
) -> Result<PathBuf, MutationError> {
    let parent = destination.parent().ok_or(MutationError::InvalidScope)?;
    let name = destination.file_name().ok_or(MutationError::InvalidScope)?;
    loop {
        let sequence = NEXT_RESTORE_NAME.fetch_add(1, Ordering::Relaxed);
        let candidate = match disposition {
            RestoreDisposition::KeepBoth => {
                let mut candidate = name.to_os_string();
                candidate.push(format!(" (existing {sequence})"));
                parent.join(candidate)
            }
            RestoreDisposition::Replace => parent.join(format!(
                ".musheen-restore-backup-{}-{sequence}",
                std::process::id()
            )),
        };
        match renameat_with(CWD, destination, CWD, &candidate, RenameFlags::NOREPLACE) {
            Ok(()) => {
                if let Err(sync_error) = sync(destination) {
                    renameat_with(CWD, &candidate, CWD, destination, RenameFlags::NOREPLACE)
                        .map_err(|rollback_error| {
                            MutationError::RecoveryRequired(
                                format!(
                                    "destination was moved to {} after directory sync failed \
                                     ({sync_error}); rollback failed: {rollback_error}",
                                    candidate.display()
                                )
                                .into(),
                            )
                        })?;
                    if let Err(rollback_sync_error) = sync(destination) {
                        return Err(MutationError::RecoveryRequired(
                            format!(
                                "destination was restored after directory sync failed \
                                 ({sync_error}), but the rollback could not be made durable: \
                                 {rollback_sync_error}"
                            )
                            .into(),
                        ));
                    }
                    return Err(map_io_error(sync_error));
                }
                return Ok(candidate);
            }
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => return Err(map_errno(error)),
        }
    }
}

fn merge_restored_directory(
    item: &trash::TrashItem,
    destination: &Path,
) -> Result<(), MutationError> {
    let source = trash_payload_path(item)?;
    let source_metadata = fs::symlink_metadata(&source).map_err(map_io_error)?;
    let destination_metadata = fs::symlink_metadata(destination).map_err(map_io_error)?;
    if !source_metadata.is_dir() || !destination_metadata.is_dir() {
        return Err(MutationError::InvalidScope);
    }
    preflight_directory_merge(&source, destination)?;

    let backup = move_destination_aside(destination, RestoreDisposition::Replace)?;
    if let Err(error) = renameat_with(CWD, &source, CWD, destination, RenameFlags::NOREPLACE) {
        let error = map_errno(error);
        restore_moved_destination(&backup, destination).map_err(|rollback| {
            MutationError::RecoveryRequired(
                format!(
                    "restoring the trashed directory failed ({error}); the previous destination \
                     remains at {} because rollback failed: {rollback}",
                    backup.display()
                )
                .into(),
            )
        })?;
        return Err(error);
    }
    if let Err(error) = sync_parent(destination) {
        return Err(rollback_restore_merge_failure(
            map_io_error(error),
            &backup,
            destination,
            &source,
            &[],
        ));
    }

    let mut moved = Vec::new();
    if let Err(error) = merge_directory_entries(&backup, destination, &mut moved) {
        return Err(rollback_restore_merge_failure(
            error,
            &backup,
            destination,
            &source,
            &moved,
        ));
    }
    if let Err(error) = fs::remove_file(&item.id) {
        return Err(rollback_restore_merge_failure(
            map_io_error(error),
            &backup,
            destination,
            &source,
            &moved,
        ));
    }
    sync_parent(Path::new(&item.id)).map_err(|error| {
        MutationError::RecoveryRequired(
            format!(
                "the directory was restored and its trash receipt was removed, but the receipt \
                 update could not be made durable: {error}; the previous destination remains at \
                 {}",
                backup.display()
            )
            .into(),
        )
    })?;
    remove_path(&backup).map_err(|error| {
        MutationError::RecoveryRequired(
            format!(
                "the directory was restored, but the previous destination remains at {}: {error}",
                backup.display()
            )
            .into(),
        )
    })?;
    sync_parent(destination).map_err(|error| {
        MutationError::RecoveryRequired(
            format!("the restored directory could not be made durable: {error}").into(),
        )
    })
}

fn rollback_restore_merge_failure(
    error: MutationError,
    backup: &Path,
    destination: &Path,
    trash_source: &Path,
    moved: &[(PathBuf, PathBuf)],
) -> MutationError {
    let original = error.to_string();
    match rollback_directory_merge(backup, destination, trash_source, moved) {
        Ok(()) => error,
        Err(rollback) => MutationError::RecoveryRequired(
            format!(
                "restore failed ({original}); rollback also failed: {rollback}; inspect {} and {}",
                destination.display(),
                trash_source.display()
            )
            .into(),
        ),
    }
}

fn trash_payload_path(item: &trash::TrashItem) -> Result<PathBuf, MutationError> {
    trash_payload_path_of_record(Path::new(&item.id))
}

/// The item kind of a trash payload from its own metadata.
fn trash_payload_kind(metadata: &fs::Metadata) -> musheen_ops::ConflictItemKind {
    if metadata.file_type().is_symlink() {
        musheen_ops::ConflictItemKind::SymbolicLink
    } else if metadata.is_dir() {
        musheen_ops::ConflictItemKind::Directory
    } else {
        musheen_ops::ConflictItemKind::File
    }
}

/// Entries whose `.trashinfo` record the trash crate could not read or
/// parse. Each is listed as unrestorable so it can be purged. Its original
/// location and deletion time are unknown, so the entry shows where its data
/// sits in Trash and when its record was written.
fn unreadable_trash_entries(listed_ids: &std::collections::HashSet<OsString>) -> Vec<LocalTrashEntry> {
    let Ok(folders) = trash::os_limited::trash_folders() else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for folder in folders {
        let Ok(records) = fs::read_dir(folder.join("info")) else {
            continue;
        };
        for record in records.flatten() {
            let info = record.path();
            if listed_ids.contains(info.as_os_str())
                || info.extension() != Some(OsStr::new("trashinfo"))
                || !record.file_type().is_ok_and(|kind| kind.is_file())
            {
                continue;
            }
            let Ok(payload) = trash_payload_path_of_record(&info) else {
                continue;
            };
            let kind = fs::symlink_metadata(&payload)
                .as_ref()
                .map_or(musheen_ops::ConflictItemKind::File, trash_payload_kind);
            let written = record
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since_epoch| i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX));
            entries.push(LocalTrashEntry {
                receipt: TrashReceipt::new(
                    StorePath::from_unix_path(payload.into_os_string()),
                    info.into_os_string().into_vec(),
                ),
                deleted_at_unix_seconds: written,
                kind,
                restorable: false,
            });
        }
    }
    entries
}

/// Purges every target, continuing past the ones that fail, and reports how
/// many failed. One entry that cannot be removed never keeps the others in
/// Trash.
fn purge_each<T>(
    targets: &[T],
    mut purge: impl FnMut(&T) -> Result<(), MutationError>,
) -> Result<(), MutationError> {
    let mut failures = Vec::new();
    for target in targets {
        if let Err(error) = purge(target) {
            failures.push(error);
        }
    }
    match failures.first() {
        None => Ok(()),
        Some(first) => Err(MutationError::Provider(
            format!(
                "{} of {} trash entries could not be purged; the first error: {first}",
                failures.len(),
                targets.len()
            )
            .into(),
        )),
    }
}

/// Removes one trash entry: its data under `files/`, then its record. Data
/// that is already gone is not an error. A tree with read-only folders is
/// made writable first, because the user asked for it to be gone.
fn purge_trash_record(info: &Path) -> Result<(), MutationError> {
    let payload = trash_payload_path_of_record(info)?;
    match fs::symlink_metadata(&payload) {
        Ok(metadata) if metadata.is_dir() => {
            if let Err(error) = fs::remove_dir_all(&payload) {
                if error.kind() != std::io::ErrorKind::PermissionDenied {
                    return Err(map_io_error(error));
                }
                make_tree_writable(&payload)?;
                fs::remove_dir_all(&payload).map_err(map_io_error)?;
            }
        }
        Ok(_) => fs::remove_file(&payload).map_err(map_io_error)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(map_io_error(error)),
    }
    fs::remove_file(info).map_err(map_io_error)
}

/// Gives the owner full access to every folder below `root` so the tree
/// can be removed. Links are not followed.
fn make_tree_writable(root: &Path) -> Result<(), MutationError> {
    let metadata = fs::symlink_metadata(root).map_err(map_io_error)?;
    if !metadata.is_dir() {
        return Ok(());
    }
    let mut permissions = metadata.permissions();
    if permissions.mode() & 0o700 != 0o700 {
        permissions.set_mode(permissions.mode() | 0o700);
        fs::set_permissions(root, permissions).map_err(map_io_error)?;
    }
    for entry in fs::read_dir(root).map_err(map_io_error)? {
        make_tree_writable(&entry.map_err(map_io_error)?.path())?;
    }
    Ok(())
}

fn trash_payload_path_of_record(info: &Path) -> Result<PathBuf, MutationError> {
    let info_directory = info.parent().ok_or(MutationError::InvalidScope)?;
    if info_directory.file_name() != Some(OsStr::new("info")) {
        return Err(MutationError::InvalidScope);
    }
    let trash_root = info_directory.parent().ok_or(MutationError::InvalidScope)?;
    let name = info
        .file_stem()
        .filter(|_| info.extension() == Some(OsStr::new("trashinfo")))
        .ok_or(MutationError::InvalidScope)?;
    Ok(trash_root.join("files").join(name))
}

fn preflight_directory_merge(source: &Path, destination: &Path) -> Result<(), MutationError> {
    for entry in fs::read_dir(source).map_err(map_io_error)? {
        let entry = entry.map_err(map_io_error)?;
        let source_child = entry.path();
        let destination_child = destination.join(entry.file_name());
        let source_metadata = fs::symlink_metadata(&source_child).map_err(map_io_error)?;
        match fs::symlink_metadata(&destination_child) {
            Ok(destination_metadata)
                if source_metadata.is_dir() && destination_metadata.is_dir() =>
            {
                preflight_directory_merge(&source_child, &destination_child)?;
            }
            Ok(_) => return Err(MutationError::Conflict),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(map_io_error(error)),
        }
    }
    Ok(())
}

fn merge_directory_entries(
    source: &Path,
    destination: &Path,
    moved: &mut Vec<(PathBuf, PathBuf)>,
) -> Result<(), MutationError> {
    for entry in fs::read_dir(source).map_err(map_io_error)? {
        let entry = entry.map_err(map_io_error)?;
        let source_child = entry.path();
        let destination_child = destination.join(entry.file_name());
        let source_metadata = fs::symlink_metadata(&source_child).map_err(map_io_error)?;
        match fs::symlink_metadata(&destination_child) {
            Ok(destination_metadata)
                if source_metadata.is_dir() && destination_metadata.is_dir() =>
            {
                merge_directory_entries(&source_child, &destination_child, moved)?;
            }
            Ok(_) => return Err(MutationError::Conflict),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                renameat_with(
                    CWD,
                    &source_child,
                    CWD,
                    &destination_child,
                    RenameFlags::NOREPLACE,
                )
                .map_err(map_errno)?;
                moved.push((source_child, destination_child));
                let (source, destination) = moved.last().expect("the moved entry was recorded");
                sync_parent(source).map_err(map_io_error)?;
                sync_parent(destination).map_err(map_io_error)?;
            }
            Err(error) => return Err(map_io_error(error)),
        }
    }
    Ok(())
}

fn rollback_directory_merge(
    backup: &Path,
    destination: &Path,
    trash_source: &Path,
    moved: &[(PathBuf, PathBuf)],
) -> Result<(), MutationError> {
    fs::create_dir_all(backup).map_err(map_io_error)?;
    for (source, target) in moved.iter().rev() {
        if let Some(parent) = source.parent() {
            fs::create_dir_all(parent).map_err(map_io_error)?;
        }
        renameat_with(CWD, target, CWD, source, RenameFlags::NOREPLACE).map_err(map_errno)?;
    }
    renameat_with(CWD, destination, CWD, trash_source, RenameFlags::NOREPLACE)
        .map_err(map_errno)?;
    restore_moved_destination(backup, destination)?;
    sync_parent(destination).map_err(map_io_error)
}

fn restore_moved_destination(backup: &Path, destination: &Path) -> Result<(), MutationError> {
    renameat_with(CWD, backup, CWD, destination, RenameFlags::NOREPLACE).map_err(map_errno)?;
    sync_parent(destination).map_err(map_io_error)
}

pub(crate) fn execute_resolved_transfer(
    store: &mut LocalStore,
    request: &CopyRequest,
    decision: &ConflictDecision,
    cancellation: &CancellationToken,
) -> Result<ResolvedTransferOutcome, ResolvedTransferFailure> {
    validate_transfer_decision(store, request, decision)
        .map_err(|error| ResolvedTransferFailure::Failed(error.to_string().into()))?;
    match decision.choice() {
        ConflictChoice::Skip => Ok(ResolvedTransferOutcome::Skipped),
        ConflictChoice::KeepBoth => {
            let destination = keep_both_destination(request.destination())?;
            let alternate = CopyRequest::new(
                request.job_id(),
                request.generation(),
                request.source().clone(),
                destination,
            )
            .with_options(request.options());
            let metadata_review =
                execute_transfer(store, &alternate, decision.operation(), cancellation)?;
            Ok(ResolvedTransferOutcome::Completed {
                path: alternate.destination().clone(),
                metadata_review,
            })
        }
        ConflictChoice::Replace | ConflictChoice::ReplaceTree => {
            let metadata_review =
                execute_replacing_transfer(store, request, decision.operation(), cancellation)?;
            Ok(ResolvedTransferOutcome::Completed {
                path: request.destination().clone(),
                metadata_review,
            })
        }
        ConflictChoice::MergeDirectory => {
            let metadata_review =
                execute_merging_transfer(store, request, decision.operation(), cancellation)?;
            Ok(ResolvedTransferOutcome::Completed {
                path: request.destination().clone(),
                metadata_review,
            })
        }
    }
}

fn validate_transfer_decision(
    store: &mut LocalStore,
    request: &CopyRequest,
    decision: &ConflictDecision,
) -> Result<(), MutationError> {
    if !matches!(
        decision.operation(),
        OperationKind::Copy | OperationKind::Move
    ) || decision.source() != request.source()
        || decision.destination() != request.destination()
    {
        return Err(MutationError::InvalidScope);
    }
    let source =
        MutationProvider::identity(store, request.source())?.ok_or(MutationError::Missing)?;
    if source.as_ref() != decision.source_identity() {
        return Err(MutationError::SourceChanged);
    }
    let destination =
        MutationProvider::identity(store, request.destination())?.ok_or(MutationError::Missing)?;
    if destination.as_ref() != decision.destination_identity() {
        return Err(MutationError::SourceChanged);
    }
    Ok(())
}

fn execute_transfer(
    store: &mut LocalStore,
    request: &CopyRequest,
    operation: OperationKind,
    cancellation: &CancellationToken,
) -> Result<Option<Box<MoveMetadataReview>>, ResolvedTransferFailure> {
    match operation {
        OperationKind::Copy => CopySession::default()
            .execute(store, request, cancellation)
            .map(|_| None)
            .map_err(ResolvedTransferFailure::Transfer),
        OperationKind::Move => execute_move(store, request, cancellation)
            .map(|outcome| outcome.into_metadata_review().map(Box::new))
            .map_err(ResolvedTransferFailure::Transfer),
        _ => Err(ResolvedTransferFailure::Failed(
            "the conflict does not describe a transfer".into(),
        )),
    }
}

fn keep_both_destination(destination: &StorePath) -> Result<StorePath, Box<str>> {
    let destination = destination
        .as_unix_path()
        .ok_or_else(|| Box::<str>::from("the destination is not a local path"))?;
    let parent = destination
        .parent()
        .ok_or_else(|| Box::<str>::from("the destination has no parent"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| Box::<str>::from("the destination has no file name"))?;
    for sequence in 1_u64.. {
        let mut alternate = name.to_os_string();
        alternate.push(format!(" (copy {sequence})"));
        let alternate = parent.join(alternate);
        match fs::symlink_metadata(&alternate) {
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(StorePath::from_unix_path(alternate.into_os_string()));
            }
            Err(error) => return Err(error.to_string().into()),
        }
    }
    unreachable!("the keep-both sequence is unbounded")
}

impl ReplacementTransaction {
    fn begin(request: &CopyRequest, destination: &Path) -> Result<Self, MutationError> {
        let parent = destination.parent().ok_or(MutationError::InvalidScope)?;
        let nonce = request.staging_nonce();
        let nonce_hex = nonce
            .iter()
            .fold(String::with_capacity(32), |mut value, byte| {
                use std::fmt::Write as _;
                let _ = write!(value, "{byte:02x}");
                value
            });
        let suffix = format!(
            "{}-{}-{nonce_hex}",
            request.job_id().get(),
            request.generation().get()
        );
        let backup = parent.join(format!("{REPLACE_BACKUP_PREFIX}{suffix}"));
        let journal = parent.join(format!("{REPLACE_JOURNAL_PREFIX}{suffix}"));
        let merging = parent.join(format!("{REPLACE_MERGING_PREFIX}{suffix}"));
        let published = parent.join(format!("{REPLACE_PUBLISHED_PREFIX}{suffix}"));
        let staging = StagingPath::for_destination_with_nonce(
            request.destination(),
            request.job_id(),
            request.generation(),
            nonce,
        )
        .map_err(|_| MutationError::InvalidScope)?
        .path()
        .as_unix_path()
        .ok_or(MutationError::Unsupported)?
        .to_path_buf();
        let document = ReplacementJournal {
            version: 1,
            job_id: request.job_id().get(),
            generation: request.generation().get(),
            nonce,
            destination: destination.as_os_str().as_bytes().to_vec(),
            backup: backup.as_os_str().as_bytes().to_vec(),
            staging: staging.as_os_str().as_bytes().to_vec(),
        };
        let bytes = encode_replacement_journal(&document);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&journal)
            .map_err(map_io_error)?;
        use std::io::Write as _;
        file.write_all(&bytes).map_err(map_io_error)?;
        file.sync_all().map_err(map_io_error)?;
        sync_parent(&journal).map_err(map_io_error)?;
        if let Err(error) = renameat_with(CWD, destination, CWD, &backup, RenameFlags::NOREPLACE) {
            let _ = fs::remove_file(&journal);
            return Err(map_errno(error));
        }
        sync_parent(destination).map_err(|error| {
            MutationError::RecoveryRequired(
                format!(
                    "the previous destination remains at {} because its journal could not be made durable: {error}",
                    backup.display()
                )
                .into(),
            )
        })?;
        Ok(Self {
            backup,
            journal,
            merging,
            published,
        })
    }

    fn mark_merging(&self) -> Result<(), MutationError> {
        write_replacement_marker(&self.merging, REPLACE_MERGING_MARKER)
    }

    fn mark_published(&self) -> Result<(), MutationError> {
        write_replacement_marker(&self.published, REPLACE_PUBLISHED_MARKER)
    }

    fn remove_journal(&self) -> Result<(), MutationError> {
        remove_replacement_marker(&self.merging)?;
        remove_replacement_marker(&self.published)?;
        match fs::remove_file(&self.journal) {
            Ok(()) => sync_parent(&self.journal).map_err(map_io_error),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(map_io_error(error)),
        }
    }
}

fn write_replacement_marker(path: &Path, contents: &[u8]) -> Result<(), MutationError> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(map_io_error)?;
    file.write_all(contents).map_err(map_io_error)?;
    file.sync_all().map_err(map_io_error)?;
    sync_parent(path).map_err(map_io_error)
}

fn remove_replacement_marker(path: &Path) -> Result<(), MutationError> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent(path).map_err(map_io_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(map_io_error(error)),
    }
}

fn recover_replacement_journal(journal_path: &Path) -> Result<(), MutationError> {
    let bytes = fs::read(journal_path).map_err(map_io_error)?;
    let journal = decode_replacement_journal(&bytes)?;
    let destination = PathBuf::from(OsString::from_vec(journal.destination));
    let backup = PathBuf::from(OsString::from_vec(journal.backup));
    let staging = PathBuf::from(OsString::from_vec(journal.staging));
    let parent = journal_path.parent().ok_or(MutationError::InvalidScope)?;
    let nonce_hex = encode_hex(&journal.nonce);
    let suffix = format!("{}-{}-{nonce_hex}", journal.job_id, journal.generation);
    let expected_backup = parent.join(format!("{REPLACE_BACKUP_PREFIX}{suffix}"));
    let expected_journal = parent.join(format!("{REPLACE_JOURNAL_PREFIX}{suffix}"));
    let merging = parent.join(format!("{REPLACE_MERGING_PREFIX}{suffix}"));
    let published = parent.join(format!("{REPLACE_PUBLISHED_PREFIX}{suffix}"));
    let staging_owned = StagingPath::is_for_destination(
        &StorePath::from_unix_path(staging.as_os_str()),
        &StorePath::from_unix_path(destination.as_os_str()),
        musheen_ops::JobId::new(journal.job_id).ok_or(MutationError::InvalidScope)?,
        musheen_ops::EventGeneration::new(journal.generation),
        journal.nonce,
    );
    if journal.version != 1
        || destination.parent() != Some(parent)
        || backup != expected_backup
        || journal_path != expected_journal
        || !staging_owned
    {
        return Err(MutationError::RecoveryRequired(
            "replacement journal paths failed ownership validation".into(),
        ));
    }
    let merging_exists = replacement_marker_exists(
        &merging,
        REPLACE_MERGING_MARKER,
        "replacement merging marker",
    )?;
    let published_exists = replacement_marker_exists(
        &published,
        REPLACE_PUBLISHED_MARKER,
        "replacement published marker",
    )?;
    if merging_exists && published_exists {
        return Err(MutationError::RecoveryRequired(
            "replacement journal has conflicting phase markers".into(),
        ));
    }
    if merging_exists {
        if !destination.exists() {
            return Err(MutationError::RecoveryRequired(
                format!(
                    "replacement recovery preserved {} because the merging destination is missing",
                    backup.display()
                )
                .into(),
            ));
        }
        if backup.exists() {
            let mut moved = Vec::new();
            merge_directory_entries(&backup, &destination, &mut moved).map_err(|error| {
                MutationError::RecoveryRequired(
                    format!("replacement recovery could not finish its directory merge: {error}")
                        .into(),
                )
            })?;
            remove_path(&backup)
                .map_err(|error| MutationError::Provider(error.to_string().into()))?;
            sync_parent(&backup).map_err(map_io_error)?;
        }
        if staging.exists() {
            remove_path(&staging)
                .map_err(|error| MutationError::Provider(error.to_string().into()))?;
            sync_parent(&staging).map_err(map_io_error)?;
        }
        fs::remove_file(&merging).map_err(map_io_error)?;
        fs::remove_file(journal_path).map_err(map_io_error)?;
        return sync_parent(journal_path).map_err(map_io_error);
    }
    if published_exists {
        if !destination.exists() {
            return Err(MutationError::RecoveryRequired(
                format!(
                    "replacement recovery preserved {} because the published destination is missing",
                    backup.display()
                )
                .into(),
            ));
        }
        if backup.exists() {
            remove_path(&backup)
                .map_err(|error| MutationError::Provider(error.to_string().into()))?;
            sync_parent(&backup).map_err(map_io_error)?;
        }
        if staging.exists() {
            remove_path(&staging)
                .map_err(|error| MutationError::Provider(error.to_string().into()))?;
            sync_parent(&staging).map_err(map_io_error)?;
        }
        fs::remove_file(&published).map_err(map_io_error)?;
        fs::remove_file(journal_path).map_err(map_io_error)?;
        return sync_parent(journal_path).map_err(map_io_error);
    }
    if backup.exists() {
        if destination.exists() {
            let recovered = recovered_original_path(&destination)?;
            renameat_with(CWD, &backup, CWD, &recovered, RenameFlags::NOREPLACE)
                .map_err(map_errno)?;
        } else {
            renameat_with(CWD, &backup, CWD, &destination, RenameFlags::NOREPLACE)
                .map_err(map_errno)?;
        }
        sync_parent(&destination).map_err(map_io_error)?;
    }
    if !destination.exists() {
        return Err(MutationError::RecoveryRequired(
            format!(
                "replacement recovery preserved staging at {} because neither destination nor backup exists",
                staging.display()
            )
            .into(),
        ));
    }
    if staging.exists() {
        remove_path(&staging).map_err(|error| MutationError::Provider(error.to_string().into()))?;
        sync_parent(&staging).map_err(map_io_error)?;
    }
    fs::remove_file(journal_path).map_err(map_io_error)?;
    sync_parent(journal_path).map_err(map_io_error)
}

fn replacement_marker_exists(
    path: &Path,
    expected: &[u8],
    label: &str,
) -> Result<bool, MutationError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(map_io_error(error)),
    };
    if !metadata.file_type().is_file() {
        return Err(MutationError::RecoveryRequired(
            format!("{label} is not a regular file").into(),
        ));
    }
    if fs::read(path).map_err(map_io_error)? == expected {
        Ok(true)
    } else {
        Err(MutationError::RecoveryRequired(
            format!("{label} is invalid").into(),
        ))
    }
}

fn encode_replacement_journal(journal: &ReplacementJournal) -> Vec<u8> {
    format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
        journal.version,
        journal.job_id,
        journal.generation,
        encode_hex(&journal.nonce),
        encode_hex(&journal.destination),
        encode_hex(&journal.backup),
        encode_hex(&journal.staging),
    )
    .into_bytes()
}

fn decode_replacement_journal(bytes: &[u8]) -> Result<ReplacementJournal, MutationError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| MutationError::RecoveryRequired("replacement journal is not UTF-8".into()))?;
    let mut lines = text.lines();
    let version = u8::try_from(parse_journal_number(lines.next(), "version")?).map_err(|_| {
        MutationError::RecoveryRequired("replacement journal version is out of range".into())
    })?;
    let job_id = parse_journal_number(lines.next(), "job id")?;
    let generation = parse_journal_number(lines.next(), "generation")?;
    let nonce = decode_hex(lines.next().unwrap_or_default())?;
    let nonce: [u8; 16] = nonce.try_into().map_err(|_| {
        MutationError::RecoveryRequired("replacement journal nonce has the wrong size".into())
    })?;
    let destination = decode_hex(lines.next().unwrap_or_default())?;
    let backup = decode_hex(lines.next().unwrap_or_default())?;
    let staging = decode_hex(lines.next().unwrap_or_default())?;
    if lines.next().is_some() {
        return Err(MutationError::RecoveryRequired(
            "replacement journal has unexpected fields".into(),
        ));
    }
    Ok(ReplacementJournal {
        version,
        job_id,
        generation,
        nonce,
        destination,
        backup,
        staging,
    })
}

fn parse_journal_number(value: Option<&str>, field: &str) -> Result<u64, MutationError> {
    value
        .ok_or_else(|| {
            MutationError::RecoveryRequired(format!("replacement journal lacks {field}").into())
        })?
        .parse()
        .map_err(|_| {
            MutationError::RecoveryRequired(
                format!("replacement journal has an invalid {field}").into(),
            )
        })
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().fold(
        String::with_capacity(bytes.len().saturating_mul(2)),
        |mut value, byte| {
            use std::fmt::Write as _;
            let _ = write!(value, "{byte:02x}");
            value
        },
    )
}

fn decode_hex(value: &str) -> Result<Vec<u8>, MutationError> {
    if !value.len().is_multiple_of(2) {
        return Err(MutationError::RecoveryRequired(
            "replacement journal has invalid hex".into(),
        ));
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = decode_hex_digit(pair[0])?;
            let low = decode_hex_digit(pair[1])?;
            Ok(high * 16 + low)
        })
        .collect()
}

fn decode_hex_digit(value: u8) -> Result<u8, MutationError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(MutationError::RecoveryRequired(
            "replacement journal has invalid hex".into(),
        )),
    }
}

fn recovered_original_path(destination: &Path) -> Result<PathBuf, MutationError> {
    let parent = destination.parent().ok_or(MutationError::InvalidScope)?;
    let name = destination.file_name().ok_or(MutationError::InvalidScope)?;
    for sequence in 1_u64.. {
        let mut recovered = name.to_os_string();
        recovered.push(format!(" (recovered original {sequence})"));
        let recovered = parent.join(recovered);
        if !recovered.try_exists().map_err(map_io_error)? {
            return Ok(recovered);
        }
    }
    unreachable!("the recovered-original sequence is unbounded")
}

fn execute_replacing_transfer(
    store: &mut LocalStore,
    request: &CopyRequest,
    operation: OperationKind,
    cancellation: &CancellationToken,
) -> Result<Option<Box<MoveMetadataReview>>, ResolvedTransferFailure> {
    let destination = request.destination().as_unix_path().ok_or_else(|| {
        ResolvedTransferFailure::Failed("the destination is not a local path".into())
    })?;
    let transaction =
        ReplacementTransaction::begin(request, destination).map_err(resolved_move_aside_failure)?;
    let backup = transaction.backup.clone();
    let metadata_review = match execute_transfer(store, request, operation, cancellation) {
        Ok(metadata_review) => metadata_review,
        Err(error) => {
            let original_error = error.to_string();
            if !replacement_destination_can_be_removed(&error) {
                return Err(ResolvedTransferFailure::NeedsAttention(
                    format!(
                        "{original_error}; the source state is uncertain, so Musheen preserved the \
                     possible new destination at {} and the previous destination at {}",
                        destination.display(),
                        backup.display()
                    )
                    .into(),
                ));
            }
            if destination.try_exists().unwrap_or(true) {
                remove_path(destination).map_err(|rollback| {
                    ResolvedTransferFailure::NeedsAttention(
                        format!(
                            "{original_error}; rollback could not remove the new destination: \
                         {rollback}"
                        )
                        .into(),
                    )
                })?;
            }
            restore_moved_destination(&backup, destination).map_err(|rollback| {
                ResolvedTransferFailure::NeedsAttention(
                format!(
                    "{original_error}; rollback could not restore the old destination: {rollback}"
                )
                .into(),
            )
            })?;
            transaction
                .remove_journal()
                .map_err(resolved_move_aside_failure)?;
            return Err(error);
        }
    };
    transaction.mark_published().map_err(|error| {
        ResolvedTransferFailure::NeedsAttention(
            format!(
                "the destination was published, but cleanup cannot start safely because its durable marker failed: {error}; the previous destination remains at {}",
                backup.display()
            )
            .into(),
        )
    })?;
    remove_path(&backup).map_err(|error| {
        ResolvedTransferFailure::NeedsAttention(
            format!(
                "the destination was published, but its previous version remains at {}: {error}",
                backup.display()
            )
            .into(),
        )
    })?;
    sync_parent(destination).map_err(|error| {
        ResolvedTransferFailure::NeedsAttention(
            format!("the destination was published but could not be made durable: {error}").into(),
        )
    })?;
    transaction
        .remove_journal()
        .map_err(resolved_move_aside_failure)?;
    Ok(metadata_review)
}

fn replacement_destination_can_be_removed(error: &ResolvedTransferFailure) -> bool {
    matches!(
        error,
        ResolvedTransferFailure::Transfer(failure)
            if failure.destination_can_be_removed_for_rollback()
    )
}

fn execute_merging_transfer(
    store: &mut LocalStore,
    request: &CopyRequest,
    operation: OperationKind,
    cancellation: &CancellationToken,
) -> Result<Option<Box<MoveMetadataReview>>, ResolvedTransferFailure> {
    let source = request
        .source()
        .as_unix_path()
        .ok_or_else(|| ResolvedTransferFailure::Failed("the source is not a local path".into()))?;
    let destination = request.destination().as_unix_path().ok_or_else(|| {
        ResolvedTransferFailure::Failed("the destination is not a local path".into())
    })?;
    preflight_directory_merge(source, destination)
        .map_err(|error| ResolvedTransferFailure::Failed(error.to_string().into()))?;
    let transaction =
        ReplacementTransaction::begin(request, destination).map_err(resolved_move_aside_failure)?;
    let backup = transaction.backup.clone();
    let metadata_review = match execute_transfer(store, request, operation, cancellation) {
        Ok(metadata_review) => metadata_review,
        Err(error) => {
            let original_error = error.to_string();
            restore_moved_destination(&backup, destination).map_err(|rollback| {
                ResolvedTransferFailure::NeedsAttention(
                format!(
                    "{original_error}; rollback could not restore the old destination: {rollback}"
                )
                .into(),
            )
            })?;
            transaction
                .remove_journal()
                .map_err(resolved_move_aside_failure)?;
            return Err(error);
        }
    };

    transaction.mark_merging().map_err(|error| {
        ResolvedTransferFailure::NeedsAttention(
            format!(
                "the destination was published, but its directory merge cannot start safely because the durable marker failed: {error}; the previous destination remains at {}",
                backup.display()
            )
            .into(),
        )
    })?;
    let mut moved = Vec::new();
    if let Err(error) = merge_directory_entries(&backup, destination, &mut moved) {
        return Err(ResolvedTransferFailure::NeedsAttention(
            format!(
                "the directory merge stopped after publishing its destination: {error}; startup recovery will resume from {}",
                backup.display()
            )
            .into(),
        ));
    }
    remove_path(&backup).map_err(|error| {
        ResolvedTransferFailure::NeedsAttention(
            format!(
                "the merged destination was published, but its backup remains at {}: {error}",
                backup.display()
            )
            .into(),
        )
    })?;
    sync_parent(destination).map_err(|error| {
        ResolvedTransferFailure::NeedsAttention(
            format!("the merged destination was published but could not be made durable: {error}")
                .into(),
        )
    })?;
    transaction
        .remove_journal()
        .map_err(resolved_move_aside_failure)?;
    Ok(metadata_review)
}

impl MutationProvider for LocalStore {
    fn allows_create(
        &mut self,
        parent: &StorePath,
        _kind: CreateKind,
    ) -> Result<bool, MutationError> {
        writable_location(parent)
    }

    fn allows_rename(&mut self, source: &StorePath) -> Result<bool, MutationError> {
        writable_location(source)
    }

    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
        let entry = ParentEntry::open(path)?;
        match read_identity(&entry) {
            Ok(identity) => Ok(Some(identity)),
            Err(rustix::io::Errno::NOENT) => Ok(None),
            Err(error) => Err(map_errno(error)),
        }
    }

    fn create(&mut self, path: &StorePath, kind: CreateKind) -> Result<(), MutationError> {
        let entry = ParentEntry::open(path)?;
        match kind {
            CreateKind::File => {
                openat(
                    &entry.parent,
                    &entry.name,
                    OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o666),
                )
                .map_err(map_errno)?;
            }
            CreateKind::Directory => {
                mkdirat(&entry.parent, &entry.name, Mode::from_raw_mode(0o777))
                    .map_err(map_errno)?;
            }
        }
        fsync(&entry.parent).map_err(map_errno)
    }

    fn rename_no_replace(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
        expected_identity: &[u8],
    ) -> Result<(), MutationError> {
        let source = ParentEntry::open(source)?;
        let destination = ParentEntry::open(destination)?;
        if read_identity(&source).map_err(map_errno)?.as_ref() != expected_identity {
            return Err(MutationError::SourceChanged);
        }
        renameat_with(
            &source.parent,
            &source.name,
            &destination.parent,
            &destination.name,
            RenameFlags::NOREPLACE,
        )
        .map_err(map_errno)?;
        fsync(&source.parent).map_err(map_errno)?;
        fsync(&destination.parent).map_err(map_errno)
    }
}

impl LinkProvider for LocalStore {
    fn allows_symbolic_links(&mut self, parent: &StorePath) -> Result<bool, MutationError> {
        supported_capability(parent, CapabilityKind::SymbolicLinks)
    }

    fn allows_hard_links(
        &mut self,
        source: &StorePath,
        parent: &StorePath,
    ) -> Result<bool, MutationError> {
        if !supported_capability(source, CapabilityKind::HardLinks)?
            || !supported_capability(parent, CapabilityKind::HardLinks)?
        {
            return Ok(false);
        }
        let source = ParentEntry::open(source)?;
        Ok(is_regular_file(
            read_statx(&source).map_err(map_errno)?.stx_mode,
        ))
    }

    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
        MutationProvider::identity(self, path)
    }

    fn filesystem_id(&mut self, path: &StorePath) -> Result<u64, MutationError> {
        let entry = ParentEntry::open(path)?;
        let stat = read_statx(&entry).map_err(map_errno)?;
        if stat.stx_mask & StatxFlags::MNT_ID.bits() != 0 {
            Ok(stat.stx_mnt_id)
        } else {
            Ok((u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor))
        }
    }

    fn create_symbolic_link(
        &mut self,
        target: &OsStr,
        destination: &StorePath,
    ) -> Result<(), MutationError> {
        let destination = ParentEntry::open(destination)?;
        symlinkat(target, &destination.parent, &destination.name).map_err(map_errno)?;
        fsync(&destination.parent).map_err(map_errno)
    }

    fn create_hard_link(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
        expected_identity: &[u8],
    ) -> Result<(), MutationError> {
        let source = ParentEntry::open(source)?;
        let destination = ParentEntry::open(destination)?;
        if read_identity(&source).map_err(map_errno)?.as_ref() != expected_identity {
            return Err(MutationError::SourceChanged);
        }
        if !is_regular_file(read_statx(&source).map_err(map_errno)?.stx_mode) {
            return Err(MutationError::Unsupported);
        }
        match linkat(
            &source.parent,
            &source.name,
            &destination.parent,
            &destination.name,
            AtFlags::empty(),
        ) {
            Ok(()) => fsync(&destination.parent).map_err(map_errno),
            Err(rustix::io::Errno::XDEV) => Err(MutationError::CrossFilesystem),
            Err(error) => Err(map_errno(error)),
        }
    }
}

impl MetadataProvider for LocalStore {
    fn preview(
        &mut self,
        root: &StorePath,
        expected_identity: &[u8],
        scope: MetadataScope,
        change: &musheen_ops::MetadataChange,
    ) -> Result<Vec<MetadataEntry>, MutationError> {
        if change.requires_permissions()
            && !supported_capability(root, CapabilityKind::Permissions)?
        {
            return Err(MutationError::Unsupported);
        }
        if change.requires_ownership() && !supported_capability(root, CapabilityKind::Ownership)? {
            return Err(MutationError::Unsupported);
        }
        if MutationProvider::identity(self, root)?.as_deref() != Some(expected_identity) {
            return Err(MutationError::SourceChanged);
        }
        let root_path = root.as_unix_path().ok_or(MutationError::Unsupported)?;
        let root_filesystem = LinkProvider::filesystem_id(self, root)?;
        let mut entries = Vec::new();

        match scope {
            MetadataScope::Single => {
                entries.push(metadata_entry(self, root_path)?);
            }
            MetadataScope::Recursive { .. } => {
                let mut traversal = WalkDir::new(root_path).follow_links(false).into_iter();
                while let Some(result) = traversal.next() {
                    let entry = result.map_err(map_walkdir)?;
                    let path = StorePath::from_unix_path(entry.path().as_os_str());
                    if entry.depth() > 0
                        && entry.file_type().is_dir()
                        && !scope.includes_nested_mounts()
                        && LinkProvider::filesystem_id(self, &path)? != root_filesystem
                    {
                        traversal.skip_current_dir();
                        continue;
                    }
                    entries.push(metadata_entry(self, entry.path())?);
                }
            }
        }
        Ok(entries)
    }

    fn apply_metadata(
        &mut self,
        entry: &MetadataEntry,
        change: &ResolvedMetadataChange,
    ) -> Result<(), MutationError> {
        let parent = ParentEntry::open(entry.path())?;
        let target = open_metadata_target(&parent, entry.kind())?;
        if identity_for_opened_entry(&parent, &target)
            .map_err(map_errno)?
            .as_ref()
            != entry.expected_identity()
        {
            return Err(MutationError::SourceChanged);
        }
        let proc_path = PathBuf::from(format!("/proc/self/fd/{}", target.as_raw_fd()));

        if change.owner().is_some() || change.group().is_some() {
            if entry.kind() == MetadataEntryKind::SymbolicLink {
                chownat(
                    &target,
                    "",
                    change.owner().map(Uid::from_raw),
                    change.group().map(Gid::from_raw),
                    AtFlags::EMPTY_PATH | AtFlags::SYMLINK_NOFOLLOW,
                )
                .map_err(map_errno)?;
            } else {
                fchown(
                    &target,
                    change.owner().map(Uid::from_raw),
                    change.group().map(Gid::from_raw),
                )
                .map_err(map_errno)?;
            }
        }
        if let Some(mode) = change.mode() {
            fchmod(&target, Mode::from_raw_mode(mode)).map_err(map_errno)?;
        }
        if let Some(acl) = change.access_acl() {
            apply_acl(&proc_path, acl, false)?;
        }
        if let Some(acl) = change.default_acl() {
            apply_acl(&proc_path, acl, true)?;
        }
        fsync(&parent.parent).map_err(map_errno)
    }
}

fn open_metadata_target(
    entry: &ParentEntry,
    kind: MetadataEntryKind,
) -> Result<OwnedFd, MutationError> {
    let flags = match kind {
        MetadataEntryKind::SymbolicLink => OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        MetadataEntryKind::Directory => {
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
        }
        MetadataEntryKind::File => OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
    };
    match openat(&entry.parent, &entry.name, flags, Mode::empty()) {
        Ok(fd) => Ok(fd),
        Err(rustix::io::Errno::ACCESS) if kind == MetadataEntryKind::File => openat(
            &entry.parent,
            &entry.name,
            OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(map_errno),
        Err(error) => Err(map_errno(error)),
    }
}

impl LocalStore {
    fn trash_environment(&self) -> Result<Option<TrashEnvironment>, MutationError> {
        #[cfg(test)]
        if let Some(environment) = &self.trash_environment {
            return Ok(Some(environment.clone()));
        }
        TrashEnvironment::current().map_err(map_io_error)
    }

    /// Where the `trash` crate would put `path`. Only a rename into a trash
    /// directory counts as trash support; a cross-device copy does not.
    fn trash_disposition(&self, path: &StorePath) -> Result<TrashDisposition, MutationError> {
        let Some(path) = path.as_unix_path() else {
            return Ok(TrashDisposition::Unusable);
        };
        if !path.is_absolute() || path.parent().is_none() {
            return Ok(TrashDisposition::Unusable);
        }
        Ok(self
            .trash_environment()?
            .map_or(TrashDisposition::Unusable, |environment| {
                environment.disposition(path)
            }))
    }
}

impl DeleteProvider for LocalStore {
    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
        MutationProvider::identity(self, path)
    }

    fn supports_trash(&mut self, path: &StorePath) -> Result<bool, MutationError> {
        Ok(self.trash_disposition(path)?.is_safe())
    }

    fn supports_permanent_delete(&mut self, path: &StorePath) -> Result<bool, MutationError> {
        writable_location(path)
    }

    fn move_to_trash(&mut self, target: &DeleteTarget) -> Result<TrashReceipt, MutationError> {
        let _guard = TRASH_LOCK
            .lock()
            .map_err(|_| MutationError::Provider("trash lock was poisoned".into()))?;
        let entry = ParentEntry::open(target.path())?;
        if read_identity(&entry).map_err(map_errno)?.as_ref() != target.expected_identity() {
            return Err(MutationError::SourceChanged);
        }
        if !self.trash_disposition(target.path())?.is_safe() {
            return Err(MutationError::Unsupported);
        }
        let before: std::collections::HashSet<_> = trash::os_limited::list()
            .map_err(map_trash_error)?
            .into_iter()
            .map(|item| item.id)
            .collect();
        let proc_path =
            PathBuf::from(format!("/proc/self/fd/{}", entry.parent.as_raw_fd())).join(&entry.name);
        trash::delete(&proc_path).map_err(map_trash_error)?;
        let original = target
            .path()
            .as_unix_path()
            .ok_or(MutationError::Unsupported)?;
        let resolved_parent = fs::read_link(proc_fd_path(&entry.parent)).map_err(map_io_error)?;
        let resolved_original = resolved_parent.join(&entry.name);
        let receipt = trash::os_limited::list()
            .map_err(map_trash_error)?
            .into_iter()
            .filter(|item| {
                !before.contains(&item.id)
                    && (item.original_path() == original
                        || item.original_path() == resolved_original)
            })
            .max_by_key(|item| item.time_deleted)
            .ok_or_else(|| MutationError::Provider("trashed item receipt was not found".into()))?;
        Ok(TrashReceipt::new(
            target.path().clone(),
            receipt.id.as_bytes().to_vec(),
        ))
    }

    fn restore_no_replace(&mut self, receipt: &TrashReceipt) -> Result<(), MutationError> {
        let _guard = TRASH_LOCK
            .lock()
            .map_err(|_| MutationError::Provider("trash lock was poisoned".into()))?;
        restore_receipt_no_replace(receipt)
    }

    fn permanently_delete(&mut self, target: &DeleteTarget) -> Result<(), MutationError> {
        let root = ParentEntry::open(target.path())?;
        if read_identity(&root).map_err(map_errno)?.as_ref() != target.expected_identity() {
            return Err(MutationError::SourceChanged);
        }
        let stat = read_statx(&root).map_err(map_errno)?;
        if is_directory(stat.stx_mode) {
            let directory = openat(
                &root.parent,
                &root.name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_errno)?;
            if identity_for_opened_entry(&root, &directory)
                .map_err(map_errno)?
                .as_ref()
                != target.expected_identity()
            {
                return Err(MutationError::SourceChanged);
            }
            let mount = filesystem_identity(&stat);
            let plan = plan_directory_removal(&directory, mount)?;
            execute_directory_removal(&directory, &plan)?;
            unlinkat(&root.parent, &root.name, AtFlags::REMOVEDIR).map_err(map_errno)?;
        } else {
            unlinkat(&root.parent, &root.name, AtFlags::empty()).map_err(map_errno)?;
        }
        fsync(&root.parent).map_err(map_errno)
    }
}

/// Restores one trashed entry to its original path with Musheen's own
/// rename. The trash crate's restore treats a link to a directory as a
/// directory and fails after creating an empty one; one rename moves any
/// kind of entry and refuses an occupied destination.
fn restore_receipt_no_replace(receipt: &TrashReceipt) -> Result<(), MutationError> {
    let id = OsString::from_vec(receipt.provider_reference().to_vec());
    let item = trash::os_limited::list()
        .map_err(map_trash_error)?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or(MutationError::Missing)?;
    let payload = trash_payload_path(&item)?;
    if fs::symlink_metadata(&payload).is_err() {
        return Err(MutationError::Missing);
    }
    let destination = receipt
        .original_path()
        .as_unix_path()
        .ok_or(MutationError::Unsupported)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(map_io_error)?;
    }
    match renameat_with(CWD, &payload, CWD, destination, RenameFlags::NOREPLACE) {
        Ok(()) => {}
        Err(rustix::io::Errno::EXIST) => return Err(MutationError::Conflict),
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
            if fs::symlink_metadata(destination).is_ok() {
                return Err(MutationError::Conflict);
            }
            fs::rename(&payload, destination).map_err(map_io_error)?;
        }
        Err(error) => return Err(map_errno(error)),
    }
    sync_parent(destination).map_err(map_io_error)?;
    fs::remove_file(Path::new(&item.id)).map_err(map_io_error)?;
    Ok(())
}

#[derive(Debug)]
struct RemovalNode {
    name: OsString,
    identity: Box<[u8]>,
    directory: bool,
    children: Vec<Self>,
}

fn plan_directory_removal(
    directory: &OwnedFd,
    root_mount: u64,
) -> Result<Vec<RemovalNode>, MutationError> {
    let mut plan = Vec::new();
    for result in fs::read_dir(proc_fd_path(directory)).map_err(map_io_error)? {
        let entry = result.map_err(map_io_error)?;
        let name = entry.file_name();
        let stat = statx(
            directory,
            &name,
            AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
            StatxFlags::BASIC_STATS | StatxFlags::BTIME | StatxFlags::MNT_ID,
        )
        .map_err(map_errno)?;
        let directory_entry = is_directory(stat.stx_mode);
        if directory_entry && filesystem_identity(&stat) != root_mount {
            return Err(MutationError::InvalidScope);
        }
        let children = if directory_entry {
            let child = openat(
                directory,
                &name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_errno)?;
            plan_directory_removal(&child, root_mount)?
        } else {
            Vec::new()
        };
        plan.push(RemovalNode {
            name,
            identity: identity_from_statx(&stat),
            directory: directory_entry,
            children,
        });
    }
    Ok(plan)
}

fn execute_directory_removal(
    directory: &OwnedFd,
    plan: &[RemovalNode],
) -> Result<(), MutationError> {
    let actual_names: std::collections::HashSet<_> = fs::read_dir(proc_fd_path(directory))
        .map_err(map_io_error)?
        .map(|entry| entry.map(|entry| entry.file_name()).map_err(map_io_error))
        .collect::<Result<_, _>>()?;
    let planned_names: std::collections::HashSet<_> =
        plan.iter().map(|node| node.name.clone()).collect();
    if actual_names != planned_names {
        return Err(MutationError::SourceChanged);
    }

    for node in plan {
        let stat = statx(
            directory,
            &node.name,
            AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
            StatxFlags::BASIC_STATS | StatxFlags::BTIME | StatxFlags::MNT_ID,
        )
        .map_err(map_errno)?;
        if identity_from_statx(&stat) != node.identity {
            return Err(MutationError::SourceChanged);
        }
        if node.directory {
            let child = openat(
                directory,
                &node.name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_errno)?;
            if identity_from_fd(&child).map_err(map_errno)? != node.identity {
                return Err(MutationError::SourceChanged);
            }
            execute_directory_removal(&child, &node.children)?;
            unlinkat(directory, &node.name, AtFlags::REMOVEDIR).map_err(map_errno)?;
        } else {
            unlinkat(directory, &node.name, AtFlags::empty()).map_err(map_errno)?;
        }
    }
    fsync(directory).map_err(map_errno)
}

fn proc_fd_path(fd: &OwnedFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

const fn is_directory(mode: u16) -> bool {
    mode & 0o170_000 == 0o040_000
}

const fn is_regular_file(mode: u16) -> bool {
    mode & 0o170_000 == 0o100_000
}

fn filesystem_identity(stat: &rustix::fs::Statx) -> u64 {
    if stat.stx_mask & StatxFlags::MNT_ID.bits() != 0 {
        stat.stx_mnt_id
    } else {
        (u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor)
    }
}

fn map_trash_error(error: trash::Error) -> MutationError {
    match error {
        trash::Error::RestoreCollision { .. } | trash::Error::RestoreTwins { .. } => {
            MutationError::Conflict
        }
        error => MutationError::Provider(error.to_string().into()),
    }
}

fn writable_location(path: &StorePath) -> Result<bool, MutationError> {
    crate::probe::probe(path)
        .map(|info| !info.is_read_only())
        .map_err(|error| MutationError::Provider(error.to_string().into()))
}

fn supported_capability(
    path: &StorePath,
    capability: CapabilityKind,
) -> Result<bool, MutationError> {
    crate::probe::probe(path)
        .map(|info| {
            matches!(
                info.capabilities().get(capability),
                CapabilityState::Supported
            )
        })
        .map_err(|error| MutationError::Provider(error.to_string().into()))
}

fn metadata_entry(store: &mut LocalStore, path: &Path) -> Result<MetadataEntry, MutationError> {
    let metadata = fs::symlink_metadata(path).map_err(map_io_error)?;
    let kind = if metadata.file_type().is_symlink() {
        MetadataEntryKind::SymbolicLink
    } else if metadata.is_dir() {
        MetadataEntryKind::Directory
    } else if metadata.is_file() {
        MetadataEntryKind::File
    } else {
        return Err(MutationError::Unsupported);
    };
    let path = StorePath::from_unix_path(path.as_os_str());
    let identity = MutationProvider::identity(store, &path)?.ok_or(MutationError::Missing)?;
    let requires_privilege = metadata.uid() != rustix::process::geteuid().as_raw();
    Ok(MetadataEntry::new(
        path,
        identity.to_vec(),
        kind,
        requires_privilege,
    ))
}

fn apply_acl(path: &Path, change: &AclChange, default: bool) -> Result<(), MutationError> {
    let acl = match change {
        AclChange::Replace(entries) => {
            let mut acl = PosixACL::empty();
            for entry in entries {
                acl.set(map_acl_qualifier(entry.qualifier()), acl_permissions(entry));
            }
            acl
        }
        AclChange::Remove => {
            if default {
                PosixACL::empty()
            } else {
                let mode = fs::metadata(path).map_err(map_io_error)?.mode() & 0o777;
                PosixACL::new(mode)
            }
        }
    };
    write_acl(path, acl, default)
}

fn write_acl(path: &Path, mut acl: PosixACL, default: bool) -> Result<(), MutationError> {
    let result = if default {
        acl.write_default_acl(path)
    } else {
        acl.write_acl(path)
    };
    result.map_err(|error| match error.kind() {
        std::io::ErrorKind::PermissionDenied => MutationError::PermissionDenied,
        std::io::ErrorKind::Unsupported => MutationError::Unsupported,
        _ => MutationError::Provider(error.to_string().into()),
    })
}

fn map_acl_qualifier(qualifier: &AclQualifier) -> Qualifier {
    match qualifier {
        AclQualifier::Owner => Qualifier::UserObj,
        AclQualifier::OwningGroup => Qualifier::GroupObj,
        AclQualifier::Other => Qualifier::Other,
        AclQualifier::User(id) => Qualifier::User(*id),
        AclQualifier::Group(id) => Qualifier::Group(*id),
        AclQualifier::Mask => Qualifier::Mask,
    }
}

fn acl_permissions(entry: &AclEntry) -> u32 {
    (u32::from(entry.read()) * ACL_READ)
        | (u32::from(entry.write()) * ACL_WRITE)
        | (u32::from(entry.execute()) * ACL_EXECUTE)
}

fn map_walkdir(error: walkdir::Error) -> MutationError {
    error.into_io_error().map_or_else(
        || MutationError::Provider("recursive traversal failed".into()),
        map_io_error,
    )
}

fn map_io_error(error: std::io::Error) -> MutationError {
    match error.kind() {
        std::io::ErrorKind::NotFound => MutationError::Missing,
        std::io::ErrorKind::AlreadyExists => MutationError::Conflict,
        std::io::ErrorKind::PermissionDenied => MutationError::PermissionDenied,
        std::io::ErrorKind::Unsupported => MutationError::Unsupported,
        _ => MutationError::Provider(error.to_string().into()),
    }
}

struct ParentEntry {
    parent: OwnedFd,
    name: OsString,
}

impl ParentEntry {
    fn open(path: &StorePath) -> Result<Self, MutationError> {
        let path = path.as_unix_path().ok_or(MutationError::Unsupported)?;
        let parent = path.parent().ok_or(MutationError::Unsupported)?;
        let name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or(MutationError::Unsupported)?;
        let parent = open(
            parent,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(map_errno)?;
        Ok(Self {
            parent,
            name: OsStr::to_os_string(name),
        })
    }
}

fn read_identity(entry: &ParentEntry) -> Result<Box<[u8]>, rustix::io::Errno> {
    let parent = statx_fd(&entry.parent)?;
    let target = read_statx(entry)?;
    Ok(entry_identity_from_stats(&parent, &target))
}

fn identity_from_fd(fd: &OwnedFd) -> Result<Box<[u8]>, rustix::io::Errno> {
    Ok(identity_from_statx(&statx_fd(fd)?))
}

fn identity_for_opened_entry(
    entry: &ParentEntry,
    target: &OwnedFd,
) -> Result<Box<[u8]>, rustix::io::Errno> {
    let parent = statx_fd(&entry.parent)?;
    let target = statx_fd(target)?;
    Ok(entry_identity_from_stats(&parent, &target))
}

fn statx_fd(fd: &OwnedFd) -> Result<rustix::fs::Statx, rustix::io::Errno> {
    statx(
        fd,
        "",
        AtFlags::EMPTY_PATH | AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::BTIME | StatxFlags::MNT_ID,
    )
}

fn entry_identity_from_stats(parent: &rustix::fs::Statx, target: &rustix::fs::Statx) -> Box<[u8]> {
    let parent = identity_from_statx(parent);
    let target = identity_from_statx(target);
    let mut identity = Vec::with_capacity(1 + parent.len() + target.len());
    identity.push(1);
    identity.extend_from_slice(&parent);
    identity.extend_from_slice(&target);
    identity.into_boxed_slice()
}

fn identity_from_statx(stat: &rustix::fs::Statx) -> Box<[u8]> {
    let has_birth_time = stat.stx_mask & StatxFlags::BTIME.bits() != 0;
    let mut identity = Vec::with_capacity(41);
    identity.extend_from_slice(&stat.stx_dev_major.to_ne_bytes());
    identity.extend_from_slice(&stat.stx_dev_minor.to_ne_bytes());
    identity.extend_from_slice(&stat.stx_mnt_id.to_ne_bytes());
    identity.extend_from_slice(&stat.stx_ino.to_ne_bytes());
    identity.push(u8::from(has_birth_time));
    if has_birth_time {
        identity.extend_from_slice(&stat.stx_btime.tv_sec.to_ne_bytes());
        identity.extend_from_slice(&stat.stx_btime.tv_nsec.to_ne_bytes());
    }
    identity.into_boxed_slice()
}

fn read_statx(entry: &ParentEntry) -> Result<rustix::fs::Statx, rustix::io::Errno> {
    statx(
        &entry.parent,
        &entry.name,
        AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::BTIME | StatxFlags::MNT_ID,
    )
}

fn map_errno(error: rustix::io::Errno) -> MutationError {
    match error {
        rustix::io::Errno::EXIST | rustix::io::Errno::NOTEMPTY => MutationError::Conflict,
        rustix::io::Errno::NOENT => MutationError::Missing,
        rustix::io::Errno::ACCESS | rustix::io::Errno::PERM => MutationError::PermissionDenied,
        rustix::io::Errno::XDEV | rustix::io::Errno::OPNOTSUPP => MutationError::Unsupported,
        _ => MutationError::Provider(error.to_string().into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_trash_purge_continues_past_a_failure_and_reports_it() {
        let mut attempted = Vec::new();
        let outcome = super::purge_each(&["first", "locked", "last"], |name| {
            attempted.push(*name);
            if *name == "locked" {
                Err(super::MutationError::PermissionDenied)
            } else {
                Ok(())
            }
        });

        assert_eq!(attempted, ["first", "locked", "last"]);
        assert_eq!(
            outcome,
            Err(super::MutationError::Provider(
                "1 of 3 trash entries could not be purged; the first error: permission denied"
                    .into()
            ))
        );
        assert_eq!(super::purge_each(&["one"], |_| Ok(())), Ok(()));
    }

    use super::*;
    use crate::TrashEnvironment;

    fn request(source: &Path, destination: &Path) -> CopyRequest {
        CopyRequest::new(
            musheen_ops::JobId::new(91).unwrap(),
            musheen_ops::EventGeneration::new(3),
            StorePath::from_unix_path(source.as_os_str()),
            StorePath::from_unix_path(destination.as_os_str()),
        )
    }

    /// Two temporary directories on different mounts: the first on /dev/shm,
    /// the second under the temp dir or next to the test binary, whichever
    /// /dev/shm does not share a device with. The cross-mount tests need a
    /// rename that fails with EXDEV; a runner without a second mount fails
    /// here instead of passing without crossing anything.
    fn two_mount_roots() -> (tempfile::TempDir, tempfile::TempDir) {
        let shared_memory =
            tempfile::tempdir_in("/dev/shm").expect("/dev/shm holds a temporary directory");
        let shared_device = fs::metadata(shared_memory.path()).unwrap().dev();
        let candidates = [
            Some(std::env::temp_dir()),
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
        ];
        for candidate in candidates.into_iter().flatten() {
            let differs = fs::metadata(&candidate)
                .map(|metadata| metadata.dev() != shared_device)
                .unwrap_or(false);
            if differs {
                let other = tempfile::tempdir_in(&candidate).unwrap();
                return (shared_memory, other);
            }
        }
        panic!("this check needs a temporary directory on a mount other than /dev/shm");
    }

    /// The trash refusal tests need a directory the runner cannot write. A
    /// privileged runner (root or CAP_DAC_OVERRIDE) writes anywhere, and the
    /// trash crate would then create its directory there, so the check
    /// cannot run as that user.
    fn assert_unwritable(path: &Path) {
        assert!(
            rustix::fs::access(path, rustix::fs::Access::WRITE_OK).is_err(),
            "this check needs an unprivileged runner: uid {} can write {}",
            rustix::process::getuid().as_raw(),
            path.display()
        );
    }

    #[test]
    fn moving_a_destination_aside_rolls_back_when_directory_sync_fails() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let destination = temporary.path().join("destination.txt");
        fs::write(&destination, b"original").expect("destination writes");

        let result =
            move_destination_aside_with_sync(&destination, RestoreDisposition::Replace, |_| {
                Err(std::io::Error::other("forced sync failure"))
            });

        assert!(matches!(result, Err(MutationError::RecoveryRequired(_))));
        assert_eq!(fs::read(&destination).unwrap(), b"original");
        let names = fs::read_dir(temporary.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names, vec![OsString::from("destination.txt")]);
    }

    #[test]
    fn replacement_journal_restores_a_hidden_original_after_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"original").unwrap();
        let request = request(&source, &destination);
        let staging = StagingPath::for_destination_with_nonce(
            request.destination(),
            request.job_id(),
            request.generation(),
            request.staging_nonce(),
        )
        .unwrap();
        let _crashed = ReplacementTransaction::begin(&request, &destination).unwrap();
        fs::write(staging.path().as_unix_path().unwrap(), b"partial").unwrap();

        LocalStore::new()
            .recover_replacements_at(request.destination())
            .unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"original");
        assert!(!staging.path().as_unix_path().unwrap().exists());
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 2);
    }

    #[test]
    fn restart_preserves_both_complete_versions_when_publish_finished() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"original").unwrap();
        let request = request(&source, &destination);
        let _crashed = ReplacementTransaction::begin(&request, &destination).unwrap();
        fs::write(&destination, b"new").unwrap();

        LocalStore::new()
            .recover_replacements_at(request.destination())
            .unwrap();

        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert_eq!(
            fs::read(temporary.path().join("destination (recovered original 1)")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn restart_never_exposes_a_partially_deleted_backup_as_an_original() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("preserved"), b"original").unwrap();
        fs::write(destination.join("deleted-before-crash"), b"original").unwrap();
        let request = request(&source, &destination);
        let crashed = ReplacementTransaction::begin(&request, &destination).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("published"), b"new").unwrap();
        crashed.mark_published().unwrap();
        fs::remove_file(crashed.backup.join("deleted-before-crash")).unwrap();

        LocalStore::new()
            .recover_replacements_at(request.destination())
            .unwrap();

        assert_eq!(fs::read(destination.join("published")).unwrap(), b"new");
        assert!(
            !temporary
                .path()
                .join("destination (recovered original 1)")
                .exists()
        );
        assert!(!crashed.backup.exists());
        assert!(!crashed.journal.exists());
        assert!(!crashed.published.exists());
    }

    #[test]
    fn restart_completes_a_partially_applied_directory_merge() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("old-a"), b"old").unwrap();
        fs::create_dir(destination.join("nested")).unwrap();
        fs::write(destination.join("nested/old-b"), b"old").unwrap();
        let request = request(&source, &destination);
        let crashed = ReplacementTransaction::begin(&request, &destination).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("new"), b"new").unwrap();
        fs::create_dir(destination.join("nested")).unwrap();
        crashed.mark_merging().unwrap();
        fs::rename(crashed.backup.join("old-a"), destination.join("old-a")).unwrap();

        LocalStore::new()
            .recover_replacements_at(request.destination())
            .unwrap();

        assert_eq!(fs::read(destination.join("new")).unwrap(), b"new");
        assert_eq!(fs::read(destination.join("old-a")).unwrap(), b"old");
        assert_eq!(fs::read(destination.join("nested/old-b")).unwrap(), b"old");
        assert!(!crashed.backup.exists());
        assert!(!crashed.journal.exists());
        assert!(!crashed.merging.exists());
    }

    #[test]
    fn recovery_never_deletes_staging_without_another_complete_copy() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        fs::write(&source, b"new").unwrap();
        fs::write(&destination, b"original").unwrap();
        let request = request(&source, &destination);
        let staging = StagingPath::for_destination_with_nonce(
            request.destination(),
            request.job_id(),
            request.generation(),
            request.staging_nonce(),
        )
        .unwrap();
        let crashed = ReplacementTransaction::begin(&request, &destination).unwrap();
        fs::write(staging.path().as_unix_path().unwrap(), b"complete new copy").unwrap();
        fs::remove_file(&crashed.backup).unwrap();

        assert!(
            LocalStore::new()
                .recover_replacements_at(request.destination())
                .is_err()
        );
        assert_eq!(
            fs::read(staging.path().as_unix_path().unwrap()).unwrap(),
            b"complete new copy"
        );
        assert!(crashed.journal.exists());
    }

    #[test]
    fn cross_device_replace_preserves_verified_destination_after_partial_removal() {
        let (source_root, destination_root) = two_mount_roots();
        let source = source_root.path().join("tree");
        let destination = destination_root.path().join("tree");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a"), b"a").unwrap();
        fs::write(source.join("b"), b"b").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("old"), b"old").unwrap();
        let request = request(&source, &destination);
        let mut store = LocalStore::new();
        store.source_removal_fault_after = Some(1);

        let error = execute_replacing_transfer(
            &mut store,
            &request,
            OperationKind::Move,
            &CancellationToken::new(),
        )
        .unwrap_err();

        assert!(matches!(error, ResolvedTransferFailure::NeedsAttention(_)));
        assert_eq!(fs::read(destination.join("a")).unwrap(), b"a");
        assert_eq!(fs::read(destination.join("b")).unwrap(), b"b");
        assert!(fs::read_dir(destination_root.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .as_bytes()
                .starts_with(REPLACE_BACKUP_PREFIX.as_bytes())
        }));
    }

    // OPS-006: a verified cross-device move removes the whole source tree,
    // including a hard-link pair whose link count changes while the first
    // link is removed.
    #[test]
    fn cross_device_move_removes_a_source_tree_holding_a_hard_link_pair() {
        let (source_root, destination_root) = two_mount_roots();
        let source = source_root.path().join("tree");
        let destination = destination_root.path().join("tree");
        fs::create_dir_all(source.join("sub")).unwrap();
        fs::write(source.join("sub/a"), b"linked").unwrap();
        fs::hard_link(source.join("sub/a"), source.join("sub/a-link")).unwrap();
        fs::write(source.join("plain"), b"plain").unwrap();
        let request = request(&source, &destination);

        execute_move(&mut LocalStore::new(), &request, &CancellationToken::new()).unwrap();

        assert!(!source.exists(), "the source tree is removed completely");
        assert_eq!(fs::read(destination.join("sub/a")).unwrap(), b"linked");
        assert_eq!(fs::read(destination.join("sub/a-link")).unwrap(), b"linked");
        assert_eq!(fs::read(destination.join("plain")).unwrap(), b"plain");
    }

    // OPS-008: where the trash crate would copy an item to the home trash
    // across devices, normal delete reports no trash support and refuses.
    #[test]
    fn trash_is_refused_where_the_trash_crate_would_copy_across_devices() {
        use std::os::unix::fs::PermissionsExt as _;

        let (home, volume) = two_mount_roots();
        let item = volume.path().join("writable").join("item.txt");
        fs::create_dir(volume.path().join("writable")).unwrap();
        fs::write(&item, b"item").unwrap();
        fs::set_permissions(volume.path(), fs::Permissions::from_mode(0o555)).unwrap();
        assert_unwritable(volume.path());
        let mut store = LocalStore::new();
        store.trash_environment = Some(TrashEnvironment {
            home_trash: home.path().join("Trash"),
            mount_points: vec![fs::canonicalize(volume.path()).unwrap(), PathBuf::from("/")],
            uid: 1000,
        });
        let path = StorePath::from_unix_path(item.as_os_str());

        let supported = DeleteProvider::supports_trash(&mut store, &path).unwrap();
        let identity = DeleteProvider::identity(&mut store, &path)
            .unwrap()
            .unwrap();
        let refusal = store.move_to_trash(&DeleteTarget::new(path, identity.to_vec()));

        fs::set_permissions(volume.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !supported,
            "a volume whose root cannot hold a trash directory has no trash"
        );
        assert!(
            matches!(refusal, Err(MutationError::Unsupported)),
            "{refusal:?}"
        );
        assert_eq!(fs::read(&item).unwrap(), b"item", "the item stays in place");
    }

    // OPS-008: a shared .Trash whose per-user directory exists but cannot be
    // written sends the trash crate to the home trash on another mount, where
    // it copies. Normal delete refuses instead.
    #[test]
    fn trash_is_refused_where_the_shared_trash_per_user_directory_is_not_writable() {
        use std::os::unix::fs::PermissionsExt as _;

        let (home, volume) = two_mount_roots();
        let item = volume.path().join("item.txt");
        fs::write(&item, b"item").unwrap();
        let shared = volume.path().join(".Trash");
        fs::create_dir(&shared).unwrap();
        fs::set_permissions(&shared, fs::Permissions::from_mode(0o1777)).unwrap();
        let mine = shared.join("1000");
        fs::create_dir(&mine).unwrap();
        fs::set_permissions(&mine, fs::Permissions::from_mode(0o555)).unwrap();
        assert_unwritable(&mine);
        let mut store = LocalStore::new();
        store.trash_environment = Some(TrashEnvironment {
            home_trash: home.path().join("Trash"),
            mount_points: vec![fs::canonicalize(volume.path()).unwrap(), PathBuf::from("/")],
            uid: 1000,
        });
        let path = StorePath::from_unix_path(item.as_os_str());

        let supported = DeleteProvider::supports_trash(&mut store, &path).unwrap();
        let identity = DeleteProvider::identity(&mut store, &path)
            .unwrap()
            .unwrap();
        let refusal = store.move_to_trash(&DeleteTarget::new(path, identity.to_vec()));

        fs::set_permissions(&mine, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            !supported,
            "an unwritable per-user directory in a shared .Trash has no trash"
        );
        assert!(
            matches!(refusal, Err(MutationError::Unsupported)),
            "{refusal:?}"
        );
        assert_eq!(fs::read(&item).unwrap(), b"item", "the item stays in place");
    }

    // OPS-008: a per-volume trash directory on another mount (a nested
    // subvolume, or here a link across mounts, which the crate follows) makes
    // the crate's rename fail with EXDEV and copy. Normal delete refuses.
    #[test]
    fn trash_is_refused_where_the_volume_trash_directory_is_on_another_mount() {
        let (elsewhere, volume) = two_mount_roots();
        let item = volume.path().join("item.txt");
        fs::write(&item, b"item").unwrap();
        let trash_target = elsewhere.path().join("volume-trash");
        fs::create_dir(&trash_target).unwrap();
        std::os::unix::fs::symlink(&trash_target, volume.path().join(".Trash-1000")).unwrap();
        let mut store = LocalStore::new();
        store.trash_environment = Some(TrashEnvironment {
            home_trash: elsewhere.path().join("Trash"),
            mount_points: vec![fs::canonicalize(volume.path()).unwrap(), PathBuf::from("/")],
            uid: 1000,
        });
        let path = StorePath::from_unix_path(item.as_os_str());

        let supported = DeleteProvider::supports_trash(&mut store, &path).unwrap();
        let identity = DeleteProvider::identity(&mut store, &path)
            .unwrap()
            .unwrap();
        let refusal = store.move_to_trash(&DeleteTarget::new(path, identity.to_vec()));

        assert!(
            !supported,
            "a trash directory on another mount cannot receive a rename"
        );
        assert!(
            matches!(refusal, Err(MutationError::Unsupported)),
            "{refusal:?}"
        );
        assert_eq!(fs::read(&item).unwrap(), b"item", "the item stays in place");
    }

    // OPS-008: a volume that can hold its own trash directory keeps trash support.
    #[test]
    fn trash_stays_supported_where_the_volume_holds_its_own_trash_directory() {
        let volume = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let item = volume.path().join("item.txt");
        fs::write(&item, b"item").unwrap();
        let mut store = LocalStore::new();
        store.trash_environment = Some(TrashEnvironment {
            home_trash: home.path().join("Trash"),
            mount_points: vec![fs::canonicalize(volume.path()).unwrap(), PathBuf::from("/")],
            uid: 1000,
        });
        let path = StorePath::from_unix_path(item.as_os_str());

        assert!(DeleteProvider::supports_trash(&mut store, &path).unwrap());
    }

    #[test]
    fn cross_device_replace_moves_a_nonempty_directory_completely() {
        let (source_root, destination_root) = two_mount_roots();
        let source = source_root.path().join("tree");
        let destination = destination_root.path().join("tree");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("a"), b"a").unwrap();
        fs::write(source.join("nested/b"), b"b").unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("old"), b"old").unwrap();
        let request = request(&source, &destination);

        execute_replacing_transfer(
            &mut LocalStore::new(),
            &request,
            OperationKind::Move,
            &CancellationToken::new(),
        )
        .unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("a")).unwrap(), b"a");
        assert_eq!(fs::read(destination.join("nested/b")).unwrap(), b"b");
        assert!(!destination.join("old").exists());
    }
}
