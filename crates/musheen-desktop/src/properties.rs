use crate::{MimeDetector, PermissionSnapshot};
use musheen_core::{CancellationToken, ItemKind};
use std::ffi::OsStr;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use walkdir::WalkDir;

const MAX_PROPERTY_TARGETS: usize = 256;
const MAX_RECURSIVE_ERRORS: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AggregateValue<T> {
    Same(T),
    Mixed,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PropertyIdentity {
    device: u64,
    inode: u64,
}

impl PropertyIdentity {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    pub fn device(self) -> u64 {
        self.device
    }

    pub fn inode(self) -> u64 {
        self.inode
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PropertyTimestamp {
    seconds: i64,
    nanoseconds: i64,
}

impl PropertyTimestamp {
    pub fn seconds(self) -> i64 {
        self.seconds
    }

    pub fn nanoseconds(self) -> i64 {
        self.nanoseconds
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct XattrEntry {
    name: Box<[u8]>,
    value: Box<[u8]>,
}

impl XattrEntry {
    pub fn name(&self) -> &OsStr {
        OsStr::from_bytes(&self.name)
    }

    pub fn value(&self) -> &[u8] {
        &self.value
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum XattrState {
    Available(Vec<XattrEntry>),
    Unavailable(Box<str>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct MutableFingerprint {
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    mode: u32,
    owner: u32,
    group: u32,
}

impl MutableFingerprint {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            mode: metadata.mode(),
            owner: metadata.uid(),
            group: metadata.gid(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ItemProperties {
    path: PathBuf,
    identity: PropertyIdentity,
    mutable_fingerprint: MutableFingerprint,
    kind: ItemKind,
    mime_type: Box<str>,
    logical_size: u64,
    allocated_size: u64,
    modified: PropertyTimestamp,
    accessed: PropertyTimestamp,
    changed: PropertyTimestamp,
    permissions: PermissionSnapshot,
    xattrs: XattrState,
}

impl ItemProperties {
    fn load(path: &Path, detector: &MimeDetector) -> Result<Self, PropertyError> {
        let metadata = fs::symlink_metadata(path).map_err(PropertyError::Io)?;
        let file_type = metadata.file_type();
        let kind = if file_type.is_dir() {
            ItemKind::Directory
        } else if file_type.is_file() {
            ItemKind::RegularFile
        } else if file_type.is_symlink() {
            ItemKind::SymbolicLink
        } else {
            ItemKind::Other
        };
        let mime_type = detector
            .detect(path)
            .map_err(|error| PropertyError::Metadata(error.to_string().into()))?
            .mime_type()
            .into();
        let permissions = PermissionSnapshot::read(path, &metadata, file_type.is_symlink());
        Ok(Self {
            path: path.to_path_buf(),
            identity: PropertyIdentity::from_metadata(&metadata),
            mutable_fingerprint: MutableFingerprint::from_metadata(&metadata),
            kind,
            mime_type,
            logical_size: metadata.len(),
            allocated_size: metadata.blocks().saturating_mul(512),
            modified: PropertyTimestamp {
                seconds: metadata.mtime(),
                nanoseconds: metadata.mtime_nsec(),
            },
            accessed: PropertyTimestamp {
                seconds: metadata.atime(),
                nanoseconds: metadata.atime_nsec(),
            },
            changed: PropertyTimestamp {
                seconds: metadata.ctime(),
                nanoseconds: metadata.ctime_nsec(),
            },
            permissions,
            xattrs: read_xattrs(path, file_type.is_symlink()),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity(&self) -> PropertyIdentity {
        self.identity
    }

    pub fn kind(&self) -> ItemKind {
        self.kind
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn logical_size(&self) -> u64 {
        self.logical_size
    }

    pub fn allocated_size(&self) -> u64 {
        self.allocated_size
    }

    pub fn modified(&self) -> PropertyTimestamp {
        self.modified
    }

    pub fn accessed(&self) -> PropertyTimestamp {
        self.accessed
    }

    pub fn changed(&self) -> PropertyTimestamp {
        self.changed
    }

    pub fn permissions(&self) -> &PermissionSnapshot {
        &self.permissions
    }

    pub fn xattrs(&self) -> &XattrState {
        &self.xattrs
    }
}

fn read_xattrs(path: &Path, is_symlink: bool) -> XattrState {
    let names = if is_symlink {
        xattr::list(path)
    } else {
        xattr::list_deref(path)
    };
    let names = match names {
        Ok(names) => names,
        Err(error) => return XattrState::Unavailable(error.to_string().into()),
    };
    let mut values = Vec::new();
    for name in names {
        if matches!(
            name.as_bytes(),
            b"system.posix_acl_access" | b"system.posix_acl_default"
        ) {
            continue;
        }
        let value = if is_symlink {
            xattr::get(path, &name)
        } else {
            xattr::get_deref(path, &name)
        };
        match value {
            Ok(Some(value)) => values.push(XattrEntry {
                name: name.as_bytes().into(),
                value: value.into_boxed_slice(),
            }),
            Ok(None) => {}
            Err(error) => return XattrState::Unavailable(error.to_string().into()),
        }
    }
    XattrState::Available(values)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregatedProperties {
    kind: AggregateValue<ItemKind>,
    mime_type: AggregateValue<Box<str>>,
    mode: AggregateValue<u32>,
    owner: AggregateValue<u32>,
    group: AggregateValue<u32>,
    modified: AggregateValue<PropertyTimestamp>,
    accessed: AggregateValue<PropertyTimestamp>,
    changed: AggregateValue<PropertyTimestamp>,
    logical_size: u64,
    allocated_size: u64,
}

impl AggregatedProperties {
    fn from_items(items: &[ItemProperties]) -> Self {
        Self {
            kind: common(items.iter().map(ItemProperties::kind)),
            mime_type: common(items.iter().map(|item| item.mime_type.clone())),
            mode: common(items.iter().map(|item| item.permissions.mode())),
            owner: common(items.iter().map(|item| item.permissions.owner())),
            group: common(items.iter().map(|item| item.permissions.group())),
            modified: common(items.iter().map(ItemProperties::modified)),
            accessed: common(items.iter().map(ItemProperties::accessed)),
            changed: common(items.iter().map(ItemProperties::changed)),
            logical_size: items
                .iter()
                .fold(0_u64, |sum, item| sum.saturating_add(item.logical_size)),
            allocated_size: items
                .iter()
                .fold(0_u64, |sum, item| sum.saturating_add(item.allocated_size)),
        }
    }

    pub fn kind(&self) -> AggregateValue<ItemKind> {
        self.kind.clone()
    }

    pub fn mime_type(&self) -> AggregateValue<Box<str>> {
        self.mime_type.clone()
    }

    pub fn mode(&self) -> AggregateValue<u32> {
        self.mode.clone()
    }

    pub fn owner(&self) -> AggregateValue<u32> {
        self.owner.clone()
    }

    pub fn group(&self) -> AggregateValue<u32> {
        self.group.clone()
    }

    pub fn modified(&self) -> AggregateValue<PropertyTimestamp> {
        self.modified.clone()
    }

    pub fn accessed(&self) -> AggregateValue<PropertyTimestamp> {
        self.accessed.clone()
    }

    pub fn changed(&self) -> AggregateValue<PropertyTimestamp> {
        self.changed.clone()
    }

    pub fn logical_size(&self) -> u64 {
        self.logical_size
    }

    pub fn allocated_size(&self) -> u64 {
        self.allocated_size
    }
}

fn common<T: Clone + Eq>(mut values: impl Iterator<Item = T>) -> AggregateValue<T> {
    let Some(first) = values.next() else {
        return AggregateValue::Unavailable;
    };
    if values.all(|value| value == first) {
        AggregateValue::Same(first)
    } else {
        AggregateValue::Mixed
    }
}

#[derive(Debug)]
pub struct PropertySnapshot {
    items: Vec<ItemProperties>,
    aggregate: AggregatedProperties,
    identity_anchors: Vec<Option<Arc<File>>>,
}

impl PropertySnapshot {
    pub fn load(paths: &[PathBuf]) -> Result<Self, PropertyError> {
        if paths.is_empty() {
            return Err(PropertyError::EmptySelection);
        }
        if paths.len() > MAX_PROPERTY_TARGETS {
            return Err(PropertyError::TooManyTargets);
        }
        let detector = MimeDetector::default();
        let items = paths
            .iter()
            .map(|path| ItemProperties::load(path, &detector))
            .collect::<Result<Vec<_>, _>>()?;
        let aggregate = AggregatedProperties::from_items(&items);
        let identity_anchors = items
            .iter()
            .map(|item| {
                (item.kind != ItemKind::SymbolicLink)
                    .then(|| File::open(&item.path).ok())
                    .flatten()
                    .map(Arc::new)
            })
            .collect();
        Ok(Self {
            items,
            aggregate,
            identity_anchors,
        })
    }

    pub fn items(&self) -> &[ItemProperties] {
        &self.items
    }

    pub fn aggregate(&self) -> &AggregatedProperties {
        &self.aggregate
    }

    pub fn refresh_probe(&self) -> PropertyRefreshProbe {
        PropertyRefreshProbe {
            targets: self
                .items
                .iter()
                .zip(&self.identity_anchors)
                .map(|(item, anchor)| RefreshTarget {
                    path: item.path.clone(),
                    identity: item.identity,
                    mutable_fingerprint: item.mutable_fingerprint,
                    identity_anchor: anchor.clone(),
                })
                .collect(),
        }
    }

    pub fn refresh_state(&self) -> Result<PropertyRefresh, PropertyError> {
        self.refresh_probe().refresh_state()
    }
}

#[derive(Clone, Debug)]
struct RefreshTarget {
    path: PathBuf,
    identity: PropertyIdentity,
    mutable_fingerprint: MutableFingerprint,
    identity_anchor: Option<Arc<File>>,
}

#[derive(Clone, Debug)]
pub struct PropertyRefreshProbe {
    targets: Vec<RefreshTarget>,
}

impl PropertyRefreshProbe {
    pub fn refresh_state(&self) -> Result<PropertyRefresh, PropertyError> {
        let mut changed = false;
        for target in &self.targets {
            if let Some(anchor) = &target.identity_anchor {
                let anchored = anchor.metadata().map_err(PropertyError::Io)?;
                if PropertyIdentity::from_metadata(&anchored) != target.identity {
                    return Ok(PropertyRefresh::Replaced);
                }
            }
            let metadata = match fs::symlink_metadata(&target.path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(PropertyRefresh::Missing);
                }
                Err(error) => return Err(PropertyError::Io(error)),
            };
            if PropertyIdentity::from_metadata(&metadata) != target.identity {
                return Ok(PropertyRefresh::Replaced);
            }
            changed |= MutableFingerprint::from_metadata(&metadata) != target.mutable_fingerprint;
        }
        Ok(if changed {
            PropertyRefresh::MetadataChanged
        } else {
            PropertyRefresh::Current
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PropertyRefresh {
    Current,
    MetadataChanged,
    Replaced,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecursiveSize {
    file_count: u64,
    directory_count: u64,
    logical_bytes: u64,
    allocated_bytes: u64,
    errors: Vec<Box<str>>,
    dropped_error_count: usize,
}

impl RecursiveSize {
    pub fn calculate(
        path: &Path,
        cancellation: CancellationToken,
    ) -> Result<Self, RecursiveSizeError> {
        if cancellation.is_cancelled() {
            return Err(RecursiveSizeError::Cancelled);
        }
        let root = fs::symlink_metadata(path).map_err(RecursiveSizeError::Io)?;
        if !root.is_dir() {
            return Err(RecursiveSizeError::NotDirectory);
        }
        let mut result = Self {
            file_count: 0,
            directory_count: 0,
            logical_bytes: 0,
            allocated_bytes: 0,
            errors: Vec::new(),
            dropped_error_count: 0,
        };
        for entry in WalkDir::new(path)
            .follow_links(false)
            .same_file_system(true)
            .max_open(32)
            .into_iter()
            .skip(1)
        {
            if cancellation.is_cancelled() {
                return Err(RecursiveSizeError::Cancelled);
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    result.push_error(error.to_string());
                    continue;
                }
            };
            let metadata = match entry.path().symlink_metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    result.push_error(error.to_string());
                    continue;
                }
            };
            if metadata.is_dir() {
                result.directory_count = result.directory_count.saturating_add(1);
            } else {
                result.file_count = result.file_count.saturating_add(1);
                result.logical_bytes = result.logical_bytes.saturating_add(metadata.len());
                result.allocated_bytes = result
                    .allocated_bytes
                    .saturating_add(metadata.blocks().saturating_mul(512));
            }
        }
        Ok(result)
    }

    fn push_error(&mut self, error: String) {
        if self.errors.len() < MAX_RECURSIVE_ERRORS {
            self.errors.push(error.into_boxed_str());
        } else {
            self.dropped_error_count = self.dropped_error_count.saturating_add(1);
        }
    }

    pub fn file_count(&self) -> u64 {
        self.file_count
    }

    pub fn directory_count(&self) -> u64 {
        self.directory_count
    }

    pub fn logical_bytes(&self) -> u64 {
        self.logical_bytes
    }

    pub fn allocated_bytes(&self) -> u64 {
        self.allocated_bytes
    }

    pub fn errors(&self) -> &[Box<str>] {
        &self.errors
    }

    pub fn dropped_error_count(&self) -> usize {
        self.dropped_error_count
    }
}

#[derive(Debug)]
pub enum PropertyError {
    EmptySelection,
    TooManyTargets,
    Io(io::Error),
    Metadata(Box<str>),
}

impl fmt::Display for PropertyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySelection => formatter.write_str("properties require at least one item"),
            Self::TooManyTargets => formatter.write_str("too many items selected for properties"),
            Self::Io(error) => write!(formatter, "could not inspect properties: {error}"),
            Self::Metadata(error) => write!(formatter, "could not inspect metadata: {error}"),
        }
    }
}

impl std::error::Error for PropertyError {}

#[derive(Debug)]
pub enum RecursiveSizeError {
    Cancelled,
    NotDirectory,
    Io(io::Error),
}

impl fmt::Display for RecursiveSizeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("recursive size calculation cancelled"),
            Self::NotDirectory => formatter.write_str("recursive size requires a directory"),
            Self::Io(error) => write!(formatter, "recursive size failed: {error}"),
        }
    }
}

impl std::error::Error for RecursiveSizeError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_ownership_is_never_reported_as_a_shared_owner() {
        assert_eq!(common([1000_u32, 1001].into_iter()), AggregateValue::Mixed);
        assert_eq!(
            common([1000_u32, 1000].into_iter()),
            AggregateValue::Same(1000)
        );
    }
}
