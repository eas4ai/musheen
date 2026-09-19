use crate::LocalStore;
use musheen_core::{ResourceLimits, Store, StorePath};
use musheen_ops::{
    CopyRequest, CopySession, EventGeneration, JobId, JobState, MetadataChange, MetadataPlan,
    MetadataScope, MutationError, MutationProvider, OperationKind, OperationPlan, ProviderLimits,
    ProviderSnapshot, Scheduler, SchedulerError, execute_move,
};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

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
}

impl FileDragPayload {
    pub fn new(sources: Vec<StorePath>, action: DropAction) -> Result<Self, DropError> {
        if sources.is_empty() {
            return Err(DropError::EmptySelection);
        }
        Ok(Self { sources, action })
    }

    #[must_use]
    pub fn sources(&self) -> &[StorePath] {
        &self.sources
    }

    #[must_use]
    pub const fn action(&self) -> DropAction {
        self.action
    }
}

#[derive(Clone, Debug)]
enum LocalOperation {
    Transfer {
        action: DropAction,
        source: StorePath,
        destination: StorePath,
    },
    Metadata(MetadataPlan),
}

#[derive(Debug)]
pub struct ReadyLocalOperation {
    id: JobId,
    generation: EventGeneration,
    cancellation: musheen_core::CancellationToken,
    operation: LocalOperation,
}

impl ReadyLocalOperation {
    #[must_use]
    pub const fn id(&self) -> JobId {
        self.id
    }

    pub fn execute(self) -> Result<(), Box<str>> {
        let mut store = LocalStore::new();
        match self.operation {
            LocalOperation::Transfer {
                action,
                source,
                destination,
            } => {
                let request = CopyRequest::new(self.id, self.generation, source, destination);
                match action {
                    DropAction::Copy => CopySession::default()
                        .execute(&mut store, &request, &self.cancellation)
                        .map(|_| ())
                        .map_err(|error| error.to_string().into()),
                    DropAction::Move => execute_move(&mut store, &request, &self.cancellation)
                        .map(|_| ())
                        .map_err(|error| error.to_string().into()),
                }
            }
            LocalOperation::Metadata(plan) => plan
                .execute(&mut store)
                .map_err(|error| error.to_string().into()),
        }
    }
}

#[derive(Debug)]
pub struct LocalOperationQueue {
    scheduler: Scheduler,
    operations: BTreeMap<JobId, LocalOperation>,
    failures: BTreeMap<JobId, Box<str>>,
}

impl LocalOperationQueue {
    #[must_use]
    pub fn new(limits: &ResourceLimits) -> Self {
        Self {
            scheduler: Scheduler::new(limits),
            operations: BTreeMap::new(),
            failures: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn can_accept(&self, payload: &FileDragPayload, target: &StorePath) -> bool {
        self.plan_drop(payload, target).is_ok()
    }

    pub fn submit_drop(
        &mut self,
        payload: FileDragPayload,
        target: StorePath,
    ) -> Result<Vec<JobId>, DropError> {
        let planned = self.plan_drop(&payload, &target)?;
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
        match result {
            Ok(()) => self.scheduler.complete(id)?,
            Err(error) => {
                self.scheduler.fail(id)?;
                self.failures.insert(id, error);
            }
        }
        self.operations.remove(&id);
        Ok(())
    }

    #[must_use]
    pub fn state(&self, id: JobId) -> Option<JobState> {
        self.scheduler.state(id)
    }

    #[must_use]
    pub fn job_count(&self) -> usize {
        self.operations.len()
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
    ) -> Result<Vec<(OperationPlan, LocalOperation)>, DropError> {
        let target_path = writable_directory(target)?;
        let store = LocalStore::new();
        let provider = provider_snapshot(&store, target);
        let mut sources = HashSet::with_capacity(payload.sources.len());
        let mut destinations = HashSet::with_capacity(payload.sources.len());
        let mut planned = Vec::with_capacity(payload.sources.len());

        for source in &payload.sources {
            let source_path = clean_absolute_path(source).ok_or_else(|| {
                DropError::InvalidSource(source.clone(), "not an absolute local path".into())
            })?;
            if !sources.insert(source_path.to_path_buf()) {
                return Err(DropError::DuplicateSource(source.clone()));
            }
            let metadata = fs::symlink_metadata(source_path).map_err(|error| {
                DropError::InvalidSource(source.clone(), error.to_string().into())
            })?;
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
            if destination_path.try_exists().map_err(|error| {
                DropError::Plan(format!("could not inspect destination: {error}").into())
            })? {
                return Err(DropError::DestinationExists(StorePath::from_unix_path(
                    destination_path.as_os_str(),
                )));
            }
            let destination = StorePath::from_unix_path(destination_path.as_os_str());
            let plan = OperationPlan::new(
                payload.action.operation_kind(),
                provider.clone(),
                Some(source.clone()),
                destination.clone(),
            )
            .map_err(|error| DropError::Plan(error.to_string().into()))?;
            planned.push((
                plan,
                LocalOperation::Transfer {
                    action: payload.action,
                    source: source.clone(),
                    destination,
                },
            ));
        }
        Ok(planned)
    }
}

fn provider_snapshot(store: &LocalStore, location: &StorePath) -> ProviderSnapshot {
    ProviderSnapshot::new(
        store.provider_id().clone(),
        store.capabilities(location),
        ProviderLimits::default(),
    )
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
    UnsupportedTarget(StorePath),
    InvalidSource(StorePath, Box<str>),
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
            Self::UnsupportedTarget(_) => formatter.write_str("the drop target is not writable"),
            Self::InvalidSource(_, reason) => write!(formatter, "invalid drop source: {reason}"),
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
