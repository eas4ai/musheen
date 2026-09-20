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
use std::path::PathBuf;

pub use mutation::LocalTrashEntry;
pub use probe::LocalFilesystemInfo;
pub use queue::{
    DropAction, DropError, FileDragPayload, LocalFailureDisposition, LocalOperationFailure,
    LocalOperationQueue, ReadyLocalOperation,
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
        use std::os::unix::fs::PermissionsExt;
        if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o222 == 0 {
            return Ok(CapabilityState::Unsupported(
                CapabilityReason::new("the destination directory is not writable")
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
        Ok(CapabilityState::Supported)
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
