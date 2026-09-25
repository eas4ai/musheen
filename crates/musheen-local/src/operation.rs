use crate::LocalStore;
use musheen_core::{CancellationToken, StorePath};
use musheen_ops::{
    CopyCapabilities, CopyProvider, EntryKind, EntrySnapshot, MetadataKind, MetadataReport,
    ProviderError, SourceMetadata, SourceRemovalToken, source_unchanged,
};
use nix::errno::Errno;
use nix::unistd::{Whence, lseek};
use posix_acl::{PosixACL, Qualifier};
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, CWD, Gid, Mode, OFlags, RenameFlags, StatxFlags, Timespec, Timestamps, Uid, chown,
    fsync, open, openat, renameat_with, statx, unlinkat, utimensat,
};
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

const COPY_BUFFER_BYTES: usize = 1024 * 1024;
type ExtendedAttribute = (OsString, Option<Vec<u8>>);

impl CopyProvider for LocalStore {
    fn capabilities(&self, source: &StorePath, destination: &StorePath) -> CopyCapabilities {
        CopyCapabilities {
            reflink: source.as_unix_path().is_some() && destination.as_unix_path().is_some(),
            sparse: source.as_unix_path().is_some() && destination.as_unix_path().is_some(),
            hard_links: source.as_unix_path().is_some() && destination.as_unix_path().is_some(),
            atomic_rename: source.as_unix_path().is_some() && destination.as_unix_path().is_some(),
        }
    }

    fn inspect(
        &mut self,
        path: &StorePath,
        follow_links: bool,
    ) -> Result<EntrySnapshot, ProviderError> {
        let path = local_path(path)?;
        let metadata = if follow_links {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        }
        .map_err(map_io)?;
        snapshot(path, &metadata)
    }

    fn create_staging(
        &mut self,
        staging: &StorePath,
        _kind: EntryKind,
    ) -> Result<(), ProviderError> {
        self.operation_metadata_skips.clear();
        self.operation_partial_metadata_skips.clear();
        self.operation_timestamps.clear();
        match fs::symlink_metadata(local_path(staging)?) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(ProviderError::StagingExists),
            Err(error) => Err(map_io(error)),
        }
    }

    fn try_hard_link(
        &mut self,
        existing: &StorePath,
        staging: &StorePath,
    ) -> Result<bool, ProviderError> {
        match fs::hard_link(local_path(existing)?, local_path(staging)?) {
            Ok(()) => Ok(true),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(code) if code == Errno::EXDEV as i32 || code == Errno::EOPNOTSUPP as i32
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(map_io(error)),
        }
    }

    fn try_reflink(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
    ) -> Result<bool, ProviderError> {
        match reflink_copy::reflink(local_path(source)?, local_path(staging)?) {
            Ok(()) => Ok(true),
            Err(error)
                if !matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound
                        | std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::AlreadyExists
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(map_io(error)),
        }
    }

    fn try_sparse_copy(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<Option<u64>, ProviderError> {
        sparse_copy(local_path(source)?, local_path(staging)?, cancellation)
    }

    fn copy_streamed(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<u64, ProviderError> {
        streamed_copy(local_path(source)?, local_path(staging)?, cancellation)
    }

    fn copy_symlink(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
    ) -> Result<(), ProviderError> {
        let target = fs::read_link(local_path(source)?).map_err(map_io)?;
        symlink(target, local_path(staging)?).map_err(map_io)
    }

    fn copy_directory(
        &mut self,
        source: &StorePath,
        staging: &StorePath,
        include_nested_mounts: bool,
        cancellation: &CancellationToken,
    ) -> Result<u64, ProviderError> {
        let source = local_path(source)?;
        let staging = local_path(staging)?;
        copy_directory_tree(
            source,
            staging,
            include_nested_mounts,
            cancellation,
            &mut self.operation_metadata_skips,
            &mut self.operation_partial_metadata_skips,
            &mut self.operation_timestamps,
        )
    }

    fn apply_metadata(
        &mut self,
        source: &StorePath,
        source_snapshot: &EntrySnapshot,
        staging: &StorePath,
    ) -> Result<MetadataReport, ProviderError> {
        let mut skipped = std::mem::take(&mut self.operation_metadata_skips);
        let mut partial_skips = std::mem::take(&mut self.operation_partial_metadata_skips);
        let source = local_path(source)?;
        let staging = local_path(staging)?;
        let root_skips = copy_metadata(source, staging, source_snapshot)?;
        record_timestamp(&mut self.operation_timestamps, staging, source_snapshot);
        if source_snapshot.kind() == EntryKind::SymbolicLink {
            partial_skips.extend(root_skips);
        } else {
            skipped.extend(root_skips);
        }
        Ok(MetadataReport::with_skipped(skipped).with_partially_skipped(partial_skips))
    }

    fn verify(
        &mut self,
        source_path: &StorePath,
        source: &EntrySnapshot,
        staging: &StorePath,
        metadata_report: &MetadataReport,
    ) -> Result<bool, ProviderError> {
        let staging = local_path(staging)?;
        let metadata = fs::symlink_metadata(staging).map_err(map_io)?;
        if entry_kind(&metadata) != source.kind() {
            return Ok(false);
        }
        let source_path = local_path(source_path)?;
        let data_matches = match source.kind() {
            EntryKind::RegularFile => Ok(metadata.len() == source.size()
                && file_digest(source_path)? == file_digest(staging)?),
            EntryKind::SymbolicLink => Ok(fs::read_link(source_path).map_err(map_io)?
                == fs::read_link(staging).map_err(map_io)?),
            EntryKind::Directory => Ok(tree_digest(source_path, metadata_report)?
                == tree_digest(staging, metadata_report)?),
            EntryKind::BlockDevice
            | EntryKind::CharacterDevice
            | EntryKind::Fifo
            | EntryKind::Socket => Ok(false),
        }?;
        if !data_matches {
            return Ok(false);
        }
        if !restore_timestamps(
            staging,
            source,
            metadata_report,
            &mut self.operation_timestamps,
        )? {
            return Ok(false);
        }
        let metadata = fs::symlink_metadata(staging).map_err(map_io)?;
        metadata_matches(source_path, source, staging, &metadata, metadata_report)
    }

    fn publish(
        &mut self,
        staging: &StorePath,
        destination: &StorePath,
        cancellation: &CancellationToken,
    ) -> Result<(), ProviderError> {
        check_cancel(cancellation)?;
        let staging = local_path(staging)?;
        let destination = local_path(destination)?;
        rename_without_replacement(staging, destination)?;
        sync_parent(destination).map_err(|_| ProviderError::PublishUnknown)
    }

    fn cleanup_staging(&mut self, staging: &StorePath) -> Result<(), ProviderError> {
        remove_path(local_path(staging)?)
    }

    fn try_atomic_move(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
    ) -> Result<bool, ProviderError> {
        let source = local_path(source)?;
        let destination = local_path(destination)?;
        match renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE) {
            Ok(()) => {
                sync_parent(source).map_err(|_| ProviderError::AtomicMoveUnknown)?;
                if source.parent() != destination.parent() {
                    sync_parent(destination).map_err(|_| ProviderError::AtomicMoveUnknown)?;
                }
                Ok(true)
            }
            Err(rustix::io::Errno::XDEV | rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
                Ok(false)
            }
            Err(error) if error == rustix::io::Errno::EXIST => Err(destination_conflict()),
            Err(error) => Err(map_rustix(error)),
        }
    }

    fn remove_source(
        &mut self,
        source: &StorePath,
        expected: &EntrySnapshot,
        prepared: &SourceRemovalToken,
    ) -> Result<(), ProviderError> {
        let source = local_path(source)?;
        let current = fs::symlink_metadata(source).map_err(map_io)?;
        if !source_unchanged(expected, &snapshot(source, &current)?) {
            return Err(ProviderError::SourceChanged);
        }
        let removal = DescriptorRemoval::prepare(source, expected.filesystem_id())?;
        if removal.token().as_bytes() != prepared.as_bytes() {
            return Err(ProviderError::SourceChanged);
        }
        #[cfg(test)]
        let fault_after = self.source_removal_fault_after;
        #[cfg(not(test))]
        let fault_after = None;
        removal.execute(fault_after)?;
        sync_parent(source).map_err(|_| ProviderError::SourceRemovalUnknown)
    }

    fn prepare_source_removal(
        &mut self,
        source: &StorePath,
        expected: &EntrySnapshot,
    ) -> Result<SourceRemovalToken, ProviderError> {
        let source = local_path(source)?;
        let current = fs::symlink_metadata(source).map_err(map_io)?;
        if !source_unchanged(expected, &snapshot(source, &current)?) {
            return Err(ProviderError::SourceChanged);
        }
        DescriptorRemoval::prepare(source, expected.filesystem_id()).map(|plan| plan.token())
    }
}

fn local_path(path: &StorePath) -> Result<&Path, ProviderError> {
    path.as_unix_path()
        .ok_or_else(|| ProviderError::Unsupported("not a local filesystem path".into()))
}

fn snapshot(path: &Path, metadata: &fs::Metadata) -> Result<EntrySnapshot, ProviderError> {
    let mut identity = Vec::with_capacity(56);
    for value in [
        metadata.dev(),
        metadata.ino(),
        metadata.size(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ] {
        identity.extend_from_slice(&value.to_ne_bytes());
    }
    Ok(EntrySnapshot::new(
        identity,
        entry_kind(metadata),
        metadata.len(),
        metadata.blocks().saturating_mul(512),
        mount_identity(path, metadata)?,
    )
    .with_metadata(SourceMetadata {
        mode: metadata.mode() & 0o7777,
        owner: metadata.uid(),
        group: metadata.gid(),
        accessed_seconds: metadata.atime(),
        accessed_nanoseconds: metadata.atime_nsec(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
    }))
}

fn entry_kind(metadata: &fs::Metadata) -> EntryKind {
    let kind = metadata.file_type();
    if kind.is_file() {
        EntryKind::RegularFile
    } else if kind.is_dir() {
        EntryKind::Directory
    } else if kind.is_symlink() {
        EntryKind::SymbolicLink
    } else if kind.is_block_device() {
        EntryKind::BlockDevice
    } else if kind.is_char_device() {
        EntryKind::CharacterDevice
    } else if kind.is_fifo() {
        EntryKind::Fifo
    } else {
        EntryKind::Socket
    }
}

fn streamed_copy(
    source: &Path,
    destination: &Path,
    cancellation: &CancellationToken,
) -> Result<u64, ProviderError> {
    let mut source = File::open(source).map_err(map_io)?;
    let mut destination = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination)
        .map_err(map_io)?;
    let copied = copy_range(&mut source, &mut destination, None, cancellation)?;
    destination.sync_all().map_err(map_io)?;
    Ok(copied)
}

fn sparse_copy(
    source_path: &Path,
    destination_path: &Path,
    cancellation: &CancellationToken,
) -> Result<Option<u64>, ProviderError> {
    let mut source = File::open(source_path).map_err(map_io)?;
    let size = source.metadata().map_err(map_io)?.len();
    let first_data = match lseek(&source, 0, Whence::SeekData) {
        Ok(offset) => Some(offset as u64),
        Err(Errno::ENXIO) => None,
        Err(Errno::EINVAL | Errno::EOPNOTSUPP) => return Ok(None),
        Err(error) => return Err(map_errno(error)),
    };
    let mut destination = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(destination_path)
        .map_err(map_io)?;
    destination.set_len(size).map_err(map_io)?;
    let mut copied = 0;
    let mut data = first_data;
    while let Some(data_offset) = data {
        check_cancel(cancellation)?;
        let hole = lseek(&source, data_offset as i64, Whence::SeekHole).map_err(map_errno)? as u64;
        source.seek(SeekFrom::Start(data_offset)).map_err(map_io)?;
        destination
            .seek(SeekFrom::Start(data_offset))
            .map_err(map_io)?;
        copied += copy_range(
            &mut source,
            &mut destination,
            Some(hole.saturating_sub(data_offset)),
            cancellation,
        )?;
        data = match lseek(&source, hole as i64, Whence::SeekData) {
            Ok(offset) => Some(offset as u64),
            Err(Errno::ENXIO) => None,
            Err(error) => return Err(map_errno(error)),
        };
    }
    destination.sync_all().map_err(map_io)?;
    Ok(Some(copied))
}

fn copy_range(
    source: &mut File,
    destination: &mut File,
    mut remaining: Option<u64>,
    cancellation: &CancellationToken,
) -> Result<u64, ProviderError> {
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut copied = 0_u64;
    loop {
        check_cancel(cancellation)?;
        let limit = remaining
            .map(|bytes| usize::try_from(bytes.min(buffer.len() as u64)).unwrap_or(buffer.len()))
            .unwrap_or(buffer.len());
        if limit == 0 {
            break;
        }
        let read = source.read(&mut buffer[..limit]).map_err(map_io)?;
        if read == 0 {
            if remaining.is_some_and(|bytes| bytes > 0) {
                return Err(ProviderError::Other(
                    "source ended before the copied extent".into(),
                ));
            }
            break;
        }
        destination.write_all(&buffer[..read]).map_err(map_io)?;
        let read = read as u64;
        copied = copied.saturating_add(read);
        if let Some(bytes) = &mut remaining {
            *bytes = bytes.saturating_sub(read);
        }
    }
    Ok(copied)
}

fn copy_directory_tree(
    source: &Path,
    destination: &Path,
    include_nested_mounts: bool,
    cancellation: &CancellationToken,
    metadata_skips: &mut Vec<MetadataKind>,
    partial_metadata_skips: &mut Vec<MetadataKind>,
    timestamps: &mut Vec<(PathBuf, SourceMetadata)>,
) -> Result<u64, ProviderError> {
    fs::create_dir(destination).map_err(map_io)?;
    let root_metadata = fs::symlink_metadata(source).map_err(map_io)?;
    let root_mount = mount_identity(source, &root_metadata)?;
    let mut hard_links = BTreeMap::<(u64, u64), PathBuf>::new();
    let mut directories = Vec::<(PathBuf, PathBuf, EntrySnapshot)>::new();
    let mut copied = 0_u64;
    for entry in WalkDir::new(source).min_depth(1).follow_links(false) {
        check_cancel(cancellation)?;
        let entry = entry.map_err(|error| ProviderError::Other(error.to_string().into()))?;
        let relative = entry
            .path()
            .strip_prefix(source)
            .map_err(|error| ProviderError::Other(error.to_string().into()))?;
        let target = destination.join(relative);
        let metadata = fs::symlink_metadata(entry.path()).map_err(map_io)?;
        ensure_mount_scope(entry.path(), &metadata, root_mount, include_nested_mounts)?;
        let kind = entry_kind(&metadata);
        let entry_snapshot = snapshot(entry.path(), &metadata)?;
        match kind {
            EntryKind::Directory => {
                fs::create_dir(&target).map_err(map_io)?;
                directories.push((
                    entry.path().to_path_buf(),
                    target.clone(),
                    entry_snapshot.clone(),
                ));
            }
            EntryKind::SymbolicLink => {
                symlink(fs::read_link(entry.path()).map_err(map_io)?, &target).map_err(map_io)?;
            }
            EntryKind::RegularFile => {
                copied = copied.saturating_add(copy_regular_tree_entry(
                    entry.path(),
                    &target,
                    &metadata,
                    cancellation,
                    &mut hard_links,
                )?);
            }
            special => {
                return Err(ProviderError::Unsupported(
                    format!("recursive copy refuses {special:?}").into(),
                ));
            }
        }
        if kind != EntryKind::Directory {
            apply_tree_metadata(
                entry.path(),
                &target,
                &entry_snapshot,
                metadata_skips,
                partial_metadata_skips,
                timestamps,
            )?;
        }
    }
    for (source, target, snapshot) in directories.into_iter().rev() {
        apply_tree_metadata(
            &source,
            &target,
            &snapshot,
            metadata_skips,
            partial_metadata_skips,
            timestamps,
        )?;
    }
    File::open(destination)
        .and_then(|directory| directory.sync_all())
        .map_err(map_io)?;
    Ok(copied)
}

fn ensure_mount_scope(
    path: &Path,
    metadata: &fs::Metadata,
    root_mount: u64,
    include_nested_mounts: bool,
) -> Result<(), ProviderError> {
    if !include_nested_mounts && mount_identity(path, metadata)? != root_mount {
        Err(ProviderError::NestedMount)
    } else {
        Ok(())
    }
}

fn copy_regular_tree_entry(
    source: &Path,
    target: &Path,
    metadata: &fs::Metadata,
    cancellation: &CancellationToken,
    hard_links: &mut BTreeMap<(u64, u64), PathBuf>,
) -> Result<u64, ProviderError> {
    let identity = (metadata.dev(), metadata.ino());
    if metadata.nlink() > 1
        && let Some(existing) = hard_links.get(&identity)
    {
        fs::hard_link(existing, target).map_err(map_io)?;
        return Ok(0);
    }
    let copied = streamed_copy(source, target, cancellation)?;
    if metadata.nlink() > 1 {
        hard_links.insert(identity, target.to_path_buf());
    }
    Ok(copied)
}

fn apply_tree_metadata(
    source: &Path,
    target: &Path,
    snapshot: &EntrySnapshot,
    metadata_skips: &mut Vec<MetadataKind>,
    partial_metadata_skips: &mut Vec<MetadataKind>,
    timestamps: &mut Vec<(PathBuf, SourceMetadata)>,
) -> Result<(), ProviderError> {
    let skipped = copy_metadata(source, target, snapshot)?;
    if snapshot.kind() == EntryKind::SymbolicLink {
        partial_metadata_skips.extend(skipped);
    } else {
        metadata_skips.extend(skipped);
    }
    record_timestamp(timestamps, target, snapshot);
    Ok(())
}

fn copy_metadata(
    source: &Path,
    destination: &Path,
    snapshot: &EntrySnapshot,
) -> Result<Vec<MetadataKind>, ProviderError> {
    if snapshot.kind() == EntryKind::SymbolicLink {
        return Ok(vec![
            MetadataKind::Timestamps,
            MetadataKind::Mode,
            MetadataKind::Ownership,
            MetadataKind::ExtendedAttributes,
            MetadataKind::AccessControlList,
        ]);
    }
    let mut skipped = Vec::new();
    let Some(metadata) = snapshot.metadata() else {
        return Ok(vec![
            MetadataKind::Timestamps,
            MetadataKind::Mode,
            MetadataKind::Ownership,
        ]);
    };
    let sync_handle = File::open(destination).map_err(map_io)?;
    skipped.extend(copy_ownership(destination, metadata)?);
    skipped.extend(copy_extended_attributes(source, destination)?);
    skipped.extend(copy_access_control_lists(
        source,
        destination,
        snapshot.kind() == EntryKind::Directory,
    )?);
    fs::set_permissions(destination, fs::Permissions::from_mode(metadata.mode)).map_err(map_io)?;
    skipped.extend(copy_timestamps(destination, metadata)?);
    sync_handle.sync_all().map_err(map_io)?;
    Ok(skipped)
}

fn copy_ownership(
    destination: &Path,
    metadata: SourceMetadata,
) -> Result<Option<MetadataKind>, ProviderError> {
    match chown(
        destination,
        Some(Uid::from_raw(metadata.owner)),
        Some(Gid::from_raw(metadata.group)),
    ) {
        Ok(()) => Ok(None),
        Err(error) if recoverable_rustix_metadata_error(error) => Ok(Some(MetadataKind::Ownership)),
        Err(error) => Err(map_rustix(error)),
    }
}

fn copy_extended_attributes(
    source: &Path,
    destination: &Path,
) -> Result<Option<MetadataKind>, ProviderError> {
    match copy_xattrs(source, destination) {
        Ok(()) => Ok(None),
        Err(error) if recoverable_metadata_error(&error) => {
            Ok(Some(MetadataKind::ExtendedAttributes))
        }
        Err(error) => Err(map_io(error)),
    }
}

fn copy_access_control_lists(
    source: &Path,
    destination: &Path,
    directory: bool,
) -> Result<Option<MetadataKind>, ProviderError> {
    match copy_acls(source, destination, directory) {
        Ok(()) => Ok(None),
        Err(std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied) => {
            Ok(Some(MetadataKind::AccessControlList))
        }
        Err(_) => Err(ProviderError::Other("ACL metadata copy failed".into())),
    }
}

fn copy_timestamps(
    destination: &Path,
    metadata: SourceMetadata,
) -> Result<Option<MetadataKind>, ProviderError> {
    match set_timestamps(destination, metadata) {
        Ok(()) => Ok(None),
        Err(error) if recoverable_rustix_metadata_error(error) => {
            Ok(Some(MetadataKind::Timestamps))
        }
        Err(error) => Err(map_rustix(error)),
    }
}

fn set_timestamps(destination: &Path, metadata: SourceMetadata) -> Result<(), rustix::io::Errno> {
    let times = Timestamps {
        last_access: Timespec {
            tv_sec: metadata.accessed_seconds,
            tv_nsec: metadata.accessed_nanoseconds,
        },
        last_modification: Timespec {
            tv_sec: metadata.modified_seconds,
            tv_nsec: metadata.modified_nanoseconds,
        },
    };
    utimensat(CWD, destination, &times, AtFlags::empty())
}

fn record_timestamp(
    timestamps: &mut Vec<(PathBuf, SourceMetadata)>,
    path: &Path,
    snapshot: &EntrySnapshot,
) {
    if snapshot.kind() != EntryKind::SymbolicLink
        && let Some(metadata) = snapshot.metadata()
    {
        timestamps.push((path.to_path_buf(), metadata));
    }
}

fn restore_timestamps(
    staging: &Path,
    source: &EntrySnapshot,
    report: &MetadataReport,
    timestamps: &mut Vec<(PathBuf, SourceMetadata)>,
) -> Result<bool, ProviderError> {
    if report
        .verification_skipped()
        .contains(&MetadataKind::Timestamps)
    {
        timestamps.clear();
        return Ok(true);
    }
    if source.kind() != EntryKind::SymbolicLink
        && !timestamps.iter().any(|(path, _)| path == staging)
        && let Some(metadata) = source.metadata()
    {
        timestamps.push((staging.to_path_buf(), metadata));
    }
    timestamps.sort_by_key(|(path, _)| std::cmp::Reverse(path.components().count()));
    for (path, expected) in timestamps.drain(..) {
        set_timestamps(&path, expected).map_err(map_rustix)?;
        let actual = fs::symlink_metadata(path).map_err(map_io)?;
        if actual.atime() != expected.accessed_seconds
            || actual.atime_nsec() != expected.accessed_nanoseconds
            || actual.mtime() != expected.modified_seconds
            || actual.mtime_nsec() != expected.modified_nanoseconds
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn copy_xattrs(source: &Path, destination: &Path) -> std::io::Result<()> {
    for name in xattr::list(source)? {
        if name.as_bytes().starts_with(b"system.posix_acl_") {
            continue;
        }
        if let Some(value) = xattr::get(source, &name)? {
            xattr::set(destination, &name, &value)?;
        }
    }
    Ok(())
}

fn copy_acls(source: &Path, destination: &Path, directory: bool) -> Result<(), std::io::ErrorKind> {
    let mut access = PosixACL::read_acl(source).map_err(|error| error.kind())?;
    if access.entries().len() > 3 {
        access
            .write_acl(destination)
            .map_err(|error| error.kind())?;
    }
    if directory {
        let mut default = PosixACL::read_default_acl(source).map_err(|error| error.kind())?;
        if !default.entries().is_empty() {
            default
                .write_default_acl(destination)
                .map_err(|error| error.kind())?;
        }
    }
    Ok(())
}

fn recoverable_metadata_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::Unsupported | std::io::ErrorKind::PermissionDenied
    )
}

fn recoverable_rustix_metadata_error(error: rustix::io::Errno) -> bool {
    recoverable_metadata_error(&std::io::Error::from_raw_os_error(error.raw_os_error()))
}

fn metadata_matches(
    source_path: &Path,
    source: &EntrySnapshot,
    destination_path: &Path,
    destination: &fs::Metadata,
    report: &MetadataReport,
) -> Result<bool, ProviderError> {
    if source.kind() == EntryKind::SymbolicLink {
        return Ok(true);
    }
    let Some(expected) = source.metadata() else {
        return Ok(false);
    };
    let skipped = report.verification_skipped();
    if !skipped.contains(&MetadataKind::Mode) && destination.mode() & 0o7777 != expected.mode {
        return Ok(false);
    }
    if !skipped.contains(&MetadataKind::Ownership)
        && (destination.uid() != expected.owner || destination.gid() != expected.group)
    {
        return Ok(false);
    }
    if !skipped.contains(&MetadataKind::Timestamps)
        && (destination.atime() != expected.accessed_seconds
            || destination.atime_nsec() != expected.accessed_nanoseconds
            || destination.mtime() != expected.modified_seconds
            || destination.mtime_nsec() != expected.modified_nanoseconds)
    {
        return Ok(false);
    }
    if !skipped.contains(&MetadataKind::ExtendedAttributes)
        && !xattrs_equal(source_path, destination_path)?
    {
        return Ok(false);
    }
    if !skipped.contains(&MetadataKind::AccessControlList)
        && !acls_equal(
            source_path,
            destination_path,
            source.kind() == EntryKind::Directory,
        )?
    {
        return Ok(false);
    }
    Ok(true)
}

fn xattrs_equal(source: &Path, destination: &Path) -> Result<bool, ProviderError> {
    Ok(read_xattrs(source)? == read_xattrs(destination)?)
}

fn read_xattrs(path: &Path) -> Result<Vec<ExtendedAttribute>, ProviderError> {
    let mut values = Vec::new();
    for name in xattr::list(path).map_err(map_io)? {
        if !name.as_bytes().starts_with(b"system.posix_acl_") {
            let value = xattr::get(path, &name).map_err(map_io)?;
            values.push((name, value));
        }
    }
    values.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(values)
}

fn acls_equal(source: &Path, destination: &Path, directory: bool) -> Result<bool, ProviderError> {
    let source_access = PosixACL::read_acl(source)
        .map_err(|error| ProviderError::Other(error.to_string().into()))?;
    let destination_access = PosixACL::read_acl(destination)
        .map_err(|error| ProviderError::Other(error.to_string().into()))?;
    if source_access != destination_access {
        return Ok(false);
    }
    if directory {
        let source_default = PosixACL::read_default_acl(source)
            .map_err(|error| ProviderError::Other(error.to_string().into()))?;
        let destination_default = PosixACL::read_default_acl(destination)
            .map_err(|error| ProviderError::Other(error.to_string().into()))?;
        return Ok(source_default == destination_default);
    }
    Ok(true)
}

fn destination_conflict() -> ProviderError {
    ProviderError::Unsupported("destination conflict requires an explicit decision".into())
}

fn rename_without_replacement(source: &Path, destination: &Path) -> Result<(), ProviderError> {
    rename_without_replacement_with(source, destination, |source, destination| {
        renameat_with(CWD, source, CWD, destination, RenameFlags::NOREPLACE)
    })
}

fn rename_without_replacement_with(
    source: &Path,
    destination: &Path,
    rename: impl FnOnce(&Path, &Path) -> Result<(), rustix::io::Errno>,
) -> Result<(), ProviderError> {
    match rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error == rustix::io::Errno::EXIST => Err(destination_conflict()),
        Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOSYS) => {
            publish_without_rename_noreplace(source, destination)
        }
        Err(error) => Err(map_rustix(error)),
    }
}

fn publish_without_rename_noreplace(
    source: &Path,
    destination: &Path,
) -> Result<(), ProviderError> {
    let metadata = fs::symlink_metadata(source).map_err(map_io)?;
    if metadata.file_type().is_symlink() {
        let target = fs::read_link(source).map_err(map_io)?;
        match symlink(&target, destination) {
            Ok(()) => fs::remove_file(source).map_err(|_| ProviderError::PublishUnknown),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(destination_conflict())
            }
            Err(error) => Err(map_io(error)),
        }
    } else if metadata.is_file() {
        match fs::hard_link(source, destination) {
            Ok(()) => fs::remove_file(source).map_err(|_| ProviderError::PublishUnknown),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Err(destination_conflict())
            }
            Err(error) => Err(map_io(error)),
        }
    } else if metadata.is_dir() {
        let source_mount = mount_identity(source, &metadata)?;
        match fs::create_dir(destination) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(destination_conflict());
            }
            Err(error) => return Err(map_io(error)),
        }
        publish_directory_without_rename(source, destination, &metadata)
            .map_err(|_| ProviderError::PublishUnknown)?;
        remove_tree_without_crossing(source, source_mount)
            .map_err(|_| ProviderError::PublishUnknown)
    } else {
        Err(ProviderError::Unsupported(
            "safe publication fallback does not support this file type".into(),
        ))
    }
}

fn publish_directory_without_rename(
    source: &Path,
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), ProviderError> {
    for entry in fs::read_dir(source).map_err(map_io)? {
        let entry = entry.map_err(map_io)?;
        let child_source = entry.path();
        let child_destination = destination.join(entry.file_name());
        let child_metadata = fs::symlink_metadata(&child_source).map_err(map_io)?;
        if child_metadata.is_dir() && !child_metadata.file_type().is_symlink() {
            fs::create_dir(&child_destination).map_err(map_io)?;
            publish_directory_without_rename(&child_source, &child_destination, &child_metadata)?;
        } else if child_metadata.file_type().is_symlink() {
            symlink(
                fs::read_link(&child_source).map_err(map_io)?,
                &child_destination,
            )
            .map_err(map_io)?;
        } else {
            fs::hard_link(&child_source, &child_destination).map_err(map_io)?;
        }
    }
    chown(
        destination,
        Some(Uid::from_raw(metadata.uid())),
        Some(Gid::from_raw(metadata.gid())),
    )
    .map_err(map_rustix)?;
    copy_xattrs(source, destination).map_err(map_io)?;
    copy_acls(source, destination, true).map_err(|kind| {
        ProviderError::Other(format!("could not preserve directory ACL: {kind:?}").into())
    })?;
    fs::set_permissions(destination, metadata.permissions()).map_err(map_io)?;
    set_timestamps(
        destination,
        SourceMetadata {
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
            accessed_seconds: metadata.atime(),
            accessed_nanoseconds: metadata.atime_nsec(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        },
    )
    .map_err(map_rustix)
}

struct DescriptorRemoval {
    parent: OwnedFd,
    name: OsString,
    root_identity: Box<[u8]>,
    root_directory: Option<(OwnedFd, Vec<DescriptorRemovalNode>)>,
}

struct DescriptorRemovalNode {
    name: OsString,
    identity: Box<[u8]>,
    directory: bool,
    children: Vec<Self>,
}

impl DescriptorRemoval {
    fn prepare(path: &Path, root_mount: u64) -> Result<Self, ProviderError> {
        let parent_path = path.parent().ok_or_else(|| {
            ProviderError::Unsupported("source has no containing directory".into())
        })?;
        let name = path
            .file_name()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ProviderError::Unsupported("source has no file name".into()))?;
        let parent = open(
            parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(map_rustix)?;
        let root = descriptor_stat(&parent, name)?;
        if descriptor_mount_identity(&root) != root_mount {
            return Err(ProviderError::NestedMount);
        }
        let root_identity = descriptor_identity(&root);
        let root_directory = if descriptor_is_directory(&root) {
            let directory = openat(
                &parent,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_rustix)?;
            if !descriptor_fd_identity_matches(&directory, &root_identity)? {
                return Err(ProviderError::SourceChanged);
            }
            let children = plan_descriptor_removal(&directory, root_mount)?;
            Some((directory, children))
        } else {
            None
        };
        Ok(Self {
            parent,
            name: name.to_os_string(),
            root_identity,
            root_directory,
        })
    }

    fn token(&self) -> SourceRemovalToken {
        let mut hasher = blake3::Hasher::new();
        hash_removal_identity(&mut hasher, &self.name, &self.root_identity);
        if let Some((_, children)) = &self.root_directory {
            hash_removal_nodes(&mut hasher, children);
        }
        SourceRemovalToken::new(hasher.finalize().as_bytes().to_vec())
    }

    fn execute(self, fault_after: Option<usize>) -> Result<(), ProviderError> {
        let mut removed = 0_usize;
        let result = if let Some((directory, children)) = self.root_directory {
            execute_descriptor_removal(&directory, &children, fault_after, &mut removed)
                .and_then(|()| ensure_descriptor_directory_empty(&directory))
                .and_then(|()| {
                    revalidate_descriptor_entry(&self.parent, &self.name, &self.root_identity)
                })
                .and_then(|()| inject_removal_fault(fault_after, removed))
                .and_then(|()| {
                    unlinkat(&self.parent, &self.name, AtFlags::REMOVEDIR).map_err(map_rustix)
                })
        } else {
            revalidate_descriptor_entry(&self.parent, &self.name, &self.root_identity)
                .and_then(|()| inject_removal_fault(fault_after, removed))
                .and_then(|()| {
                    unlinkat(&self.parent, &self.name, AtFlags::empty()).map_err(map_rustix)
                })
        };
        match result {
            Ok(()) => Ok(()),
            Err(_) if removed > 0 => Err(ProviderError::SourcePartiallyRemoved),
            Err(error) => Err(error),
        }
    }
}

fn plan_descriptor_removal(
    directory: &OwnedFd,
    root_mount: u64,
) -> Result<Vec<DescriptorRemovalNode>, ProviderError> {
    let mut plan = Vec::new();
    for result in fs::read_dir(descriptor_fd_path(directory)).map_err(map_io)? {
        let entry = result.map_err(map_io)?;
        let name = entry.file_name();
        let stat = descriptor_stat(directory, &name)?;
        if descriptor_mount_identity(&stat) != root_mount {
            return Err(ProviderError::NestedMount);
        }
        let identity = descriptor_identity(&stat);
        let directory_entry = descriptor_is_directory(&stat);
        let children = if directory_entry {
            let child = openat(
                directory,
                &name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_rustix)?;
            if !descriptor_fd_identity_matches(&child, &identity)? {
                return Err(ProviderError::SourceChanged);
            }
            plan_descriptor_removal(&child, root_mount)?
        } else {
            Vec::new()
        };
        plan.push(DescriptorRemovalNode {
            name,
            identity,
            directory: directory_entry,
            children,
        });
    }
    plan.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
    Ok(plan)
}

fn execute_descriptor_removal(
    directory: &OwnedFd,
    plan: &[DescriptorRemovalNode],
    fault_after: Option<usize>,
    removed: &mut usize,
) -> Result<(), ProviderError> {
    let actual_names = fs::read_dir(descriptor_fd_path(directory))
        .map_err(map_io)?
        .map(|entry| entry.map(|entry| entry.file_name()).map_err(map_io))
        .collect::<Result<std::collections::BTreeSet<_>, _>>()?;
    let planned_names = plan
        .iter()
        .map(|node| node.name.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if actual_names != planned_names {
        return Err(ProviderError::SourceChanged);
    }

    for node in plan {
        revalidate_descriptor_entry(directory, &node.name, &node.identity)?;
        inject_removal_fault(fault_after, *removed)?;
        if node.directory {
            let child = openat(
                directory,
                &node.name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_rustix)?;
            if !descriptor_fd_identity_matches(&child, &node.identity)? {
                return Err(ProviderError::SourceChanged);
            }
            execute_descriptor_removal(&child, &node.children, fault_after, removed)?;
            ensure_descriptor_directory_empty(&child)?;
            revalidate_descriptor_entry(directory, &node.name, &node.identity)?;
            inject_removal_fault(fault_after, *removed)?;
            unlinkat(directory, &node.name, AtFlags::REMOVEDIR).map_err(map_rustix)?;
        } else {
            unlinkat(directory, &node.name, AtFlags::empty()).map_err(map_rustix)?;
        }
        *removed = (*removed).saturating_add(1);
    }
    fsync(directory).map_err(map_rustix)
}

fn ensure_descriptor_directory_empty(directory: &OwnedFd) -> Result<(), ProviderError> {
    if fs::read_dir(descriptor_fd_path(directory))
        .map_err(map_io)?
        .next()
        .is_some()
    {
        Err(ProviderError::SourceChanged)
    } else {
        Ok(())
    }
}

fn revalidate_descriptor_entry(
    parent: &OwnedFd,
    name: &OsStr,
    expected: &[u8],
) -> Result<(), ProviderError> {
    if descriptor_identity_matches(&descriptor_stat(parent, name)?, expected) {
        Ok(())
    } else {
        Err(ProviderError::SourceChanged)
    }
}

fn inject_removal_fault(fault_after: Option<usize>, removed: usize) -> Result<(), ProviderError> {
    if fault_after == Some(removed) {
        Err(ProviderError::PermissionDenied)
    } else {
        Ok(())
    }
}

fn descriptor_stat(parent: &OwnedFd, name: &OsStr) -> Result<rustix::fs::Statx, ProviderError> {
    statx(
        parent,
        name,
        AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    )
    .map_err(map_rustix)
}

fn descriptor_fd_identity_matches(fd: &OwnedFd, expected: &[u8]) -> Result<bool, ProviderError> {
    statx(
        fd,
        "",
        AtFlags::EMPTY_PATH | AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT,
        StatxFlags::BASIC_STATS | StatxFlags::MNT_ID,
    )
    .map(|stat| descriptor_identity_matches(&stat, expected))
    .map_err(map_rustix)
}

/// The first identity byte names the rule the identity was built under, so a
/// later check compares the entry under the rule chosen when it was planned.
/// A regular entry keeps size, mtime and ctime, which refuses a rewrite even
/// when its length and mtime were restored. An entry whose inode had more
/// than one link at planning time leaves ctime out: unlinking one link of a
/// pair updates the shared inode's ctime, and the second link would otherwise
/// fail revalidation and leave the source half removed. That link count may
/// already be one when the second link is checked, which is why the rule
/// travels with the identity instead of being read again.
const IDENTITY_RULE_FULL: u8 = 0;
const IDENTITY_RULE_SHARED_INODE: u8 = 1;

fn descriptor_identity(stat: &rustix::fs::Statx) -> Box<[u8]> {
    let rule = if !descriptor_is_directory(stat) && stat.stx_nlink > 1 {
        IDENTITY_RULE_SHARED_INODE
    } else {
        IDENTITY_RULE_FULL
    };
    descriptor_identity_under(stat, rule)
}

fn descriptor_identity_matches(stat: &rustix::fs::Statx, expected: &[u8]) -> bool {
    expected
        .first()
        .is_some_and(|rule| descriptor_identity_under(stat, *rule).as_ref() == expected)
}

fn descriptor_identity_under(stat: &rustix::fs::Statx, rule: u8) -> Box<[u8]> {
    let mut identity = Vec::with_capacity(96);
    identity.push(rule);
    identity.extend_from_slice(&stat.stx_dev_major.to_ne_bytes());
    identity.extend_from_slice(&stat.stx_dev_minor.to_ne_bytes());
    identity.extend_from_slice(&stat.stx_mnt_id.to_ne_bytes());
    identity.extend_from_slice(&stat.stx_ino.to_ne_bytes());
    identity.extend_from_slice(&stat.stx_mode.to_ne_bytes());
    // Removing a directory's planned children necessarily changes its size,
    // mtime, and ctime. Its descriptor, inode, type, mode, and exact child-name
    // plan still prove that the entry is the directory we prepared.
    if !descriptor_is_directory(stat) {
        identity.extend_from_slice(&stat.stx_size.to_ne_bytes());
        identity.extend_from_slice(&stat.stx_mtime.tv_sec.to_ne_bytes());
        identity.extend_from_slice(&stat.stx_mtime.tv_nsec.to_ne_bytes());
        if rule == IDENTITY_RULE_FULL {
            identity.extend_from_slice(&stat.stx_ctime.tv_sec.to_ne_bytes());
            identity.extend_from_slice(&stat.stx_ctime.tv_nsec.to_ne_bytes());
        }
    }
    identity.into_boxed_slice()
}

fn descriptor_mount_identity(stat: &rustix::fs::Statx) -> u64 {
    if stat.stx_mask & StatxFlags::MNT_ID.bits() != 0 {
        stat.stx_mnt_id
    } else {
        (u64::from(stat.stx_dev_major) << 32) | u64::from(stat.stx_dev_minor)
    }
}

const fn descriptor_is_directory(stat: &rustix::fs::Statx) -> bool {
    stat.stx_mode & 0o170_000 == 0o040_000
}

fn descriptor_fd_path(fd: &OwnedFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

fn hash_removal_nodes(hasher: &mut blake3::Hasher, nodes: &[DescriptorRemovalNode]) {
    for node in nodes {
        hash_removal_identity(hasher, &node.name, &node.identity);
        hasher.update(&[u8::from(node.directory)]);
        hash_removal_nodes(hasher, &node.children);
        hasher.update(&[0xff]);
    }
}

fn hash_removal_identity(hasher: &mut blake3::Hasher, name: &OsStr, identity: &[u8]) {
    hash_bytes(hasher, name.as_bytes());
    hash_bytes(hasher, identity);
}

pub(crate) fn remove_path(path: &Path) -> Result<(), ProviderError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            remove_tree_without_crossing(path, mount_identity(path, &metadata)?)
        }
        Ok(_) => fs::remove_file(path).map_err(map_io),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(map_io(error)),
    }
}

fn remove_tree_without_crossing(path: &Path, mount_id: u64) -> Result<(), ProviderError> {
    let mut plan = Vec::new();
    collect_removal_plan(path, mount_id, &mut plan)?;
    for (entry, directory) in plan {
        if directory {
            fs::remove_dir(entry).map_err(map_io)?;
        } else {
            fs::remove_file(entry).map_err(map_io)?;
        }
    }
    Ok(())
}

fn collect_removal_plan(
    path: &Path,
    mount_id: u64,
    plan: &mut Vec<(PathBuf, bool)>,
) -> Result<(), ProviderError> {
    let metadata = fs::symlink_metadata(path).map_err(map_io)?;
    if mount_identity(path, &metadata)? != mount_id {
        return Err(ProviderError::NestedMount);
    }
    let directory = metadata.is_dir();
    if directory {
        for entry in fs::read_dir(path).map_err(map_io)? {
            collect_removal_plan(&entry.map_err(map_io)?.path(), mount_id, plan)?;
        }
    }
    plan.push((path.to_path_buf(), directory));
    Ok(())
}

fn mount_identity(path: &Path, metadata: &fs::Metadata) -> Result<u64, ProviderError> {
    match statx(
        CWD,
        path,
        AtFlags::NO_AUTOMOUNT | AtFlags::SYMLINK_NOFOLLOW,
        StatxFlags::MNT_ID,
    ) {
        Ok(stat) if stat.stx_mask & StatxFlags::MNT_ID.bits() != 0 => Ok(stat.stx_mnt_id),
        Ok(_) => Ok(metadata.dev()),
        Err(rustix::io::Errno::NOSYS | rustix::io::Errno::INVAL) => Ok(metadata.dev()),
        Err(error) => Err(map_rustix(error)),
    }
}

pub(crate) fn sync_parent(path: &Path) -> std::io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("path has no parent directory"))?;
    File::open(parent)?.sync_all()
}

fn check_cancel(cancellation: &CancellationToken) -> Result<(), ProviderError> {
    cancellation
        .wait_if_paused()
        .map_err(|_| ProviderError::Cancelled)
}

fn map_io(error: std::io::Error) -> ProviderError {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        ProviderError::PermissionDenied
    } else if error.raw_os_error() == Some(Errno::ENOSPC as i32) {
        ProviderError::OutOfSpace
    } else {
        ProviderError::Other(error.to_string().into())
    }
}

fn map_errno(error: Errno) -> ProviderError {
    map_io(std::io::Error::from_raw_os_error(error as i32))
}

fn map_rustix(error: rustix::io::Errno) -> ProviderError {
    map_io(std::io::Error::from_raw_os_error(error.raw_os_error()))
}

fn file_digest(path: &Path) -> Result<blake3::Hash, ProviderError> {
    let mut file = File::open(path).map_err(map_io)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    loop {
        let read = file.read(&mut buffer).map_err(map_io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize())
}

fn tree_digest(root: &Path, report: &MetadataReport) -> Result<blake3::Hash, ProviderError> {
    let mut entries = WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ProviderError::Other(error.to_string().into()))?;
    entries.sort_by(|left, right| {
        left.path()
            .strip_prefix(root)
            .unwrap_or(left.path())
            .as_os_str()
            .as_bytes()
            .cmp(
                right
                    .path()
                    .strip_prefix(root)
                    .unwrap_or(right.path())
                    .as_os_str()
                    .as_bytes(),
            )
    });
    let mut hasher = blake3::Hasher::new();
    for entry in entries {
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| ProviderError::Other(error.to_string().into()))?;
        hasher.update(relative.as_os_str().as_bytes());
        hasher.update(&[0]);
        let metadata = fs::symlink_metadata(entry.path()).map_err(map_io)?;
        hasher.update(&[entry_kind(&metadata) as u8]);
        match entry_kind(&metadata) {
            EntryKind::RegularFile => hasher.update(file_digest(entry.path())?.as_bytes()),
            EntryKind::SymbolicLink => hasher.update(
                fs::read_link(entry.path())
                    .map_err(map_io)?
                    .as_os_str()
                    .as_bytes(),
            ),
            _ => &mut hasher,
        };
        hash_verified_metadata(&mut hasher, entry.path(), &metadata, report)?;
        hasher.update(&[0xff]);
    }
    Ok(hasher.finalize())
}

fn hash_verified_metadata(
    hasher: &mut blake3::Hasher,
    path: &Path,
    metadata: &fs::Metadata,
    report: &MetadataReport,
) -> Result<(), ProviderError> {
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    let skipped = report.verification_skipped();
    if !skipped.contains(&MetadataKind::Mode) {
        hasher.update(&(metadata.mode() & 0o7777).to_le_bytes());
    }
    if !skipped.contains(&MetadataKind::Ownership) {
        hasher.update(&metadata.uid().to_le_bytes());
        hasher.update(&metadata.gid().to_le_bytes());
    }
    if !skipped.contains(&MetadataKind::Timestamps) {
        hasher.update(&metadata.mtime().to_le_bytes());
        hasher.update(&metadata.mtime_nsec().to_le_bytes());
    }
    if !skipped.contains(&MetadataKind::ExtendedAttributes) {
        for (name, value) in read_xattrs(path)? {
            hash_bytes(hasher, name.as_bytes());
            match value {
                Some(value) => {
                    hasher.update(&[1]);
                    hash_bytes(hasher, &value);
                }
                None => {
                    hasher.update(&[0]);
                }
            }
        }
    }
    if !skipped.contains(&MetadataKind::AccessControlList) {
        let access = PosixACL::read_acl(path)
            .map_err(|error| ProviderError::Other(error.to_string().into()))?;
        hash_acl(hasher, &access);
        if metadata.is_dir() {
            let default = PosixACL::read_default_acl(path)
                .map_err(|error| ProviderError::Other(error.to_string().into()))?;
            hash_acl(hasher, &default);
        }
    }
    Ok(())
}

fn hash_bytes(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn hash_acl(hasher: &mut blake3::Hasher, acl: &PosixACL) {
    for entry in acl.entries() {
        let (tag, id) = match entry.qual {
            Qualifier::Undefined => (0, 0),
            Qualifier::UserObj => (1, 0),
            Qualifier::GroupObj => (2, 0),
            Qualifier::Other => (3, 0),
            Qualifier::User(id) => (4, id),
            Qualifier::Group(id) => (5, id),
            Qualifier::Mask => (6, 0),
        };
        hasher.update(&[tag]);
        hasher.update(&id.to_le_bytes());
        hasher.update(&entry.perm.to_le_bytes());
    }
    hasher.update(&[0xff]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store_path(path: &Path) -> StorePath {
        StorePath::from_unix_path(path.as_os_str())
    }

    #[test]
    fn rename_noreplace_einval_uses_atomic_creation_fallback() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage");
        let destination = root.path().join("published");
        fs::write(&source, b"complete").unwrap();

        rename_without_replacement_with(&source, &destination, |_, _| {
            Err(rustix::io::Errno::INVAL)
        })
        .unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(destination).unwrap(), b"complete");
    }

    #[test]
    fn rename_noreplace_einval_publishes_a_complete_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage");
        let destination = root.path().join("published");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), b"complete").unwrap();
        symlink("file", source.join("link")).unwrap();

        rename_without_replacement_with(&source, &destination, |_, _| {
            Err(rustix::io::Errno::INVAL)
        })
        .unwrap();

        assert!(!source.exists());
        assert_eq!(fs::read(destination.join("file")).unwrap(), b"complete");
        assert_eq!(
            fs::read_link(destination.join("link")).unwrap(),
            Path::new("file")
        );
    }

    #[test]
    fn fallback_publish_failure_leaves_no_partial_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("stage");
        let destination = root.path().join("published");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("first"), b"complete").unwrap();
        let locked = source.join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("inner"), b"hidden").unwrap();
        // An unreadable child directory fails the publication half way.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let outcome = rename_without_replacement_with(&source, &destination, |_, _| {
            Err(rustix::io::Errno::INVAL)
        });

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(outcome.is_err(), "an unreadable child fails the fallback");
        assert!(
            !destination.exists(),
            "a failed publication leaves nothing under the destination name"
        );
        assert_eq!(fs::read(source.join("first")).unwrap(), b"complete");
        assert_eq!(fs::read(locked.join("inner")).unwrap(), b"hidden");
    }

    #[test]
    fn sparse_files_inside_a_copied_folder_keep_their_holes() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("folder");
        let destination = root.path().join("copied");
        fs::create_dir(&source).unwrap();
        let logical_size = 8 * 1024 * 1024;
        let data_offset = 4 * 1024 * 1024;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(source.join("sparse"))
            .unwrap();
        file.set_len(logical_size).unwrap();
        file.seek(SeekFrom::Start(data_offset)).unwrap();
        file.write_all(b"island").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let original = fs::metadata(source.join("sparse")).unwrap();
        assert!(
            original.blocks() * 512 < logical_size,
            "the fixture holds a hole on this filesystem"
        );

        copy_directory_tree(
            &source,
            &destination,
            false,
            &CancellationToken::new(),
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .unwrap();

        let copied = fs::metadata(destination.join("sparse")).unwrap();
        assert_eq!(copied.len(), logical_size);
        assert!(
            copied.blocks() * 512 < logical_size,
            "the copy inside the folder keeps the hole; it holds {} blocks",
            copied.blocks()
        );
        let mut copied = File::open(destination.join("sparse")).unwrap();
        copied.seek(SeekFrom::Start(data_offset)).unwrap();
        let mut data = [0_u8; 6];
        copied.read_exact(&mut data).unwrap();
        assert_eq!(&data, b"island");
    }

    #[test]
    fn descriptor_removal_refuses_a_new_descendant() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("tree");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("original"), b"kept").unwrap();
        let path = store_path(&source);
        let mut provider = LocalStore::new();
        let expected = provider.inspect(&path, false).unwrap();
        let token = provider.prepare_source_removal(&path, &expected).unwrap();
        fs::write(source.join("concurrent"), b"new").unwrap();

        assert_eq!(
            provider.remove_source(&path, &expected, &token),
            Err(ProviderError::SourceChanged)
        );
        assert_eq!(fs::read(source.join("concurrent")).unwrap(), b"new");
    }

    #[test]
    fn descriptor_removal_refuses_a_swapped_descendant() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("tree");
        fs::create_dir(&source).unwrap();
        let child = source.join("child");
        fs::write(&child, b"old").unwrap();
        let path = store_path(&source);
        let mut provider = LocalStore::new();
        let expected = provider.inspect(&path, false).unwrap();
        let token = provider.prepare_source_removal(&path, &expected).unwrap();
        fs::rename(&child, source.join("removed-child")).unwrap();
        fs::write(&child, b"replacement").unwrap();

        assert_eq!(
            provider.remove_source(&path, &expected, &token),
            Err(ProviderError::SourceChanged)
        );
        assert_eq!(fs::read(child).unwrap(), b"replacement");
    }

    #[test]
    fn descriptor_removal_deletes_an_unchanged_directory_tree() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("tree");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::write(source.join("first"), b"first").unwrap();
        fs::write(source.join("nested/second"), b"second").unwrap();
        let path = store_path(&source);
        let mut provider = LocalStore::new();
        let expected = provider.inspect(&path, false).unwrap();
        let token = provider.prepare_source_removal(&path, &expected).unwrap();

        provider.remove_source(&path, &expected, &token).unwrap();

        assert!(!source.exists());
    }

    #[test]
    fn atime_change_does_not_invalidate_source_removal() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        fs::write(&source, b"read changes atime on relatime mounts").unwrap();
        let old = Timespec {
            tv_sec: 1,
            tv_nsec: 0,
        };
        utimensat(
            CWD,
            &source,
            &Timestamps {
                last_access: old,
                last_modification: old,
            },
            AtFlags::empty(),
        )
        .unwrap();
        let path = store_path(&source);
        let mut provider = LocalStore::new();
        let expected = provider.inspect(&path, false).unwrap();
        let mut bytes = Vec::new();
        File::open(&source)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        let token = provider.prepare_source_removal(&path, &expected).unwrap();

        provider.remove_source(&path, &expected, &token).unwrap();
        assert!(!source.exists());
    }

    #[test]
    fn removal_reports_partial_progress_after_injected_fault() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("tree");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("a"), b"a").unwrap();
        fs::write(source.join("b"), b"b").unwrap();
        let path = store_path(&source);
        let mut provider = LocalStore::new();
        let expected = provider.inspect(&path, false).unwrap();
        let token = provider.prepare_source_removal(&path, &expected).unwrap();
        provider.source_removal_fault_after = Some(1);

        assert_eq!(
            provider.remove_source(&path, &expected, &token),
            Err(ProviderError::SourcePartiallyRemoved)
        );
        assert!(source.exists());
        assert_eq!(fs::read_dir(source).unwrap().count(), 1);
    }

    // OPS-006: the removal identity keeps ctime for an entry with one link, so
    // a child rewritten in place after planning is refused at removal even
    // when its length and mtime were restored.
    #[test]
    fn a_regular_entry_rewritten_with_a_restored_mtime_is_refused_at_removal() {
        use std::io::Write as _;

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("tree");
        fs::create_dir(&source).unwrap();
        let child = source.join("child");
        fs::write(&child, b"original").unwrap();
        let before = fs::metadata(&child).unwrap();
        let path = store_path(&source);
        let mut provider = LocalStore::new();
        let expected = provider.inspect(&path, false).unwrap();
        let token = provider.prepare_source_removal(&path, &expected).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::OpenOptions::new()
            .write(true)
            .open(&child)
            .unwrap()
            .write_all(b"REWRITE!")
            .unwrap();
        let restored = Timespec {
            tv_sec: before.mtime(),
            tv_nsec: before.mtime_nsec(),
        };
        utimensat(
            CWD,
            &child,
            &Timestamps {
                last_access: restored,
                last_modification: restored,
            },
            AtFlags::empty(),
        )
        .unwrap();
        let after = fs::metadata(&child).unwrap();
        assert_eq!(after.len(), before.len());
        assert_eq!(after.mtime_nsec(), before.mtime_nsec());

        assert_eq!(
            provider.remove_source(&path, &expected, &token),
            Err(ProviderError::SourceChanged)
        );
        assert_eq!(
            fs::read(&child).unwrap(),
            b"REWRITE!",
            "the changed child stays"
        );
    }
}
