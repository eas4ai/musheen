use crate::metadata::io_error;
use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, StoreError, StorePath,
};
use nix::sys::statfs::{
    BTRFS_SUPER_MAGIC, EXT4_SUPER_MAGIC, MSDOS_SUPER_MAGIC, NFS_SUPER_MAGIC, TMPFS_MAGIC,
    XFS_SUPER_MAGIC, statfs,
};
use proc_mounts::MountIter;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalFilesystemInfo {
    filesystem_type: Box<str>,
    mount_point: PathBuf,
    read_only: bool,
    available_bytes: u64,
}

impl LocalFilesystemInfo {
    #[must_use]
    pub fn filesystem_type(&self) -> &str {
        &self.filesystem_type
    }

    #[must_use]
    pub fn mount_point(&self) -> &Path {
        &self.mount_point
    }

    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    #[must_use]
    pub fn available_bytes(&self) -> u64 {
        self.available_bytes
    }
}

pub(crate) fn probe(path: &StorePath) -> Result<LocalFilesystemInfo, StoreError> {
    let unix_path = path.as_unix_path().ok_or_else(|| {
        StoreError::unsupported("probe", "the local provider accepts only Unix paths")
    })?;
    let stats = statfs(unix_path).map_err(|error| {
        io_error(
            "probe filesystem",
            Some(path.clone()),
            std::io::Error::from_raw_os_error(error as i32),
        )
    })?;
    let mut mount = None;
    for candidate in MountIter::new().map_err(|error| io_error("read mount table", None, error))? {
        let candidate = candidate.map_err(|error| io_error("parse mount table", None, error))?;
        if unix_path.starts_with(&candidate.dest)
            && mount
                .as_ref()
                .is_none_or(|current: &proc_mounts::MountInfo| {
                    candidate.dest.as_os_str().len() > current.dest.as_os_str().len()
                })
        {
            mount = Some(candidate);
        }
    }
    let filesystem_type = mount.as_ref().map_or_else(
        || filesystem_name_from_magic(stats.filesystem_type()).into(),
        |mount| mount.fstype.clone().into(),
    );
    let mount_point = mount
        .as_ref()
        .map_or_else(|| PathBuf::from("/"), |mount| mount.dest.clone());
    let read_only = mount
        .as_ref()
        .is_some_and(|mount| mount.options.iter().any(|option| option == "ro"));
    let blocks = stats.blocks_available();
    let block_size = u64::try_from(stats.block_size()).unwrap_or(0);

    Ok(LocalFilesystemInfo {
        filesystem_type,
        mount_point,
        read_only,
        available_bytes: blocks.saturating_mul(block_size),
    })
}

pub(crate) fn capabilities(path: &StorePath) -> CapabilityMatrix {
    match probe(path) {
        Ok(info) => capabilities_from_info(&info),
        Err(error) => {
            let reason = CapabilityReason::new(format!("filesystem probe failed: {error}"))
                .expect("a formatted probe error is visible");
            CapabilityMatrix::new(|_| CapabilityState::Unknown(reason.clone()))
        }
    }
}

fn capabilities_from_info(info: &LocalFilesystemInfo) -> CapabilityMatrix {
    let filesystem = info.filesystem_type.as_ref();
    let fat_like = matches!(filesystem, "vfat" | "msdos" | "exfat" | "ntfs" | "ntfs3");
    let native_unix = matches!(
        filesystem,
        "ext2" | "ext3" | "ext4" | "btrfs" | "xfs" | "tmpfs" | "overlay" | "nfs" | "nfs4"
    );
    CapabilityMatrix::new(|kind| {
        if info.read_only && mutation_capability(kind) {
            return unsupported("the containing mount is read-only");
        }
        match kind {
            CapabilityKind::Permissions | CapabilityKind::Ownership if native_unix => {
                CapabilityState::Supported
            }
            CapabilityKind::SymbolicLinks | CapabilityKind::HardLinks if native_unix => {
                CapabilityState::Supported
            }
            CapabilityKind::SparseFiles if native_unix => CapabilityState::Supported,
            CapabilityKind::ExtendedAttributes if native_unix => CapabilityState::Supported,
            CapabilityKind::ReflinkCopies if matches!(filesystem, "btrfs" | "xfs") => {
                CapabilityState::Supported
            }
            CapabilityKind::Trash => {
                unknown("trash support depends on desktop and directory policy")
            }
            CapabilityKind::AtomicRename if native_unix => CapabilityState::Supported,
            CapabilityKind::Watching => CapabilityState::Supported,
            CapabilityKind::CaseSensitivity if native_unix => CapabilityState::Supported,
            _ if fat_like => unsupported("the filesystem does not provide this Unix capability"),
            _ => unknown("the filesystem capability has not been safely established"),
        }
    })
}

fn mutation_capability(kind: CapabilityKind) -> bool {
    !matches!(
        kind,
        CapabilityKind::Watching | CapabilityKind::CaseSensitivity
    )
}

fn supported_magic_name(filesystem: nix::sys::statfs::FsType) -> Option<&'static str> {
    if filesystem == EXT4_SUPER_MAGIC {
        Some("ext4")
    } else if filesystem == BTRFS_SUPER_MAGIC {
        Some("btrfs")
    } else if filesystem == XFS_SUPER_MAGIC {
        Some("xfs")
    } else if filesystem == TMPFS_MAGIC {
        Some("tmpfs")
    } else if filesystem == MSDOS_SUPER_MAGIC {
        Some("vfat")
    } else if filesystem == NFS_SUPER_MAGIC {
        Some("nfs")
    } else {
        None
    }
}

fn filesystem_name_from_magic(filesystem: nix::sys::statfs::FsType) -> String {
    supported_magic_name(filesystem)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{filesystem:?}"))
}

fn unsupported(reason: &'static str) -> CapabilityState {
    CapabilityState::Unsupported(
        CapabilityReason::new(reason).expect("the built-in capability reason is visible"),
    )
}

fn unknown(reason: &'static str) -> CapabilityState {
    CapabilityState::Unknown(
        CapabilityReason::new(reason).expect("the built-in capability reason is visible"),
    )
}
