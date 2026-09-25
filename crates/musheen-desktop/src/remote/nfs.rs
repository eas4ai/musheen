use musheen_core::{
    BoxFuture, CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityReason,
    CapabilityState, DirectoryWatch, MutationRequest, Page, PageRequest, ProviderId,
    SearchCapabilities, SearchQuery, SearchStream, Store, StoreError, StoreItem, StorePath,
};
use musheen_local::LocalStore;
use proc_mounts::MountIter;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NfsMount {
    source: PathBuf,
    mount_point: PathBuf,
    filesystem_type: Box<str>,
    read_only: bool,
}

impl NfsMount {
    pub fn new(
        source: impl Into<PathBuf>,
        mount_point: impl Into<PathBuf>,
        filesystem_type: impl Into<Box<str>>,
        read_only: bool,
    ) -> Result<Self, StoreError> {
        let source = source.into();
        let mount_point = mount_point.into();
        let filesystem_type = filesystem_type.into();
        if source.as_os_str().is_empty()
            || !mount_point.is_absolute()
            || filesystem_type.trim().is_empty()
        {
            return Err(StoreError::Backend("kernel mount record is invalid".into()));
        }
        Ok(Self {
            source,
            mount_point,
            filesystem_type,
            read_only,
        })
    }

    fn is_nfs(&self) -> bool {
        matches!(self.filesystem_type.as_ref(), "nfs" | "nfs4")
    }
}

pub trait NfsMountSource {
    fn mounts(&self) -> Result<Vec<NfsMount>, StoreError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct KernelNfsMounts;

impl NfsMountSource for KernelNfsMounts {
    fn mounts(&self) -> Result<Vec<NfsMount>, StoreError> {
        let mounts = MountIter::new()
            .map_err(|_| StoreError::Backend("the kernel mount table could not be read".into()))?;
        mounts
            .map(|mount| {
                let mount = mount.map_err(|_| {
                    StoreError::Backend("the kernel mount table contains an invalid record".into())
                })?;
                NfsMount::new(
                    mount.source,
                    mount.dest,
                    mount.fstype,
                    mount.options.iter().any(|option| option == "ro"),
                )
            })
            .collect()
    }
}

pub struct MountedNfsStore {
    local: LocalStore,
    mount: NfsMount,
}

impl MountedNfsStore {
    pub fn new(path: impl AsRef<Path>, source: impl NfsMountSource) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if !path.is_absolute() || has_parent_component(path) {
            return Err(StoreError::Backend(
                "NFS location must be an absolute path".into(),
            ));
        }
        let mount = source
            .mounts()?
            .into_iter()
            .filter(|mount| mount.is_nfs() && path.starts_with(&mount.mount_point))
            .max_by_key(|mount| mount.mount_point.as_os_str().len())
            .ok_or_else(|| {
                StoreError::Backend("the path is not inside a kernel-mounted NFS filesystem".into())
            })?;
        Ok(Self {
            local: LocalStore::new(),
            mount,
        })
    }

    pub fn from_kernel_mount(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::new(path, KernelNfsMounts)
    }

    #[must_use]
    pub fn mount_point(&self) -> &Path {
        &self.mount.mount_point
    }

    #[must_use]
    pub fn mount_source(&self) -> &Path {
        &self.mount.source
    }

    fn validate_path(&self, path: &StorePath) -> Result<(), StoreError> {
        let path = path.as_unix_path().ok_or_else(|| {
            StoreError::Backend("mounted NFS accepts only local Unix paths".into())
        })?;
        if has_parent_component(path) || !path.starts_with(&self.mount.mount_point) {
            return Err(StoreError::Backend(
                "the path is outside this kernel NFS mount".into(),
            ));
        }
        Ok(())
    }

    fn validate_mutation_paths(&self, request: &MutationRequest) -> Result<(), StoreError> {
        if let Some(source) = request.source() {
            self.validate_path(source)?;
        }
        self.validate_path(request.destination())
    }
}

impl Store for MountedNfsStore {
    fn provider_id(&self) -> &ProviderId {
        self.local.provider_id()
    }

    fn capabilities(&self, location: &StorePath) -> CapabilityMatrix {
        if self.validate_path(location).is_err() {
            return CapabilityMatrix::new(|_| {
                CapabilityState::Unsupported(
                    CapabilityReason::new("the path is outside this kernel NFS mount")
                        .expect("the NFS capability reason is valid"),
                )
            });
        }
        let local = self.local.capabilities(location);
        CapabilityMatrix::new(|kind| {
            if self.mount.read_only && mutation_capability(kind) {
                CapabilityState::Unsupported(
                    CapabilityReason::new("the kernel NFS mount is read-only")
                        .expect("the NFS capability reason is valid"),
                )
            } else {
                local.get(kind).clone()
            }
        })
    }

    fn resolve_item(&self, path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        self.validate_path(path)?;
        self.local.resolve_item(path)
    }

    fn location_writable(&self, path: &StorePath) -> Result<CapabilityState, StoreError> {
        self.validate_path(path)?;
        if self.mount.read_only {
            Ok(CapabilityState::Unsupported(
                CapabilityReason::new("the kernel NFS mount is read-only")
                    .expect("the NFS writable-location reason is valid"),
            ))
        } else {
            self.local.location_writable(path)
        }
    }

    fn executable_state(&self, path: &StorePath) -> Result<CapabilityState, StoreError> {
        self.validate_path(path)?;
        self.local.executable_state(path)
    }

    fn search_capabilities(&self, location: &StorePath) -> SearchCapabilities {
        if self.validate_path(location).is_ok() {
            self.local.search_capabilities(location)
        } else {
            SearchCapabilities::default()
        }
    }

    fn search<'a>(
        &'a self,
        scope: &'a StorePath,
        query: SearchQuery,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn SearchStream>, StoreError>> {
        if let Err(error) = self.validate_path(scope) {
            return Box::pin(async move { Err(error) });
        }
        self.local.search(scope, query, cancellation)
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
        if let Err(error) = self.validate_path(location) {
            return Box::pin(async move { Err(error) });
        }
        self.local.read_directory(location, request, cancellation)
    }

    fn watch_directory<'a>(
        &'a self,
        location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
        if let Err(error) = self.validate_path(location) {
            return Box::pin(async move { Err(error) });
        }
        self.local.watch_directory(location, cancellation)
    }

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
        self.validate_mutation_paths(request)?;
        if self.mount.read_only {
            Err(request.unsupported("the kernel NFS mount is read-only"))
        } else {
            self.local.validate_mutation(request)
        }
    }

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        if let Err(error) = self.validate_mutation_paths(&request) {
            Box::pin(async move { Err(error) })
        } else if self.mount.read_only {
            let result = cancellation
                .check()
                .and_then(|()| Err(request.unsupported("the kernel NFS mount is read-only")));
            Box::pin(async move { result })
        } else {
            self.local.mutate(request, cancellation)
        }
    }
}

fn has_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| component == Component::ParentDir)
}

fn mutation_capability(kind: CapabilityKind) -> bool {
    !matches!(
        kind,
        CapabilityKind::Watching | CapabilityKind::CaseSensitivity | CapabilityKind::Tags
    )
}
