use crate::LocalStore;
use crate::operation::{remove_path, sync_parent};
use musheen_core::{CancellationToken, CapabilityKind, CapabilityState, StorePath};
use musheen_ops::{
    AclChange, AclEntry, AclQualifier, ConflictChoice, ConflictDecision, CopyRequest, CopySession,
    CreateKind, DeleteProvider, DeleteTarget, LinkProvider, MetadataEntry, MetadataEntryKind,
    MetadataProvider, MetadataScope, MutationError, MutationProvider, OperationFailure,
    OperationKind, ResolvedMetadataChange, StagingPath, TrashReceipt, execute_move,
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
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use walkdir::WalkDir;

static TRASH_LOCK: Mutex<()> = Mutex::new(());
static NEXT_RESTORE_NAME: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub(crate) enum ResolvedTransferFailure {
    Transfer(OperationFailure),
    Failed(Box<str>),
    NeedsAttention(Box<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedTransferOutcome {
    Skipped,
    Completed(StorePath),
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
}

impl LocalTrashEntry {
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
}

impl LocalStore {
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
        let mut entries = trash::os_limited::list()
            .map_err(map_trash_error)?
            .into_iter()
            .map(|item| {
                let metadata =
                    fs::symlink_metadata(trash_payload_path(&item)?).map_err(map_io_error)?;
                let kind = if metadata.file_type().is_symlink() {
                    musheen_ops::ConflictItemKind::SymbolicLink
                } else if metadata.is_dir() {
                    musheen_ops::ConflictItemKind::Directory
                } else {
                    musheen_ops::ConflictItemKind::File
                };
                Ok(LocalTrashEntry {
                    receipt: TrashReceipt::new(
                        StorePath::from_unix_path(item.original_path().into_os_string()),
                        item.id.as_bytes().to_vec(),
                    ),
                    deleted_at_unix_seconds: item.time_deleted,
                    kind,
                })
            })
            .collect::<Result<Vec<_>, MutationError>>()?;
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
        let mut available = trash::os_limited::list()
            .map_err(map_trash_error)?
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect::<std::collections::HashMap<_, _>>();
        let mut requested = std::collections::HashSet::with_capacity(receipts.len());
        let mut selected = Vec::with_capacity(receipts.len());
        for receipt in receipts {
            if !requested.insert(receipt.provider_reference().to_vec()) {
                return Err(MutationError::BatchCollision);
            }
            let id = OsString::from_vec(receipt.provider_reference().to_vec());
            selected.push(available.remove(&id).ok_or(MutationError::Missing)?);
        }
        trash::os_limited::purge_all(selected).map_err(map_trash_error)
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
    let info = Path::new(&item.id);
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
            execute_transfer(store, &alternate, decision.operation(), cancellation)?;
            Ok(ResolvedTransferOutcome::Completed(
                alternate.destination().clone(),
            ))
        }
        ConflictChoice::Replace | ConflictChoice::ReplaceTree => {
            execute_replacing_transfer(store, request, decision.operation(), cancellation)?;
            Ok(ResolvedTransferOutcome::Completed(
                request.destination().clone(),
            ))
        }
        ConflictChoice::MergeDirectory => {
            execute_merging_transfer(store, request, decision.operation(), cancellation)?;
            Ok(ResolvedTransferOutcome::Completed(
                request.destination().clone(),
            ))
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
) -> Result<(), ResolvedTransferFailure> {
    match operation {
        OperationKind::Copy => CopySession::default()
            .execute(store, request, cancellation)
            .map(|_| ())
            .map_err(ResolvedTransferFailure::Transfer),
        OperationKind::Move => execute_move(store, request, cancellation)
            .map(|_| ())
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

fn execute_replacing_transfer(
    store: &mut LocalStore,
    request: &CopyRequest,
    operation: OperationKind,
    cancellation: &CancellationToken,
) -> Result<(), ResolvedTransferFailure> {
    let destination = request.destination().as_unix_path().ok_or_else(|| {
        ResolvedTransferFailure::Failed("the destination is not a local path".into())
    })?;
    let backup = move_destination_aside(destination, RestoreDisposition::Replace)
        .map_err(resolved_move_aside_failure)?;
    if let Err(error) = execute_transfer(store, request, operation, cancellation) {
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
        return Err(error);
    }
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
    })
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
) -> Result<(), ResolvedTransferFailure> {
    let source = request
        .source()
        .as_unix_path()
        .ok_or_else(|| ResolvedTransferFailure::Failed("the source is not a local path".into()))?;
    let destination = request.destination().as_unix_path().ok_or_else(|| {
        ResolvedTransferFailure::Failed("the destination is not a local path".into())
    })?;
    preflight_directory_merge(source, destination)
        .map_err(|error| ResolvedTransferFailure::Failed(error.to_string().into()))?;
    let backup = move_destination_aside(destination, RestoreDisposition::Replace)
        .map_err(resolved_move_aside_failure)?;
    if let Err(error) = execute_transfer(store, request, operation, cancellation) {
        let original_error = error.to_string();
        restore_moved_destination(&backup, destination).map_err(|rollback| {
            ResolvedTransferFailure::NeedsAttention(
                format!(
                    "{original_error}; rollback could not restore the old destination: {rollback}"
                )
                .into(),
            )
        })?;
        return Err(error);
    }

    let mut moved = Vec::new();
    if let Err(error) = merge_directory_entries(&backup, destination, &mut moved) {
        rollback_transfer_merge(&backup, request, operation, &moved).map_err(|rollback| {
            ResolvedTransferFailure::NeedsAttention(
                format!("{error}; merge rollback failed: {rollback}").into(),
            )
        })?;
        return Err(ResolvedTransferFailure::Failed(error.to_string().into()));
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
    })
}

fn rollback_transfer_merge(
    backup: &Path,
    request: &CopyRequest,
    operation: OperationKind,
    moved: &[(PathBuf, PathBuf)],
) -> Result<(), MutationError> {
    let destination = request
        .destination()
        .as_unix_path()
        .ok_or(MutationError::Unsupported)?;
    fs::create_dir_all(backup).map_err(map_io_error)?;
    for (source, target) in moved.iter().rev() {
        if let Some(parent) = source.parent() {
            fs::create_dir_all(parent).map_err(map_io_error)?;
        }
        renameat_with(CWD, target, CWD, source, RenameFlags::NOREPLACE).map_err(map_errno)?;
    }
    match operation {
        OperationKind::Copy => {
            remove_path(destination)
                .map_err(|error| MutationError::Provider(error.to_string().into()))?;
        }
        OperationKind::Move => {
            let source = request
                .source()
                .as_unix_path()
                .ok_or(MutationError::Unsupported)?;
            renameat_with(CWD, destination, CWD, source, RenameFlags::NOREPLACE)
                .map_err(map_errno)?;
        }
        _ => return Err(MutationError::InvalidScope),
    }
    restore_moved_destination(backup, destination)
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
        Ok(supported_capability(source, CapabilityKind::HardLinks)?
            && supported_capability(parent, CapabilityKind::HardLinks)?)
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

impl DeleteProvider for LocalStore {
    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError> {
        MutationProvider::identity(self, path)
    }

    fn supports_trash(&mut self, path: &StorePath) -> Result<bool, MutationError> {
        Ok(path
            .as_unix_path()
            .is_some_and(|path| path.is_absolute() && path.parent().is_some()))
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

fn restore_receipt_no_replace(receipt: &TrashReceipt) -> Result<(), MutationError> {
    let id = OsString::from_vec(receipt.provider_reference().to_vec());
    let item = trash::os_limited::list()
        .map_err(map_trash_error)?
        .into_iter()
        .find(|item| item.id == id)
        .ok_or(MutationError::Missing)?;
    trash::os_limited::restore_all([item]).map_err(map_trash_error)
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
    use super::*;

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
}
