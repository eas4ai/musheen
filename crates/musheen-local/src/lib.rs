//! Local Linux filesystem provider.

mod enumerate;
mod metadata;
mod probe;
mod traverse;
mod watch;

use enumerate::EnumerationRegistry;
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityMatrix, DirectoryWatch, MutationRequest, Page,
    PageRequest, ProviderId, Store, StoreError, StoreItem, StorePath,
};

pub use probe::LocalFilesystemInfo;
pub use traverse::{LocalTraversal, TraversalOptions};

pub struct LocalStore {
    provider: ProviderId,
    enumerations: EnumerationRegistry,
}

impl LocalStore {
    #[must_use]
    pub fn new() -> Self {
        let provider = ProviderId::new("local").expect("the built-in provider ID is valid");
        Self {
            enumerations: EnumerationRegistry::new(provider.clone()),
            provider,
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
