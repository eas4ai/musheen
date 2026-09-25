use musheen_core::{DisplayPath, StorePath};
use musheen_desktop::STATUS_SCHEMA_VERSION;
pub(crate) mod custom_actions;
use custom_actions::CustomActionStatus;
use musheen_ops::{EventGeneration, JobId, OperationKind, StagingPath, TrashReceipt};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

/// The most finished entries the status center keeps; the oldest drop first.
const FINISHED_HISTORY: usize = 500;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatus {
    Pending,
    Running,
    Paused,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Recoverable,
    NeedsAttention,
    PartialSuccess,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    RetryFailed,
    Resume,
    DiscardStaging,
    ViewLocation,
    ResolveConflict,
}

impl RecoveryAction {
    const fn label(self) -> &'static str {
        match self {
            Self::RetryFailed => "Retry failed item",
            Self::Resume => "Resume",
            Self::DiscardStaging => "Discard app-owned staging data",
            Self::ViewLocation => "View location",
            Self::ResolveConflict => "Resolve conflict",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OperationFailure {
    item: StorePath,
    cause: Box<str>,
    actions: Vec<RecoveryAction>,
    #[serde(default)]
    recovery_staging: Option<StorePath>,
}

impl OperationFailure {
    #[must_use]
    pub fn message(&self, kind: OperationKind) -> String {
        let actions = self
            .actions
            .iter()
            .map(|action| action.label())
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{kind:?} failed for {}: {}. {actions}",
            DisplayPath::from_store_path(&self.item).as_str(),
            self.cause
        )
    }

    #[must_use]
    pub const fn item(&self) -> &StorePath {
        &self.item
    }

    #[must_use]
    pub fn actions(&self) -> &[RecoveryAction] {
        &self.actions
    }

    #[must_use]
    pub const fn recovery_staging(&self) -> Option<&StorePath> {
        self.recovery_staging.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationStatusEntry {
    id: JobId,
    generation: EventGeneration,
    kind: OperationKind,
    location: StorePath,
    status: OperationStatus,
    total_items: Option<u64>,
    completed_items: u64,
    failures: Vec<OperationFailure>,
    dismissed: bool,
}

impl OperationStatusEntry {
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
        self.kind
    }

    #[must_use]
    pub const fn location(&self) -> &StorePath {
        &self.location
    }

    #[must_use]
    pub const fn status(&self) -> OperationStatus {
        self.status
    }

    #[must_use]
    pub const fn completed_items(&self) -> u64 {
        self.completed_items
    }

    #[must_use]
    pub const fn total_items(&self) -> Option<u64> {
        self.total_items
    }

    #[must_use]
    pub fn failures(&self) -> &[OperationFailure] {
        &self.failures
    }

    #[must_use]
    pub const fn dismissed(&self) -> bool {
        self.dismissed
    }
}

#[derive(Clone, Debug, Default)]
pub struct StatusCenterModel {
    custom_actions: Vec<CustomActionStatus>,
    entries: BTreeMap<JobId, OperationStatusEntry>,
    order: Vec<JobId>,
    /// The jobs pruning dropped since the hub last took them.
    pruned: Vec<JobId>,
}

impl StatusCenterModel {
    pub fn reconcile_recovery_staging(
        &mut self,
        mut is_available: impl FnMut(&StorePath) -> bool,
        mut can_resume: impl FnMut(JobId) -> bool,
    ) -> bool {
        let mut changed = false;
        for entry in self.entries.values_mut() {
            if entry.status != OperationStatus::Recoverable {
                continue;
            }
            let staging_available = entry.failures.iter().any(|failure| {
                failure
                    .recovery_staging
                    .as_ref()
                    .is_some_and(&mut is_available)
            });
            if !staging_available {
                mark_recovery_staging_unavailable(entry);
                changed = true;
                continue;
            }
            if !can_resume(entry.id) {
                changed |= remove_unavailable_resume_action(entry);
            }
        }
        changed
    }

    pub fn mark_unfinished_interrupted(&mut self) -> bool {
        let mut changed = self.interrupt_custom_actions();
        for entry in self.entries.values_mut() {
            if matches!(
                entry.status,
                OperationStatus::Pending | OperationStatus::Running | OperationStatus::Paused
            ) {
                entry.status = OperationStatus::Interrupted;
                if entry.failures.is_empty() {
                    entry.failures.push(OperationFailure {
                        item: entry.location.clone(),
                        cause: "the application stopped before this operation finished; verify the source and destination, then run it again"
                            .into(),
                        actions: vec![RecoveryAction::ViewLocation],
                        recovery_staging: None,
                    });
                }
                changed = true;
            }
        }
        changed
    }

    pub fn register(
        &mut self,
        id: JobId,
        generation: EventGeneration,
        kind: OperationKind,
        location: StorePath,
        total_items: Option<u64>,
    ) -> Result<(), StatusCenterError> {
        if self.entries.contains_key(&id) {
            return Err(StatusCenterError::DuplicateJob(id));
        }
        self.entries.insert(
            id,
            OperationStatusEntry {
                id,
                generation,
                kind,
                location,
                status: OperationStatus::Pending,
                total_items,
                completed_items: 0,
                failures: Vec::new(),
                dismissed: false,
            },
        );
        self.order.push(id);
        self.prune_finished();
        Ok(())
    }

    pub fn mark_running(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if !matches!(
            entry.status,
            OperationStatus::Pending | OperationStatus::Interrupted
        ) {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.status = OperationStatus::Running;
        Ok(())
    }

    pub fn record_item_success(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if !matches!(
            entry.status,
            OperationStatus::Running | OperationStatus::Paused
        ) {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.completed_items = entry.completed_items.saturating_add(1);
        Ok(())
    }

    pub fn record_failure(
        &mut self,
        id: JobId,
        item: StorePath,
        cause: impl Into<Box<str>>,
        actions: impl IntoIterator<Item = RecoveryAction>,
    ) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if !matches!(
            entry.status,
            OperationStatus::Running | OperationStatus::Paused
        ) {
            return Err(StatusCenterError::InvalidState(id));
        }
        let actions = actions.into_iter().collect::<Vec<_>>();
        if actions.is_empty() {
            return Err(StatusCenterError::MissingRecoveryAction);
        }
        entry.failures.push(OperationFailure {
            item,
            cause: cause.into(),
            actions,
            recovery_staging: None,
        });
        Ok(())
    }

    pub fn record_recoverable_failure(
        &mut self,
        id: JobId,
        item: StorePath,
        recovery_staging: StorePath,
        cause: impl Into<Box<str>>,
    ) -> Result<(), StatusCenterError> {
        if !StagingPath::is_owned_path(&recovery_staging) {
            return Err(StatusCenterError::InvalidRecoveryStaging(id));
        }
        let entry = self.entry_mut(id)?;
        if !matches!(
            entry.status,
            OperationStatus::Running | OperationStatus::Paused
        ) {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.failures.push(OperationFailure {
            item,
            cause: cause.into(),
            actions: vec![
                RecoveryAction::Resume,
                RecoveryAction::DiscardStaging,
                RecoveryAction::ViewLocation,
            ],
            recovery_staging: Some(recovery_staging),
        });
        Ok(())
    }

    pub fn complete(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        entry.status = if entry.failures.is_empty() {
            OperationStatus::Completed
        } else if entry.completed_items > 0 {
            OperationStatus::PartialSuccess
        } else {
            OperationStatus::Failed
        };
        self.prune_finished();
        Ok(())
    }

    pub fn mark_paused(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if entry.status != OperationStatus::Running {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.status = OperationStatus::Paused;
        Ok(())
    }

    pub fn mark_resumed(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if entry.status != OperationStatus::Paused {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.status = OperationStatus::Running;
        Ok(())
    }

    pub fn mark_cancelled(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        self.entry_mut(id)?.status = OperationStatus::Cancelled;
        self.prune_finished();
        Ok(())
    }

    /// Keeps at most [`FINISHED_HISTORY`] finished entries, dropping the
    /// oldest first. An entry that is pending, running, paused, interrupted
    /// or waiting on the user is never dropped.
    fn prune_finished(&mut self) {
        let entries = &self.entries;
        let finished = self
            .order
            .iter()
            .filter(|id| entries.get(id).is_some_and(Self::is_finished))
            .count();
        let mut excess = finished.saturating_sub(FINISHED_HISTORY);
        if excess == 0 {
            return;
        }
        let mut dropped = Vec::with_capacity(excess);
        self.order.retain(|id| {
            if excess > 0 && entries.get(id).is_some_and(Self::is_finished) {
                excess -= 1;
                dropped.push(*id);
                false
            } else {
                true
            }
        });
        for id in &dropped {
            self.entries.remove(id);
        }
        self.pruned.extend(dropped);
    }

    /// The jobs pruning dropped since the last call, for the hub to forget.
    pub fn take_pruned(&mut self) -> Vec<JobId> {
        std::mem::take(&mut self.pruned)
    }

    pub fn mark_interrupted(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        self.entry_mut(id)?.status = OperationStatus::Interrupted;
        Ok(())
    }

    pub fn mark_recoverable(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        self.mark_recovery_status(id, OperationStatus::Recoverable)
    }

    pub fn mark_needs_attention(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        self.mark_recovery_status(id, OperationStatus::NeedsAttention)
    }

    pub fn mark_retry_pending(
        &mut self,
        id: JobId,
        generation: EventGeneration,
    ) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if !matches!(
            entry.status,
            OperationStatus::Failed
                | OperationStatus::PartialSuccess
                | OperationStatus::Interrupted
        ) {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.generation = generation;
        entry.status = OperationStatus::Pending;
        entry.completed_items = 0;
        entry.failures.clear();
        entry.dismissed = false;
        Ok(())
    }

    pub fn mark_recovery_retry_pending(
        &mut self,
        id: JobId,
        generation: EventGeneration,
    ) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if entry.status != OperationStatus::Recoverable
            || !entry
                .failures
                .iter()
                .any(|failure| failure.recovery_staging.is_some())
        {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.generation = generation;
        entry.status = OperationStatus::Pending;
        entry.completed_items = 0;
        entry.failures.clear();
        entry.dismissed = false;
        Ok(())
    }

    pub fn mark_metadata_review_pending(
        &mut self,
        id: JobId,
        generation: EventGeneration,
    ) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if entry.status != OperationStatus::NeedsAttention {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.generation = generation;
        entry.status = OperationStatus::Pending;
        entry.completed_items = 0;
        entry.failures.clear();
        entry.dismissed = false;
        Ok(())
    }

    pub fn acknowledge_metadata_review_keep_source(
        &mut self,
        id: JobId,
    ) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if entry.status != OperationStatus::NeedsAttention {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.status = OperationStatus::Cancelled;
        Ok(())
    }

    pub fn mark_staging_discarded(
        &mut self,
        id: JobId,
        retry_available: bool,
    ) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if entry.status != OperationStatus::Recoverable {
            return Err(StatusCenterError::InvalidState(id));
        }
        let mut changed = false;
        for failure in &mut entry.failures {
            if failure.recovery_staging.take().is_some() {
                failure.cause = format!(
                    "{}; app-owned recovery staging was discarded safely",
                    failure.cause
                )
                .into();
                failure.actions = if retry_available {
                    vec![RecoveryAction::RetryFailed, RecoveryAction::ViewLocation]
                } else {
                    vec![RecoveryAction::ViewLocation]
                };
                changed = true;
            }
        }
        if !changed {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.status = if retry_available {
            OperationStatus::Failed
        } else {
            OperationStatus::NeedsAttention
        };
        Ok(())
    }

    pub fn dismiss(&mut self, id: JobId) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if matches!(
            entry.status,
            OperationStatus::Pending | OperationStatus::Running
        ) {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.dismissed = true;
        Ok(())
    }

    #[must_use]
    pub fn entry(&self, id: JobId) -> Option<&OperationStatusEntry> {
        self.entries.get(&id)
    }

    pub fn visible_entries(&self) -> Vec<&OperationStatusEntry> {
        self.history()
            .into_iter()
            .filter(|entry| !entry.dismissed)
            .collect()
    }

    pub fn history(&self) -> Vec<&OperationStatusEntry> {
        self.order
            .iter()
            .filter_map(|id| self.entries.get(id))
            .collect()
    }

    #[must_use]
    pub fn highest_job_id(&self) -> Option<JobId> {
        self.entries.keys().next_back().copied()
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| {
                matches!(
                    entry.status,
                    OperationStatus::Pending | OperationStatus::Running | OperationStatus::Paused
                )
            })
            .count()
    }

    pub fn retry_targets(&self, id: JobId) -> Result<Vec<StorePath>, StatusCenterError> {
        let entry = self
            .entries
            .get(&id)
            .ok_or(StatusCenterError::UnknownJob(id))?;
        Ok(entry
            .failures
            .iter()
            .filter(|failure| failure.actions.contains(&RecoveryAction::RetryFailed))
            .map(|failure| failure.item.clone())
            .collect())
    }

    pub fn to_json(&self) -> Result<Vec<u8>, StatusCenterError> {
        let entries = self
            .history()
            .into_iter()
            .map(EntryDocument::from)
            .collect();
        serde_json::to_vec(&StatusDocument {
            custom_actions: self.custom_actions.clone(),
            schema_version: STATUS_SCHEMA_VERSION,
            entries,
        })
        .map_err(|error| StatusCenterError::Document(error.to_string().into()))
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, StatusCenterError> {
        let document: StatusDocument = serde_json::from_slice(bytes)
            .map_err(|error| StatusCenterError::Document(error.to_string().into()))?;
        if document.schema_version != STATUS_SCHEMA_VERSION {
            return Err(StatusCenterError::UnsupportedSchema(
                document.schema_version,
            ));
        }
        let mut model = Self::default();
        if document.custom_actions.len() > 128
            || document
                .custom_actions
                .iter()
                .any(|entry| entry.context.targets.len() > 16)
        {
            return Err(StatusCenterError::Document(
                "custom action history exceeds limits".into(),
            ));
        }
        model.custom_actions = document.custom_actions;
        for entry in document.entries {
            let entry = OperationStatusEntry::try_from(entry)?;
            if model.entries.insert(entry.id, entry.clone()).is_some() {
                return Err(StatusCenterError::DuplicateJob(entry.id));
            }
            model.order.push(entry.id);
        }
        // A document written before the bound existed may hold more; the
        // queue has no records for a restored job, so nothing is forgotten.
        model.prune_finished();
        model.pruned.clear();
        Ok(model)
    }

    fn is_finished(entry: &OperationStatusEntry) -> bool {
        matches!(
            entry.status,
            OperationStatus::Completed
                | OperationStatus::Failed
                | OperationStatus::Cancelled
                | OperationStatus::PartialSuccess
        )
    }

    fn entry_mut(&mut self, id: JobId) -> Result<&mut OperationStatusEntry, StatusCenterError> {
        self.entries
            .get_mut(&id)
            .ok_or(StatusCenterError::UnknownJob(id))
    }

    fn mark_recovery_status(
        &mut self,
        id: JobId,
        status: OperationStatus,
    ) -> Result<(), StatusCenterError> {
        let entry = self.entry_mut(id)?;
        if !matches!(
            entry.status,
            OperationStatus::Running | OperationStatus::Paused
        ) || entry.failures.is_empty()
        {
            return Err(StatusCenterError::InvalidState(id));
        }
        entry.status = status;
        Ok(())
    }
}

fn mark_recovery_staging_unavailable(entry: &mut OperationStatusEntry) {
    entry.status = OperationStatus::NeedsAttention;
    for failure in &mut entry.failures {
        failure.recovery_staging = None;
        failure.cause = format!(
            "{}; the recovery staging path is no longer available",
            failure.cause
        )
        .into();
        failure.actions = vec![RecoveryAction::ViewLocation];
    }
}

fn remove_unavailable_resume_action(entry: &mut OperationStatusEntry) -> bool {
    let mut changed = false;
    for failure in &mut entry.failures {
        let before = failure.actions.len();
        failure
            .actions
            .retain(|action| *action != RecoveryAction::Resume);
        if failure.actions.len() != before {
            failure.cause = format!(
                "{}; the original queued operation is unavailable after restart",
                failure.cause
            )
            .into();
            changed = true;
        }
    }
    changed
}

#[derive(Serialize, Deserialize)]
struct StatusDocument {
    #[serde(default)]
    custom_actions: Vec<CustomActionStatus>,
    schema_version: u32,
    entries: Vec<EntryDocument>,
}

#[derive(Serialize, Deserialize)]
struct EntryDocument {
    id: u64,
    generation: u64,
    kind: Box<str>,
    location: StorePath,
    status: OperationStatus,
    total_items: Option<u64>,
    completed_items: u64,
    failures: Vec<OperationFailure>,
    dismissed: bool,
}

impl From<&OperationStatusEntry> for EntryDocument {
    fn from(entry: &OperationStatusEntry) -> Self {
        Self {
            id: entry.id.get(),
            generation: entry.generation.get(),
            kind: operation_kind_name(entry.kind).into(),
            location: entry.location.clone(),
            status: entry.status,
            total_items: entry.total_items,
            completed_items: entry.completed_items,
            failures: entry.failures.clone(),
            dismissed: entry.dismissed,
        }
    }
}

impl TryFrom<EntryDocument> for OperationStatusEntry {
    type Error = StatusCenterError;

    fn try_from(entry: EntryDocument) -> Result<Self, Self::Error> {
        let id = JobId::new(entry.id).ok_or(StatusCenterError::InvalidJobId)?;
        if entry
            .total_items
            .is_some_and(|total| entry.completed_items > total)
        {
            return Err(StatusCenterError::InvalidProgress(id));
        }
        if entry.failures.iter().any(|failure| {
            failure
                .recovery_staging
                .as_ref()
                .is_some_and(|staging| !StagingPath::is_owned_path(staging))
        }) {
            return Err(StatusCenterError::InvalidRecoveryStaging(id));
        }
        let mut status = entry.status;
        let mut failures = entry.failures;
        if status == OperationStatus::Recoverable
            && !failures
                .iter()
                .any(|failure| failure.recovery_staging.is_some())
        {
            status = OperationStatus::NeedsAttention;
            for failure in &mut failures {
                failure.cause = format!(
                    "{}; the persisted recovery staging path is unavailable",
                    failure.cause
                )
                .into();
                failure.actions = vec![RecoveryAction::ViewLocation];
            }
        }
        Ok(Self {
            id,
            generation: EventGeneration::new(entry.generation),
            kind: parse_operation_kind(&entry.kind)?,
            location: entry.location,
            status,
            total_items: entry.total_items,
            completed_items: entry.completed_items,
            failures,
            dismissed: entry.dismissed,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfirmationDefault {
    Cancel,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DestructiveConfirmation {
    command: Box<str>,
    scope: Box<str>,
    reversibility: Box<str>,
    location: StorePath,
}

impl DestructiveConfirmation {
    #[must_use]
    pub fn new(
        command: impl Into<Box<str>>,
        scope: impl Into<Box<str>>,
        reversibility: impl Into<Box<str>>,
        location: StorePath,
    ) -> Self {
        Self {
            command: command.into(),
            scope: scope.into(),
            reversibility: reversibility.into(),
            location,
        }
    }

    #[must_use]
    pub const fn default_action(&self) -> ConfirmationDefault {
        ConfirmationDefault::Cancel
    }

    #[must_use]
    pub fn message(&self) -> String {
        format!(
            "{} for {} at {}: {}.",
            self.command,
            self.scope,
            DisplayPath::from_store_path(&self.location).as_str(),
            self.reversibility
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrashItem {
    receipt: TrashReceipt,
    deleted_at_unix_seconds: i64,
    kind: musheen_ops::ConflictItemKind,
    restorable: bool,
}

impl TrashItem {
    #[must_use]
    pub const fn new(receipt: TrashReceipt, deleted_at_unix_seconds: i64) -> Self {
        Self {
            receipt,
            deleted_at_unix_seconds,
            kind: musheen_ops::ConflictItemKind::File,
            restorable: true,
        }
    }

    #[must_use]
    pub const fn with_kind(
        receipt: TrashReceipt,
        deleted_at_unix_seconds: i64,
        kind: musheen_ops::ConflictItemKind,
    ) -> Self {
        Self {
            receipt,
            deleted_at_unix_seconds,
            kind,
            restorable: true,
        }
    }

    /// Marks whether the entry's data is still in Trash. An entry whose data
    /// is missing is listed so it can be purged, but it cannot be restored.
    #[must_use]
    pub const fn with_restorable(mut self, restorable: bool) -> Self {
        self.restorable = restorable;
        self
    }

    #[must_use]
    pub const fn is_restorable(&self) -> bool {
        self.restorable
    }

    #[must_use]
    pub const fn receipt(&self) -> &TrashReceipt {
        &self.receipt
    }

    #[must_use]
    pub const fn deleted_at_unix_seconds(&self) -> i64 {
        self.deleted_at_unix_seconds
    }

    #[must_use]
    pub const fn kind(&self) -> musheen_ops::ConflictItemKind {
        self.kind
    }
}

#[derive(Clone, Debug)]
pub struct TrashSurfaceModel {
    items: Vec<TrashItem>,
}

impl TrashSurfaceModel {
    #[must_use]
    pub fn new(mut items: Vec<TrashItem>) -> Self {
        items.sort_by_key(|item| std::cmp::Reverse(item.deleted_at_unix_seconds));
        Self { items }
    }

    #[must_use]
    pub fn items(&self) -> &[TrashItem] {
        &self.items
    }

    pub fn restore_receipt(&self, index: usize) -> Result<&TrashReceipt, StatusCenterError> {
        let item = self
            .items
            .get(index)
            .ok_or(StatusCenterError::UnknownTrashItem(index))?;
        if !item.is_restorable() {
            return Err(StatusCenterError::UnrestorableTrashItem(index));
        }
        Ok(item.receipt())
    }

    #[must_use]
    pub fn empty_challenge(&self) -> EmptyTrashChallenge {
        EmptyTrashChallenge {
            item_count: self.items.len(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyTrashChallenge {
    item_count: usize,
}

impl EmptyTrashChallenge {
    pub fn confirm(
        self,
        item_count: usize,
        acknowledges_no_recovery: bool,
    ) -> Result<EmptyTrashConfirmation, StatusCenterError> {
        if item_count != self.item_count || !acknowledges_no_recovery {
            return Err(StatusCenterError::ConfirmationRequired);
        }
        Ok(EmptyTrashConfirmation { item_count })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyTrashConfirmation {
    item_count: usize,
}

impl EmptyTrashConfirmation {
    #[must_use]
    pub const fn item_count(self) -> usize {
        self.item_count
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatusCenterError {
    UnknownJob(JobId),
    DuplicateJob(JobId),
    InvalidState(JobId),
    InvalidProgress(JobId),
    InvalidRecoveryStaging(JobId),
    InvalidJobId,
    MissingRecoveryAction,
    UnknownTrashItem(usize),
    /// The entry's data is missing from Trash; it can be purged, not restored.
    UnrestorableTrashItem(usize),
    ConfirmationRequired,
    UnsupportedSchema(u32),
    UnknownOperationKind(Box<str>),
    Document(Box<str>),
}

impl fmt::Display for StatusCenterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownJob(id) => write!(formatter, "unknown job {}", id.get()),
            Self::DuplicateJob(id) => write!(formatter, "job {} is already recorded", id.get()),
            Self::InvalidState(id) => {
                write!(formatter, "job {} cannot perform that action", id.get())
            }
            Self::InvalidProgress(id) => write!(formatter, "job {} has invalid progress", id.get()),
            Self::InvalidRecoveryStaging(id) => {
                write!(
                    formatter,
                    "job {} has an invalid recovery staging path",
                    id.get()
                )
            }
            Self::InvalidJobId => formatter.write_str("the status document has an invalid job ID"),
            Self::MissingRecoveryAction => formatter.write_str("a failure needs a recovery action"),
            Self::UnknownTrashItem(index) => write!(formatter, "trash item {index} does not exist"),
            Self::UnrestorableTrashItem(index) => {
                write!(formatter, "trash item {index} has no data left to restore")
            }
            Self::ConfirmationRequired => {
                formatter.write_str("the destructive scope was not confirmed")
            }
            Self::UnsupportedSchema(version) => {
                write!(formatter, "unsupported status schema {version}")
            }
            Self::UnknownOperationKind(kind) => write!(formatter, "unknown operation kind {kind}"),
            Self::Document(message) => formatter.write_str(message),
        }
    }
}

impl Error for StatusCenterError {}

fn operation_kind_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Copy => "copy",
        OperationKind::Move => "move",
        OperationKind::Trash => "trash",
        OperationKind::PermanentDelete => "permanent_delete",
        OperationKind::CreateFile => "create_file",
        OperationKind::CreateDirectory => "create_directory",
        OperationKind::Rename => "rename",
        OperationKind::SymbolicLink => "symbolic_link",
        OperationKind::HardLink => "hard_link",
        OperationKind::SetPermissions => "set_permissions",
        OperationKind::SetOwnership => "set_ownership",
        OperationKind::SetExtendedAttribute => "set_extended_attribute",
        OperationKind::Compress => "compress",
        OperationKind::Extract => "extract",
        OperationKind::Restore => "restore",
        OperationKind::Hash => "hash",
        OperationKind::Preview => "preview",
    }
}

fn parse_operation_kind(value: &str) -> Result<OperationKind, StatusCenterError> {
    match value {
        "copy" => Ok(OperationKind::Copy),
        "move" => Ok(OperationKind::Move),
        "trash" => Ok(OperationKind::Trash),
        "permanent_delete" => Ok(OperationKind::PermanentDelete),
        "create_file" => Ok(OperationKind::CreateFile),
        "create_directory" => Ok(OperationKind::CreateDirectory),
        "rename" => Ok(OperationKind::Rename),
        "symbolic_link" => Ok(OperationKind::SymbolicLink),
        "hard_link" => Ok(OperationKind::HardLink),
        "set_permissions" => Ok(OperationKind::SetPermissions),
        "set_ownership" => Ok(OperationKind::SetOwnership),
        "set_extended_attribute" => Ok(OperationKind::SetExtendedAttribute),
        "compress" => Ok(OperationKind::Compress),
        "extract" => Ok(OperationKind::Extract),
        "restore" => Ok(OperationKind::Restore),
        "hash" => Ok(OperationKind::Hash),
        "preview" => Ok(OperationKind::Preview),
        other => Err(StatusCenterError::UnknownOperationKind(other.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(number: u64) -> JobId {
        JobId::new(number).unwrap()
    }

    #[test]
    fn status_history_retains_at_most_500_finished_entries_and_every_unfinished_one() {
        let mut model = StatusCenterModel::default();
        for number in 1..=600 {
            model
                .register(
                    job(number),
                    EventGeneration::new(0),
                    OperationKind::Copy,
                    StorePath::from_unix_path(format!("/work/{number}")),
                    Some(1),
                )
                .unwrap();
            if number > 10 {
                model.mark_running(job(number)).unwrap();
                model.complete(job(number)).unwrap();
            }
        }

        let finished = model
            .history()
            .iter()
            .filter(|entry| entry.status() == OperationStatus::Completed)
            .count();
        assert!(finished <= 500, "{finished} finished entries are retained");
        for number in 1..=10 {
            assert_eq!(
                model.entry(job(number)).map(OperationStatusEntry::status),
                Some(OperationStatus::Pending),
                "pending job {number} is kept"
            );
        }
        let restored = StatusCenterModel::from_json(&model.to_json().unwrap()).unwrap();
        assert!(
            restored.history().len() <= 510,
            "the persisted document holds {} entries",
            restored.history().len()
        );
    }
}
