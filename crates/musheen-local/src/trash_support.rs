//! Decides whether normal delete can trash an item without the `trash`
//! crate copying it across devices.
//!
//! The crate (5.2.9, `freedesktop.rs`) picks a trash directory like this.
//! An item on the same mount as the home trash is renamed into the home
//! trash. Otherwise the item's mount must hold `.Trash/$uid` or
//! `.Trash-$uid`; the crate creates `.Trash-$uid` when the mount root is
//! writable. When that fails with permission denied, the crate copies the
//! item to the home trash and deletes the original. That copy is
//! unverified and loses hard links, sparse layout, and timestamps, so
//! Musheen reports no trash support for such items and OPS-008 offers
//! permanent delete instead.

use rustix::fs::{Access, access};
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
    /// Renamed into the home trash on the same device.
    HomeTrash,
    /// Renamed into a trash directory on the item's own mount.
    VolumeTrash,
    /// Copied to the home trash across devices, then deleted.
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
        let item = canonicalize_or_parents(item);
        let item_top = self.top_dir(&item);
        if item_top == self.top_dir(&self.home_trash) {
            return if same_device(&item, &self.home_trash) {
                TrashDisposition::HomeTrash
            } else {
                TrashDisposition::WouldCopy
            };
        }
        let shared = item_top.join(".Trash");
        if is_valid_shared_trash(&shared) {
            let mine = shared.join(self.uid.to_string());
            if mine.is_dir() && writable(&mine) {
                return TrashDisposition::VolumeTrash;
            }
        }
        let mine = item_top.join(format!(".Trash-{}", self.uid));
        match fs::symlink_metadata(&mine) {
            Ok(metadata) if metadata.is_dir() => {
                if writable(&mine) {
                    TrashDisposition::VolumeTrash
                } else {
                    TrashDisposition::WouldCopy
                }
            }
            Ok(_) => TrashDisposition::Unusable,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if writable(item_top) {
                    TrashDisposition::VolumeTrash
                } else {
                    TrashDisposition::WouldCopy
                }
            }
            Err(_) => TrashDisposition::Unusable,
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

fn same_device(item: &Path, home_trash: &Path) -> bool {
    let Ok(item) = fs::symlink_metadata(item) else {
        return false;
    };
    let Some(existing) = nearest_existing(home_trash) else {
        return false;
    };
    item.dev() == existing.dev()
}

fn nearest_existing(path: &Path) -> Option<fs::Metadata> {
    path.ancestors().find_map(|ancestor| fs::metadata(ancestor).ok())
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
            [PathBuf::from("/run/user/1000"), PathBuf::from("/media/disk one")]
        );
    }

    #[test]
    fn the_longest_mount_point_wins() {
        let environment = TrashEnvironment {
            home_trash: PathBuf::from("/home/user/.local/share/Trash"),
            mount_points: vec![PathBuf::from("/home/user/data"), PathBuf::from("/home"), PathBuf::from("/")],
            uid: 1000,
        };
        assert_eq!(environment.top_dir(Path::new("/home/user/data/x")), Path::new("/home/user/data"));
        assert_eq!(environment.top_dir(Path::new("/home/user/x")), Path::new("/home"));
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
}
