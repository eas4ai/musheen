use crate::{FaultGate, RecordingMetrics, RecordingStore};
use musheen_core::{
    BoxFuture, CancellationToken, CapabilityMatrix, DirectoryWatch, MutationRequest, Page,
    PageRequest, ProviderId, Store, StoreError, StoreItem, StorePath,
};

/// Wraps the normal recording provider with one failed page request.
#[derive(Debug)]
pub struct FaultingReadStore {
    inner: RecordingStore,
    read_gate: FaultGate,
}

impl FaultingReadStore {
    #[must_use]
    pub const fn new(inner: RecordingStore, fail_on_read: usize) -> Self {
        Self {
            inner,
            read_gate: FaultGate::new(fail_on_read),
        }
    }

    #[must_use]
    pub fn metrics(&self) -> RecordingMetrics {
        self.inner.metrics()
    }

    #[must_use]
    pub fn read_calls(&self) -> usize {
        self.read_gate.calls()
    }
}

impl Store for FaultingReadStore {
    fn provider_id(&self) -> &ProviderId {
        self.inner.provider_id()
    }

    fn capabilities(&self, location: &StorePath) -> CapabilityMatrix {
        self.inner.capabilities(location)
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        if self.read_gate.trip() {
            return Box::pin(async move {
                cancellation.check()?;
                Err(StoreError::Backend("injected page read failure".into()))
            });
        }
        self.inner.read_directory(location, request, cancellation)
    }

    fn watch_directory<'a>(
        &'a self,
        location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        self.inner.watch_directory(location, cancellation)
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        self.inner.validate_mutation(request)
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        self.inner.mutate(request, cancellation)
    }
}
