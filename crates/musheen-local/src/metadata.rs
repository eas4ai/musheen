use musheen_core::{DisplayPath, ItemId, ItemKind, ProviderId, StoreError, StoreItem, StorePath};
use std::fs::{self, DirEntry, Metadata};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(crate) fn item_from_dir_entry(
    provider: &ProviderId,
    entry: &DirEntry,
) -> Result<StoreItem, StoreError> {
    item_from_path_with_metadata(
        provider,
        &entry.path(),
        entry.metadata().map_err(|error| {
            io_error(
                "read directory entry metadata",
                Some(StorePath::from_unix_path(entry.path().into_os_string())),
                error,
            )
        })?,
    )
}

pub(crate) fn item_from_path(provider: &ProviderId, path: &Path) -> Result<StoreItem, StoreError> {
    let store_path = StorePath::from_unix_path(path.as_os_str().to_os_string());
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| io_error("read item metadata", Some(store_path.clone()), error))?;
    item_from_path_with_metadata(provider, path, metadata)
}

/// The item a path leads to after following symbolic links, described at
/// the path itself. A dangling link is a not-found error.
pub(crate) fn link_target_from_path(
    provider: &ProviderId,
    path: &Path,
) -> Result<StoreItem, StoreError> {
    let store_path = StorePath::from_unix_path(path.as_os_str().to_os_string());
    let metadata = fs::metadata(path)
        .map_err(|error| io_error("read link target metadata", Some(store_path), error))?;
    item_from_path_with_metadata(provider, path, metadata)
}

fn item_from_path_with_metadata(
    provider: &ProviderId,
    path: &Path,
    metadata: Metadata,
) -> Result<StoreItem, StoreError> {
    let store_path = StorePath::from_unix_path(path.as_os_str().to_os_string());
    let id = local_item_id(provider, path, &metadata)?;
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
    let display_name = path.file_name().unwrap_or(path.as_os_str());
    let size = file_type.is_file().then_some(metadata.len());

    let item = StoreItem::new(id, store_path, DisplayPath::from(display_name), kind, size);
    let modified = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());
    Ok(match modified {
        Some(seconds) => item.with_modified_unix_seconds(seconds),
        None => item,
    })
}

/// The local provider's identity for an open file: the item ID a listing
/// or `resolve_item` gives the file it names, so a caller can check that a
/// file it opened is the item it was shown.
pub fn item_id_of_open_file(
    provider: &ProviderId,
    path: &Path,
    file: &std::fs::File,
) -> Result<ItemId, StoreError> {
    let metadata = file.metadata().map_err(|error| {
        io_error(
            "read open file metadata",
            Some(StorePath::from_unix_path(path.as_os_str())),
            error,
        )
    })?;
    local_item_id(provider, path, &metadata)
}

fn local_item_id(
    provider: &ProviderId,
    path: &Path,
    metadata: &Metadata,
) -> Result<ItemId, StoreError> {
    let device = metadata.dev();
    let inode = metadata.ino();
    let key = if device != 0 || inode != 0 {
        let mut key = Vec::with_capacity(17);
        key.push(0);
        key.extend_from_slice(&device.to_be_bytes());
        key.extend_from_slice(&inode.to_be_bytes());
        key
    } else {
        // Linux filesystems normally supply device/inode identity. A provider that
        // does not gets a path-bound replacement identity. A rename replaces that
        // identity and therefore forces reconciliation instead of guessing.
        let mut key = Vec::with_capacity(path.as_os_str().as_bytes().len() + 1);
        key.push(1);
        key.extend_from_slice(path.as_os_str().as_bytes());
        key
    };

    ItemId::new(provider.clone(), key)
        .map_err(|error| StoreError::Backend(error.to_string().into()))
}

pub(crate) fn io_error(
    operation: &'static str,
    path: Option<StorePath>,
    error: std::io::Error,
) -> StoreError {
    StoreError::Io {
        operation,
        kind: error.kind(),
        path,
        message: error.to_string().into(),
    }
}
