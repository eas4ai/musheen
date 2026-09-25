use super::VolumeError;
use nix::sys::statvfs::statvfs;
use proc_mounts::MountList;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity {
    total_bytes: u64,
    available_bytes: u64,
}

impl Capacity {
    #[must_use]
    pub const fn new(total_bytes: u64, available_bytes: u64) -> Self {
        Self {
            total_bytes,
            available_bytes,
        }
    }

    #[must_use]
    pub const fn total_bytes(self) -> u64 {
        self.total_bytes
    }

    #[must_use]
    pub const fn available_bytes(self) -> u64 {
        self.available_bytes
    }

    #[must_use]
    pub const fn used_bytes(self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MountRecord {
    source: PathBuf,
    destination: PathBuf,
    filesystem_type: Box<str>,
    read_only: bool,
}

impl MountRecord {
    #[must_use]
    pub fn new(
        source: impl Into<PathBuf>,
        destination: impl Into<PathBuf>,
        filesystem_type: impl Into<Box<str>>,
        read_only: bool,
    ) -> Self {
        Self {
            source: source.into(),
            destination: destination.into(),
            filesystem_type: filesystem_type.into(),
            read_only,
        }
    }

    #[must_use]
    pub fn source(&self) -> &Path {
        &self.source
    }

    #[must_use]
    pub fn destination(&self) -> &Path {
        &self.destination
    }

    #[must_use]
    pub fn filesystem_type(&self) -> &str {
        &self.filesystem_type
    }

    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }
}

/// The only application-owned mount-table boundary. Browser and UI code consume
/// volume snapshots and never parse `/proc/mounts` themselves.
pub trait MountProvider: Send + Sync {
    fn snapshot(&self) -> Result<Vec<MountRecord>, VolumeError>;
    fn capacity(&self, path: &Path) -> Result<Capacity, VolumeError>;
}

#[derive(Clone, Debug)]
pub struct ProcMountProvider {
    mount_table: PathBuf,
}

impl ProcMountProvider {
    #[must_use]
    pub fn system() -> Self {
        Self {
            mount_table: PathBuf::from("/proc/mounts"),
        }
    }

    #[must_use]
    pub fn from_path(path: impl Into<PathBuf>) -> Self {
        Self {
            mount_table: path.into(),
        }
    }
}

impl Default for ProcMountProvider {
    fn default() -> Self {
        Self::system()
    }
}

impl MountProvider for ProcMountProvider {
    fn snapshot(&self) -> Result<Vec<MountRecord>, VolumeError> {
        let mounts = MountList::new_from_file(&self.mount_table).map_err(|error| {
            VolumeError::MountTable(
                format!("could not read {}: {error}", self.mount_table.display()).into(),
            )
        })?;
        Ok(mounts
            .0
            .into_iter()
            .map(|mount| {
                let read_only = mount.options.iter().any(|option| option == "ro");
                MountRecord::new(mount.source, mount.dest, mount.fstype, read_only)
            })
            .collect())
    }

    fn capacity(&self, path: &Path) -> Result<Capacity, VolumeError> {
        let information = statvfs(path).map_err(|error| VolumeError::Capacity {
            path: path.to_path_buf(),
            reason: error.to_string().into(),
        })?;
        let fragment_size = information.fragment_size();
        Ok(Capacity::new(
            information.blocks().saturating_mul(fragment_size),
            information.blocks_available().saturating_mul(fragment_size),
        ))
    }
}
