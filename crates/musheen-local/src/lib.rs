//! Local Linux filesystem provider.

mod enumerate;
mod metadata;
mod mutation;
mod operation;
mod probe;
mod queue;
mod search;
mod traverse;
mod watch;

use enumerate::EnumerationRegistry;
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityMatrix, CapabilityReason, CapabilityState,
    DirectoryWatch, MutationRequest, Page, PageRequest, ProviderId, SearchCapabilities,
    SearchQuery, SearchStream, Store, StoreError, StoreItem, StorePath,
};
use musheen_ops::{MetadataKind, SourceMetadata};
use posix_acl::{ACL_EXECUTE, ACL_WRITE, PosixACL, Qualifier};
use std::path::PathBuf;

pub use mutation::LocalTrashEntry;
pub use probe::LocalFilesystemInfo;
pub use queue::{
    ActiveOperationPaths, DropAction, DropError, FileDragPayload, LocalFailureDisposition,
    LocalOperationFailure, LocalOperationOutcome, LocalOperationQueue, ProviderTransferExecution,
    ProviderTransferRoute, ReadyLocalOperation, TransferOutcome,
};
pub use traverse::{LocalTraversal, TraversalOptions};

pub struct LocalStore {
    provider: ProviderId,
    enumerations: EnumerationRegistry,
    operation_metadata_skips: Vec<MetadataKind>,
    operation_timestamps: Vec<(PathBuf, SourceMetadata)>,
}

impl LocalStore {
    /// Returns false only when a local session location is known to be absent.
    /// Permission and transient I/O failures remain restorable so startup can
    /// present the real provider error instead of silently replacing the path.
    #[must_use]
    pub fn session_location_exists(path: &StorePath) -> bool {
        let Some(path) = path.as_unix_path() else {
            return true;
        };
        path.try_exists().unwrap_or(true)
    }
}

impl LocalStore {
    #[must_use]
    pub fn new() -> Self {
        let provider = ProviderId::new("local").expect("the built-in provider ID is valid");
        Self {
            enumerations: EnumerationRegistry::new(provider.clone()),
            provider,
            operation_metadata_skips: Vec::new(),
            operation_timestamps: Vec::new(),
        }
    }

    pub fn probe(&self, location: &StorePath) -> Result<LocalFilesystemInfo, StoreError> {
        probe::probe(location)
    }

    pub fn traverse(
        &self,
        root: &StorePath,
        options: TraversalOptions,
    ) -> Result<LocalTraversal, StoreError> {
        LocalTraversal::new(self.provider.clone(), root, options)
    }
}

impl Default for LocalStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Store for LocalStore {
    fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    fn capabilities(&self, location: &StorePath) -> CapabilityMatrix {
        probe::capabilities(location)
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        let Some(path) = path.as_unix_path() else {
            return Ok(None);
        };
        match metadata::item_from_path(&self.provider, path) {
            Ok(item) => Ok(Some(item)),
            Err(StoreError::Io {
                kind: std::io::ErrorKind::NotFound,
                ..
            }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn location_writable(&self, path: &StorePath) -> Result<CapabilityState, StoreError> {
        let Some(path) = path.as_unix_path() else {
            return Ok(CapabilityState::Unknown(
                CapabilityReason::new("this local provider path is not a Unix directory")
                    .expect("the writable-location reason is valid"),
            ));
        };
        let metadata = std::fs::symlink_metadata(path).map_err(|error| StoreError::Io {
            operation: "read directory access metadata",
            kind: error.kind(),
            path: Some(StorePath::from_unix_path(path.as_os_str())),
            message: error.to_string().into(),
        })?;
        if !metadata.file_type().is_dir() {
            return Ok(CapabilityState::Unsupported(
                CapabilityReason::new("the destination is not a directory")
                    .expect("the writable-location reason is valid"),
            ));
        }
        if self
            .probe(&StorePath::from_unix_path(path.as_os_str()))?
            .is_read_only()
        {
            return Ok(CapabilityState::Unsupported(
                CapabilityReason::new("the containing mount is read-only")
                    .expect("the writable-location reason is valid"),
            ));
        }
        match directory_allows_current_user(path, &metadata) {
            Ok(true) => {}
            Ok(false) => {
                return Ok(CapabilityState::Unsupported(
                    CapabilityReason::new(
                        "the current user lacks write and search permission for the destination directory",
                    )
                    .expect("the writable-location reason is valid"),
                ));
            }
            Err(reason) => return Ok(CapabilityState::Unknown(reason)),
        }
        Ok(CapabilityState::Supported)
    }

    fn executable_state(&self, path: &StorePath) -> Result<CapabilityState, StoreError> {
        let Some(path) = path.as_unix_path() else {
            return Ok(CapabilityState::Unknown(
                CapabilityReason::new("this local provider path is not a Unix file")
                    .expect("the executable-state reason is valid"),
            ));
        };
        use std::os::unix::fs::PermissionsExt;
        let metadata = std::fs::metadata(path).map_err(|error| StoreError::Io {
            operation: "read executable metadata",
            kind: error.kind(),
            path: Some(StorePath::from_unix_path(path.as_os_str())),
            message: error.to_string().into(),
        })?;
        if metadata.file_type().is_file() && metadata.permissions().mode() & 0o111 != 0 {
            Ok(CapabilityState::Supported)
        } else {
            Ok(CapabilityState::Unsupported(
                CapabilityReason::new("the item is not marked executable by its provider metadata")
                    .expect("the executable-state reason is valid"),
            ))
        }
    }

    fn search_capabilities(&self, _location: &StorePath) -> SearchCapabilities {
        SearchCapabilities::all()
    }

    fn search<'a>(
        &'a self,
        scope: &'a StorePath,
        query: SearchQuery,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn SearchStream>, StoreError>> {
        let result = self
            .search_capabilities(scope)
            .validate(&query)
            .map_err(|error| StoreError::Backend(error.to_string().into()))
            .and_then(|()| search::start(self.provider.clone(), scope, query, cancellation));
        Box::pin(async move { result })
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        Box::pin(async move {
            self.enumerations
                .read_page(location, request, &cancellation)
        })
    }

    fn watch_directory<'a>(
        &'a self,
        location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        Box::pin(async move {
            let watch = watch::LocalWatch::open(self.provider.clone(), location, &cancellation)?;
            Ok(Box::new(watch) as Box<dyn DirectoryWatch>)
        })
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        Err(request.unsupported("the foundation local provider is read-only"))
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        let validation = cancellation
            .check()
            .and_then(|()| self.validate_mutation(&request));
        Box::pin(async move { validation })
    }
}

/// POSIX directory mutation needs both write and search (`x`) access.  This
/// evaluates the effective uid, supplementary groups and access ACL instead
/// of treating any of the three write mode bits as the current user's access.
/// An unreadable ACL is deliberately Unknown: presenting a mutating command
/// as enabled would be less safe than an explanatory disabled row.
fn directory_allows_current_user(
    path: &std::path::Path,
    metadata: &std::fs::Metadata,
) -> Result<bool, CapabilityReason> {
    use std::os::unix::fs::MetadataExt;

    let uid = rustix::process::geteuid().as_raw();
    if uid == 0 {
        return Ok(true);
    }
    let mut groups = rustix::process::getgroups().map_err(|error| {
        CapabilityReason::new(format!(
            "the provider could not read the current groups: {error}"
        ))
        .expect("the group lookup reason is valid")
    })?;
    let effective_group = rustix::process::getegid();
    if !groups.contains(&effective_group) {
        groups.push(effective_group);
    }
    let group_ids = groups
        .into_iter()
        .map(|group| group.as_raw())
        .collect::<Vec<_>>();
    let acl = PosixACL::read_acl(path).map_err(|error| {
        CapabilityReason::new(format!(
            "the provider could not verify access ACLs: {error}"
        ))
        .expect("the ACL lookup reason is valid")
    })?;
    Ok(acl_allows_directory_mutation(
        &acl,
        uid,
        metadata.uid(),
        metadata.gid(),
        &group_ids,
    ))
}

fn acl_allows_directory_mutation(
    acl: &PosixACL,
    uid: u32,
    owner: u32,
    owning_group: u32,
    group_ids: &[u32],
) -> bool {
    let required = ACL_WRITE | ACL_EXECUTE;
    if owner == uid {
        return acl.get(Qualifier::UserObj).unwrap_or_default() & required == required;
    }
    if let Some(named_user) = acl.get(Qualifier::User(uid)) {
        return apply_acl_mask(named_user, acl) & required == required;
    }
    let mut group_matched = false;
    for entry in acl.entries() {
        let matches = match entry.qual {
            Qualifier::GroupObj => group_ids.contains(&owning_group),
            Qualifier::Group(group) => group_ids.contains(&group),
            _ => false,
        };
        if matches {
            group_matched = true;
            // Linux checks each matching group separately. Bits from
            // different entries cannot combine to satisfy a request.
            if apply_acl_mask(entry.perm, acl) & required == required {
                return true;
            }
        }
    }
    !group_matched && acl.get(Qualifier::Other).unwrap_or_default() & required == required
}

fn apply_acl_mask(permissions: u32, acl: &PosixACL) -> u32 {
    permissions & acl.get(Qualifier::Mask).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod acl_tests {
    use super::*;

    #[test]
    fn matching_zero_permission_group_does_not_fall_through_to_other() {
        let acl = PosixACL::new(0o703);
        assert!(!acl_allows_directory_mutation(&acl, 42, 1, 7, &[7]));
        assert!(acl_allows_directory_mutation(&acl, 42, 1, 7, &[8]));
        let mut acl = acl;
        acl.set(Qualifier::Group(8), 0);
        acl.set(Qualifier::Mask, ACL_WRITE | ACL_EXECUTE);
        assert!(!acl_allows_directory_mutation(&acl, 42, 1, 7, &[8]));
    }

    #[test]
    fn one_matching_group_must_grant_both_write_and_search_after_mask() {
        let mut acl = PosixACL::new(0o703);
        acl.set(Qualifier::Group(8), ACL_WRITE);
        acl.set(Qualifier::Group(9), ACL_EXECUTE);
        acl.set(Qualifier::Mask, ACL_WRITE | ACL_EXECUTE);
        assert!(!acl_allows_directory_mutation(&acl, 42, 1, 7, &[8, 9]));
        acl.set(Qualifier::GroupObj, ACL_WRITE);
        assert!(!acl_allows_directory_mutation(&acl, 42, 1, 7, &[7, 9]));
        acl.set(Qualifier::Group(9), ACL_WRITE | ACL_EXECUTE);
        assert!(acl_allows_directory_mutation(&acl, 42, 1, 7, &[8, 9]));
        acl.set(Qualifier::GroupObj, ACL_WRITE | ACL_EXECUTE);
        assert!(acl_allows_directory_mutation(&acl, 42, 1, 7, &[7]));
        acl.set(Qualifier::Mask, ACL_WRITE);
        assert!(!acl_allows_directory_mutation(&acl, 42, 1, 7, &[8, 9]));
        acl.set(Qualifier::Mask, ACL_WRITE | ACL_EXECUTE);
        acl.set(Qualifier::User(42), 0);
        assert!(!acl_allows_directory_mutation(&acl, 42, 1, 7, &[8, 9]));
    }

    #[test]
    fn owner_named_user_and_other_keep_their_precedence_and_mask_rules() {
        let mut acl = PosixACL::new(0o303);
        acl.set(Qualifier::User(42), ACL_WRITE | ACL_EXECUTE);
        acl.set(Qualifier::Mask, ACL_WRITE);
        assert!(acl_allows_directory_mutation(&acl, 1, 1, 7, &[7]));
        assert!(!acl_allows_directory_mutation(&acl, 42, 1, 7, &[8]));
        assert!(acl_allows_directory_mutation(&acl, 43, 1, 7, &[8]));
        acl.set(Qualifier::Mask, ACL_WRITE | ACL_EXECUTE);
        assert!(acl_allows_directory_mutation(&acl, 42, 1, 7, &[7]));
        acl.set(Qualifier::UserObj, 0);
        acl.set(Qualifier::User(1), ACL_WRITE | ACL_EXECUTE);
        assert!(!acl_allows_directory_mutation(&acl, 1, 1, 7, &[8]));
    }
}
