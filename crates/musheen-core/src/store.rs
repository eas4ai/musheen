use crate::{
    CancellationToken, CapabilityMatrix, CapabilityReason, CapabilityState, DirectoryWatch, Page,
    PageRequest, ProviderId, SearchCapabilities, SearchQuery, SearchStream, StoreItem, StorePath,
};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationKind {
    CreateDirectory,
    CreateFile,
    Rename,
    Copy,
    Move,
    Trash,
    PermanentDelete,
    SymbolicLink,
    HardLink,
    SetPermissions,
    SetOwnership,
    SetExtendedAttribute,
}

impl MutationKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CreateDirectory => "create_directory",
            Self::CreateFile => "create_file",
            Self::Rename => "rename",
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Trash => "trash",
            Self::PermanentDelete => "permanent_delete",
            Self::SymbolicLink => "symbolic_link",
            Self::HardLink => "hard_link",
            Self::SetPermissions => "set_permissions",
            Self::SetOwnership => "set_ownership",
            Self::SetExtendedAttribute => "set_extended_attribute",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationRequest {
    CreateDirectory {
        path: StorePath,
    },
    CreateFile {
        path: StorePath,
    },
    Rename {
        source: StorePath,
        destination: StorePath,
    },
    Copy {
        source: StorePath,
        destination: StorePath,
    },
    Move {
        source: StorePath,
        destination: StorePath,
    },
    Trash {
        target: StorePath,
    },
    PermanentDelete {
        target: StorePath,
    },
    SymbolicLink {
        source: StorePath,
        destination: StorePath,
    },
    HardLink {
        source: StorePath,
        destination: StorePath,
    },
    SetPermissions {
        target: StorePath,
    },
    SetOwnership {
        target: StorePath,
    },
    SetExtendedAttribute {
        target: StorePath,
    },
}

impl MutationRequest {
    #[must_use]
    pub fn trash(target: StorePath) -> Self {
        Self::Trash { target }
    }

    #[must_use]
    pub fn kind(&self) -> MutationKind {
        match self {
            Self::CreateDirectory { .. } => MutationKind::CreateDirectory,
            Self::CreateFile { .. } => MutationKind::CreateFile,
            Self::Rename { .. } => MutationKind::Rename,
            Self::Copy { .. } => MutationKind::Copy,
            Self::Move { .. } => MutationKind::Move,
            Self::Trash { .. } => MutationKind::Trash,
            Self::PermanentDelete { .. } => MutationKind::PermanentDelete,
            Self::SymbolicLink { .. } => MutationKind::SymbolicLink,
            Self::HardLink { .. } => MutationKind::HardLink,
            Self::SetPermissions { .. } => MutationKind::SetPermissions,
            Self::SetOwnership { .. } => MutationKind::SetOwnership,
            Self::SetExtendedAttribute { .. } => MutationKind::SetExtendedAttribute,
        }
    }

    #[must_use]
    pub fn source(&self) -> Option<&StorePath> {
        match self {
            Self::Rename { source, .. }
            | Self::Copy { source, .. }
            | Self::Move { source, .. }
            | Self::SymbolicLink { source, .. }
            | Self::HardLink { source, .. } => Some(source),
            _ => None,
        }
    }

    #[must_use]
    pub fn destination(&self) -> &StorePath {
        match self {
            Self::CreateDirectory { path } | Self::CreateFile { path } => path,
            Self::Rename { destination, .. }
            | Self::Copy { destination, .. }
            | Self::Move { destination, .. }
            | Self::SymbolicLink { destination, .. }
            | Self::HardLink { destination, .. } => destination,
            Self::Trash { target }
            | Self::PermanentDelete { target }
            | Self::SetPermissions { target }
            | Self::SetOwnership { target }
            | Self::SetExtendedAttribute { target } => target,
        }
    }

    #[must_use]
    pub fn unsupported(&self, reason: &'static str) -> StoreError {
        StoreError::unsupported(self.kind().as_str(), reason)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    Cancelled,
    Unsupported {
        operation: Box<str>,
        reason: CapabilityReason,
    },
    InvalidContinuation,
    PageTooLarge {
        requested: usize,
        returned: usize,
    },
    InvalidLimit {
        resource: &'static str,
        value: usize,
        minimum: usize,
        maximum: usize,
    },
    ResourceLimit {
        resource: &'static str,
        value: usize,
        maximum: usize,
    },
    WatchEnded,
    Io {
        operation: &'static str,
        kind: std::io::ErrorKind,
        path: Option<StorePath>,
        message: Box<str>,
    },
    Backend(Box<str>),
}

impl StoreError {
    #[must_use]
    pub fn unsupported(operation: &'static str, reason: &'static str) -> Self {
        Self::Unsupported {
            operation: operation.into(),
            reason: CapabilityReason::new(reason)
                .expect("static unsupported-operation reasons must contain visible text"),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("store operation was cancelled"),
            Self::Unsupported { operation, reason } => {
                write!(formatter, "{operation} is unsupported: {}", reason.as_str())
            }
            Self::InvalidContinuation => formatter.write_str("provider continuation is invalid"),
            Self::PageTooLarge {
                requested,
                returned,
            } => write!(
                formatter,
                "provider returned {returned} items for a {requested}-item page"
            ),
            Self::InvalidLimit {
                resource,
                value,
                minimum,
                maximum,
            } => write!(
                formatter,
                "{resource} must be between {minimum} and {maximum}, got {value}"
            ),
            Self::ResourceLimit {
                resource,
                value,
                maximum,
            } => {
                write!(
                    formatter,
                    "{resource} must not exceed {maximum}, got {value}"
                )
            }
            Self::WatchEnded => formatter.write_str("directory watch ended unexpectedly"),
            Self::Io {
                operation,
                kind,
                path,
                message,
            } => {
                write!(formatter, "{operation} failed with {kind:?}")?;
                if let Some(path) = path {
                    write!(
                        formatter,
                        " for {}",
                        crate::DisplayPath::from_store_path(path).as_str()
                    )?;
                }
                write!(formatter, ": {message}")
            }
            Self::Backend(message) => formatter.write_str(message),
        }
    }
}

impl Error for StoreError {}

/// Portable provider boundary. Futures are boxed to keep the trait object-safe.
pub trait Store: Send + Sync {
    fn provider_id(&self) -> &ProviderId;

    fn capabilities(&self, location: &StorePath) -> CapabilityMatrix;

    /// Resolves a path immediately at the provider boundary. Context-menu
    /// actions use this to reject a same-path replacement instead of trusting
    /// an already-rendered directory row.
    fn resolve_item(&self, _path: &StorePath) -> Result<Option<StoreItem>, StoreError> {
        Err(StoreError::unsupported(
            "resolve item identity",
            "this provider does not expose current item identity",
        ))
    }

    /// Provider-supplied access fact for a concrete directory. This is kept
    /// separate from operation capabilities such as atomic rename.
    fn location_writable(&self, _path: &StorePath) -> Result<CapabilityState, StoreError> {
        Ok(CapabilityState::Unknown(
            CapabilityReason::new("the provider did not report whether this location is writable")
                .expect("the default writable-location reason is valid"),
        ))
    }

    fn search_capabilities(&self, _location: &StorePath) -> SearchCapabilities {
        SearchCapabilities::default()
    }

    fn search<'a>(
        &'a self,
        _scope: &'a StorePath,
        _query: SearchQuery,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn SearchStream>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            Err(StoreError::unsupported(
                "search",
                "this provider does not implement recursive search",
            ))
        })
    }

    fn read_directory<'a>(
        &'a self,
        location: &'a StorePath,
        request: PageRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Page<StoreItem>, StoreError>>;

    fn watch_directory<'a>(
        &'a self,
        location: &'a StorePath,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>>;

    fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError>;

    fn mutate<'a>(
        &'a self,
        request: MutationRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
}
