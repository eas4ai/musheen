use crate::LocalStore;
use crate::mutation::{
    ResolvedTransferFailure, ResolvedTransferOutcome, execute_resolved_transfer,
};
use musheen_core::{
    CommandTargetRef, DisplayPath, ItemId, ProviderId, ResourceLimits, Store, StorePath,
};
use musheen_ops::{
    ArchiveOperationPlan, ConflictDecision, ConflictItemKind, ConflictRecord, CopyRequest,
    CopySession, CreateRequest, DeleteTarget, EventGeneration, JobId, JobState, MetadataChange,
    MetadataPlan, MetadataScope, MoveMetadataReview, MutationError, MutationProvider,
    OperationFailure, OperationKind, OperationPlan, PermanentDeleteConfirmation,
    PermanentDeleteRequest, ProviderLimits, ProviderSnapshot, PublicationState, RenameRequest,
    Scheduler, SchedulerError, SourceState, TrashReceipt, complete_move_after_metadata_review,
    execute_create, execute_delete, execute_move, execute_permanent_delete, execute_rename,
    execute_restore,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DropAction {
    Copy,
    Move,
}

impl DropAction {
    const fn operation_kind(self) -> OperationKind {
        match self {
            Self::Copy => OperationKind::Copy,
            Self::Move => OperationKind::Move,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileDragPayload {
    sources: Vec<StorePath>,
    action: DropAction,
    expected_identities: Option<Vec<ItemId>>,
}

impl FileDragPayload {
    pub fn new(sources: Vec<StorePath>, action: DropAction) -> Result<Self, DropError> {
        if sources.is_empty() {
            return Err(DropError::EmptySelection);
        }
        Ok(Self {
            sources,
            action,
            expected_identities: None,
        })
    }

    /// Binds a context-menu transfer to the provider identities observed when
    /// the user opened the menu. Drag sources that lack an identity retain the
    /// legacy path-only constructor.
    pub fn with_expected_identities(
        sources: Vec<StorePath>,
        expected_identities: Vec<ItemId>,
        action: DropAction,
    ) -> Result<Self, DropError> {
        if sources.len() != expected_identities.len() {
            return Err(DropError::IdentityCountMismatch);
        }
        let mut payload = Self::new(sources, action)?;
        payload.expected_identities = Some(expected_identities);
        Ok(payload)
    }

    #[must_use]
    pub fn sources(&self) -> &[StorePath] {
        &self.sources
    }

    #[must_use]
    pub const fn action(&self) -> DropAction {
        self.action
    }

    #[must_use]
    pub fn expected_identity(&self, index: usize) -> Option<&ItemId> {
        self.expected_identities
            .as_ref()
            .and_then(|identities| identities.get(index))
    }
}

#[derive(Clone, Debug)]
enum LocalOperation {
    Transfer {
        action: DropAction,
        source: StorePath,
        destination: StorePath,
        decision: Option<ConflictDecision>,
        expected_identity: Option<ItemId>,
        expected_raw_identity: Option<Box<[u8]>>,
        expected_destination_parent_identity: Option<Box<[u8]>>,
        provider_route: Option<Arc<dyn ProviderTransferRoute>>,
    },
    FinalizeMove(Box<MoveMetadataReview>),
    Metadata(MetadataPlan),
    Create(CreateRequest),
    Rename(RenameRequest),
    Trash(DeleteTarget),
    Restore {
        receipt: TrashReceipt,
        expected_parent_identity: Box<[u8]>,
    },
    PermanentDelete {
        request: PermanentDeleteRequest,
        confirmation: PermanentDeleteConfirmation,
    },
    Archive {
        plan: ArchiveOperationPlan,
        route: Arc<dyn ArchiveOperationRoute>,
    },
}

#[derive(Clone, Debug)]
struct RenameUndo {
    reverse: RenameRequest,
    original: StorePath,
}

impl RenameUndo {
    fn from_completed(request: &RenameRequest) -> Option<Self> {
        let original = request.source().clone();
        let destination = request.destination().ok()?;
        if destination == original {
            return None;
        }
        let original_name = clean_absolute_path(&original)?.file_name()?.to_os_string();
        Some(Self {
            reverse: RenameRequest::new(
                destination,
                original_name,
                request.expected_identity().to_vec(),
            ),
            original,
        })
    }

    fn is_available(&self) -> bool {
        let mut store = LocalStore::new();
        matches!(
            MutationProvider::identity(&mut store, self.reverse.source()),
            Ok(Some(identity)) if identity.as_ref() == self.reverse.expected_identity()
        ) && matches!(
            MutationProvider::identity(&mut store, &self.original),
            Ok(None)
        ) && matches!(
            MutationProvider::allows_rename(&mut store, self.reverse.source()),
            Ok(true)
        )
    }
}

#[derive(Clone, Debug)]
struct MoveUndo {
    moved: StorePath,
    original: StorePath,
    original_parent: StorePath,
    expected_identity: Box<[u8]>,
    expected_original_parent_identity: Box<[u8]>,
    expected_item_id: ItemId,
}

impl MoveUndo {
    fn from_completed(original: StorePath, moved: StorePath) -> Option<Self> {
        let moved_path = clean_absolute_path(&moved)?;
        let original_parent = StorePath::from_unix_path(clean_absolute_path(&original)?.parent()?);
        let mut store = LocalStore::new();
        let expected_identity = MutationProvider::identity(&mut store, &moved).ok()??;
        let expected_original_parent_identity =
            MutationProvider::identity(&mut store, &original_parent).ok()??;
        if !matches!(MutationProvider::identity(&mut store, &original), Ok(None)) {
            return None;
        }
        let expected_item_id = crate::metadata::item_from_path(store.provider_id(), moved_path)
            .ok()?
            .id()
            .clone();
        if !matches!(
            MutationProvider::identity(&mut store, &moved),
            Ok(Some(identity)) if identity == expected_identity
        ) {
            return None;
        }
        Some(Self {
            moved,
            original,
            original_parent,
            expected_identity,
            expected_original_parent_identity,
            expected_item_id,
        })
    }

    fn is_available(&self) -> bool {
        let mut store = LocalStore::new();
        matches!(
            MutationProvider::identity(&mut store, &self.moved),
            Ok(Some(identity)) if identity == self.expected_identity
        ) && matches!(
            MutationProvider::identity(&mut store, &self.original),
            Ok(None)
        ) && matches!(
            MutationProvider::identity(&mut store, &self.original_parent),
            Ok(Some(identity)) if identity == self.expected_original_parent_identity
        ) && matches!(
            MutationProvider::allows_rename(&mut store, &self.moved),
            Ok(true)
        ) && writable_directory(&self.original_parent).is_ok()
    }
}

#[derive(Clone, Debug)]
struct TrashUndo {
    receipt: TrashReceipt,
    original_parent: StorePath,
    expected_parent_identity: Box<[u8]>,
}

impl TrashUndo {
    fn from_completed(receipt: TrashReceipt) -> Option<Self> {
        let mut store = LocalStore::new();
        let receipt = store
            .list_trash()
            .ok()?
            .into_iter()
            .find(|entry| entry.receipt().provider_reference() == receipt.provider_reference())?
            .receipt()
            .clone();
        let original_parent = StorePath::from_unix_path(
            clean_absolute_path(receipt.original_path())?
                .parent()?
                .as_os_str(),
        );
        let expected_parent_identity =
            MutationProvider::identity(&mut store, &original_parent).ok()??;
        Some(Self {
            receipt,
            original_parent,
            expected_parent_identity,
        })
    }

    fn is_available(&self) -> bool {
        let mut store = LocalStore::new();
        matches!(
            MutationProvider::identity(&mut store, self.receipt.original_path()),
            Ok(None)
        ) && matches!(
            MutationProvider::identity(&mut store, &self.original_parent),
            Ok(Some(identity)) if identity == self.expected_parent_identity
        ) && writable_directory(&self.original_parent).is_ok()
            && store
                .list_trash()
                .is_ok_and(|entries| entries.iter().any(|entry| entry.receipt() == &self.receipt))
    }
}

#[derive(Clone, Debug)]
enum UndoCandidate {
    Rename(RenameUndo),
    Move(MoveUndo),
    Trash(TrashUndo),
}

impl UndoCandidate {
    fn is_available(&self) -> bool {
        match self {
            Self::Rename(undo) => undo.is_available(),
            Self::Move(undo) => undo.is_available(),
            Self::Trash(undo) => undo.is_available(),
        }
    }

    fn paths(&self) -> [StorePath; 2] {
        match self {
            Self::Rename(undo) => [undo.reverse.source().clone(), undo.original.clone()],
            Self::Move(undo) => [undo.moved.clone(), undo.original.clone()],
            Self::Trash(undo) => [
                undo.receipt.original_path().clone(),
                undo.receipt.original_path().clone(),
            ],
        }
    }

    const fn kind(&self) -> OperationKind {
        match self {
            Self::Rename(_) => OperationKind::Rename,
            Self::Move(_) => OperationKind::Move,
            Self::Trash(_) => OperationKind::Restore,
        }
    }
}

impl LocalOperation {
    const fn kind(&self) -> OperationKind {
        match self {
            Self::Transfer { action, .. } => action.operation_kind(),
            Self::FinalizeMove(_) => OperationKind::Move,
            Self::Metadata(plan) => {
                if plan.requires_permissions() {
                    OperationKind::SetPermissions
                } else {
                    OperationKind::SetOwnership
                }
            }
            Self::Create(request) => match request.kind() {
                musheen_ops::CreateKind::File => OperationKind::CreateFile,
                musheen_ops::CreateKind::Directory => OperationKind::CreateDirectory,
            },
            Self::Rename(_) => OperationKind::Rename,
            Self::Trash(_) => OperationKind::Trash,
            Self::Restore { .. } => OperationKind::Restore,
            Self::PermanentDelete { .. } => OperationKind::PermanentDelete,
            Self::Archive { plan, .. } => plan.kind(),
        }
    }

    fn affected_path(&self) -> &StorePath {
        match self {
            Self::Transfer { source, .. } => source,
            Self::FinalizeMove(review) => review.source(),
            Self::Metadata(plan) => plan.root(),
            Self::Create(request) => request.parent(),
            Self::Rename(request) => request.source(),
            Self::Trash(target) => target.path(),
            Self::Restore { receipt, .. } => receipt.original_path(),
            Self::PermanentDelete { request, .. } => request.location(),
            Self::Archive { plan, .. } => plan.destination(),
        }
    }

    fn affected_paths(&self) -> Vec<StorePath> {
        match self {
            Self::Transfer {
                source,
                destination,
                ..
            } => vec![source.clone(), destination.clone()],
            Self::FinalizeMove(review) => {
                vec![review.source().clone(), review.destination().clone()]
            }
            Self::Metadata(plan) => vec![plan.root().clone()],
            Self::Create(request) => vec![request.parent().clone()],
            Self::Rename(request) => vec![request.source().clone()],
            Self::Trash(target) => vec![target.path().clone()],
            Self::Restore { receipt, .. } => vec![receipt.original_path().clone()],
            Self::PermanentDelete { request, .. } => request
                .targets()
                .iter()
                .map(|target| target.path().clone())
                .collect(),
            Self::Archive { plan, .. } => plan
                .sources()
                .iter()
                .cloned()
                .chain(std::iter::once(plan.destination().clone()))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveOperationPaths {
    pub id: JobId,
    pub kind: OperationKind,
    pub paths: Vec<StorePath>,
}

#[derive(Debug)]
pub struct ReadyLocalOperation {
    id: JobId,
    generation: EventGeneration,
    cancellation: musheen_core::CancellationToken,
    operation: LocalOperation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransferOutcome {
    Skipped,
    Completed(CommandTargetRef),
    MetadataReview {
        target: CommandTargetRef,
        review: Box<MoveMetadataReview>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalOperationOutcome {
    Transfer(TransferOutcome),
    Metadata,
    Mutation,
    Trash(TrashReceipt),
    Archive,
}

#[derive(Clone, Copy, Debug)]
pub struct ArchiveOperationExecution<'a> {
    id: JobId,
    generation: EventGeneration,
    plan: &'a ArchiveOperationPlan,
    cancellation: &'a musheen_core::CancellationToken,
}

impl<'a> ArchiveOperationExecution<'a> {
    #[must_use]
    pub const fn id(self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn generation(self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn plan(self) -> &'a ArchiveOperationPlan {
        self.plan
    }

    #[must_use]
    pub const fn cancellation(self) -> &'a musheen_core::CancellationToken {
        self.cancellation
    }
}

pub trait ArchiveOperationRoute: fmt::Debug + Send + Sync {
    fn execute_archive(&self, execution: ArchiveOperationExecution<'_>) -> Result<(), Box<str>>;
}

#[derive(Clone, Copy, Debug)]
pub struct ProviderTransferExecution<'a> {
    id: JobId,
    generation: EventGeneration,
    action: DropAction,
    source: &'a StorePath,
    destination: &'a StorePath,
    expected_identity: Option<&'a ItemId>,
    cancellation: &'a musheen_core::CancellationToken,
}

impl<'a> ProviderTransferExecution<'a> {
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn generation(&self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn action(&self) -> DropAction {
        self.action
    }

    #[must_use]
    pub const fn source(&self) -> &'a StorePath {
        self.source
    }

    #[must_use]
    pub const fn destination(&self) -> &'a StorePath {
        self.destination
    }

    #[must_use]
    pub const fn expected_identity(&self) -> Option<&'a ItemId> {
        self.expected_identity
    }

    #[must_use]
    pub const fn cancellation(&self) -> &'a musheen_core::CancellationToken {
        self.cancellation
    }
}

/// Executes transfers whose destination uses a provider-owned opaque path.
/// Implementations must revalidate `expected_identity` immediately before
/// mutating the source and must return the exact completed destination target.
pub trait ProviderTransferRoute: fmt::Debug + Send + Sync {
    fn source_provider_id(&self) -> &musheen_core::ProviderId;

    fn destination_provider_id(&self) -> &musheen_core::ProviderId;

    fn plan_destination(
        &self,
        action: DropAction,
        source: &StorePath,
        target: &StorePath,
        expected_identity: Option<&ItemId>,
    ) -> Result<StorePath, Box<str>>;

    fn provider_snapshot(&self, location: &StorePath) -> ProviderSnapshot;

    fn execute_transfer(
        &self,
        execution: ProviderTransferExecution<'_>,
    ) -> Result<CommandTargetRef, Box<str>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalFailureDisposition {
    Failed,
    Recoverable,
    NeedsAttention,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalOperationFailure {
    message: Box<str>,
    disposition: LocalFailureDisposition,
    recovery_staging: Option<StorePath>,
}

impl LocalOperationFailure {
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub const fn disposition(&self) -> LocalFailureDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn recovery_staging(&self) -> Option<&StorePath> {
        self.recovery_staging.as_ref()
    }

    fn failed(message: impl Into<Box<str>>) -> Self {
        Self {
            message: message.into(),
            disposition: LocalFailureDisposition::Failed,
            recovery_staging: None,
        }
    }

    fn from_transfer(error: OperationFailure) -> Self {
        let disposition = if error.publication_state() == PublicationState::Unknown
            || error.source_state() == SourceState::Unknown
            || error.destination_published()
        {
            LocalFailureDisposition::NeedsAttention
        } else if error.staging_retained().is_some() {
            LocalFailureDisposition::Recoverable
        } else {
            LocalFailureDisposition::Failed
        };
        let recovery_staging = error.staging_retained().cloned();
        let destination = DisplayPath::from_store_path(error.destination());
        let message = match error.staging_retained() {
            Some(staging) => format!(
                "{error} for destination {}; recovery staging remains at {}",
                destination.as_str(),
                DisplayPath::from_store_path(staging).as_str()
            ),
            None => format!("{error} for destination {}", destination.as_str()),
        };
        Self {
            message: message.into(),
            disposition,
            recovery_staging,
        }
    }

    fn from_resolved(error: ResolvedTransferFailure) -> Self {
        match error {
            ResolvedTransferFailure::Transfer(error) => Self::from_transfer(error),
            ResolvedTransferFailure::Failed(message) => Self::failed(message),
            ResolvedTransferFailure::NeedsAttention(message) => Self {
                message,
                disposition: LocalFailureDisposition::NeedsAttention,
                recovery_staging: None,
            },
        }
    }
}

impl ReadyLocalOperation {
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    #[must_use]
    pub const fn generation(&self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        self.operation.kind()
    }

    #[must_use]
    pub fn affected_path(&self) -> &StorePath {
        self.operation.affected_path()
    }

    pub fn execute(self) -> Result<(), Box<str>> {
        self.execute_detailed()
            .map(|_| ())
            .map_err(|error| error.message)
    }

    pub fn execute_detailed(self) -> Result<LocalOperationOutcome, LocalOperationFailure> {
        let mut store = LocalStore::new();
        match self.operation {
            LocalOperation::Transfer {
                action,
                source,
                destination,
                decision,
                expected_identity,
                expected_raw_identity,
                expected_destination_parent_identity,
                provider_route,
            } => {
                validate_undo_transfer(
                    &mut store,
                    &source,
                    &destination,
                    expected_raw_identity.as_deref(),
                    expected_destination_parent_identity.as_deref(),
                )?;
                if let Some(route) = provider_route {
                    let target = route
                        .execute_transfer(ProviderTransferExecution {
                            id: self.id,
                            generation: self.generation,
                            action,
                            source: &source,
                            destination: &destination,
                            expected_identity: expected_identity.as_ref(),
                            cancellation: &self.cancellation,
                        })
                        .map_err(LocalOperationFailure::failed)?;
                    return Ok(LocalOperationOutcome::Transfer(TransferOutcome::Completed(
                        target,
                    )));
                }
                if let Some(expected_identity) = expected_identity {
                    let current = crate::metadata::item_from_path(
                        store.provider_id(),
                        clean_absolute_path(&source).ok_or_else(|| {
                            LocalOperationFailure::failed(
                                "source path is not a local absolute path",
                            )
                        })?,
                    )
                    .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
                    if current.id() != &expected_identity {
                        return Err(LocalOperationFailure::failed(
                            "the source identity changed before the transfer could execute",
                        ));
                    }
                }
                let request = CopyRequest::new(self.id, self.generation, source, destination);
                if let Some(decision) = decision {
                    let outcome = execute_resolved_transfer(
                        &mut store,
                        &request,
                        &decision,
                        &self.cancellation,
                    )
                    .map_err(LocalOperationFailure::from_resolved)?;
                    return match outcome {
                        ResolvedTransferOutcome::Skipped => {
                            Ok(LocalOperationOutcome::Transfer(TransferOutcome::Skipped))
                        }
                        ResolvedTransferOutcome::Completed {
                            path,
                            metadata_review,
                        } => transfer_completed(&store, path, metadata_review),
                    };
                }
                match action {
                    DropAction::Copy => {
                        CopySession::default()
                            .execute(&mut store, &request, &self.cancellation)
                            .map_err(LocalOperationFailure::from_transfer)?;
                        transfer_completed(&store, request.destination().clone(), None)
                    }
                    DropAction::Move => {
                        let outcome = execute_move(&mut store, &request, &self.cancellation)
                            .map_err(LocalOperationFailure::from_transfer)?;
                        transfer_completed(
                            &store,
                            request.destination().clone(),
                            outcome.into_metadata_review().map(Box::new),
                        )
                    }
                }
            }
            LocalOperation::FinalizeMove(review) => {
                let destination = review.destination().clone();
                complete_move_after_metadata_review(&mut store, *review, &self.cancellation)
                    .map_err(LocalOperationFailure::from_transfer)?;
                transfer_completed(&store, destination, None)
            }
            LocalOperation::Metadata(plan) => plan
                .execute_controlled(&mut store, &self.cancellation)
                .map(|()| LocalOperationOutcome::Metadata)
                .map_err(|error| LocalOperationFailure::failed(error.to_string())),
            LocalOperation::Create(request) => execute_create(&mut store, &request)
                .map(|_| LocalOperationOutcome::Mutation)
                .map_err(|error| LocalOperationFailure::failed(error.to_string())),
            LocalOperation::Rename(request) => execute_rename(&mut store, &request)
                .map(|_| LocalOperationOutcome::Mutation)
                .map_err(|error| LocalOperationFailure::failed(error.to_string())),
            LocalOperation::Trash(target) => execute_delete(&mut store, vec![target])
                .and_then(|outcome| {
                    if let Some(failure) = outcome.failures().first() {
                        return Err(failure.error().clone());
                    }
                    outcome
                        .trashed()
                        .first()
                        .cloned()
                        .ok_or(MutationError::Missing)
                })
                .map(LocalOperationOutcome::Trash)
                .map_err(|error| LocalOperationFailure::failed(error.to_string())),
            LocalOperation::Restore {
                receipt,
                expected_parent_identity,
            } => {
                validate_undo_transfer(
                    &mut store,
                    receipt.original_path(),
                    receipt.original_path(),
                    None,
                    Some(&expected_parent_identity),
                )?;
                execute_restore(&mut store, &receipt)
                    .map(|()| LocalOperationOutcome::Mutation)
                    .map_err(|error| LocalOperationFailure::failed(error.to_string()))
            }
            LocalOperation::PermanentDelete {
                request,
                confirmation,
            } => execute_permanent_delete(&mut store, &request, &confirmation)
                .and_then(|outcome| {
                    outcome
                        .failures()
                        .first()
                        .map_or(Ok(()), |failure| Err(failure.error().clone()))
                })
                .map(|()| LocalOperationOutcome::Mutation)
                .map_err(|error| LocalOperationFailure::failed(error.to_string())),
            LocalOperation::Archive { plan, route } => route
                .execute_archive(ArchiveOperationExecution {
                    id: self.id,
                    generation: self.generation,
                    plan: &plan,
                    cancellation: &self.cancellation,
                })
                .map(|()| LocalOperationOutcome::Archive)
                .map_err(LocalOperationFailure::failed),
        }
    }
}

fn validate_undo_transfer(
    store: &mut LocalStore,
    source: &StorePath,
    destination: &StorePath,
    expected_source_identity: Option<&[u8]>,
    expected_parent_identity: Option<&[u8]>,
) -> Result<(), LocalOperationFailure> {
    if let Some(expected) = expected_source_identity {
        let current = MutationProvider::identity(store, source)
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        if current.as_deref() != Some(expected) {
            return Err(LocalOperationFailure::failed(
                "the source identity changed before undo could execute",
            ));
        }
    }
    if let Some(expected) = expected_parent_identity {
        let parent = clean_absolute_path(destination)
            .and_then(Path::parent)
            .ok_or_else(|| LocalOperationFailure::failed("undo destination has no local parent"))?;
        let parent = StorePath::from_unix_path(parent.as_os_str());
        let current = MutationProvider::identity(store, &parent)
            .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
        if current.as_deref() != Some(expected) {
            return Err(LocalOperationFailure::failed(
                "the destination parent changed before undo could execute",
            ));
        }
    }
    Ok(())
}

fn transfer_completed(
    store: &LocalStore,
    path: StorePath,
    metadata_review: Option<Box<MoveMetadataReview>>,
) -> Result<LocalOperationOutcome, LocalOperationFailure> {
    let item = store
        .resolve_item(&path)
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?
        .ok_or_else(|| {
            LocalOperationFailure::failed(
                "the completed transfer destination could not be resolved",
            )
        })?;
    let target = CommandTargetRef::new(item.id().clone(), path)
        .map_err(|error| LocalOperationFailure::failed(error.to_string()))?;
    Ok(LocalOperationOutcome::Transfer(metadata_review.map_or(
        TransferOutcome::Completed(target.clone()),
        |review| TransferOutcome::MetadataReview { target, review },
    )))
}

#[derive(Debug)]
pub struct LocalOperationQueue {
    scheduler: Scheduler,
    operations: BTreeMap<JobId, LocalOperation>,
    failures: BTreeMap<JobId, Box<str>>,
    undo_candidates: BTreeMap<JobId, UndoCandidate>,
    provider_routes: HashMap<
        (musheen_core::ProviderId, musheen_core::ProviderId),
        Arc<dyn ProviderTransferRoute>,
    >,
    archive_route: Option<Arc<dyn ArchiveOperationRoute>>,
}

impl LocalOperationQueue {
    #[must_use]
    pub fn new(limits: &ResourceLimits) -> Self {
        Self {
            scheduler: Scheduler::new(limits),
            operations: BTreeMap::new(),
            failures: BTreeMap::new(),
            undo_candidates: BTreeMap::new(),
            provider_routes: HashMap::new(),
            archive_route: None,
        }
    }

    pub fn starting_after(limits: &ResourceLimits, last_job_id: JobId) -> Result<Self, DropError> {
        Ok(Self {
            scheduler: Scheduler::new_starting_after(limits, last_job_id)?,
            operations: BTreeMap::new(),
            failures: BTreeMap::new(),
            undo_candidates: BTreeMap::new(),
            provider_routes: HashMap::new(),
            archive_route: None,
        })
    }

    pub fn register_provider_transfer_route(&mut self, route: Arc<dyn ProviderTransferRoute>) {
        self.provider_routes.insert(
            (
                route.source_provider_id().clone(),
                route.destination_provider_id().clone(),
            ),
            route,
        );
    }

    pub fn register_archive_route(&mut self, route: Arc<dyn ArchiveOperationRoute>) {
        self.archive_route = Some(route);
    }

    #[must_use]
    pub fn can_accept(&self, payload: &FileDragPayload, target: &StorePath) -> bool {
        self.inspect_drop(payload, target).is_ok()
    }

    pub fn conflicts_for_drop(
        &self,
        payload: &FileDragPayload,
        target: &StorePath,
    ) -> Result<Vec<ConflictRecord>, DropError> {
        Ok(self
            .inspect_drop(payload, target)?
            .into_iter()
            .filter_map(|candidate| candidate.conflict)
            .collect())
    }

    pub fn submit_drop(
        &mut self,
        payload: FileDragPayload,
        target: StorePath,
    ) -> Result<Vec<JobId>, DropError> {
        let planned = self.plan_drop(&payload, &target, &[])?;
        self.enqueue_planned(planned)
    }

    pub fn submit_drop_resolved(
        &mut self,
        payload: FileDragPayload,
        target: StorePath,
        decisions: Vec<ConflictDecision>,
    ) -> Result<Vec<JobId>, DropError> {
        let planned = self.plan_drop(&payload, &target, &decisions)?;
        self.enqueue_planned(planned)
    }

    pub fn submit_metadata(&mut self, plans: Vec<MetadataPlan>) -> Result<Vec<JobId>, DropError> {
        if plans.is_empty() {
            return Err(DropError::Mutation(MutationError::NoChanges));
        }
        let store = LocalStore::new();
        let mut planned = Vec::with_capacity(plans.len());
        for plan in plans {
            let root = plan.root().clone();
            let kind = if plan.requires_permissions() {
                OperationKind::SetPermissions
            } else {
                OperationKind::SetOwnership
            };
            let provider = provider_snapshot(&store, &root);
            let operation_plan = OperationPlan::new(kind, provider, None, root)
                .map_err(|error| DropError::Plan(error.to_string().into()))?;
            planned.push((operation_plan, LocalOperation::Metadata(plan)));
        }
        self.enqueue_planned(planned)
    }

    pub fn submit_metadata_changes(
        &mut self,
        roots: Vec<StorePath>,
        scope: MetadataScope,
        change: MetadataChange,
    ) -> Result<Vec<JobId>, DropError> {
        if roots.is_empty() {
            return Err(DropError::EmptySelection);
        }
        let mut store = LocalStore::new();
        let plans = roots
            .into_iter()
            .map(|root| {
                let identity =
                    MutationProvider::identity(&mut store, &root)?.ok_or(MutationError::Missing)?;
                MetadataPlan::preflight(&mut store, root, identity.to_vec(), scope, change.clone())
            })
            .collect::<Result<Vec<_>, MutationError>>()?;
        self.submit_metadata(plans)
    }

    pub fn submit_create(&mut self, request: CreateRequest) -> Result<JobId, DropError> {
        let kind = match request.kind() {
            musheen_ops::CreateKind::File => OperationKind::CreateFile,
            musheen_ops::CreateKind::Directory => OperationKind::CreateDirectory,
        };
        let destination = request.destination()?;
        let store = LocalStore::new();
        let plan = OperationPlan::new(
            kind,
            provider_snapshot(&store, request.parent()),
            None,
            destination,
        )
        .map_err(|error| DropError::Plan(error.to_string().into()))?;
        self.enqueue_planned(vec![(plan, LocalOperation::Create(request))])
            .map(|mut ids| ids.remove(0))
    }

    pub fn submit_rename(&mut self, request: RenameRequest) -> Result<JobId, DropError> {
        let source = request.source().clone();
        let destination = request.destination()?;
        let store = LocalStore::new();
        let plan = OperationPlan::new(
            OperationKind::Rename,
            provider_snapshot(&store, &source),
            Some(source),
            destination,
        )
        .map_err(|error| DropError::Plan(error.to_string().into()))?;
        self.enqueue_planned(vec![(plan, LocalOperation::Rename(request))])
            .map(|mut ids| ids.remove(0))
    }

    pub fn submit_trash(&mut self, targets: Vec<DeleteTarget>) -> Result<Vec<JobId>, DropError> {
        if targets.is_empty() {
            return Err(DropError::EmptySelection);
        }
        let store = LocalStore::new();
        let planned = targets
            .into_iter()
            .map(|target| {
                let location = target.path().clone();
                let plan = OperationPlan::new(
                    OperationKind::Trash,
                    provider_snapshot(&store, &location),
                    None,
                    location,
                )
                .map_err(|error| DropError::Plan(error.to_string().into()))?;
                Ok((plan, LocalOperation::Trash(target)))
            })
            .collect::<Result<Vec<_>, DropError>>()?;
        self.enqueue_planned(planned)
    }

    pub fn submit_permanent_delete(
        &mut self,
        request: PermanentDeleteRequest,
        confirmation: PermanentDeleteConfirmation,
    ) -> Result<JobId, DropError> {
        let location = request.location().clone();
        let store = LocalStore::new();
        let plan = OperationPlan::new(
            OperationKind::PermanentDelete,
            provider_snapshot(&store, &location),
            None,
            location,
        )
        .map_err(|error| DropError::Plan(error.to_string().into()))?;
        self.enqueue_planned(vec![(
            plan,
            LocalOperation::PermanentDelete {
                request,
                confirmation,
            },
        )])
        .map(|mut ids| ids.remove(0))
    }

    pub fn submit_archive(&mut self, plan: ArchiveOperationPlan) -> Result<JobId, DropError> {
        let route = self
            .archive_route
            .clone()
            .ok_or_else(|| DropError::Plan("archive operation provider is unavailable".into()))?;
        let store = LocalStore::new();
        let operation_plan = plan
            .clone()
            .into_operation_plan(provider_snapshot(&store, plan.destination()))
            .map_err(|error| DropError::Plan(error.to_string().into()))?;
        self.enqueue_planned(vec![(
            operation_plan,
            LocalOperation::Archive { plan, route },
        )])
        .map(|mut ids| ids.remove(0))
    }

    pub fn start_ready(&mut self) -> Result<Vec<ReadyLocalOperation>, DropError> {
        self.scheduler
            .start_ready()?
            .into_iter()
            .map(|scheduled| {
                let operation = self
                    .operations
                    .get(&scheduled.id())
                    .cloned()
                    .ok_or(DropError::MissingOperation(scheduled.id()))?;
                Ok(ReadyLocalOperation {
                    id: scheduled.id(),
                    generation: scheduled.generation(),
                    cancellation: scheduled.cancellation().clone(),
                    operation,
                })
            })
            .collect()
    }

    pub fn finish(&mut self, id: JobId, result: Result<(), Box<str>>) -> Result<(), DropError> {
        if self.scheduler.state(id) == Some(JobState::Cancelling) {
            self.scheduler.finish_cancel(id)?;
            self.failures.remove(&id);
            self.operations.remove(&id);
            return Ok(());
        }
        match result {
            Ok(()) => {
                self.scheduler.complete(id)?;
                let undo = self
                    .operations
                    .remove(&id)
                    .and_then(|operation| match operation {
                        LocalOperation::Rename(request) => {
                            RenameUndo::from_completed(&request).map(UndoCandidate::Rename)
                        }
                        LocalOperation::Transfer {
                            action: DropAction::Move,
                            source,
                            destination,
                            decision: None,
                            provider_route: None,
                            ..
                        } => MoveUndo::from_completed(source, destination).map(UndoCandidate::Move),
                        _ => None,
                    });
                if let Some(undo) = undo {
                    self.remember_undo(id, undo);
                }
            }
            Err(error) => {
                self.scheduler.fail(id)?;
                self.failures.insert(id, error);
            }
        }
        Ok(())
    }

    pub fn finish_with_outcome(
        &mut self,
        id: JobId,
        outcome: LocalOperationOutcome,
    ) -> Result<(), DropError> {
        let receipt = match &outcome {
            LocalOperationOutcome::Trash(receipt) => match self.operations.get(&id) {
                Some(LocalOperation::Trash(target)) if target.path() == receipt.original_path() => {
                    Some(receipt.clone())
                }
                _ => return Err(DropError::UndoUnavailable(id)),
            },
            _ => None,
        };
        self.finish(id, Ok(()))?;
        if self.scheduler.state(id) == Some(JobState::Completed)
            && let Some(undo) = receipt.and_then(TrashUndo::from_completed)
        {
            self.remember_undo(id, UndoCandidate::Trash(undo));
        }
        Ok(())
    }

    fn remember_undo(&mut self, id: JobId, undo: UndoCandidate) {
        self.undo_candidates.insert(id, undo);
        if self.undo_candidates.len() > 100 {
            self.undo_candidates.pop_first();
        }
    }

    pub fn finish_metadata_review(
        &mut self,
        id: JobId,
        review: Box<MoveMetadataReview>,
    ) -> Result<(), DropError> {
        self.scheduler.fail(id)?;
        self.operations
            .insert(id, LocalOperation::FinalizeMove(review));
        self.failures
            .insert(id, "metadata loss requires confirmation".into());
        Ok(())
    }

    #[must_use]
    pub fn can_confirm_metadata_loss(&self, id: JobId) -> bool {
        self.scheduler.state(id) == Some(JobState::Failed)
            && matches!(
                self.operations.get(&id),
                Some(LocalOperation::FinalizeMove(_))
            )
    }

    pub fn confirm_metadata_loss(&mut self, id: JobId) -> Result<EventGeneration, DropError> {
        if !self.can_confirm_metadata_loss(id) {
            return Err(DropError::MissingOperation(id));
        }
        let generation = self.scheduler.retry(id)?;
        self.failures.remove(&id);
        Ok(generation)
    }

    pub fn keep_source_after_metadata_review(&mut self, id: JobId) -> Result<(), DropError> {
        if !self.can_confirm_metadata_loss(id) {
            return Err(DropError::MissingOperation(id));
        }
        self.scheduler.retry(id)?;
        self.scheduler.cancel(id)?;
        self.operations.remove(&id);
        self.failures.remove(&id);
        Ok(())
    }

    pub fn pause(&mut self, id: JobId) -> Result<(), DropError> {
        self.scheduler.pause(id)?;
        Ok(())
    }

    pub fn resume(&mut self, id: JobId) -> Result<(), DropError> {
        self.scheduler.resume(id)?;
        Ok(())
    }

    pub fn cancel(&mut self, id: JobId) -> Result<(), DropError> {
        let queued = self.scheduler.state(id) == Some(JobState::Queued);
        self.scheduler.cancel(id)?;
        if queued {
            self.operations.remove(&id);
        }
        Ok(())
    }

    pub fn retry(&mut self, id: JobId) -> Result<EventGeneration, DropError> {
        if !self.operations.contains_key(&id) {
            return Err(DropError::MissingOperation(id));
        }
        let generation = self.scheduler.retry(id)?;
        self.failures.remove(&id);
        Ok(generation)
    }

    #[must_use]
    pub fn can_retry(&self, id: JobId) -> bool {
        self.operations.contains_key(&id)
            && matches!(
                self.scheduler.state(id),
                Some(JobState::Failed | JobState::Interrupted)
            )
    }

    #[must_use]
    pub fn has_undo_candidate(&self, id: JobId) -> bool {
        self.scheduler.state(id) == Some(JobState::Completed)
            && self.undo_candidates.contains_key(&id)
    }

    #[must_use]
    pub fn can_undo(&self, id: JobId) -> bool {
        self.has_undo_candidate(id)
            && self
                .undo_candidates
                .get(&id)
                .is_some_and(UndoCandidate::is_available)
    }

    #[must_use]
    pub fn undo_paths(&self, id: JobId) -> Option<[StorePath; 2]> {
        self.undo_candidates.get(&id).map(UndoCandidate::paths)
    }

    #[must_use]
    pub fn undo_kind(&self, id: JobId) -> Option<OperationKind> {
        self.undo_candidates.get(&id).map(UndoCandidate::kind)
    }

    pub fn submit_undo(&mut self, id: JobId) -> Result<JobId, DropError> {
        if !self.can_undo(id) {
            return Err(DropError::UndoUnavailable(id));
        }
        let undo = self.undo_candidates[&id].clone();
        let undo_job = match undo {
            UndoCandidate::Rename(undo) => self.submit_rename(undo.reverse)?,
            UndoCandidate::Move(undo) => {
                let payload = FileDragPayload::with_expected_identities(
                    vec![undo.moved],
                    vec![undo.expected_item_id],
                    DropAction::Move,
                )?;
                let mut planned = self.plan_drop(&payload, &undo.original_parent, &[])?;
                if planned.len() != 1 {
                    return Err(DropError::UndoUnavailable(id));
                }
                let LocalOperation::Transfer {
                    expected_raw_identity,
                    expected_destination_parent_identity,
                    ..
                } = &mut planned[0].1
                else {
                    return Err(DropError::UndoUnavailable(id));
                };
                *expected_raw_identity = Some(undo.expected_identity);
                *expected_destination_parent_identity =
                    Some(undo.expected_original_parent_identity);
                self.enqueue_planned(planned)?
                    .into_iter()
                    .next()
                    .ok_or(DropError::UndoUnavailable(id))?
            }
            UndoCandidate::Trash(undo) => {
                let original = undo.receipt.original_path().clone();
                let source = StorePath::from_provider_key(
                    ProviderId::new("local.trash")
                        .map_err(|error| DropError::Plan(error.to_string().into()))?,
                    undo.receipt.provider_reference().to_vec(),
                )
                .map_err(|error| DropError::Plan(error.to_string().into()))?;
                let store = LocalStore::new();
                let plan = OperationPlan::new(
                    OperationKind::Restore,
                    provider_snapshot(&store, &original),
                    Some(source),
                    original,
                )
                .map_err(|error| DropError::Plan(error.to_string().into()))?;
                self.enqueue_planned(vec![(
                    plan,
                    LocalOperation::Restore {
                        receipt: undo.receipt,
                        expected_parent_identity: undo.expected_parent_identity,
                    },
                )])?
                .into_iter()
                .next()
                .ok_or(DropError::UndoUnavailable(id))?
            }
        };
        self.undo_candidates.remove(&id);
        Ok(undo_job)
    }

    #[must_use]
    pub fn operation_paths(&self, id: JobId) -> Option<Vec<StorePath>> {
        self.operations.get(&id).map(LocalOperation::affected_paths)
    }

    pub fn interrupt(&mut self, id: JobId) -> Result<(), DropError> {
        self.scheduler.interrupt(id)?;
        Ok(())
    }

    #[must_use]
    pub fn state(&self, id: JobId) -> Option<JobState> {
        self.scheduler.state(id)
    }

    #[must_use]
    pub fn job_count(&self) -> usize {
        self.operations
            .keys()
            .filter(|id| {
                matches!(
                    self.scheduler.state(**id),
                    Some(
                        JobState::Queued
                            | JobState::Running
                            | JobState::Paused
                            | JobState::Cancelling
                    )
                )
            })
            .count()
    }

    #[must_use]
    pub fn active_operation_paths(&self) -> Vec<ActiveOperationPaths> {
        self.operations
            .iter()
            .filter(|(id, _)| {
                matches!(
                    self.scheduler.state(**id),
                    Some(
                        JobState::Queued
                            | JobState::Running
                            | JobState::Paused
                            | JobState::Cancelling
                    )
                )
            })
            .map(|(id, operation)| ActiveOperationPaths {
                id: *id,
                kind: operation.kind(),
                paths: operation.affected_paths(),
            })
            .collect()
    }

    #[must_use]
    pub fn failure(&self, id: JobId) -> Option<&str> {
        self.failures.get(&id).map(AsRef::as_ref)
    }

    fn enqueue_planned(
        &mut self,
        planned: Vec<(OperationPlan, LocalOperation)>,
    ) -> Result<Vec<JobId>, DropError> {
        let mut ids = Vec::with_capacity(planned.len());
        for (plan, operation) in planned {
            let id = self.scheduler.enqueue(plan)?;
            self.operations.insert(id, operation);
            ids.push(id);
        }
        Ok(ids)
    }

    fn plan_drop(
        &self,
        payload: &FileDragPayload,
        target: &StorePath,
        decisions: &[ConflictDecision],
    ) -> Result<Vec<(OperationPlan, LocalOperation)>, DropError> {
        let candidates = self.inspect_drop(payload, target)?;
        let mut matched = HashSet::with_capacity(decisions.len());
        let mut planned = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let decision = if let Some(conflict) = candidate.conflict.as_ref() {
                let (index, decision) = decisions
                    .iter()
                    .enumerate()
                    .find(|(_, decision)| decision_matches_conflict(decision, conflict))
                    .ok_or_else(|| DropError::DestinationExists(conflict.destination().clone()))?;
                if !matched.insert(index) {
                    return Err(DropError::Mutation(MutationError::InvalidScope));
                }
                Some(decision.clone())
            } else {
                None
            };
            planned.push((
                candidate.plan,
                LocalOperation::Transfer {
                    action: candidate.action,
                    source: candidate.source,
                    destination: candidate.destination,
                    decision,
                    expected_identity: candidate.expected_identity,
                    expected_raw_identity: None,
                    expected_destination_parent_identity: None,
                    provider_route: candidate.provider_route,
                },
            ));
        }
        if matched.len() != decisions.len() {
            return Err(DropError::Mutation(MutationError::InvalidScope));
        }
        Ok(planned)
    }

    fn inspect_drop(
        &self,
        payload: &FileDragPayload,
        target: &StorePath,
    ) -> Result<Vec<DropCandidate>, DropError> {
        let destination_provider = path_provider_id(target);
        let local_provider = local_provider_id();
        if destination_provider != local_provider
            || payload
                .sources
                .iter()
                .any(|source| path_provider_id(source) != local_provider)
        {
            return self.inspect_provider_drop(payload, target, &destination_provider);
        }
        let target_path = writable_directory(target)?;
        let canonical_target = fs::canonicalize(target_path)
            .map_err(|_| DropError::UnsupportedTarget(target.clone()))?;
        let mut store = LocalStore::new();
        let provider = provider_snapshot(&store, target);
        let mut sources = HashSet::with_capacity(payload.sources.len());
        let mut destinations = HashSet::with_capacity(payload.sources.len());
        let mut planned = Vec::with_capacity(payload.sources.len());

        for (index, source) in payload.sources.iter().enumerate() {
            let source_path = clean_absolute_path(source).ok_or_else(|| {
                DropError::InvalidSource(source.clone(), "not an absolute local path".into())
            })?;
            if !sources.insert(source_path.to_path_buf()) {
                return Err(DropError::DuplicateSource(source.clone()));
            }
            let metadata = fs::symlink_metadata(source_path).map_err(|error| {
                DropError::InvalidSource(source.clone(), error.to_string().into())
            })?;
            let expected_identity = payload.expected_identity(index).cloned();
            if let Some(expected_identity) = expected_identity.as_ref() {
                let current = crate::metadata::item_from_path(store.provider_id(), source_path)
                    .map_err(|error| {
                        DropError::InvalidSource(source.clone(), error.to_string().into())
                    })?;
                if current.id() != expected_identity {
                    return Err(DropError::SourceIdentityChanged(source.clone()));
                }
            }
            let kind = metadata.file_type();
            if !(kind.is_file() || kind.is_dir() || kind.is_symlink()) {
                return Err(DropError::InvalidSource(
                    source.clone(),
                    "special files cannot be transferred".into(),
                ));
            }
            if kind.is_dir() {
                let canonical_source = fs::canonicalize(source_path).map_err(|error| {
                    DropError::InvalidSource(source.clone(), error.to_string().into())
                })?;
                if canonical_target.starts_with(&canonical_source) {
                    return Err(DropError::RecursiveTarget(source.clone()));
                }
            }
            let name = source_path.file_name().ok_or_else(|| {
                DropError::InvalidSource(source.clone(), "the source has no file name".into())
            })?;
            let destination_path = target_path.join(name);
            if destination_path == source_path {
                return Err(DropError::NoChange(source.clone()));
            }
            if !destinations.insert(destination_path.clone()) {
                return Err(DropError::DuplicateDestination(StorePath::from_unix_path(
                    destination_path.as_os_str(),
                )));
            }
            let destination = StorePath::from_unix_path(destination_path.as_os_str());
            let conflict = match fs::symlink_metadata(&destination_path) {
                Ok(destination_metadata) => {
                    let source_identity = MutationProvider::identity(&mut store, source)?
                        .ok_or(MutationError::Missing)?;
                    let destination_identity =
                        MutationProvider::identity(&mut store, &destination)?
                            .ok_or(MutationError::Missing)?;
                    Some(
                        ConflictRecord::new(
                            payload.action.operation_kind(),
                            source.clone(),
                            source_identity.to_vec(),
                            conflict_kind(&metadata),
                            destination.clone(),
                            destination_identity.to_vec(),
                            conflict_kind(&destination_metadata),
                        )
                        .map_err(|error| DropError::Plan(error.to_string().into()))?,
                    )
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => {
                    return Err(DropError::Plan(
                        format!("could not inspect destination: {error}").into(),
                    ));
                }
            };
            let plan = OperationPlan::new(
                payload.action.operation_kind(),
                provider.clone(),
                Some(source.clone()),
                destination.clone(),
            )
            .map_err(|error| DropError::Plan(error.to_string().into()))?;
            planned.push(DropCandidate {
                plan,
                action: payload.action,
                source: source.clone(),
                destination,
                conflict,
                expected_identity,
                provider_route: None,
            });
        }
        Ok(planned)
    }

    fn inspect_provider_drop(
        &self,
        payload: &FileDragPayload,
        target: &StorePath,
        destination_provider: &musheen_core::ProviderId,
    ) -> Result<Vec<DropCandidate>, DropError> {
        let mut sources = HashSet::with_capacity(payload.sources.len());
        let mut destinations = HashSet::with_capacity(payload.sources.len());
        let mut planned = Vec::with_capacity(payload.sources.len());
        for (index, source) in payload.sources.iter().enumerate() {
            if !sources.insert(source.clone()) {
                return Err(DropError::DuplicateSource(source.clone()));
            }
            let expected_identity = payload.expected_identity(index).cloned();
            let source_provider = expected_identity.as_ref().map_or_else(
                || path_provider_id(source),
                |identity| identity.provider().clone(),
            );
            if path_provider_id(source) != source_provider {
                return Err(DropError::Plan(
                    "the source identity belongs to another provider".into(),
                ));
            }
            let route = self
                .provider_routes
                .get(&(source_provider, destination_provider.clone()))
                .cloned()
                .ok_or_else(|| DropError::UnsupportedTarget(target.clone()))?;
            let destination = route
                .plan_destination(payload.action, source, target, expected_identity.as_ref())
                .map_err(DropError::Plan)?;
            if path_provider_id(&destination) != *route.destination_provider_id() {
                return Err(DropError::Plan(
                    "the provider route returned a destination owned by another provider".into(),
                ));
            }
            if !destinations.insert(destination.clone()) {
                return Err(DropError::DuplicateDestination(destination));
            }
            let plan = OperationPlan::new(
                payload.action.operation_kind(),
                route.provider_snapshot(&destination),
                Some(source.clone()),
                destination.clone(),
            )
            .map_err(|error| DropError::Plan(error.to_string().into()))?;
            planned.push(DropCandidate {
                plan,
                action: payload.action,
                source: source.clone(),
                destination,
                conflict: None,
                expected_identity,
                provider_route: Some(Arc::clone(&route)),
            });
        }
        Ok(planned)
    }
}

struct DropCandidate {
    plan: OperationPlan,
    action: DropAction,
    source: StorePath,
    destination: StorePath,
    conflict: Option<ConflictRecord>,
    expected_identity: Option<ItemId>,
    provider_route: Option<Arc<dyn ProviderTransferRoute>>,
}

fn decision_matches_conflict(decision: &ConflictDecision, conflict: &ConflictRecord) -> bool {
    decision.operation() == conflict.operation()
        && decision.source() == conflict.source()
        && decision.destination() == conflict.destination()
        && decision.source_identity() == conflict.source_identity()
        && decision.destination_identity() == conflict.destination_identity()
}

fn conflict_kind(metadata: &fs::Metadata) -> ConflictItemKind {
    if metadata.file_type().is_symlink() {
        ConflictItemKind::SymbolicLink
    } else if metadata.is_dir() {
        ConflictItemKind::Directory
    } else {
        ConflictItemKind::File
    }
}

fn provider_snapshot(store: &LocalStore, location: &StorePath) -> ProviderSnapshot {
    ProviderSnapshot::new(
        store.provider_id().clone(),
        store.capabilities(location),
        ProviderLimits::default(),
    )
}

fn local_provider_id() -> musheen_core::ProviderId {
    musheen_core::ProviderId::new("local").expect("the built-in local provider ID is valid")
}

fn path_provider_id(path: &StorePath) -> musheen_core::ProviderId {
    path.provider_key()
        .map_or_else(local_provider_id, |(provider, _)| provider.clone())
}

fn clean_absolute_path(path: &StorePath) -> Option<&Path> {
    let path = path.as_unix_path()?;
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return None;
    }
    Some(path)
}

fn writable_directory(target: &StorePath) -> Result<&Path, DropError> {
    let path =
        clean_absolute_path(target).ok_or_else(|| DropError::UnsupportedTarget(target.clone()))?;
    let metadata = fs::metadata(path).map_err(|_| DropError::UnsupportedTarget(target.clone()))?;
    if !metadata.is_dir() || metadata.permissions().mode() & 0o222 == 0 {
        return Err(DropError::UnsupportedTarget(target.clone()));
    }
    let store = LocalStore::new();
    if store
        .probe(target)
        .map_err(|error| DropError::Plan(error.to_string().into()))?
        .is_read_only()
    {
        return Err(DropError::UnsupportedTarget(target.clone()));
    }
    Ok(path)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DropError {
    EmptySelection,
    IdentityCountMismatch,
    UnsupportedTarget(StorePath),
    InvalidSource(StorePath, Box<str>),
    SourceIdentityChanged(StorePath),
    DuplicateSource(StorePath),
    DuplicateDestination(StorePath),
    DestinationExists(StorePath),
    RecursiveTarget(StorePath),
    NoChange(StorePath),
    MissingOperation(JobId),
    UndoUnavailable(JobId),
    Plan(Box<str>),
    Mutation(MutationError),
    Scheduler(SchedulerError),
}

impl fmt::Display for DropError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptySelection => formatter.write_str("the drop contains no files"),
            Self::IdentityCountMismatch => {
                formatter.write_str("drop sources and identities differ")
            }
            Self::UnsupportedTarget(_) => formatter.write_str("the drop target is not writable"),
            Self::InvalidSource(_, reason) => write!(formatter, "invalid drop source: {reason}"),
            Self::SourceIdentityChanged(_) => {
                formatter.write_str("the drop source identity changed before it could be queued")
            }
            Self::DuplicateSource(_) => formatter.write_str("the drop repeats a source"),
            Self::DuplicateDestination(_) => {
                formatter.write_str("multiple sources resolve to the same destination")
            }
            Self::DestinationExists(_) => formatter.write_str("the destination already exists"),
            Self::RecursiveTarget(_) => {
                formatter.write_str("a directory cannot be dropped into itself")
            }
            Self::NoChange(_) => formatter.write_str("the source is already in that directory"),
            Self::MissingOperation(id) => write!(formatter, "job {} has no operation", id.get()),
            Self::UndoUnavailable(id) => {
                write!(formatter, "job {} cannot be safely undone", id.get())
            }
            Self::Plan(message) => formatter.write_str(message),
            Self::Mutation(error) => error.fmt(formatter),
            Self::Scheduler(error) => error.fmt(formatter),
        }
    }
}

impl Error for DropError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Mutation(error) => Some(error),
            Self::Scheduler(error) => Some(error),
            _ => None,
        }
    }
}

impl From<MutationError> for DropError {
    fn from(error: MutationError) -> Self {
        Self::Mutation(error)
    }
}

impl From<SchedulerError> for DropError {
    fn from(error: SchedulerError) -> Self {
        Self::Scheduler(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct AcceptDecisions;

    impl musheen_ops::ConflictDecisionJournal for AcceptDecisions {
        fn persist_decision(&mut self, _: &ConflictDecision) -> Result<(), MutationError> {
            Ok(())
        }
    }

    #[derive(Debug, Default)]
    struct RecordingArchiveRoute {
        execution: Mutex<Option<(JobId, EventGeneration, ArchiveOperationPlan)>>,
    }

    impl ArchiveOperationRoute for RecordingArchiveRoute {
        fn execute_archive(
            &self,
            execution: ArchiveOperationExecution<'_>,
        ) -> Result<(), Box<str>> {
            *self.execution.lock().unwrap() = Some((
                execution.id(),
                execution.generation(),
                execution.plan().clone(),
            ));
            Ok(())
        }
    }

    fn metadata_review(
        source: StorePath,
        destination: StorePath,
    ) -> musheen_ops::MoveMetadataReview {
        musheen_ops::MoveMetadataReview::new(
            source,
            destination,
            musheen_ops::EntrySnapshot::new(
                b"source".to_vec(),
                musheen_ops::EntryKind::RegularFile,
                8,
                8,
                1,
            ),
            musheen_ops::SourceRemovalToken::new(b"source".to_vec()),
            musheen_ops::CopyStrategy::Streamed,
            musheen_ops::MetadataReport::with_skipped([musheen_ops::MetadataKind::Ownership]),
        )
    }

    fn finish_one(queue: &mut LocalOperationQueue) -> LocalOperationOutcome {
        let operation = queue
            .start_ready()
            .expect("operation starts")
            .into_iter()
            .next()
            .expect("one operation is ready");
        let id = operation.id();
        let outcome = operation.execute_detailed().expect("operation succeeds");
        queue
            .finish_with_outcome(id, outcome.clone())
            .expect("operation finishes");
        outcome
    }

    #[test]
    fn resolved_transaction_attention_is_never_flattened_to_retryable_failure() {
        let failure =
            LocalOperationFailure::from_resolved(ResolvedTransferFailure::NeedsAttention(
                "the published destination needs inspection".into(),
            ));

        assert_eq!(
            failure.disposition(),
            LocalFailureDisposition::NeedsAttention
        );
        assert_eq!(
            failure.message(),
            "the published destination needs inspection"
        );
    }

    #[test]
    fn metadata_review_stays_failed_until_the_user_confirms_source_removal() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source.txt");
        let destination_directory = temporary.path().join("destination");
        fs::write(&source_path, b"contents").unwrap();
        fs::create_dir(&destination_directory).unwrap();
        let source = StorePath::from_unix_path(source_path.as_os_str());
        let target = StorePath::from_unix_path(destination_directory.as_os_str());
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let id = queue
            .submit_drop(
                FileDragPayload::new(vec![source.clone()], DropAction::Move).unwrap(),
                target,
            )
            .unwrap()[0];
        let destination = queue.operation_paths(id).unwrap()[1].clone();
        let _running = queue.start_ready().unwrap();

        queue
            .finish_metadata_review(
                id,
                Box::new(metadata_review(source.clone(), destination.clone())),
            )
            .unwrap();

        assert_eq!(queue.state(id), Some(JobState::Failed));
        assert!(queue.can_confirm_metadata_loss(id));
        assert_eq!(queue.operation_paths(id), Some(vec![source, destination]));

        queue.confirm_metadata_loss(id).unwrap();

        assert_eq!(queue.state(id), Some(JobState::Queued));
        assert_eq!(queue.start_ready().unwrap().len(), 1);
    }

    #[test]
    fn keeping_the_source_cancels_the_pending_removal() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("source.txt");
        let destination_directory = temporary.path().join("destination");
        fs::write(&source_path, b"contents").unwrap();
        fs::create_dir(&destination_directory).unwrap();
        let source = StorePath::from_unix_path(source_path.as_os_str());
        let target = StorePath::from_unix_path(destination_directory.as_os_str());
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let id = queue
            .submit_drop(
                FileDragPayload::new(vec![source.clone()], DropAction::Move).unwrap(),
                target,
            )
            .unwrap()[0];
        let destination = queue.operation_paths(id).unwrap()[1].clone();
        let _running = queue.start_ready().unwrap();
        queue
            .finish_metadata_review(id, Box::new(metadata_review(source, destination)))
            .unwrap();

        queue.keep_source_after_metadata_review(id).unwrap();

        assert_eq!(queue.state(id), Some(JobState::Cancelled));
        assert!(!queue.can_confirm_metadata_loss(id));
        assert!(queue.start_ready().unwrap().is_empty());
    }

    #[test]
    fn queued_create_rename_and_permanent_delete_mutate_real_files_safely() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let parent = StorePath::from_unix_path(temporary.path().as_os_str());
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());

        queue
            .submit_create(CreateRequest::new(
                parent.clone(),
                "folder".into(),
                musheen_ops::CreateKind::Directory,
            ))
            .expect("create is queued");
        assert_eq!(finish_one(&mut queue), LocalOperationOutcome::Mutation);
        let source_path = temporary.path().join("folder");
        assert!(source_path.is_dir());

        let source = StorePath::from_unix_path(source_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &source)
            .expect("identity lookup succeeds")
            .expect("created folder exists");
        queue
            .submit_rename(RenameRequest::new(
                source,
                "renamed".into(),
                identity.into_vec(),
            ))
            .expect("rename is queued");
        assert_eq!(finish_one(&mut queue), LocalOperationOutcome::Mutation);
        let renamed_path = temporary.path().join("renamed");
        assert!(renamed_path.is_dir());

        let renamed = StorePath::from_unix_path(renamed_path.as_os_str());
        let identity = MutationProvider::identity(&mut store, &renamed)
            .expect("identity lookup succeeds")
            .expect("renamed folder exists");
        let request = PermanentDeleteRequest::new(
            parent.clone(),
            vec![DeleteTarget::new(renamed, identity.into_vec())],
        )
        .expect("delete request is valid");
        let confirmation = request
            .challenge()
            .confirm(1, &parent, true)
            .expect("explicit confirmation is valid");
        queue
            .submit_permanent_delete(request, confirmation)
            .expect("delete is queued");
        assert_eq!(finish_one(&mut queue), LocalOperationOutcome::Mutation);
        assert!(!renamed_path.exists());
    }

    #[test]
    fn completed_rename_can_be_undone_only_while_its_target_is_unchanged() {
        let temporary = tempfile::tempdir().unwrap();
        let original_path = temporary.path().join("original");
        let renamed_path = temporary.path().join("renamed");
        fs::write(&original_path, b"original contents").unwrap();
        let original = StorePath::from_unix_path(original_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &original)
            .unwrap()
            .unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_rename(RenameRequest::new(
                original.clone(),
                "renamed".into(),
                identity.into_vec(),
            ))
            .unwrap();
        assert!(!queue.can_undo(job));
        assert_eq!(finish_one(&mut queue), LocalOperationOutcome::Mutation);
        assert!(queue.can_undo(job));
        assert!(queue.has_undo_candidate(job));

        let undo_job = queue.submit_undo(job).unwrap();
        assert!(!queue.can_undo(job));
        assert!(!queue.has_undo_candidate(job));
        assert_eq!(finish_one(&mut queue), LocalOperationOutcome::Mutation);
        assert_eq!(queue.state(undo_job), Some(JobState::Completed));
        assert_eq!(fs::read(&original_path).unwrap(), b"original contents");
        assert!(!renamed_path.exists());
    }

    #[test]
    fn rename_undo_disappears_after_replacement_or_source_reoccupation() {
        let temporary = tempfile::tempdir().unwrap();
        let original_path = temporary.path().join("original");
        let renamed_path = temporary.path().join("renamed");
        fs::write(&original_path, b"original contents").unwrap();
        let original = StorePath::from_unix_path(original_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &original)
            .unwrap()
            .unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_rename(RenameRequest::new(
                original,
                "renamed".into(),
                identity.into_vec(),
            ))
            .unwrap();
        finish_one(&mut queue);
        fs::write(&original_path, b"new occupant").unwrap();
        assert!(!queue.can_undo(job));
        fs::remove_file(&original_path).unwrap();
        fs::remove_file(&renamed_path).unwrap();
        fs::write(&renamed_path, b"replacement").unwrap();
        assert!(!queue.can_undo(job));
        assert!(queue.submit_undo(job).is_err());
        assert_eq!(fs::read(&renamed_path).unwrap(), b"replacement");
    }

    #[test]
    fn completed_local_move_can_be_undone_without_replacing_an_occupied_source() {
        let temporary = tempfile::tempdir().unwrap();
        let original_directory = temporary.path().join("original");
        let moved_directory = temporary.path().join("moved");
        fs::create_dir(&original_directory).unwrap();
        fs::create_dir(&moved_directory).unwrap();
        let original_path = original_directory.join("item.txt");
        let moved_path = moved_directory.join("item.txt");
        fs::write(&original_path, b"original contents").unwrap();
        let original = StorePath::from_unix_path(original_path.as_os_str());
        let destination_directory = StorePath::from_unix_path(moved_directory.as_os_str());
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_drop(
                FileDragPayload::new(vec![original], DropAction::Move).unwrap(),
                destination_directory,
            )
            .unwrap()[0];
        let _ = finish_one(&mut queue);
        assert!(!original_path.exists());
        assert_eq!(fs::read(&moved_path).unwrap(), b"original contents");
        assert!(queue.can_undo(job));

        fs::write(&original_path, b"new occupant").unwrap();
        assert!(!queue.can_undo(job));
        assert!(queue.submit_undo(job).is_err());
        assert_eq!(fs::read(&original_path).unwrap(), b"new occupant");
        fs::remove_file(&original_path).unwrap();

        let undo_job = queue.submit_undo(job).unwrap();
        finish_one(&mut queue);
        assert_eq!(queue.state(undo_job), Some(JobState::Completed));
        assert_eq!(fs::read(&original_path).unwrap(), b"original contents");
        assert!(!moved_path.exists());
    }

    #[test]
    fn completed_replacing_move_never_offers_undo() {
        let temporary = tempfile::tempdir().unwrap();
        let source_directory = temporary.path().join("source");
        let target_directory = temporary.path().join("target");
        fs::create_dir(&source_directory).unwrap();
        fs::create_dir(&target_directory).unwrap();
        let source_path = source_directory.join("item.txt");
        let target_path = target_directory.join("item.txt");
        fs::write(&source_path, b"new contents").unwrap();
        fs::write(&target_path, b"old contents").unwrap();
        let payload = FileDragPayload::new(
            vec![StorePath::from_unix_path(source_path.as_os_str())],
            DropAction::Move,
        )
        .unwrap();
        let target = StorePath::from_unix_path(target_directory.as_os_str());
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let conflict = queue.conflicts_for_drop(&payload, &target).unwrap();
        assert_eq!(conflict.len(), 1);
        let decision = musheen_ops::ConflictPolicies::default()
            .decide(
                &conflict[0],
                musheen_ops::ConflictChoice::Replace,
                musheen_ops::ApplyScope::ThisConflict,
                &mut AcceptDecisions,
            )
            .unwrap();
        let job = queue
            .submit_drop_resolved(payload, target, vec![decision])
            .unwrap()[0];
        finish_one(&mut queue);

        assert_eq!(fs::read(&target_path).unwrap(), b"new contents");
        assert!(!source_path.exists());
        assert!(!queue.has_undo_candidate(job));
        assert!(!queue.can_undo(job));
        assert!(matches!(
            queue.submit_undo(job),
            Err(DropError::UndoUnavailable(id)) if id == job
        ));
    }

    #[test]
    fn queued_move_undo_rejects_a_replaced_moved_item_at_execution() {
        let temporary = tempfile::tempdir().unwrap();
        let original_directory = temporary.path().join("original");
        let moved_directory = temporary.path().join("moved");
        fs::create_dir(&original_directory).unwrap();
        fs::create_dir(&moved_directory).unwrap();
        let original_path = original_directory.join("item.txt");
        let moved_path = moved_directory.join("item.txt");
        fs::write(&original_path, b"original contents").unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_drop(
                FileDragPayload::new(
                    vec![StorePath::from_unix_path(original_path.as_os_str())],
                    DropAction::Move,
                )
                .unwrap(),
                StorePath::from_unix_path(moved_directory.as_os_str()),
            )
            .unwrap()[0];
        finish_one(&mut queue);
        let undo_job = queue.submit_undo(job).unwrap();
        fs::remove_file(&moved_path).unwrap();
        fs::write(&moved_path, b"replacement").unwrap();

        let ready = queue.start_ready().unwrap().pop().unwrap();
        assert_eq!(ready.id(), undo_job);
        assert!(ready.execute_detailed().is_err());
        assert_eq!(fs::read(&moved_path).unwrap(), b"replacement");
        assert!(!original_path.exists());
    }

    #[test]
    fn completed_move_undo_is_unavailable_after_target_replacement() {
        let temporary = tempfile::tempdir().unwrap();
        let original_directory = temporary.path().join("original");
        let moved_directory = temporary.path().join("moved");
        fs::create_dir(&original_directory).unwrap();
        fs::create_dir(&moved_directory).unwrap();
        let original_path = original_directory.join("item.txt");
        let moved_path = moved_directory.join("item.txt");
        fs::write(&original_path, b"original contents").unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_drop(
                FileDragPayload::new(
                    vec![StorePath::from_unix_path(original_path.as_os_str())],
                    DropAction::Move,
                )
                .unwrap(),
                StorePath::from_unix_path(moved_directory.as_os_str()),
            )
            .unwrap()[0];
        finish_one(&mut queue);
        fs::remove_file(&moved_path).unwrap();
        fs::write(&moved_path, b"replacement").unwrap();

        assert!(!queue.can_undo(job));
        assert!(queue.submit_undo(job).is_err());
        assert_eq!(fs::read(&moved_path).unwrap(), b"replacement");
        assert!(!original_path.exists());
    }

    #[test]
    fn queued_move_undo_rejects_a_swapped_original_parent() {
        let temporary = tempfile::tempdir().unwrap();
        let original_directory = temporary.path().join("original");
        let moved_directory = temporary.path().join("moved");
        let other_directory = temporary.path().join("other");
        fs::create_dir(&original_directory).unwrap();
        fs::create_dir(&moved_directory).unwrap();
        fs::create_dir(&other_directory).unwrap();
        let original_path = original_directory.join("item.txt");
        let moved_path = moved_directory.join("item.txt");
        fs::write(&original_path, b"original contents").unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_drop(
                FileDragPayload::new(
                    vec![StorePath::from_unix_path(original_path.as_os_str())],
                    DropAction::Move,
                )
                .unwrap(),
                StorePath::from_unix_path(moved_directory.as_os_str()),
            )
            .unwrap()[0];
        finish_one(&mut queue);
        queue.submit_undo(job).unwrap();
        fs::rename(&original_directory, temporary.path().join("detached")).unwrap();
        std::os::unix::fs::symlink(&other_directory, &original_directory).unwrap();

        let ready = queue.start_ready().unwrap().pop().unwrap();
        assert!(ready.execute_detailed().is_err());
        assert_eq!(fs::read(moved_path).unwrap(), b"original contents");
        assert!(!other_directory.join("item.txt").exists());
    }

    #[test]
    fn completed_trash_can_be_undone_through_a_new_restore_job() {
        let temporary = tempfile::tempdir().unwrap();
        let original_path = temporary.path().join("discarded.txt");
        fs::write(&original_path, b"recoverable contents").unwrap();
        let original = StorePath::from_unix_path(original_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &original)
            .unwrap()
            .unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_trash(vec![DeleteTarget::new(original, identity.into_vec())])
            .unwrap()[0];
        let _ = finish_one(&mut queue);
        assert!(!original_path.exists());
        assert!(queue.can_undo(job));

        let undo_job = queue.submit_undo(job).unwrap();
        let _ = finish_one(&mut queue);
        assert_eq!(queue.state(undo_job), Some(JobState::Completed));
        assert_eq!(fs::read(&original_path).unwrap(), b"recoverable contents");
    }

    #[test]
    fn trash_undo_requires_an_empty_original_and_a_live_receipt() {
        let temporary = tempfile::tempdir().unwrap();
        let original_path = temporary.path().join("discarded.txt");
        fs::write(&original_path, b"recoverable contents").unwrap();
        let original = StorePath::from_unix_path(original_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &original)
            .unwrap()
            .unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_trash(vec![DeleteTarget::new(original, identity.into_vec())])
            .unwrap()[0];
        let LocalOperationOutcome::Trash(receipt) = finish_one(&mut queue) else {
            panic!("trash produces a receipt");
        };

        fs::write(&original_path, b"new occupant").unwrap();
        assert!(!queue.can_undo(job));
        assert!(queue.submit_undo(job).is_err());
        assert_eq!(fs::read(&original_path).unwrap(), b"new occupant");
        fs::remove_file(&original_path).unwrap();
        assert!(queue.can_undo(job));

        store.purge_trash(&[receipt]).unwrap();
        assert!(!queue.can_undo(job));
        assert!(queue.submit_undo(job).is_err());
        assert!(!original_path.exists());
    }

    #[test]
    fn queued_trash_undo_refuses_a_new_occupant() {
        let temporary = tempfile::tempdir().unwrap();
        let original_path = temporary.path().join("discarded.txt");
        fs::write(&original_path, b"recoverable contents").unwrap();
        let original = StorePath::from_unix_path(original_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &original)
            .unwrap()
            .unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_trash(vec![DeleteTarget::new(original, identity.into_vec())])
            .unwrap()[0];
        let LocalOperationOutcome::Trash(receipt) = finish_one(&mut queue) else {
            panic!("trash produces a receipt");
        };

        let undo_job = queue.submit_undo(job).unwrap();
        fs::write(&original_path, b"new occupant").unwrap();
        let operation = queue.start_ready().unwrap().pop().unwrap();
        assert_eq!(operation.id(), undo_job);
        assert!(operation.execute_detailed().is_err());
        queue
            .finish(undo_job, Err("restore conflict".into()))
            .unwrap();
        assert_eq!(queue.state(undo_job), Some(JobState::Failed));
        assert_eq!(fs::read(&original_path).unwrap(), b"new occupant");

        store.purge_trash(&[receipt]).unwrap();
    }

    #[test]
    fn trash_undo_restores_a_file_trashed_through_a_symlinked_parent() {
        let temporary = tempfile::tempdir().unwrap();
        let actual_directory = temporary.path().join("actual");
        let linked_directory = temporary.path().join("linked");
        fs::create_dir(&actual_directory).unwrap();
        std::os::unix::fs::symlink(&actual_directory, &linked_directory).unwrap();
        let actual_path = actual_directory.join("discarded.txt");
        let linked_path = linked_directory.join("discarded.txt");
        fs::write(&actual_path, b"recoverable contents").unwrap();

        let linked = StorePath::from_unix_path(linked_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &linked)
            .unwrap()
            .unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        let job = queue
            .submit_trash(vec![DeleteTarget::new(linked, identity.into_vec())])
            .unwrap()[0];
        let LocalOperationOutcome::Trash(receipt) = finish_one(&mut queue) else {
            panic!("trash produces a receipt");
        };
        assert!(!actual_path.exists());
        let available = queue.can_undo(job);
        if !available {
            store.purge_trash(&[receipt]).unwrap();
        }
        assert!(available);

        let undo_job = queue.submit_undo(job).unwrap();
        let _ = finish_one(&mut queue);
        assert_eq!(queue.state(undo_job), Some(JobState::Completed));
        assert_eq!(fs::read(&actual_path).unwrap(), b"recoverable contents");
    }

    #[test]
    fn create_and_rename_reject_invalid_names_before_queueing() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let parent = StorePath::from_unix_path(temporary.path().as_os_str());
        let source_path = temporary.path().join("source");
        fs::write(&source_path, b"source").unwrap();
        let source = StorePath::from_unix_path(source_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = MutationProvider::identity(&mut store, &source)
            .unwrap()
            .unwrap();
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());

        assert_eq!(
            queue.submit_create(CreateRequest::new(
                parent,
                "../outside".into(),
                musheen_ops::CreateKind::File,
            )),
            Err(DropError::Mutation(MutationError::InvalidName))
        );
        assert_eq!(
            queue.submit_rename(RenameRequest::new(
                source,
                "../outside".into(),
                identity.into_vec(),
            )),
            Err(DropError::Mutation(MutationError::InvalidName))
        );
        assert!(queue.start_ready().unwrap().is_empty());
    }

    #[test]
    fn archive_plan_runs_through_the_registered_queue_route() {
        let temporary = tempfile::tempdir().unwrap();
        let source = StorePath::from_unix_path(temporary.path().join("source.txt"));
        let destination = StorePath::from_unix_path(temporary.path().join("source.txt.zip"));
        fs::write(source.as_unix_path().unwrap(), b"archive me").unwrap();
        let plan = ArchiveOperationPlan::create(
            vec![source],
            destination.clone(),
            musheen_ops::ArchiveCodec::Zip,
            musheen_ops::ArchiveConflictPolicy::Fail,
            false,
        )
        .unwrap();
        let route = Arc::new(RecordingArchiveRoute::default());
        let mut queue = LocalOperationQueue::new(&ResourceLimits::default());
        queue.register_archive_route(route.clone());

        let id = queue.submit_archive(plan.clone()).unwrap();

        assert_eq!(finish_one(&mut queue), LocalOperationOutcome::Archive);
        assert_eq!(queue.state(id), Some(JobState::Completed));
        assert_eq!(
            *route.execution.lock().unwrap(),
            Some((id, EventGeneration::new(0), plan))
        );
        assert!(!destination.as_unix_path().unwrap().exists());
    }
}
