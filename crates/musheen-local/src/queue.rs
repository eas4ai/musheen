use crate::LocalStore;
use crate::mutation::{
    ResolvedTransferFailure, ResolvedTransferOutcome, execute_resolved_transfer,
};
use musheen_core::{CommandTargetRef, DisplayPath, ItemId, ResourceLimits, Store, StorePath};
use musheen_ops::{
    ConflictDecision, ConflictItemKind, ConflictRecord, CopyRequest, CopySession, EventGeneration,
    JobId, JobState, MetadataChange, MetadataPlan, MetadataScope, MutationError, MutationProvider,
    OperationFailure, OperationKind, OperationPlan, ProviderLimits, ProviderSnapshot,
    PublicationState, Scheduler, SchedulerError, SourceState, execute_move,
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
        provider_route: Option<Arc<dyn ProviderTransferRoute>>,
    },
    Metadata(MetadataPlan),
}

impl LocalOperation {
    const fn kind(&self) -> OperationKind {
        match self {
            Self::Transfer { action, .. } => action.operation_kind(),
            Self::Metadata(plan) => {
                if plan.requires_permissions() {
                    OperationKind::SetPermissions
                } else {
                    OperationKind::SetOwnership
                }
            }
        }
    }

    fn affected_path(&self) -> &StorePath {
        match self {
            Self::Transfer { source, .. } => source,
            Self::Metadata(plan) => plan.root(),
        }
    }
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LocalOperationOutcome {
    Transfer(TransferOutcome),
    Metadata,
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
                provider_route,
            } => {
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
                        ResolvedTransferOutcome::Completed(path) => {
                            transfer_completed(&store, path)
                        }
                    };
                }
                match action {
                    DropAction::Copy => {
                        CopySession::default()
                            .execute(&mut store, &request, &self.cancellation)
                            .map_err(LocalOperationFailure::from_transfer)?;
                    }
                    DropAction::Move => {
                        execute_move(&mut store, &request, &self.cancellation)
                            .map_err(LocalOperationFailure::from_transfer)?;
                    }
                };
                transfer_completed(&store, request.destination().clone())
            }
            LocalOperation::Metadata(plan) => plan
                .execute_controlled(&mut store, &self.cancellation)
                .map(|()| LocalOperationOutcome::Metadata)
                .map_err(|error| LocalOperationFailure::failed(error.to_string())),
        }
    }
}

fn transfer_completed(
    store: &LocalStore,
    path: StorePath,
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
    Ok(LocalOperationOutcome::Transfer(TransferOutcome::Completed(
        target,
    )))
}

#[derive(Debug)]
pub struct LocalOperationQueue {
    scheduler: Scheduler,
    operations: BTreeMap<JobId, LocalOperation>,
    failures: BTreeMap<JobId, Box<str>>,
    provider_routes: HashMap<
        (musheen_core::ProviderId, musheen_core::ProviderId),
        Arc<dyn ProviderTransferRoute>,
    >,
}

impl LocalOperationQueue {
    #[must_use]
    pub fn new(limits: &ResourceLimits) -> Self {
        Self {
            scheduler: Scheduler::new(limits),
            operations: BTreeMap::new(),
            failures: BTreeMap::new(),
            provider_routes: HashMap::new(),
        }
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
                self.operations.remove(&id);
            }
            Err(error) => {
                self.scheduler.fail(id)?;
                self.failures.insert(id, error);
            }
        }
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
            if kind.is_dir() && target_path.starts_with(source_path) {
                return Err(DropError::RecursiveTarget(source.clone()));
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
    let metadata =
        fs::symlink_metadata(path).map_err(|_| DropError::UnsupportedTarget(target.clone()))?;
    if !metadata.file_type().is_dir() || metadata.permissions().mode() & 0o222 == 0 {
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
}
