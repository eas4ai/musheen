//! Decides whether normal delete can trash an item without the `trash`
//! crate copying it across mounts.
//!
//! The crate (5.2.9, `freedesktop.rs`) picks a trash directory like this.
//! An item on the same mount as the home trash is renamed into the home
//! trash. Otherwise the crate uses `.Trash/$uid` inside a valid shared
//! `.Trash` (a real directory with the sticky bit) when that directory
//! exists, else `.Trash-$uid` on the item's mount root, which it creates
//! when missing. When that per-volume step fails with permission denied,
//! the crate moves the item to the home trash instead. Every move is a
//! rename; a rename that crosses a mount or a device (a nested btrfs
//! subvolume) fails with EXDEV and the crate falls back to copy and
//! delete. That copy is unverified and loses hard
//! links, sparse layout, and timestamps, so Musheen reports no trash
//! support for such items and OPS-008 offers permanent delete instead.

use rustix::fs::{Access, AtFlags, CWD, StatxFlags, access, statx};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The facts trash support is decided from: where the home trash lives,
/// the mount points sorted longest first, and the user whose per-volume
/// trash directory would be used.
#[derive(Clone, Debug)]
pub(crate) struct TrashEnvironment {
    pub(crate) home_trash: PathBuf,
    pub(crate) mount_points: Vec<PathBuf>,
    pub(crate) uid: u32,
}

/// Where the `trash` crate would put an item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TrashDisposition {
    /// Renamed into the home trash on the same mount.
    HomeTrash,
    /// Renamed into a trash directory on the item's own mount.
    VolumeTrash,
    /// Copied across mounts, then deleted.
    WouldCopy,
    /// The crate would fail before moving anything, or no home is known.
    Unusable,
}

impl TrashDisposition {
    /// True when the crate would rename the item, never copy it.
    pub(crate) const fn is_safe(self) -> bool {
        matches!(self, Self::HomeTrash | Self::VolumeTrash)
    }
}

impl TrashEnvironment {
    /// Reads the home trash location, the mount table, and the user id the
    /// way the `trash` crate does. `Ok(None)` means no home is configured.
    pub(crate) fn current() -> io::Result<Option<Self>> {
        let Some(home_trash) = home_trash() else {
            return Ok(None);
        };
        let mut mount_points = parse_mount_points(&fs::read("/proc/self/mounts")?);
        mount_points.sort_by_key(|point| std::cmp::Reverse(point.as_os_str().len()));
        Ok(Some(Self {
            home_trash: canonicalize_or_parents(&home_trash),
            mount_points,
            uid: rustix::process::getuid().as_raw(),
        }))
    }

    /// Where the crate would put `item`.
    pub(crate) fn disposition(&self, item: &Path) -> TrashDisposition {
        let item = canonical_item_path(item);
        let item_top = self.top_dir(&item);
        if item_top == self.top_dir(&self.home_trash) {
            return self.home_trash_disposition(&item);
        }
        let shared = item_top.join(".Trash");
        if is_valid_shared_trash(&shared) {
            let mine = shared.join(self.uid.to_string());
            if mine.is_dir() {
                // The crate uses this directory and looks no further.
                return self.volume_trash_disposition(&item, &mine);
            }
        }
        let mine = item_top.join(format!(".Trash-{}", self.uid));
        match fs::symlink_metadata(&mine) {
            // The crate follows a link here, so the target decides.
            Ok(_) => match fs::metadata(&mine) {
                Ok(metadata) if metadata.is_dir() => self.volume_trash_disposition(&item, &mine),
                // A file or a dangling link: creating the directory fails
                // with an error the crate reports instead of copying.
                _ => TrashDisposition::Unusable,
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if writable(item_top) {
                    // The crate creates the directory on the mount root.
                    if same_mount(&item, item_top) {
                        TrashDisposition::VolumeTrash
                    } else {
                        TrashDisposition::WouldCopy
                    }
                } else {
                    self.home_trash_disposition(&item)
                }
            }
            Err(_) => TrashDisposition::Unusable,
        }
    }

    /// The crate renames the item into the home trash. Across mounts that
    /// rename fails and the crate copies.
    fn home_trash_disposition(&self, item: &Path) -> TrashDisposition {
        if same_mount(item, &self.home_trash) {
            TrashDisposition::HomeTrash
        } else {
            TrashDisposition::WouldCopy
        }
    }

    /// The crate writes `files` and `info` under `folder` and renames the
    /// item into `files`. A folder it cannot write fails with permission
    /// denied, which sends the crate to the home trash instead.
    fn volume_trash_disposition(&self, item: &Path, folder: &Path) -> TrashDisposition {
        if !trash_folder_writable(folder) {
            return self.home_trash_disposition(item);
        }
        if same_mount(item, &folder.join("files")) {
            TrashDisposition::VolumeTrash
        } else {
            TrashDisposition::WouldCopy
        }
    }

    /// The longest mount point that contains `path`, or the root.
    fn top_dir(&self, path: &Path) -> &Path {
        self.mount_points
            .iter()
            .map(PathBuf::as_path)
            .find(|point| path.starts_with(point))
            .unwrap_or_else(|| Path::new("/"))
    }
}

fn home_trash() -> Option<PathBuf> {
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME")
        && !data_home.is_empty()
    {
        return Some(PathBuf::from(data_home).join("Trash"));
    }
    let home = std::env::var_os("HOME")?;
    (!home.is_empty()).then(|| PathBuf::from(home).join(".local/share/Trash"))
}

/// Mount directories from `/proc/self/mounts`, with the octal escapes the
/// kernel writes for space, tab, newline, and backslash decoded.
fn parse_mount_points(mounts: &[u8]) -> Vec<PathBuf> {
    mounts
        .split(|byte| *byte == b'\n')
        .filter_map(|line| line.split(|byte| *byte == b' ').nth(1))
        .filter(|field| !field.is_empty())
        .map(|field| PathBuf::from(std::ffi::OsStr::from_bytes(&decode_mount_escapes(field))))
        .collect()
}

fn decode_mount_escapes(field: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] == b'\\' && field.len() - index >= 4 {
            let digits = &field[index + 1..index + 4];
            if digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                let value = digits
                    .iter()
                    .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'));
                if let Ok(byte) = u8::try_from(value) {
                    decoded.push(byte);
                    index += 4;
                    continue;
                }
            }
        }
        decoded.push(field[index]);
        index += 1;
    }
    decoded
}

/// The path the crate trashes: the canonical parent joined with the item's
/// own name, so a symbolic link is trashed as itself and not as its target.
fn canonical_item_path(item: &Path) -> PathBuf {
    match (item.parent(), item.file_name()) {
        (Some(parent), Some(name)) => canonicalize_or_parents(parent).join(name),
        _ => canonicalize_or_parents(item),
    }
}

/// Canonicalizes a path, resolving through the nearest existing ancestor
/// when the path itself does not exist, as the `trash` crate does.
fn canonicalize_or_parents(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::canonicalize(current) {
            Ok(canonical) => {
                return missing
                    .iter()
                    .rev()
                    .fold(canonical, |path, component| path.join(component));
            }
            Err(_) => match (current.file_name(), current.parent()) {
                (Some(name), Some(parent)) => {
                    missing.push(name.to_owned());
                    current = parent;
                }
                _ => return path.to_path_buf(),
            },
        }
    }
}

/// Whether the crate can write the `files` and `info` directories of a
/// trash folder: an existing one must be writable, a missing one needs a
/// writable folder to be created in.
fn trash_folder_writable(folder: &Path) -> bool {
    ["files", "info"].iter().all(|name| {
        let child = folder.join(name);
        if child.is_dir() {
            writable(&child)
        } else {
            writable(folder)
        }
    })
}

/// Whether a rename of `item` into `target` stays on one filesystem.
/// `target` may not exist yet; the crate creates it under its nearest
/// existing ancestor.
fn same_mount(item: &Path, target: &Path) -> bool {
    let Some(existing) = target
        .ancestors()
        .find(|ancestor| fs::metadata(ancestor).is_ok())
    else {
        return false;
    };
    match (
        filesystem_identity(item, AtFlags::SYMLINK_NOFOLLOW),
        filesystem_identity(existing, AtFlags::empty()),
    ) {
        (Some(item), Some(target)) => rename_stays_on_one_filesystem(&item, &target),
        _ => false,
    }
}

/// What rename(2) needs to agree on. The mount: a rename across mounts
/// fails with EXDEV even when both sides are one device (a bind mount).
/// The device: a nested btrfs subvolume shares its parent's mount id but
/// has its own device, and a rename across its boundary fails the same way.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FilesystemIdentity {
    /// `None` on kernels without STATX_MNT_ID.
    mount: Option<u64>,
    device: (u32, u32),
}

fn rename_stays_on_one_filesystem(item: &FilesystemIdentity, target: &FilesystemIdentity) -> bool {
    item.device == target.device && item.mount == target.mount
}

fn filesystem_identity(path: &Path, flags: AtFlags) -> Option<FilesystemIdentity> {
    let stat = statx(CWD, path, flags | AtFlags::NO_AUTOMOUNT, StatxFlags::MNT_ID).ok()?;
    let mount = (stat.stx_mask & StatxFlags::MNT_ID.bits() != 0).then_some(stat.stx_mnt_id);
    Some(FilesystemIdentity {
        mount,
        device: (stat.stx_dev_major, stat.stx_dev_minor),
    })
}

/// A shared `.Trash` directory is usable only when it is a real directory
/// with the sticky bit set, as the freedesktop trash specification requires.
fn is_valid_shared_trash(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.is_dir() && metadata.mode() & 0o1000 != 0)
        .unwrap_or(false)
}

fn writable(path: &Path) -> bool {
    access(path, Access::WRITE_OK).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_points_decode_the_kernel_escapes() {
        let mounts = b"tmpfs /run/user/1000 tmpfs rw 0 0\nsda1 /media/disk\\040one ext4 rw 0 0\n";
        assert_eq!(
            parse_mount_points(mounts),
            [
                PathBuf::from("/run/user/1000"),
                PathBuf::from("/media/disk one")
            ]
        );
    }

    #[test]
    fn the_longest_mount_point_wins() {
        let environment = TrashEnvironment {
            home_trash: PathBuf::from("/home/user/.local/share/Trash"),
            mount_points: vec![
                PathBuf::from("/home/user/data"),
                PathBuf::from("/home"),
                PathBuf::from("/"),
            ],
            uid: 1000,
        };
        assert_eq!(
            environment.top_dir(Path::new("/home/user/data/x")),
            Path::new("/home/user/data")
        );
        assert_eq!(
            environment.top_dir(Path::new("/home/user/x")),
            Path::new("/home")
        );
        assert_eq!(environment.top_dir(Path::new("/srv/x")), Path::new("/"));
    }

    #[test]
    fn a_missing_home_trash_resolves_through_its_parents() {
        let temporary = tempfile::tempdir().unwrap();
        let canonical = fs::canonicalize(temporary.path()).unwrap();
        assert_eq!(
            canonicalize_or_parents(&temporary.path().join("missing/Trash")),
            canonical.join("missing/Trash")
        );
    }

    // OPS-008: a nested btrfs subvolume keeps its parent's mount id but has
    // its own device; a bind mount keeps the device but not the mount id.
    // rename(2) fails across either boundary, so the crate would copy.
    #[test]
    fn trash_is_refused_where_the_device_differs_under_one_mount_id() {
        let parent = FilesystemIdentity {
            mount: Some(40),
            device: (0, 60),
        };
        let subvolume = FilesystemIdentity {
            mount: Some(40),
            device: (0, 61),
        };
        let bind_mount = FilesystemIdentity {
            mount: Some(41),
            device: (0, 60),
        };
        assert!(rename_stays_on_one_filesystem(&parent, &parent));
        assert!(!rename_stays_on_one_filesystem(&parent, &subvolume));
        assert!(!rename_stays_on_one_filesystem(&parent, &bind_mount));
    }

    #[test]
    fn a_link_keeps_its_own_name_when_canonicalized() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(
            canonical_item_path(&link),
            fs::canonicalize(temporary.path()).unwrap().join("link")
        );
    }
}
