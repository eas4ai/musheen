use crate::LocalStore;
use musheen_core::{CapabilityKind, CapabilityState, StorePath};
use musheen_ops::{
    AclChange, AclEntry, AclQualifier, CreateKind, DeleteProvider, DeleteTarget, LinkProvider,
    MetadataEntry, MetadataEntryKind, MetadataProvider, MetadataScope, MutationError,
    MutationProvider, ResolvedMetadataChange, TrashReceipt,
};
use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, PosixACL, Qualifier};
use rustix::fd::OwnedFd;
use rustix::fs::{
    AtFlags, Gid, Mode, OFlags, RenameFlags, StatxFlags, Uid, chownat, fchmod, fchown, fsync,
    linkat, mkdirat, open, openat, renameat_with, statx, symlinkat, unlinkat,
};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use walkdir::WalkDir;

static TRASH_LOCK: Mutex<()> = Mutex::new(());

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
        let id = OsString::from_vec(receipt.provider_reference().to_vec());
        let item = trash::os_limited::list()
            .map_err(map_trash_error)?
            .into_iter()
            .find(|item| item.id == id)
            .ok_or(MutationError::Missing)?;
        trash::os_limited::restore_all([item]).map_err(map_trash_error)
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
