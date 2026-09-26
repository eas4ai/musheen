pub use musheen_local::{
    ArchiveOperationExecution, ArchiveOperationRoute, DropAction, DropError, FileDragPayload,
    LocalFailureDisposition, LocalOperationFailure, LocalOperationOutcome, LocalOperationQueue,
    LocalStore, ProviderTransferRoute, ReadyLocalOperation, TransferOutcome,
};

use crate::providers::ProviderRuntime;
use crate::{RecoveryAction, StatusCenterError, StatusCenterModel};
use gpui_kit::{AppContext, Context};
use musheen_core::{CommandTargetRef, ResourceLimits, StorePath};
use musheen_desktop::{
    ArchiveError, ArchiveOperationLimits, ArchivePassword, ArchivePasswordProvider,
    FileJournalStorage, PasswordRequest, StatusStore, execute_archive_plan,
};
use musheen_ops::{
    ArchiveOperationPlan, ConflictDecision, ConflictRecord, CreateRequest, DeleteTarget,
    HardLinkRequest, JobId, JobState, Journal, MetadataChange, MetadataScope, OperationKind,
    PermanentDeleteConfirmation, PermanentDeleteRequest, RenameRequest, SymbolicLinkRequest,
};
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

struct DesktopArchiveRoute {
    journal: Mutex<Journal<FileJournalStorage>>,
    limits: ArchiveOperationLimits,
}

pub(crate) struct NoArchivePasswords;

impl ArchivePasswordProvider for NoArchivePasswords {
    fn request_password(
        &self,
        _request: &PasswordRequest,
    ) -> Result<Option<ArchivePassword>, ArchiveError> {
        Ok(None)
    }
}

impl fmt::Debug for DesktopArchiveRoute {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DesktopArchiveRoute")
    }
}

impl DesktopArchiveRoute {
    fn for_current_user() -> Result<Self, Box<str>> {
        Self::with_storage(
            FileJournalStorage::for_current_user().map_err(|error| error.to_string())?,
        )
    }

    fn with_storage(storage: FileJournalStorage) -> Result<Self, Box<str>> {
        let journal = Journal::open(storage).map_err(|error| error.to_string())?;
        Ok(Self {
            journal: Mutex::new(journal),
            limits: ArchiveOperationLimits::default(),
        })
    }
}

impl ArchiveOperationRoute for DesktopArchiveRoute {
    fn execute_archive(&self, execution: ArchiveOperationExecution<'_>) -> Result<(), Box<str>> {
        let mut journal = self
            .journal
            .lock()
            .map_err(|_| Box::<str>::from("archive journal lock is unavailable"))?;
        execute_archive_plan(
            execution.plan(),
            &self.limits,
            &NoArchivePasswords,
            execution.cancellation(),
            &mut journal,
            execution.id(),
            execution.generation(),
        )
        .map(|_| ())
        .map_err(|error| error.to_string().into())
    }
}

#[derive(Debug)]
enum StatusPersistenceCommand {
    Snapshot {
        revision: u64,
        status: StatusCenterModel,
    },
    Flush(std::sync::mpsc::Sender<Result<(), Box<str>>>),
}

#[derive(Debug)]
struct StatusPersistence {
    sender: Option<std::sync::mpsc::Sender<StatusPersistenceCommand>>,
    worker: Option<JoinHandle<()>>,
}

impl StatusPersistence {
    fn spawn(
        store: StatusStore,
        persistence_error: Arc<Mutex<Option<Box<str>>>>,
        status_revision: Arc<AtomicU64>,
    ) -> Result<Self, OperationHubError> {
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = std::thread::Builder::new()
            .name("musheen-status-persistence".into())
            .spawn(move || {
                let mut last_result = Ok(());
                let mut persisted_revision = 0;
                while let Ok(command) = receiver.recv() {
                    let (mut revision, mut snapshot) = match command {
                        StatusPersistenceCommand::Snapshot { revision, status } => {
                            (revision, status)
                        }
                        StatusPersistenceCommand::Flush(acknowledgement) => {
                            let _ = acknowledgement.send(last_result.clone());
                            continue;
                        }
                    };
                    let mut flushes = Vec::new();
                    while let Ok(command) = receiver.try_recv() {
                        match command {
                            StatusPersistenceCommand::Snapshot {
                                revision: newer_revision,
                                status: newer,
                            } if newer_revision > revision => {
                                revision = newer_revision;
                                snapshot = newer;
                            }
                            StatusPersistenceCommand::Snapshot { .. } => {}
                            StatusPersistenceCommand::Flush(acknowledgement) => {
                                flushes.push(acknowledgement);
                            }
                        }
                    }
                    if revision > persisted_revision {
                        last_result = snapshot
                            .to_json()
                            .map_err(|error| error.to_string().into())
                            .and_then(|document| {
                                store
                                    .save(&document)
                                    .map_err(|error| error.to_string().into())
                            });
                        persisted_revision = revision;
                        if let Ok(mut error) = persistence_error.lock() {
                            *error = last_result.as_ref().err().cloned();
                        }
                        status_revision.fetch_add(1, Ordering::AcqRel);
                    }
                    for acknowledgement in flushes {
                        let _ = acknowledgement.send(last_result.clone());
                    }
                }
            })
            .map_err(|error| OperationHubError::Storage(error.to_string().into()))?;
        Ok(Self {
            sender: Some(sender),
            worker: Some(worker),
        })
    }

    fn persist(&self, revision: u64, status: StatusCenterModel) -> Result<(), Box<str>> {
        self.sender
            .as_ref()
            .ok_or_else(|| Box::<str>::from("the status persistence worker stopped"))?
            .send(StatusPersistenceCommand::Snapshot { revision, status })
            .map_err(|_| Box::<str>::from("the status persistence worker stopped"))
    }

    fn flush(&self) -> Result<(), Box<str>> {
        let (acknowledgement, result) = std::sync::mpsc::channel();
        self.sender
            .as_ref()
            .ok_or_else(|| Box::<str>::from("the status persistence worker stopped"))?
            .send(StatusPersistenceCommand::Flush(acknowledgement))
            .map_err(|_| Box::<str>::from("the status persistence worker stopped"))?;
        result
            .recv()
            .map_err(|_| Box::<str>::from("the status persistence worker stopped"))?
    }
}

impl Drop for StatusPersistence {
    fn drop(&mut self) {
        let _ = self.flush();
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) struct OperationMountReservation {
    queue: Arc<Mutex<LocalOperationQueue>>,
    reservations: Arc<Mutex<BTreeMap<u64, Vec<std::path::PathBuf>>>>,
    id: u64,
    mounts: Vec<std::path::PathBuf>,
}

impl OperationMountReservation {
    pub(crate) fn operations(&self) -> Vec<(JobId, OperationKind)> {
        let queue = self
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        operations_using_mounts(&queue, &self.mounts)
    }
}

impl Drop for OperationMountReservation {
    fn drop(&mut self) {
        self.reservations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.id);
    }
}

#[derive(Clone, Debug)]
pub struct OperationHub {
    queue: Arc<Mutex<LocalOperationQueue>>,
    status: Arc<Mutex<StatusCenterModel>>,
    persistence: Option<Arc<StatusPersistence>>,
    persistence_error: Arc<Mutex<Option<Box<str>>>>,
    archive_route_error: Arc<Mutex<Option<Box<str>>>>,
    status_revision: Arc<AtomicU64>,
    reservations: Arc<Mutex<BTreeMap<u64, Vec<std::path::PathBuf>>>>,
    next_reservation: Arc<AtomicU64>,
    /// Jobs the status center pruned, forgotten by the queue on its next run.
    pending_forget: Arc<Mutex<Vec<JobId>>>,
}

impl OperationHub {
    pub(crate) fn submit_custom_action(
        &self,
        context: crate::status_center::custom_actions::CustomActionContext,
    ) -> Result<u64, musheen_desktop::CustomActionError> {
        let id = self
            .status
            .lock()
            .map_err(|_| musheen_desktop::CustomActionError::InvalidDocument)?
            .register_custom_action(context)?;
        self.persist_status();
        Ok(id)
    }

    pub(crate) fn finish_custom_action(
        &self,
        id: u64,
        result: &Result<(), musheen_desktop::CustomActionError>,
    ) {
        if let Ok(mut status) = self.status.lock() {
            status.finish_custom_action(id, result);
        }
        self.persist_status();
    }

    pub(crate) fn reject_custom_action(
        &self,
        context: crate::status_center::custom_actions::CustomActionContext,
        error: &musheen_desktop::CustomActionError,
    ) {
        if let Ok(mut status) = self.status.lock() {
            status.reject_custom_action(context, error);
        }
        self.persist_status();
    }

    pub(crate) fn dismiss_custom_action(&self, id: u64) {
        if let Ok(mut status) = self.status.lock() {
            status.dismiss_custom_action(id);
        }
        self.persist_status();
    }
    #[must_use]
    pub fn new(limits: &ResourceLimits) -> Self {
        Self::new_with_provider_runtime(limits, &ProviderRuntime::for_current_user())
    }

    #[must_use]
    pub(crate) fn new_with_provider_runtime(
        limits: &ResourceLimits,
        providers: &ProviderRuntime,
    ) -> Self {
        let mut queue = LocalOperationQueue::new(limits);
        providers.configure_queue(&mut queue);
        Self {
            queue: Arc::new(Mutex::new(queue)),
            status: Arc::new(Mutex::new(StatusCenterModel::default())),
            persistence: None,
            persistence_error: Arc::new(Mutex::new(None)),
            archive_route_error: Arc::new(Mutex::new(None)),
            status_revision: Arc::new(AtomicU64::new(0)),
            reservations: Arc::new(Mutex::new(BTreeMap::new())),
            next_reservation: Arc::new(AtomicU64::new(1)),
            pending_forget: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_status_store(
        limits: &ResourceLimits,
        store: StatusStore,
    ) -> Result<Self, OperationHubError> {
        Self::with_status_store_and_provider_runtime(
            limits,
            store,
            &ProviderRuntime::for_current_user(),
        )
    }

    pub(crate) fn with_status_store_and_provider_runtime(
        limits: &ResourceLimits,
        store: StatusStore,
        providers: &ProviderRuntime,
    ) -> Result<Self, OperationHubError> {
        let mut status = load_status(&store)?;
        let interrupted = status.mark_unfinished_interrupted();
        let mut local_store = LocalStore::new();
        let recovery_error = recover_local_operation_artifacts(&status, &mut local_store);
        let recovery_reconciled = status.reconcile_recovery_staging(
            |path| local_store.recovery_staging_available(path),
            |_| false,
        );
        if interrupted || recovery_reconciled {
            let document = status.to_json()?;
            store
                .save(&document)
                .map_err(|error| OperationHubError::Storage(error.to_string().into()))?;
        }
        let mut queue = match status.highest_job_id() {
            Some(last_job_id) => LocalOperationQueue::starting_after(limits, last_job_id)?,
            None => LocalOperationQueue::new(limits),
        };
        providers.configure_queue(&mut queue);
        let status = Arc::new(Mutex::new(status));
        let persistence_error = Arc::new(Mutex::new(recovery_error));
        let status_revision = Arc::new(AtomicU64::new(0));
        let persistence = Arc::new(StatusPersistence::spawn(
            store,
            Arc::clone(&persistence_error),
            Arc::clone(&status_revision),
        )?);
        Ok(Self {
            queue: Arc::new(Mutex::new(queue)),
            status,
            persistence: Some(persistence),
            persistence_error,
            archive_route_error: Arc::new(Mutex::new(None)),
            status_revision,
            reservations: Arc::new(Mutex::new(BTreeMap::new())),
            next_reservation: Arc::new(AtomicU64::new(1)),
            pending_forget: Arc::new(Mutex::new(Vec::new())),
        })
    }

    #[must_use]
    pub fn for_current_user(limits: &ResourceLimits) -> Self {
        Self::for_current_user_with_provider_runtime(limits, &ProviderRuntime::for_current_user())
    }

    pub(crate) fn for_current_user_with_provider_runtime(
        limits: &ResourceLimits,
        providers: &ProviderRuntime,
    ) -> Self {
        let hub = match Self::with_status_store_and_provider_runtime(
            limits,
            StatusStore::for_current_user(),
            providers,
        ) {
            Ok(hub) => hub,
            Err(error) => {
                let hub = Self::new_with_provider_runtime(limits, providers);
                if let Ok(mut persistence_error) = hub.persistence_error.lock() {
                    *persistence_error = Some(error.to_string().into());
                }
                hub
            }
        };
        match DesktopArchiveRoute::for_current_user() {
            Ok(route) => {
                if let Ok(mut queue) = hub.queue.lock() {
                    queue.register_archive_route(Arc::new(route));
                }
            }
            Err(error) => {
                if let Ok(mut archive_route_error) = hub.archive_route_error.lock() {
                    *archive_route_error = Some(error);
                }
            }
        }
        hub
    }

    /// Runs archive jobs with their journal in `directory`, so a test never
    /// writes the user's archive journal.
    #[cfg(test)]
    pub(crate) fn with_archive_journal_at(self, directory: &std::path::Path) -> Self {
        let storage = FileJournalStorage::at(directory).expect("the test archive journal opens");
        let route = DesktopArchiveRoute::with_storage(storage).expect("the test archive route opens");
        self.queue
            .lock()
            .expect("the operation queue lock is available")
            .register_archive_route(Arc::new(route));
        self
    }

    #[must_use]
    pub fn status(&self) -> Arc<Mutex<StatusCenterModel>> {
        Arc::clone(&self.status)
    }

    #[must_use]
    pub fn persistence_error(&self) -> Option<Box<str>> {
        self.persistence_error
            .lock()
            .ok()
            .and_then(|error| error.clone())
    }

    #[must_use]
    pub fn status_revision(&self) -> u64 {
        self.status_revision.load(Ordering::Acquire)
    }

    pub(crate) fn operations_using_mounts(
        &self,
        mounts: &[std::path::PathBuf],
    ) -> Vec<(JobId, OperationKind)> {
        self.queue.lock().map_or_else(
            |_| Vec::new(),
            |queue| operations_using_mounts(&queue, mounts),
        )
    }

    pub(crate) fn reserve_mounts(
        &self,
        mounts: &[std::path::PathBuf],
    ) -> Result<OperationMountReservation, OperationHubError> {
        let id = self.next_reservation.fetch_add(1, Ordering::Relaxed);
        self.reservations
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?
            .insert(id, mounts.to_vec());
        Ok(OperationMountReservation {
            queue: Arc::clone(&self.queue),
            reservations: Arc::clone(&self.reservations),
            id,
            mounts: mounts.to_vec(),
        })
    }

    pub fn can_accept_drop(&self, payload: &FileDragPayload, target: &StorePath) -> bool {
        self.queue
            .lock()
            .is_ok_and(|queue| queue.can_accept(payload, target))
    }

    pub fn submit_drop(
        &self,
        payload: FileDragPayload,
        target: StorePath,
    ) -> Result<Vec<JobId>, OperationHubError> {
        self.submit_drop_with_decisions(payload, target, Vec::new())
    }

    pub fn conflicts_for_drop(
        &self,
        payload: &FileDragPayload,
        target: &StorePath,
    ) -> Result<Vec<ConflictRecord>, OperationHubError> {
        self.queue
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?
            .conflicts_for_drop(payload, target)
            .map_err(Into::into)
    }

    pub fn submit_drop_resolved(
        &self,
        payload: FileDragPayload,
        target: StorePath,
        decisions: Vec<ConflictDecision>,
    ) -> Result<Vec<JobId>, OperationHubError> {
        self.submit_drop_with_decisions(payload, target, decisions)
    }

    fn submit_drop_with_decisions(
        &self,
        payload: FileDragPayload,
        target: StorePath,
        decisions: Vec<ConflictDecision>,
    ) -> Result<Vec<JobId>, OperationHubError> {
        let kind = match payload.action() {
            DropAction::Copy => OperationKind::Copy,
            DropAction::Move => OperationKind::Move,
        };
        let sources = payload.sources().to_vec();
        let submitted_target = target.clone();
        let submissions =
            self.with_unreserved_queue(sources.iter().chain(std::iter::once(&target)), |queue| {
                queue
                    .submit_drop_resolved(payload, submitted_target, decisions)?
                    .into_iter()
                    .map(|id| {
                        let destination = queue
                            .operation_paths(id)
                            .and_then(|paths| paths.into_iter().nth(1))
                            .ok_or(DropError::MissingOperation(id))?;
                        Ok((id, destination))
                    })
                    .collect::<Result<Vec<_>, DropError>>()
            })?;
        let mut status = self
            .status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?;
        for (id, location) in submissions.iter().cloned() {
            status.register(
                id,
                musheen_ops::EventGeneration::new(0),
                kind,
                location,
                Some(1),
            )?;
        }
        drop(status);
        self.persist_status();
        Ok(submissions.into_iter().map(|(id, _)| id).collect())
    }

    pub fn submit_metadata_changes(
        &self,
        roots: Vec<StorePath>,
        scope: MetadataScope,
        change: MetadataChange,
    ) -> Result<Vec<JobId>, OperationHubError> {
        let locations = roots.clone();
        let kind = if change.requires_permissions() {
            OperationKind::SetPermissions
        } else {
            OperationKind::SetOwnership
        };
        let submitted_roots = roots.clone();
        let ids = self.with_unreserved_queue(roots.iter(), |queue| {
            queue.submit_metadata_changes(submitted_roots, scope, change)
        })?;
        let mut status = self
            .status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?;
        for (id, location) in ids.iter().copied().zip(locations) {
            status.register(
                id,
                musheen_ops::EventGeneration::new(0),
                kind,
                location,
                Some(1),
            )?;
        }
        drop(status);
        self.persist_status();
        Ok(ids)
    }

    pub fn submit_create(&self, request: CreateRequest) -> Result<JobId, OperationHubError> {
        let location = request.parent().clone();
        let kind = match request.kind() {
            musheen_ops::CreateKind::File => OperationKind::CreateFile,
            musheen_ops::CreateKind::Directory => OperationKind::CreateDirectory,
        };
        let id = self.with_unreserved_queue([&location], |queue| queue.submit_create(request))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                id,
                musheen_ops::EventGeneration::new(0),
                kind,
                location,
                Some(1),
            )?;
        self.persist_status();
        Ok(id)
    }

    pub fn submit_symbolic_link(
        &self,
        request: SymbolicLinkRequest,
        source: CommandTargetRef,
    ) -> Result<JobId, OperationHubError> {
        let location = source.path().clone();
        let id = self.with_unreserved_queue([&location], |queue| {
            queue.submit_symbolic_link(request, source)
        })?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                id,
                musheen_ops::EventGeneration::new(0),
                OperationKind::SymbolicLink,
                location,
                Some(1),
            )?;
        self.persist_status();
        Ok(id)
    }

    pub fn submit_duplicate(&self, target: CommandTargetRef) -> Result<JobId, OperationHubError> {
        let source = target.path().clone();
        let (id, destination) = self.with_unreserved_queue([&source], |queue| {
            let id = queue.submit_duplicate(target)?;
            let destination = queue
                .operation_paths(id)
                .and_then(|paths| paths.into_iter().nth(1))
                .ok_or(DropError::MissingOperation(id))?;
            Ok((id, destination))
        })?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                id,
                musheen_ops::EventGeneration::new(0),
                OperationKind::Copy,
                destination,
                Some(1),
            )?;
        self.persist_status();
        Ok(id)
    }

    pub fn submit_hard_link(
        &self,
        request: HardLinkRequest,
        source: CommandTargetRef,
    ) -> Result<JobId, OperationHubError> {
        let location = source.path().clone();
        let id = self
            .with_unreserved_queue([&location], |queue| queue.submit_hard_link(request, source))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                id,
                musheen_ops::EventGeneration::new(0),
                OperationKind::HardLink,
                location,
                Some(1),
            )?;
        self.persist_status();
        Ok(id)
    }

    pub fn submit_archive(&self, plan: ArchiveOperationPlan) -> Result<JobId, OperationHubError> {
        if let Some(error) = self
            .archive_route_error
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?
            .clone()
        {
            return Err(OperationHubError::Storage(error));
        }
        let kind = plan.kind();
        let location = plan.destination().clone();
        let paths = plan
            .sources()
            .iter()
            .cloned()
            .chain(std::iter::once(plan.destination().clone()))
            .collect::<Vec<_>>();
        let id = self.with_unreserved_queue(paths.iter(), |queue| queue.submit_archive(plan))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                id,
                musheen_ops::EventGeneration::new(0),
                kind,
                location,
                Some(1),
            )?;
        self.persist_status();
        Ok(id)
    }

    pub fn submit_rename(&self, request: RenameRequest) -> Result<JobId, OperationHubError> {
        let location = request.source().clone();
        let id = self.with_unreserved_queue([&location], |queue| queue.submit_rename(request))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                id,
                musheen_ops::EventGeneration::new(0),
                OperationKind::Rename,
                location,
                Some(1),
            )?;
        self.persist_status();
        Ok(id)
    }

    #[must_use]
    pub fn has_undo_candidate(&self, id: JobId) -> bool {
        let Ok(reserved) = self.reservations.lock() else {
            return false;
        };
        let Ok(queue) = self.queue.lock() else {
            return false;
        };
        queue.has_undo_candidate(id)
            && queue.undo_paths(id).is_some_and(|paths| {
                !operation_paths_are_reserved(reserved.values().flatten(), &paths)
            })
    }

    #[must_use]
    pub fn can_undo(&self, id: JobId) -> bool {
        let Ok(reserved) = self.reservations.lock() else {
            return false;
        };
        let Ok(queue) = self.queue.lock() else {
            return false;
        };
        queue.can_undo(id)
            && queue.undo_paths(id).is_some_and(|paths| {
                !operation_paths_are_reserved(reserved.values().flatten(), &paths)
            })
    }

    pub fn submit_undo(&self, id: JobId) -> Result<JobId, OperationHubError> {
        let (paths, kind) = {
            let queue = self
                .queue
                .lock()
                .map_err(|_| OperationHubError::QueueLock)?;
            (
                queue.undo_paths(id).ok_or(DropError::UndoUnavailable(id))?,
                queue.undo_kind(id).ok_or(DropError::UndoUnavailable(id))?,
            )
        };
        let location = paths[0].clone();
        let undo_id = self.with_unreserved_queue(paths.iter(), |queue| queue.submit_undo(id))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                undo_id,
                musheen_ops::EventGeneration::new(0),
                kind,
                location,
                Some(1),
            )?;
        self.persist_status();
        Ok(undo_id)
    }

    pub fn submit_trash(
        &self,
        targets: Vec<DeleteTarget>,
    ) -> Result<Vec<JobId>, OperationHubError> {
        let locations = targets
            .iter()
            .map(|target| target.path().clone())
            .collect::<Vec<_>>();
        let ids =
            self.with_unreserved_queue(locations.iter(), |queue| queue.submit_trash(targets))?;
        let mut status = self
            .status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?;
        for (id, location) in ids.iter().copied().zip(locations) {
            status.register(
                id,
                musheen_ops::EventGeneration::new(0),
                OperationKind::Trash,
                location,
                Some(1),
            )?;
        }
        drop(status);
        self.persist_status();
        Ok(ids)
    }

    pub fn submit_permanent_delete(
        &self,
        request: PermanentDeleteRequest,
        confirmation: PermanentDeleteConfirmation,
    ) -> Result<JobId, OperationHubError> {
        let location = request.location().clone();
        let item_count = request.targets().len() as u64;
        let paths = request
            .targets()
            .iter()
            .map(|target| target.path().clone())
            .collect::<Vec<_>>();
        let id = self.with_unreserved_queue(paths.iter(), |queue| {
            queue.submit_permanent_delete(request, confirmation)
        })?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .register(
                id,
                musheen_ops::EventGeneration::new(0),
                OperationKind::PermanentDelete,
                location,
                Some(item_count),
            )?;
        self.persist_status();
        Ok(id)
    }

    pub fn pause(&self, id: JobId) -> Result<(), OperationHubError> {
        self.queue
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?
            .pause(id)?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .mark_paused(id)?;
        self.persist_status();
        Ok(())
    }

    pub fn resume(&self, id: JobId) -> Result<(), OperationHubError> {
        self.queue
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?
            .resume(id)?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .mark_resumed(id)?;
        self.persist_status();
        Ok(())
    }

    #[must_use]
    pub fn can_confirm_metadata_loss(&self, id: JobId) -> bool {
        self.job_is_unreserved(id)
            && self
                .queue
                .lock()
                .is_ok_and(|queue| queue.can_confirm_metadata_loss(id))
    }

    pub fn confirm_metadata_loss(&self, id: JobId) -> Result<(), OperationHubError> {
        let generation = self.with_unreserved_job(id, |queue| queue.confirm_metadata_loss(id))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .mark_metadata_review_pending(id, generation)?;
        self.persist_status();
        Ok(())
    }

    pub fn keep_source_after_metadata_review(&self, id: JobId) -> Result<(), OperationHubError> {
        self.with_unreserved_job(id, |queue| queue.keep_source_after_metadata_review(id))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .acknowledge_metadata_review_keep_source(id)?;
        self.persist_status();
        Ok(())
    }

    pub fn cancel(&self, id: JobId) -> Result<(), OperationHubError> {
        let state = {
            let mut queue = self
                .queue
                .lock()
                .map_err(|_| OperationHubError::QueueLock)?;
            queue.cancel(id)?
        };
        if state == JobState::Cancelled {
            self.status
                .lock()
                .map_err(|_| OperationHubError::StatusLock)?
                .mark_cancelled(id)?;
        }
        self.persist_status();
        Ok(())
    }

    pub fn retry(&self, id: JobId) -> Result<(), OperationHubError> {
        let generation = self.with_unreserved_job(id, |queue| queue.retry(id))?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .mark_retry_pending(id, generation)?;
        self.persist_status();
        Ok(())
    }

    #[must_use]
    pub fn can_retry(&self, id: JobId) -> bool {
        self.job_is_unreserved(id)
    }

    #[must_use]
    pub fn can_resume_recovery(&self, id: JobId) -> bool {
        self.can_retry(id)
            && self
                .recovery_staging(id)
                .is_ok_and(|staging| LocalStore::new().recovery_staging_available(&staging))
    }

    #[must_use]
    pub fn can_discard_recovery(&self, id: JobId) -> bool {
        self.recovery_staging(id).is_ok_and(|staging| {
            self.recovery_is_unreserved(id, &staging)
                && LocalStore::new().recovery_staging_available(&staging)
        })
    }

    pub fn resume_recovery(&self, id: JobId) -> Result<(), OperationHubError> {
        let staging = self.recovery_staging(id)?;
        let generation = self.with_unreserved_job(id, |queue| {
            if !queue.can_retry(id) {
                return Err(DropError::MissingOperation(id));
            }
            LocalStore::new().discard_recovery_staging(&staging)?;
            queue.retry(id)
        })?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .mark_recovery_retry_pending(id, generation)?;
        self.persist_status();
        Ok(())
    }

    pub fn discard_recovery(&self, id: JobId) -> Result<(), OperationHubError> {
        let staging = self.recovery_staging(id)?;
        let retry_available = self.can_retry(id);
        self.with_unreserved_recovery(id, &staging, || {
            LocalStore::new().discard_recovery_staging(&staging)?;
            Ok(())
        })?;
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .mark_staging_discarded(id, retry_available)?;
        self.persist_status();
        Ok(())
    }

    pub fn dismiss(&self, id: JobId) -> Result<(), OperationHubError> {
        self.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .dismiss(id)?;
        self.persist_status();
        Ok(())
    }

    fn persist_status(&self) {
        let revision = self
            .status_revision
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let (snapshot, pruned) = match self.status.lock() {
            Ok(mut status) => {
                let pruned = status.take_pruned();
                (self.persistence.as_ref().map(|_| status.clone()), pruned)
            }
            Err(_) => {
                if let Ok(mut error) = self.persistence_error.lock() {
                    *error = Some("the status center lock is poisoned".into());
                }
                return;
            }
        };
        if !pruned.is_empty()
            && let Ok(mut pending) = self.pending_forget.lock()
        {
            pending.extend(pruned);
        }
        let (Some(persistence), Some(snapshot)) = (self.persistence.as_ref(), snapshot) else {
            return;
        };
        if let Err(error) = persistence.persist(revision, snapshot)
            && let Ok(mut persistence_error) = self.persistence_error.lock()
        {
            *persistence_error = Some(error);
        }
    }

    #[cfg(test)]
    fn flush_status_persistence(&self) -> Result<(), Box<str>> {
        self.persistence
            .as_ref()
            .map_or(Ok(()), |persistence| persistence.flush())
    }

    fn with_unreserved_queue<'a, T>(
        &self,
        paths: impl IntoIterator<Item = &'a StorePath>,
        submit: impl FnOnce(&mut LocalOperationQueue) -> Result<T, DropError>,
    ) -> Result<T, OperationHubError> {
        let paths = paths.into_iter().collect::<Vec<_>>();
        let reserved = self
            .reservations
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?;
        if reserved.values().flatten().any(|mount| {
            paths.iter().any(|path| {
                path.as_unix_path()
                    .is_some_and(|path| path.starts_with(mount))
            })
        }) {
            return Err(OperationHubError::MountReserved);
        }
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?;
        submit(&mut queue).map_err(Into::into)
    }

    fn with_unreserved_job<T>(
        &self,
        id: JobId,
        transition: impl FnOnce(&mut LocalOperationQueue) -> Result<T, DropError>,
    ) -> Result<T, OperationHubError> {
        let reserved = self
            .reservations
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?;
        let mut queue = self
            .queue
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?;
        let paths = queue
            .operation_paths(id)
            .ok_or(DropError::MissingOperation(id))?;
        if operation_paths_are_reserved(reserved.values().flatten(), &paths) {
            return Err(OperationHubError::MountReserved);
        }
        transition(&mut queue).map_err(Into::into)
    }

    fn job_is_unreserved(&self, id: JobId) -> bool {
        let Ok(reserved) = self.reservations.lock() else {
            return false;
        };
        let Ok(queue) = self.queue.lock() else {
            return false;
        };
        queue.can_retry(id)
            && queue.operation_paths(id).is_some_and(|paths| {
                !operation_paths_are_reserved(reserved.values().flatten(), &paths)
            })
    }

    fn recovery_is_unreserved(&self, id: JobId, staging: &StorePath) -> bool {
        let Ok(reserved) = self.reservations.lock() else {
            return false;
        };
        let Ok(queue) = self.queue.lock() else {
            return false;
        };
        let mut paths = queue.operation_paths(id).unwrap_or_default();
        paths.push(staging.clone());
        !operation_paths_are_reserved(reserved.values().flatten(), &paths)
    }

    fn with_unreserved_recovery<T>(
        &self,
        id: JobId,
        staging: &StorePath,
        transition: impl FnOnce() -> Result<T, DropError>,
    ) -> Result<T, OperationHubError> {
        let reserved = self
            .reservations
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?;
        let mut paths = self
            .queue
            .lock()
            .map_err(|_| OperationHubError::QueueLock)?
            .operation_paths(id)
            .unwrap_or_default();
        paths.push(staging.clone());
        if operation_paths_are_reserved(reserved.values().flatten(), &paths) {
            return Err(OperationHubError::MountReserved);
        }
        transition().map_err(Into::into)
    }

    fn recovery_staging(&self, id: JobId) -> Result<StorePath, OperationHubError> {
        let status = self
            .status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?;
        let entry = status
            .entry(id)
            .ok_or(crate::StatusCenterError::UnknownJob(id))?;
        entry
            .failures()
            .iter()
            .find_map(|failure| failure.recovery_staging().cloned())
            .ok_or_else(|| crate::StatusCenterError::InvalidState(id).into())
    }
}

fn operation_paths_are_reserved<'a>(
    mounts: impl IntoIterator<Item = &'a std::path::PathBuf>,
    paths: &[StorePath],
) -> bool {
    mounts.into_iter().any(|mount| {
        paths.iter().any(|path| {
            path.as_unix_path()
                .is_some_and(|path| path.starts_with(mount))
        })
    })
}

fn operations_using_mounts(
    queue: &LocalOperationQueue,
    mounts: &[std::path::PathBuf],
) -> Vec<(JobId, OperationKind)> {
    queue
        .active_operation_paths()
        .into_iter()
        .filter(|operation| {
            operation.paths.iter().any(|path| {
                path.as_unix_path()
                    .is_some_and(|path| mounts.iter().any(|mount| path.starts_with(mount)))
            })
        })
        .map(|operation| (operation.id, operation.kind))
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OperationHubError {
    QueueLock,
    StatusLock,
    Queue(DropError),
    Status(crate::StatusCenterError),
    Mutation(musheen_ops::MutationError),
    Storage(Box<str>),
    MountReserved,
}

impl fmt::Display for OperationHubError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::QueueLock => formatter.write_str("the operation queue lock is poisoned"),
            Self::StatusLock => formatter.write_str("the status center lock is poisoned"),
            Self::Queue(error) => error.fmt(formatter),
            Self::Status(error) => error.fmt(formatter),
            Self::Mutation(error) => error.fmt(formatter),
            Self::Storage(error) => formatter.write_str(error),
            Self::MountReserved => formatter.write_str("the destination volume is reserved"),
        }
    }
}

impl Error for OperationHubError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Queue(error) => Some(error),
            Self::Status(error) => Some(error),
            Self::Mutation(error) => Some(error),
            Self::QueueLock | Self::StatusLock | Self::Storage(_) | Self::MountReserved => None,
        }
    }
}

impl From<DropError> for OperationHubError {
    fn from(error: DropError) -> Self {
        Self::Queue(error)
    }
}

impl From<crate::StatusCenterError> for OperationHubError {
    fn from(error: crate::StatusCenterError) -> Self {
        Self::Status(error)
    }
}

impl From<musheen_ops::MutationError> for OperationHubError {
    fn from(error: musheen_ops::MutationError) -> Self {
        Self::Mutation(error)
    }
}

pub(crate) fn spawn_ready_hub_operations<V>(
    hub: OperationHub,
    cx: &mut Context<V>,
    on_finish: impl Fn(&mut V, JobId, Option<LocalOperationOutcome>, Option<Box<str>>, &mut Context<V>)
    + Clone
    + 'static,
) -> Result<(), OperationHubError>
where
    V: 'static,
{
    let ready = {
        let mut queue = hub.queue.lock().map_err(|_| OperationHubError::QueueLock)?;
        // The forget list is taken only once the queue is held, so a queue
        // lock failure leaves it for the next spawn.
        let forgotten = hub
            .pending_forget
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for id in forgotten {
            queue.forget(id);
        }
        queue.start_ready()?
    };

    for operation in ready {
        let id = operation.id();
        let affected_path = operation.affected_path().clone();
        hub.status
            .lock()
            .map_err(|_| OperationHubError::StatusLock)?
            .mark_running(id)?;
        hub.persist_status();
        let hub = hub.clone();
        let on_finish = on_finish.clone();
        let queue_hub = hub.clone();
        let work = cx.background_spawn(async move {
            let result = operation.execute_detailed();
            // The queue's bookkeeping, including the stats that remember an
            // undo candidate, stays off the UI thread.
            let finish = queue_hub
                .queue
                .lock()
                .map_err(|_| OperationHubError::QueueLock)
                .and_then(|mut queue| {
                    let state = match result.as_ref() {
                        Ok(LocalOperationOutcome::Transfer(TransferOutcome::MetadataReview {
                            review,
                            ..
                        })) => queue.finish_metadata_review(id, review.clone())?,
                        Ok(outcome) => queue.finish_with_outcome(id, outcome.clone())?,
                        Err(error) => queue.finish(id, Err(error.message().to_owned().into()))?,
                    };
                    Ok(Some(state))
                });
            (result, finish)
        });
        cx.spawn(async move |this, cx| {
            let (result, finish) = work.await;
            let outcome = result.as_ref().ok().cloned();
            let failure = result.as_ref().err().cloned();
            let status_result = hub
                .status
                .lock()
                .map_err(|_| OperationHubError::StatusLock)
                .and_then(|mut status| {
                    record_finished_operation(
                        &mut status,
                        id,
                        finish.as_ref().ok().copied().flatten(),
                        affected_path,
                        outcome.as_ref(),
                        failure.as_ref(),
                    )
                    .map_err(Into::into)
                });
            hub.persist_status();
            let error = finish
                .err()
                .or_else(|| status_result.err())
                .map(|error| error.to_string().into())
                .or_else(|| failure.map(|failure| failure.message().to_owned().into()))
                .or_else(|| hub.persistence_error());
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                on_finish(state, id, outcome, error, cx);
            });
        })
        .detach();
    }
    Ok(())
}

fn record_finished_operation(
    status: &mut StatusCenterModel,
    id: JobId,
    state: Option<JobState>,
    affected_path: StorePath,
    outcome: Option<&LocalOperationOutcome>,
    failure: Option<&LocalOperationFailure>,
) -> Result<(), StatusCenterError> {
    if let Some(LocalOperationOutcome::Transfer(TransferOutcome::MetadataReview {
        review, ..
    })) = outcome
    {
        status.record_failure(
            id,
            review.source().clone(),
            metadata_review_message(review.metadata()),
            [RecoveryAction::ViewLocation],
        )?;
        return status.mark_needs_attention(id);
    }
    match state {
        Some(JobState::Cancelled) => status.mark_cancelled(id),
        Some(JobState::Completed) => {
            status.record_item_success(id)?;
            status.complete(id)
        }
        Some(JobState::Failed) => record_failed_operation(status, id, affected_path, failure),
        _ => Ok(()),
    }
}

fn metadata_review_message(report: &musheen_ops::MetadataReport) -> Box<str> {
    let skipped = report
        .skipped()
        .iter()
        .map(|kind| match kind {
            musheen_ops::MetadataKind::Timestamps => "timestamps",
            musheen_ops::MetadataKind::Mode => "permissions",
            musheen_ops::MetadataKind::Ownership => "ownership",
            musheen_ops::MetadataKind::ExtendedAttributes => "extended attributes",
            musheen_ops::MetadataKind::AccessControlList => "access control lists",
            musheen_ops::MetadataKind::SparseLayout => "sparse layout",
            musheen_ops::MetadataKind::HardLinkRelationship => "hard-link relationships",
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "the destination was copied, but the source was kept because these metadata could not be preserved: {skipped}"
    )
    .into()
}

fn record_failed_operation(
    status: &mut StatusCenterModel,
    id: JobId,
    affected_path: StorePath,
    failure: Option<&LocalOperationFailure>,
) -> Result<(), StatusCenterError> {
    let mut disposition = failure
        .map(LocalOperationFailure::disposition)
        .unwrap_or(LocalFailureDisposition::Failed);
    let recovery_staging = failure
        .and_then(LocalOperationFailure::recovery_staging)
        .cloned();
    if disposition == LocalFailureDisposition::Recoverable && recovery_staging.is_none() {
        disposition = LocalFailureDisposition::NeedsAttention;
    }
    let message = failure
        .map(LocalOperationFailure::message)
        .unwrap_or("operation failed");
    if disposition == LocalFailureDisposition::Recoverable {
        let staging = recovery_staging.ok_or(StatusCenterError::InvalidRecoveryStaging(id))?;
        status.record_recoverable_failure(id, affected_path, staging, message)?;
    } else {
        status.record_failure(
            id,
            affected_path,
            message,
            recovery_actions(disposition).iter().copied(),
        )?;
    }
    match disposition {
        LocalFailureDisposition::Failed => status.complete(id),
        LocalFailureDisposition::Recoverable => status.mark_recoverable(id),
        LocalFailureDisposition::NeedsAttention => status.mark_needs_attention(id),
    }
}

const fn recovery_actions(disposition: LocalFailureDisposition) -> &'static [RecoveryAction] {
    match disposition {
        LocalFailureDisposition::Failed => {
            &[RecoveryAction::RetryFailed, RecoveryAction::ViewLocation]
        }
        LocalFailureDisposition::Recoverable => &[
            RecoveryAction::Resume,
            RecoveryAction::DiscardStaging,
            RecoveryAction::ViewLocation,
        ],
        LocalFailureDisposition::NeedsAttention => &[RecoveryAction::ViewLocation],
    }
}

fn load_status(store: &StatusStore) -> Result<StatusCenterModel, OperationHubError> {
    let primary = store
        .load()
        .map_err(|error| OperationHubError::Storage(error.to_string().into()))?;
    let Some(primary) = primary else {
        return Ok(StatusCenterModel::default());
    };
    match StatusCenterModel::from_json(&primary) {
        Ok(status) => Ok(status),
        Err(primary_error) => {
            let backup = store
                .load_backup()
                .map_err(|error| OperationHubError::Storage(error.to_string().into()))?;
            match backup {
                Some(backup) => StatusCenterModel::from_json(&backup).map_err(Into::into),
                None => Err(primary_error.into()),
            }
        }
    }
}

fn recover_local_operation_artifacts(
    status: &StatusCenterModel,
    local_store: &mut LocalStore,
) -> Option<Box<str>> {
    let protected_staging = status
        .history()
        .into_iter()
        .flat_map(|entry| entry.failures())
        .filter_map(|failure| failure.recovery_staging().cloned())
        .collect::<Vec<_>>();
    let recovery_locations = status
        .history()
        .into_iter()
        .map(|entry| entry.location().clone())
        .collect::<Vec<_>>();
    let mut recovered_directories = BTreeSet::new();
    let mut recovery_error = None;
    for location in recovery_locations {
        let Some(parent) = location.as_unix_path().and_then(std::path::Path::parent) else {
            continue;
        };
        if !recovered_directories.insert(parent.to_path_buf()) {
            continue;
        }
        if let Err(error) = local_store.recover_replacements_at(&location) {
            recovery_error.get_or_insert_with(|| error.to_string().into());
            continue;
        }
        if let Err(error) = local_store.cleanup_stale_staging_at(&location, &protected_staging) {
            recovery_error.get_or_insert_with(|| error.to_string().into());
        }
    }
    recovery_error
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OperationStatus;
    use musheen_ops::{EventGeneration, StagingPath};
    use standard_library::fs as filesystem;
    use std as standard_library;
    use std::sync::mpsc;
    use std::time::Duration;

    fn recoverable_hub() -> (tempfile::TempDir, OperationHub, JobId, StorePath) {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let hub = OperationHub::new(&ResourceLimits::default());
        let (id, staging) = recoverable_copy(&hub, temporary.path(), "source");
        (temporary, hub, id, staging)
    }

    fn recoverable_copy(
        hub: &OperationHub,
        root: &std::path::Path,
        name: &str,
    ) -> (JobId, StorePath) {
        let source = root.join(format!("{name}.txt"));
        let target = root.join(format!("{name}-target"));
        filesystem::write(&source, b"source").unwrap();
        filesystem::create_dir(&target).unwrap();
        let source = StorePath::from_unix_path(source.into_os_string());
        let target = StorePath::from_unix_path(target.into_os_string());
        let id = hub
            .submit_drop(
                FileDragPayload::new(vec![source.clone()], DropAction::Copy).unwrap(),
                target.clone(),
            )
            .unwrap()[0];
        let destination = StorePath::from_unix_path(
            target
                .as_unix_path()
                .unwrap()
                .join(format!("{name}.txt"))
                .into_os_string(),
        );
        let staging = StagingPath::for_destination(&destination, id, EventGeneration::new(0))
            .unwrap()
            .path()
            .clone();
        filesystem::write(staging.as_unix_path().unwrap(), b"partial").unwrap();

        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        assert_eq!(operation.id(), id);
        hub.status.lock().unwrap().mark_running(id).unwrap();
        hub.queue
            .lock()
            .unwrap()
            .finish(id, Err("forced recoverable failure".into()))
            .unwrap();
        let mut status = hub.status.lock().unwrap();
        status
            .record_recoverable_failure(id, source, staging.clone(), "forced recoverable failure")
            .unwrap();
        status.mark_recoverable(id).unwrap();
        drop(status);
        (id, staging)
    }

    fn failed_copy(hub: &OperationHub, source: StorePath, target: StorePath) -> JobId {
        let id = hub
            .submit_drop(
                FileDragPayload::new(vec![source], DropAction::Copy).unwrap(),
                target,
            )
            .unwrap()[0];
        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        let affected_path = operation.affected_path().clone();
        hub.status.lock().unwrap().mark_running(id).unwrap();
        hub.queue
            .lock()
            .unwrap()
            .finish(id, Err("forced failure".into()))
            .unwrap();
        record_finished_operation(
            &mut hub.status.lock().unwrap(),
            id,
            Some(JobState::Failed),
            affected_path,
            None,
            None,
        )
        .unwrap();
        id
    }

    fn hub_with_invalid_replacement_journal() -> (tempfile::TempDir, OperationHub, JobId) {
        let temporary = tempfile::tempdir().unwrap();
        let destination_directory = temporary.path().join("destination");
        filesystem::create_dir(&destination_directory).unwrap();
        let destination = StorePath::from_unix_path(destination_directory.join("file.txt"));
        let id = JobId::new(91).unwrap();
        let mut status = StatusCenterModel::default();
        status
            .register(
                id,
                EventGeneration::new(0),
                OperationKind::Copy,
                destination,
                Some(1),
            )
            .unwrap();
        let store = StatusStore::at(temporary.path().join("status/operations.json"));
        store.save(&status.to_json().unwrap()).unwrap();
        filesystem::write(
            destination_directory.join(".musheen-replace-journal-v1-invalid"),
            b"invalid journal",
        )
        .unwrap();
        let hub = OperationHub::with_status_store(&ResourceLimits::default(), store).unwrap();
        (temporary, hub, id)
    }

    #[test]
    fn transfer_history_records_destination_path_for_restart_recovery() {
        let temporary = tempfile::tempdir().unwrap();
        let source_directory = temporary.path().join("source");
        let destination_directory = temporary.path().join("destination");
        filesystem::create_dir(&source_directory).unwrap();
        filesystem::create_dir(&destination_directory).unwrap();
        let source = source_directory.join("file.txt");
        filesystem::write(&source, b"source").unwrap();
        let source = StorePath::from_unix_path(source);
        let target = StorePath::from_unix_path(destination_directory.clone());
        let hub = OperationHub::new(&ResourceLimits::default());

        let id = hub
            .submit_drop(
                FileDragPayload::new(vec![source], DropAction::Copy).unwrap(),
                target,
            )
            .unwrap()[0];

        assert_eq!(
            hub.status.lock().unwrap().entry(id).unwrap().location(),
            &StorePath::from_unix_path(destination_directory.join("file.txt"))
        );
    }

    #[test]
    fn completed_rename_undo_creates_a_new_tracked_job() {
        let temporary = tempfile::tempdir().unwrap();
        let source_path = temporary.path().join("before.txt");
        filesystem::write(&source_path, b"contents").unwrap();
        let source = StorePath::from_unix_path(source_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = musheen_ops::MutationProvider::identity(&mut store, &source)
            .unwrap()
            .unwrap();
        let hub = OperationHub::new(&ResourceLimits::default());
        let job = hub
            .submit_rename(RenameRequest::new(
                source,
                "after.txt".into(),
                identity.into_vec(),
            ))
            .unwrap();
        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        operation.execute().unwrap();
        hub.queue.lock().unwrap().finish(job, Ok(())).unwrap();
        assert!(hub.can_undo(job));
        assert!(hub.has_undo_candidate(job));

        let undo_job = hub.submit_undo(job).unwrap();
        assert!(!hub.can_undo(job));
        assert!(!hub.has_undo_candidate(job));
        assert_eq!(
            hub.status.lock().unwrap().entry(undo_job).unwrap().kind(),
            OperationKind::Rename
        );
        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        operation.execute().unwrap();
        hub.queue.lock().unwrap().finish(undo_job, Ok(())).unwrap();
        assert_eq!(filesystem::read(source_path).unwrap(), b"contents");
    }

    #[test]
    fn completed_move_undo_is_tracked_as_a_move() {
        let temporary = tempfile::tempdir().unwrap();
        let original_directory = temporary.path().join("original");
        let moved_directory = temporary.path().join("moved");
        filesystem::create_dir(&original_directory).unwrap();
        filesystem::create_dir(&moved_directory).unwrap();
        let original_path = original_directory.join("item.txt");
        filesystem::write(&original_path, b"contents").unwrap();
        let hub = OperationHub::new(&ResourceLimits::default());
        let job = hub
            .submit_drop(
                FileDragPayload::new(
                    vec![StorePath::from_unix_path(original_path.as_os_str())],
                    DropAction::Move,
                )
                .unwrap(),
                StorePath::from_unix_path(moved_directory.as_os_str()),
            )
            .unwrap()[0];
        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        operation.execute().unwrap();
        hub.queue.lock().unwrap().finish(job, Ok(())).unwrap();
        assert!(hub.can_undo(job));

        let undo_job = hub.submit_undo(job).unwrap();
        assert_eq!(
            hub.status.lock().unwrap().entry(undo_job).unwrap().kind(),
            OperationKind::Move
        );
        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        operation.execute().unwrap();
        hub.queue.lock().unwrap().finish(undo_job, Ok(())).unwrap();
        assert_eq!(filesystem::read(original_path).unwrap(), b"contents");
    }

    #[test]
    fn completed_trash_undo_is_tracked_as_a_restore() {
        let temporary = tempfile::tempdir().unwrap();
        let original_path = temporary.path().join("discarded.txt");
        filesystem::write(&original_path, b"contents").unwrap();
        let original = StorePath::from_unix_path(original_path.as_os_str());
        let mut store = LocalStore::new();
        let identity = musheen_ops::MutationProvider::identity(&mut store, &original)
            .unwrap()
            .unwrap();
        let hub = OperationHub::new(&ResourceLimits::default());
        let job = hub
            .submit_trash(vec![DeleteTarget::new(original, identity.into_vec())])
            .unwrap()[0];
        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        let outcome = operation.execute_detailed().unwrap();
        hub.queue
            .lock()
            .unwrap()
            .finish_with_outcome(job, outcome)
            .unwrap();
        assert!(hub.can_undo(job));

        let undo_job = hub.submit_undo(job).unwrap();
        assert_eq!(
            hub.status.lock().unwrap().entry(undo_job).unwrap().kind(),
            OperationKind::Restore
        );
        let operation = hub
            .queue
            .lock()
            .unwrap()
            .start_ready()
            .unwrap()
            .pop()
            .unwrap();
        let outcome = operation.execute_detailed().unwrap();
        hub.queue
            .lock()
            .unwrap()
            .finish_with_outcome(undo_job, outcome)
            .unwrap();
        assert_eq!(filesystem::read(original_path).unwrap(), b"contents");
    }

    #[test]
    fn status_persistence_worker_flushes_the_latest_snapshot() {
        let temporary = tempfile::tempdir().unwrap();
        let store = StatusStore::at(temporary.path().join("status/operations.json"));
        let hub =
            OperationHub::with_status_store(&ResourceLimits::default(), store.clone()).unwrap();
        let parent = StorePath::from_unix_path(temporary.path());

        let first = hub
            .submit_create(CreateRequest::new(
                parent.clone(),
                "first.txt".into(),
                musheen_ops::CreateKind::File,
            ))
            .unwrap();
        let second = hub
            .submit_create(CreateRequest::new(
                parent,
                "second.txt".into(),
                musheen_ops::CreateKind::File,
            ))
            .unwrap();
        hub.flush_status_persistence().unwrap();

        let document = store.load().unwrap().unwrap();
        let restored = StatusCenterModel::from_json(&document).unwrap();
        assert!(restored.entry(first).is_some());
        assert!(restored.entry(second).is_some());

        filesystem::remove_dir_all(temporary.path().join("status")).unwrap();
        filesystem::write(temporary.path().join("status"), b"blocked").unwrap();
        hub.submit_create(CreateRequest::new(
            StorePath::from_unix_path(temporary.path()),
            "third.txt".into(),
            musheen_ops::CreateKind::File,
        ))
        .unwrap();
        assert!(hub.flush_status_persistence().is_err());
        assert!(hub.persistence_error().is_some());
    }

    #[test]
    fn invalid_replacement_journal_preserves_status_and_reports_error() {
        let (_temporary, hub, id) = hub_with_invalid_replacement_journal();

        assert!(hub.status.lock().unwrap().entry(id).is_some());
        assert!(
            hub.persistence_error()
                .is_some_and(|error| error.contains("replacement journal"))
        );
    }

    #[test]
    fn startup_removes_stale_staging_from_interrupted_operations() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = StorePath::from_unix_path(temporary.path().join("destination.txt"));
        let id = JobId::new(92).unwrap();
        let generation = EventGeneration::new(4);
        let mut status = StatusCenterModel::default();
        status
            .register(
                id,
                generation,
                OperationKind::Copy,
                destination.clone(),
                Some(1),
            )
            .unwrap();
        let store = StatusStore::at(temporary.path().join("status/operations.json"));
        store.save(&status.to_json().unwrap()).unwrap();
        let staging =
            StagingPath::for_destination_with_nonce(&destination, id, generation, [0x33; 16])
                .unwrap();
        filesystem::write(staging.path().as_unix_path().unwrap(), b"partial").unwrap();

        let hub = OperationHub::with_status_store(&ResourceLimits::default(), store).unwrap();

        assert!(!staging.path().as_unix_path().unwrap().exists());
        assert_eq!(
            hub.status.lock().unwrap().entry(id).unwrap().status(),
            OperationStatus::Interrupted
        );
    }

    #[test]
    fn startup_preserves_recorded_recovery_staging() {
        let temporary = tempfile::tempdir().unwrap();
        let destination = StorePath::from_unix_path(temporary.path().join("destination.txt"));
        let id = JobId::new(93).unwrap();
        let generation = EventGeneration::new(5);
        let staging =
            StagingPath::for_destination_with_nonce(&destination, id, generation, [0x44; 16])
                .unwrap()
                .path()
                .clone();
        filesystem::write(staging.as_unix_path().unwrap(), b"recoverable").unwrap();
        let mut status = StatusCenterModel::default();
        status
            .register(
                id,
                generation,
                OperationKind::Copy,
                destination.clone(),
                Some(1),
            )
            .unwrap();
        status.mark_running(id).unwrap();
        status
            .record_recoverable_failure(id, destination, staging.clone(), "resume available")
            .unwrap();
        status.mark_recoverable(id).unwrap();
        let store = StatusStore::at(temporary.path().join("status/operations.json"));
        store.save(&status.to_json().unwrap()).unwrap();

        let hub = OperationHub::with_status_store(&ResourceLimits::default(), store).unwrap();

        assert_eq!(
            filesystem::read(staging.as_unix_path().unwrap()).unwrap(),
            b"recoverable"
        );
        assert!(
            hub.status
                .lock()
                .unwrap()
                .entry(id)
                .unwrap()
                .failures()
                .iter()
                .any(|failure| failure.recovery_staging() == Some(&staging))
        );
    }

    #[test]
    fn recoverable_jobs_resume_only_after_owned_staging_is_discarded() {
        let (temporary, hub, id, staging) = recoverable_hub();
        assert!(hub.can_resume_recovery(id));

        let reservation = hub
            .reserve_mounts(&[temporary.path().to_path_buf()])
            .unwrap();
        assert!(!hub.can_resume_recovery(id));
        assert!(matches!(
            hub.resume_recovery(id),
            Err(OperationHubError::MountReserved)
        ));
        assert!(staging.as_unix_path().unwrap().exists());
        drop(reservation);

        hub.resume_recovery(id).unwrap();

        assert!(!staging.as_unix_path().unwrap().exists());
        assert_eq!(hub.queue.lock().unwrap().state(id), Some(JobState::Queued));
        assert_eq!(
            hub.status.lock().unwrap().entry(id).unwrap().status(),
            crate::OperationStatus::Pending
        );
    }

    #[test]
    fn discarding_recovery_staging_keeps_the_failed_job_retryable() {
        let (_temporary, hub, id, staging) = recoverable_hub();
        assert!(hub.can_discard_recovery(id));

        hub.discard_recovery(id).unwrap();

        assert!(!staging.as_unix_path().unwrap().exists());
        assert!(hub.can_retry(id));
        assert_eq!(
            hub.status.lock().unwrap().entry(id).unwrap().status(),
            crate::OperationStatus::Failed
        );
    }

    #[test]
    fn mount_reservation_blocks_only_matching_recovery_discard() {
        let temporary = tempfile::tempdir().unwrap();
        let hub = OperationHub::new(&ResourceLimits::default());
        let (reserved_id, reserved_staging) = recoverable_copy(&hub, temporary.path(), "reserved");
        let unrelated_root = tempfile::tempdir().unwrap();
        let (unrelated_id, unrelated_staging) =
            recoverable_copy(&hub, unrelated_root.path(), "unrelated");
        let reservation = hub
            .reserve_mounts(&[temporary.path().to_path_buf()])
            .unwrap();

        assert!(!hub.can_discard_recovery(reserved_id));
        assert!(matches!(
            hub.discard_recovery(reserved_id),
            Err(OperationHubError::MountReserved)
        ));
        assert!(reserved_staging.as_unix_path().unwrap().exists());

        assert!(hub.can_discard_recovery(unrelated_id));
        hub.discard_recovery(unrelated_id).unwrap();
        assert!(!unrelated_staging.as_unix_path().unwrap().exists());

        drop(reservation);
        let source_reservation = hub
            .reserve_mounts(&[temporary.path().join("reserved.txt")])
            .unwrap();
        assert!(!hub.can_discard_recovery(reserved_id));
        assert!(matches!(
            hub.discard_recovery(reserved_id),
            Err(OperationHubError::MountReserved)
        ));
        assert!(reserved_staging.as_unix_path().unwrap().exists());
        drop(source_reservation);

        assert!(hub.can_discard_recovery(reserved_id));
        hub.discard_recovery(reserved_id).unwrap();
        assert!(!reserved_staging.as_unix_path().unwrap().exists());
    }

    #[test]
    fn mount_reservation_refuses_new_jobs_without_holding_the_queue_lock() {
        let temporary = tempfile::tempdir().unwrap();
        let mount = temporary.path().join("mounted");
        let source = temporary.path().join("source.txt");
        filesystem::create_dir(&mount).unwrap();
        filesystem::write(&source, b"source").unwrap();
        let target = StorePath::from_unix_path(mount.clone().into_os_string());
        let payload = FileDragPayload::new(
            vec![StorePath::from_unix_path(source.into_os_string())],
            DropAction::Copy,
        )
        .unwrap();
        let hub = OperationHub::new(&ResourceLimits::default());
        let reservation = hub.reserve_mounts(std::slice::from_ref(&mount)).unwrap();

        assert!(matches!(
            hub.submit_drop(payload.clone(), target.clone()),
            Err(OperationHubError::MountReserved)
        ));

        let (finished, receiver) = mpsc::sync_channel(1);
        let probe = hub.clone();
        std::thread::spawn(move || {
            let _ = finished.send(probe.can_accept_drop(&payload, &target));
        });
        receiver
            .recv_timeout(Duration::from_millis(250))
            .expect("queue access remains responsive while a mount is reserved");

        drop(reservation);
    }

    #[test]
    fn mount_reservation_blocks_failed_job_retry_but_not_unrelated_retry() {
        let temporary = tempfile::tempdir().unwrap();
        let reserved = temporary.path().join("reserved");
        let unrelated = temporary.path().join("unrelated");
        let target = temporary.path().join("target");
        filesystem::create_dir_all(&reserved).unwrap();
        filesystem::create_dir_all(&unrelated).unwrap();
        filesystem::create_dir_all(&target).unwrap();
        let reserved_source = reserved.join("reserved.txt");
        let unrelated_source = unrelated.join("unrelated.txt");
        filesystem::write(&reserved_source, b"reserved").unwrap();
        filesystem::write(&unrelated_source, b"unrelated").unwrap();
        let hub = OperationHub::new(&ResourceLimits::default());
        let target = StorePath::from_unix_path(target.into_os_string());
        let reserved_id = failed_copy(
            &hub,
            StorePath::from_unix_path(reserved_source.into_os_string()),
            target.clone(),
        );
        let unrelated_id = failed_copy(
            &hub,
            StorePath::from_unix_path(unrelated_source.into_os_string()),
            target,
        );
        let reservation = hub.reserve_mounts(std::slice::from_ref(&reserved)).unwrap();

        assert!(!hub.can_retry(reserved_id));
        assert!(matches!(
            hub.retry(reserved_id),
            Err(OperationHubError::MountReserved)
        ));
        assert!(hub.can_retry(unrelated_id));
        hub.retry(unrelated_id).unwrap();
        let ready = hub.queue.lock().unwrap().start_ready().unwrap();
        assert_eq!(
            ready
                .iter()
                .map(ReadyLocalOperation::id)
                .collect::<Vec<_>>(),
            vec![unrelated_id]
        );

        drop(reservation);
        assert!(hub.can_retry(reserved_id));
        hub.retry(reserved_id).unwrap();
    }
}
