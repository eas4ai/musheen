use rustix::fs::{Mode, OFlags, openat};
use std::ffi::OsString;
use std::fs::File;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::io::AsRawFd as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::broker::{FileIdentity, open_absolute_no_symlinks};
use super::{BrokerError, Clock, PrivilegeProvider};

static NEXT_GRANT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct RootCapabilityDescriptor {
    #[serde(with = "super::request::path_bytes")]
    root: PathBuf,
    device: u64,
    inode: u64,
    expires_at_unix_millis: u64,
    grant_id: Box<str>,
}

impl RootCapabilityDescriptor {
    pub fn capture(
        root: impl AsRef<Path>,
        expires_at_unix_millis: u64,
    ) -> Result<Self, BrokerError> {
        let file = open_absolute_no_symlinks(root.as_ref(), true)?;
        Self::from_file(root.as_ref(), &file, expires_at_unix_millis)
    }

    pub(crate) fn from_file(
        root: &Path,
        file: &File,
        expires_at_unix_millis: u64,
    ) -> Result<Self, BrokerError> {
        let metadata = file.metadata().map_err(|_| BrokerError::Io)?;
        Ok(Self {
            root: root.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
            expires_at_unix_millis,
            grant_id: format!(
                "{}-{}",
                std::process::id(),
                NEXT_GRANT_ID.fetch_add(1, Ordering::Relaxed)
            )
            .into_boxed_str(),
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }

    #[must_use]
    pub const fn expires_at_unix_millis(&self) -> u64 {
        self.expires_at_unix_millis
    }
}

#[derive(Debug)]
pub struct RootGrant {
    root: PathBuf,
    file: File,
    identity: FileIdentity,
    grant_id: Box<str>,
    expires_at_unix_millis: u64,
    provider: PrivilegeProvider,
}

impl RootGrant {
    pub fn open(
        root: impl AsRef<Path>,
        grant_id: impl Into<Box<str>>,
        expires_at_unix_millis: u64,
        provider: PrivilegeProvider,
    ) -> Result<Self, BrokerError> {
        let root = root.as_ref();
        let file = open_absolute_no_symlinks(root, true)?;
        let identity = FileIdentity::from_metadata(&file.metadata().map_err(|_| BrokerError::Io)?);
        Ok(Self {
            root: root.to_path_buf(),
            file,
            identity,
            grant_id: grant_id.into(),
            expires_at_unix_millis,
            provider,
        })
    }

    pub(crate) fn from_descriptor_file(
        descriptor: RootCapabilityDescriptor,
        file: File,
        provider: PrivilegeProvider,
    ) -> Result<Self, BrokerError> {
        let metadata = file.metadata().map_err(|_| BrokerError::Io)?;
        if metadata.dev() != descriptor.device || metadata.ino() != descriptor.inode {
            return Err(BrokerError::TargetReplaced);
        }
        Ok(Self {
            root: descriptor.root,
            file,
            identity: FileIdentity::from_metadata(&metadata),
            grant_id: descriptor.grant_id,
            expires_at_unix_millis: descriptor.expires_at_unix_millis,
            provider,
        })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn grant_id(&self) -> &str {
        &self.grant_id
    }

    #[must_use]
    pub const fn expires_at_unix_millis(&self) -> u64 {
        self.expires_at_unix_millis
    }

    #[must_use]
    pub const fn provider(&self) -> PrivilegeProvider {
        self.provider
    }
}

#[derive(Debug)]
pub struct RootedEntry {
    path: PathBuf,
    file: File,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum RootedEntryKind {
    Directory,
    RegularFile,
    SymbolicLink,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootedDirectoryEntry {
    name: OsString,
    identity: [u8; 16],
    kind: RootedEntryKind,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
}

impl RootedDirectoryEntry {
    #[must_use]
    pub fn name(&self) -> &std::ffi::OsStr {
        &self.name
    }

    #[must_use]
    pub const fn identity(&self) -> &[u8; 16] {
        &self.identity
    }

    #[must_use]
    pub const fn kind(&self) -> RootedEntryKind {
        self.kind
    }

    #[must_use]
    pub const fn size(&self) -> Option<u64> {
        self.size
    }

    #[must_use]
    pub const fn modified_unix_seconds(&self) -> Option<i64> {
        self.modified_unix_seconds
    }
}

impl RootedEntry {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn file(&self) -> &File {
        &self.file
    }
}

impl PartialEq for RootedEntry {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.identity == other.identity
    }
}

impl Eq for RootedEntry {}

pub struct RootedStore<C> {
    grant: RootGrant,
    clock: C,
}

impl<C: Clock> RootedStore<C> {
    #[must_use]
    pub const fn new(grant: RootGrant, clock: C) -> Self {
        Self { grant, clock }
    }

    #[must_use]
    pub const fn grant(&self) -> &RootGrant {
        &self.grant
    }

    pub fn resolve(&self, relative: &Path) -> Result<RootedEntry, BrokerError> {
        if self.grant.expires_at_unix_millis <= self.clock.now_unix_millis() {
            return Err(BrokerError::AuthorizationExpired);
        }
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(BrokerError::ScopeEscape);
        }
        let mut directory = self.grant.file.try_clone().map_err(|_| BrokerError::Io)?;
        let mut resolved = PathBuf::new();
        for component in relative.components() {
            let Component::Normal(name) = component else {
                continue;
            };
            let file = File::from(
                openat(
                    &directory,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|error| {
                    if error == rustix::io::Errno::LOOP || error == rustix::io::Errno::NOTDIR {
                        BrokerError::SymlinkRefused
                    } else {
                        BrokerError::Io
                    }
                })?,
            );
            directory = file;
            resolved.push(name);
        }
        let metadata = directory.metadata().map_err(|_| BrokerError::Io)?;
        let root_metadata = self.grant.file.metadata().map_err(|_| BrokerError::Io)?;
        if FileIdentity::from_metadata(&root_metadata) != self.grant.identity {
            return Err(BrokerError::TargetReplaced);
        }
        Ok(RootedEntry {
            path: self.grant.root.join(resolved),
            identity: FileIdentity::from_metadata(&metadata),
            file: directory,
        })
    }

    pub fn read_directory(
        &self,
        relative: &Path,
    ) -> Result<Vec<RootedDirectoryEntry>, BrokerError> {
        let directory = self.resolve(relative)?;
        let descriptor_path =
            PathBuf::from(format!("/proc/self/fd/{}", directory.file().as_raw_fd()));
        let mut entries = std::fs::read_dir(descriptor_path)
            .map_err(|_| BrokerError::Io)?
            .map(|entry| {
                let entry = entry.map_err(|_| BrokerError::Io)?;
                let metadata = entry.metadata().map_err(|_| BrokerError::Io)?;
                let file_type = entry.file_type().map_err(|_| BrokerError::Io)?;
                let kind = if file_type.is_dir() {
                    RootedEntryKind::Directory
                } else if file_type.is_file() {
                    RootedEntryKind::RegularFile
                } else if file_type.is_symlink() {
                    RootedEntryKind::SymbolicLink
                } else {
                    RootedEntryKind::Other
                };
                let mut identity = [0_u8; 16];
                identity[..8].copy_from_slice(&metadata.dev().to_le_bytes());
                identity[8..].copy_from_slice(&metadata.ino().to_le_bytes());
                Ok(RootedDirectoryEntry {
                    name: entry.file_name(),
                    identity,
                    kind,
                    size: file_type.is_file().then_some(metadata.len()),
                    modified_unix_seconds: metadata
                        .mtime()
                        .is_positive()
                        .then_some(metadata.mtime()),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        entries.sort_by(|left, right| left.name.as_bytes().cmp(right.name.as_bytes()));
        Ok(entries)
    }
}
