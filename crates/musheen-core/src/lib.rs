//! Portable domain contracts for Musheen.

mod cancel;
mod capability;
mod command;
mod context;
mod customization;
mod error;
mod item;
mod limits;
mod page;
mod path;
mod search;
mod store;
mod watch;

pub use cancel::CancellationToken;
pub use capability::{CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState};
pub use command::*;
pub use context::{
    ActiveLayout, CommandContext, CommandParameterError, CommandParameters, CommandTarget,
    CommandTargetRef, DesktopApplicationId, OpenWithIntent, ProviderAction, ProviderActionMatrix,
    ResolvedDestination, RunKind,
};
pub use customization::*;
pub use error::CoreError;
pub use item::{ItemId, ItemKind, StoreItem};
pub use limits::{ResourceLimitConfig, ResourceLimits};
pub use page::{Continuation, Page, PageRequest, PagingPolicy, TotalHint};
pub use path::{DisplayPath, ProviderId, StorePath};
pub use search::*;
pub use store::{BoxFuture, MutationKind, MutationRequest, Store, StoreError};
pub use watch::{DirectoryWatch, ReconcileBuffer, WatchEvent, WatchFailure, WatchSemantics};
