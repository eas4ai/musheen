use super::budget::{ArchiveBudget, ArchiveOperationError, ArchiveOperationLimits};
use super::create::{
    ArchiveCleanupIntent, append_archive_phase, cleanup_path, cleanup_phase, deletion_path,
    local_path, path_identity_with_controls, publication_quarantine_path, publish_staging,
    remove_owned_journaled, sync_parent,
};
use musheen_core::CancellationToken;
use musheen_ops::{
    ArchiveCleanupKind, ArchivePathIdentity, EventGeneration, JobId, Journal, JournalPhase,
    JournalRecord, JournalStorage, RecoveryDecision, StagingPath,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveRecoveryAction {
    Resume,
    Rollback,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ArchiveRecoveryRequest {
    job_id: JobId,
    generation: EventGeneration,
    phase: JournalPhase,
    recommended: RecoveryDecision,
}

impl ArchiveRecoveryRequest {
    #[must_use]
    pub const fn job_id(self) -> JobId {
        self.job_id
    }

    #[must_use]
    pub const fn generation(self) -> EventGeneration {
        self.generation
    }

    #[must_use]
    pub const fn phase(self) -> JournalPhase {
        self.phase
    }

    #[must_use]
    pub const fn recommended(self) -> RecoveryDecision {
        self.recommended
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveRecoveryOutcome {
    RolledBack,
    Completed,
}

/// Finds interrupted archive jobs without mutating the filesystem or journal.
pub fn recover_archive_operations<S: JournalStorage>(
    journal: &Journal<S>,
) -> Result<Vec<ArchiveRecoveryRequest>, ArchiveOperationError> {
    recover_archive_operations_with_cancellation(journal, &CancellationToken::new())
}

/// Finds interrupted archive jobs using bounded, cancellable identity traversal.
pub fn recover_archive_operations_with_cancellation<S: JournalStorage>(
    journal: &Journal<S>,
    cancellation: &CancellationToken,
) -> Result<Vec<ArchiveRecoveryRequest>, ArchiveOperationError> {
    latest_archive_records(journal)
        .into_iter()
        .filter(|record| {
            !matches!(
                record.phase(),
                JournalPhase::Completed | JournalPhase::RolledBack
            )
        })
        .map(|record| recovery_request(record, cancellation))
        .collect()
}

/// Applies one action that the caller obtained and explicitly approved.
pub fn apply_archive_recovery<S: JournalStorage>(
    journal: &mut Journal<S>,
    request: &ArchiveRecoveryRequest,
    action: ArchiveRecoveryAction,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    apply_archive_recovery_with_cancellation(journal, request, action, &CancellationToken::new())
}

/// Applies an approved recovery action using bounded, cancellable identity traversal.
pub fn apply_archive_recovery_with_cancellation<S: JournalStorage>(
    journal: &mut Journal<S>,
    request: &ArchiveRecoveryRequest,
    action: ArchiveRecoveryAction,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let record = latest_archive_records(journal)
        .into_iter()
        .find(|record| {
            record.job_id() == request.job_id
                && record.generation() == request.generation
                && record.phase() == request.phase
        })
        .ok_or(ArchiveOperationError::RecoveryConsentRequired)?;
    apply_record(journal, &record, action, cancellation)
}

fn latest_archive_records<S: JournalStorage>(journal: &Journal<S>) -> Vec<JournalRecord> {
    let mut latest = std::collections::BTreeMap::new();
    for record in journal.records() {
        if record.archive_checkpoint().is_some() {
            latest.insert((record.job_id(), record.generation()), record.clone());
        }
    }
    latest.into_values().collect()
}

fn recovery_request(
    record: JournalRecord,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryRequest, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let plan = checkpoint.plan();
    let budget = recovery_budget(checkpoint, cancellation);
    let staging = local_path(checkpoint.staging())?;
    let destination = local_path(plan.destination())?;
    let owned_path = checkpoint.staging_nonce().is_some_and(|nonce| {
        StagingPath::is_for_destination(
            checkpoint.staging(),
            plan.destination(),
            record.job_id(),
            record.generation(),
            nonce,
        )
    });
    let current_staging = if owned_path {
        path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?
    } else {
        None
    };
    let current_destination =
        path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?;
    let staging_matches = owned_path && current_staging == checkpoint.staging_identity();
    let destination_matches = current_destination == checkpoint.destination_before();
    let recommended = match record.phase() {
        JournalPhase::Planned => RecoveryDecision::Rollback,
        JournalPhase::StagingCreated | JournalPhase::DataCopied | JournalPhase::MetadataApplied
            if staging_matches && destination_matches =>
        {
            RecoveryDecision::Rollback
        }
        JournalPhase::DestinationPublished
        | JournalPhase::PublishedDestinationCleanupPlanned
        | JournalPhase::PublishedDestinationCleanupQuarantined
        | JournalPhase::StagingCleaned
            if current_destination == checkpoint.destination_after() =>
        {
            RecoveryDecision::Resume
        }
        JournalPhase::PrepublishStageCleanupPlanned
        | JournalPhase::PrepublishStageCleanupQuarantined => RecoveryDecision::Rollback,
        JournalPhase::Completed | JournalPhase::RolledBack => RecoveryDecision::NoAction,
        _ => RecoveryDecision::Ask,
    };
    Ok(ArchiveRecoveryRequest {
        job_id: record.job_id(),
        generation: record.generation(),
        phase: record.phase(),
        recommended,
    })
}

fn apply_record<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    action: ArchiveRecoveryAction,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let plan = checkpoint.plan();
    let budget = recovery_budget(checkpoint, cancellation);
    let staging = local_path(checkpoint.staging())?;
    verify_staging_path(record, checkpoint.staging(), plan.destination())?;
    let destination = local_path(plan.destination())?;

    match (record.phase(), action) {
        (JournalPhase::Planned, ArchiveRecoveryAction::Rollback) => {
            // No stage identity was committed. A path that now exists cannot be proven ours.
            append_archive_phase(
                journal,
                record.job_id(),
                record.generation(),
                JournalPhase::RolledBack,
                plan,
                &staging,
                checkpoint.destination_before(),
                None,
                Some(&budget),
            )?;
            Ok(ArchiveRecoveryOutcome::RolledBack)
        }
        (JournalPhase::StagingCreated, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::DataCopied, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::MetadataApplied, ArchiveRecoveryAction::Rollback) => {
            verify_exact(
                checkpoint.staging_identity(),
                path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?,
            )?;
            verify_exact(
                checkpoint.destination_before(),
                path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?,
            )?;
            finish_prepublish_cleanup(journal, record, &staging, &budget, cancellation)
        }
        (JournalPhase::DataCopied, ArchiveRecoveryAction::Resume)
        | (JournalPhase::MetadataApplied, ArchiveRecoveryAction::Resume)
        | (JournalPhase::DestinationQuarantinePlanned, ArchiveRecoveryAction::Resume)
        | (JournalPhase::DestinationQuarantined, ArchiveRecoveryAction::Resume)
        | (JournalPhase::StagePublishPlanned, ArchiveRecoveryAction::Resume)
        | (JournalPhase::PublishRollbackPlanned, ArchiveRecoveryAction::Resume)
        | (JournalPhase::PublishedPayloadQuarantined, ArchiveRecoveryAction::Resume)
        | (JournalPhase::DestinationRestorePlanned, ArchiveRecoveryAction::Resume)
        | (JournalPhase::DestinationRestored, ArchiveRecoveryAction::Resume)
        | (JournalPhase::StageRestorePlanned, ArchiveRecoveryAction::Resume)
        | (JournalPhase::RecoveryRequired, ArchiveRecoveryAction::Resume) => resume_publish(
            journal,
            record,
            &staging,
            &destination,
            &budget,
            cancellation,
        ),
        (JournalPhase::PrepublishStageCleanupPlanned, _)
        | (JournalPhase::PrepublishStageCleanupQuarantined, _) => {
            finish_prepublish_cleanup(journal, record, &staging, &budget, cancellation)
        }
        (JournalPhase::PublishedDestinationCleanupPlanned, ArchiveRecoveryAction::Resume)
        | (JournalPhase::PublishedDestinationCleanupQuarantined, ArchiveRecoveryAction::Resume) => {
            finish_published_recovery(
                journal,
                record,
                &staging,
                &destination,
                &budget,
                cancellation,
            )
        }
        (JournalPhase::RecoveryRequired, ArchiveRecoveryAction::Rollback) => rollback_publication(
            journal,
            record,
            &staging,
            &destination,
            &budget,
            cancellation,
        ),
        (JournalPhase::DestinationQuarantinePlanned, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::DestinationQuarantined, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::StagePublishPlanned, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::PublishRollbackPlanned, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::PublishedPayloadQuarantined, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::DestinationRestorePlanned, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::DestinationRestored, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::StageRestorePlanned, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::PublishedDestinationCleanupPlanned, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::PublishedDestinationCleanupQuarantined, ArchiveRecoveryAction::Rollback) => {
            rollback_publication(
                journal,
                record,
                &staging,
                &destination,
                &budget,
                cancellation,
            )
        }
        (JournalPhase::DestinationPublished, ArchiveRecoveryAction::Resume) => {
            finish_published_recovery(
                journal,
                record,
                &staging,
                &destination,
                &budget,
                cancellation,
            )
        }
        (JournalPhase::StagingCleaned, ArchiveRecoveryAction::Resume) => {
            verify_exact(
                checkpoint.destination_after(),
                path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?,
            )?;
            // The durable cleanup checkpoint proves the old stage is gone. Never delete an object
            // that later appears at the same name because it has no committed ownership identity.
            append_archive_phase(
                journal,
                record.job_id(),
                record.generation(),
                JournalPhase::Completed,
                plan,
                &staging,
                checkpoint.destination_before(),
                checkpoint.destination_after(),
                Some(&budget),
            )?;
            Ok(ArchiveRecoveryOutcome::Completed)
        }
        _ => Err(ArchiveOperationError::RecoveryConsentRequired),
    }
}

fn resume_publish<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    destination: &std::path::Path,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let current_staging = path_identity_with_controls(staging, Some(budget), Some(cancellation))?;
    let cleanup = if checkpoint.cleanup_kind() == Some(ArchiveCleanupKind::PublishedDestination) {
        checkpoint.cleanup().map(local_path).transpose()?.ok_or(
            ArchiveOperationError::UnsafePath("published cleanup has no source path"),
        )?
    } else {
        cleanup_path(staging)?
    };
    let cleanup_quarantine =
        if checkpoint.cleanup_kind() == Some(ArchiveCleanupKind::PublishedDestination) {
            checkpoint
                .cleanup_deletion()
                .map(local_path)
                .transpose()?
                .ok_or(ArchiveOperationError::UnsafePath(
                    "published cleanup has no quarantine path",
                ))?
        } else {
            deletion_path(&cleanup)?
        };
    let current_cleanup = path_identity_with_controls(&cleanup, Some(budget), Some(cancellation))?;
    let publication_quarantine = checkpoint
        .publication_quarantine()
        .map(local_path)
        .transpose()?
        .unwrap_or(publication_quarantine_path(staging)?);
    let current_publication_quarantine =
        path_identity_with_controls(&publication_quarantine, Some(budget), Some(cancellation))?;
    let current_destination =
        path_identity_with_controls(destination, Some(budget), Some(cancellation))?;
    let publish_already_happened = current_destination == checkpoint.staging_identity()
        && (current_cleanup == checkpoint.destination_before()
            || (checkpoint.destination_before().is_none() && current_cleanup.is_none()));
    if publish_already_happened {
        sync_parent(destination)?;
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::DestinationPublished,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            current_destination,
            Some(budget),
        )?;
        finish_published(
            journal,
            record,
            staging,
            &cleanup,
            &cleanup_quarantine,
            current_cleanup,
            current_destination,
            budget,
            cancellation,
        )?;
        return Ok(ArchiveRecoveryOutcome::Completed);
    }
    if current_publication_quarantine == checkpoint.staging_identity()
        && current_destination.is_none()
        && current_cleanup == checkpoint.destination_before()
        && checkpoint.destination_before().is_some()
    {
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::DestinationRestorePlanned,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            Some(budget),
        )?;
        rename_sibling(&cleanup, destination)?;
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::DestinationRestored,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            checkpoint.destination_before(),
            Some(budget),
        )?;
        return resume_publish(journal, record, staging, destination, budget, cancellation);
    }
    if checkpoint.destination_before().is_some()
        && current_staging == checkpoint.staging_identity()
        && current_destination.is_none()
        && current_cleanup == checkpoint.destination_before()
    {
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::DestinationRestorePlanned,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            Some(budget),
        )?;
        if checkpoint.destination_before().is_some() {
            rename_sibling(&cleanup, destination)?;
        }
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::DestinationRestored,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            checkpoint.destination_before(),
            Some(budget),
        )?;
        return resume_publish(journal, record, staging, destination, budget, cancellation);
    }
    verify_exact(checkpoint.destination_before(), current_destination)?;
    verify_exact(None, current_cleanup)?;
    let (publish_source, publish_source_identity) =
        if current_staging == checkpoint.staging_identity() {
            (staging, current_staging)
        } else if current_publication_quarantine == checkpoint.staging_identity() {
            (
                publication_quarantine.as_path(),
                current_publication_quarantine,
            )
        } else {
            return Err(ArchiveOperationError::UnsafePath(
                "owned archive payload is unavailable for recovery",
            ));
        };
    let outcome = {
        let mut publication_checkpoint = |phase, destination_after| {
            append_archive_phase(
                journal,
                record.job_id(),
                record.generation(),
                phase,
                checkpoint.plan(),
                staging,
                checkpoint.destination_before(),
                destination_after,
                Some(budget),
            )
        };
        publish_staging(
            publish_source,
            destination,
            &cleanup,
            checkpoint.plan().conflict_policy(),
            checkpoint
                .staging_identity()
                .ok_or(ArchiveOperationError::UnsafePath(
                    "archive recovery has no staging identity",
                ))?,
            checkpoint.destination_before(),
            Some(budget),
            Some(cancellation),
            &mut publication_checkpoint,
        )?
    };
    if matches!(outcome, super::create::ArchiveOperationOutcome::Skipped) {
        let publish_deletion = deletion_path(publish_source)?;
        let cleanup = ArchiveCleanupIntent::new(
            ArchiveCleanupKind::PrepublishStage,
            publish_source,
            &publish_deletion,
            publish_source_identity,
        );
        return finish_prepublish_path(journal, record, staging, cleanup, budget, cancellation);
    }
    sync_parent(destination)?;
    let destination_after =
        path_identity_with_controls(destination, Some(budget), Some(cancellation))?;
    finish_published(
        journal,
        record,
        staging,
        &cleanup,
        &cleanup_quarantine,
        checkpoint.destination_before(),
        destination_after,
        budget,
        cancellation,
    )?;
    Ok(ArchiveRecoveryOutcome::Completed)
}

fn rollback_publication<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    destination: &std::path::Path,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let (rollback, rollback_quarantine) =
        if checkpoint.cleanup_kind() == Some(ArchiveCleanupKind::PublishedDestination) {
            let (source, quarantine, identity) =
                checkpoint_cleanup(checkpoint, ArchiveCleanupKind::PublishedDestination)?;
            if identity != checkpoint.destination_before() {
                return Err(ArchiveOperationError::UnsafePath(
                    "published destination cleanup identity changed",
                ));
            }
            (source, quarantine)
        } else {
            let source = cleanup_path(staging)?;
            let quarantine = deletion_path(&source)?;
            (source, quarantine)
        };
    let publication_quarantine = checkpoint
        .publication_quarantine()
        .map(local_path)
        .transpose()?
        .unwrap_or(publication_quarantine_path(staging)?);
    let staging_identity = path_identity_with_controls(staging, Some(budget), Some(cancellation))?;
    let mut destination_identity =
        path_identity_with_controls(destination, Some(budget), Some(cancellation))?;
    let mut rollback_identity =
        path_identity_with_controls(&rollback, Some(budget), Some(cancellation))?;
    let mut rollback_quarantine_identity =
        path_identity_with_controls(&rollback_quarantine, Some(budget), Some(cancellation))?;
    let mut publication_identity =
        path_identity_with_controls(&publication_quarantine, Some(budget), Some(cancellation))?;

    if destination_identity == checkpoint.staging_identity()
        && checkpoint.destination_before().is_some()
        && rollback_identity != checkpoint.destination_before()
        && rollback_quarantine_identity != checkpoint.destination_before()
    {
        return Err(ArchiveOperationError::RecoveryConsentRequired);
    }

    if destination_identity == checkpoint.staging_identity() {
        if publication_identity.is_some() {
            return Err(ArchiveOperationError::UnsafePath(
                "archive publication quarantine is occupied",
            ));
        }
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::PublishRollbackPlanned,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            destination_identity,
            Some(budget),
        )?;
        rename_sibling(destination, &publication_quarantine)?;
        destination_identity = None;
        publication_identity = checkpoint.staging_identity();
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::PublishedPayloadQuarantined,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            Some(budget),
        )?;
    }

    if destination_identity.is_none()
        && (rollback_identity == checkpoint.destination_before()
            || rollback_quarantine_identity == checkpoint.destination_before())
    {
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::DestinationRestorePlanned,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            Some(budget),
        )?;
        if rollback_identity == checkpoint.destination_before() {
            rename_sibling(&rollback, destination)?;
            rollback_identity = None;
        } else if rollback_quarantine_identity == checkpoint.destination_before() {
            rename_sibling(&rollback_quarantine, destination)?;
            rollback_quarantine_identity = None;
        }
        destination_identity = checkpoint.destination_before();
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::DestinationRestored,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            destination_identity,
            Some(budget),
        )?;
    }
    verify_exact(checkpoint.destination_before(), destination_identity)?;
    verify_exact(None, rollback_identity)?;
    verify_exact(None, rollback_quarantine_identity)?;

    let owned = if publication_identity == checkpoint.staging_identity() {
        Some((publication_quarantine.as_path(), publication_identity))
    } else if staging_identity == checkpoint.staging_identity() {
        Some((staging, staging_identity))
    } else {
        None
    };
    if let Some((owned_path, owned_identity)) = owned {
        let owned_quarantine = deletion_path(owned_path)?;
        let cleanup = ArchiveCleanupIntent::new(
            ArchiveCleanupKind::PrepublishStage,
            owned_path,
            &owned_quarantine,
            owned_identity,
        );
        return finish_prepublish_path(journal, record, staging, cleanup, budget, cancellation);
    }
    // A foreign object may occupy the old stage name. It is not part of this transaction and is
    // deliberately left untouched after the owned payload is removed from its quarantine.
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::RolledBack,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        destination_identity,
        Some(budget),
    )?;
    Ok(ArchiveRecoveryOutcome::RolledBack)
}

fn rename_sibling(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(), ArchiveOperationError> {
    use rustix::fs::{Mode, OFlags, RenameFlags, open, renameat_with};
    let parent_path = source.parent().ok_or(ArchiveOperationError::UnsafePath(
        "archive transaction path needs a parent",
    ))?;
    if destination.parent() != Some(parent_path) {
        return Err(ArchiveOperationError::UnsafePath(
            "archive transaction paths must be siblings",
        ));
    }
    let source_name = source.file_name().ok_or(ArchiveOperationError::UnsafePath(
        "archive transaction source needs a file name",
    ))?;
    let destination_name = destination
        .file_name()
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive transaction target needs a file name",
        ))?;
    let parent = open(
        parent_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(super::create::map_errno)?;
    renameat_with(
        &parent,
        source_name,
        &parent,
        destination_name,
        RenameFlags::NOREPLACE,
    )
    .map_err(super::create::map_errno)?;
    rustix::fs::fsync(&parent).map_err(super::create::map_errno)
}

fn verify_staging_path(
    record: &JournalRecord,
    staging: &musheen_core::StorePath,
    destination: &musheen_core::StorePath,
) -> Result<(), ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    if checkpoint.staging_nonce().is_some_and(|nonce| {
        StagingPath::is_for_destination(
            staging,
            destination,
            record.job_id(),
            record.generation(),
            nonce,
        )
    }) {
        Ok(())
    } else {
        Err(ArchiveOperationError::UnsafePath(
            "archive recovery staging path is not owned by Musheen",
        ))
    }
}

fn finish_prepublish_cleanup<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let (source, quarantine, identity) =
        if checkpoint.cleanup_kind() == Some(ArchiveCleanupKind::PrepublishStage) {
            checkpoint_cleanup(checkpoint, ArchiveCleanupKind::PrepublishStage)?
        } else {
            let quarantine = checkpoint
                .stage_deletion()
                .map(local_path)
                .transpose()?
                .unwrap_or(deletion_path(staging)?);
            (
                staging.to_path_buf(),
                quarantine,
                checkpoint.staging_identity(),
            )
        };
    let cleanup = ArchiveCleanupIntent::new(
        ArchiveCleanupKind::PrepublishStage,
        &source,
        &quarantine,
        identity,
    );
    finish_prepublish_path(journal, record, staging, cleanup, budget, cancellation)
}

fn finish_prepublish_path<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    cleanup: ArchiveCleanupIntent<'_>,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    if !matches!(
        record.phase(),
        JournalPhase::PrepublishStageCleanupPlanned
            | JournalPhase::PrepublishStageCleanupQuarantined
    ) {
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            cleanup_phase(JournalPhase::PrepublishStageCleanupPlanned, cleanup),
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            Some(budget),
        )?;
    }
    let mut quarantined = || {
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            cleanup_phase(JournalPhase::PrepublishStageCleanupQuarantined, cleanup),
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            Some(budget),
        )
    };
    remove_owned_journaled(
        cleanup.source(),
        cleanup.quarantine(),
        cleanup.identity(),
        budget,
        cancellation,
        &mut quarantined,
    )?;
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::RolledBack,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        checkpoint.destination_before(),
        Some(budget),
    )?;
    Ok(ArchiveRecoveryOutcome::RolledBack)
}

fn finish_published_recovery<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    destination: &std::path::Path,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    verify_exact(
        checkpoint.destination_after(),
        path_identity_with_controls(destination, Some(budget), Some(cancellation))?,
    )?;
    let (source, quarantine, identity) =
        if checkpoint.cleanup_kind() == Some(ArchiveCleanupKind::PublishedDestination) {
            checkpoint_cleanup(checkpoint, ArchiveCleanupKind::PublishedDestination)?
        } else {
            let source = cleanup_path(staging)?;
            let quarantine = deletion_path(&source)?;
            (source, quarantine, checkpoint.destination_before())
        };
    finish_published(
        journal,
        record,
        staging,
        &source,
        &quarantine,
        identity,
        checkpoint.destination_after(),
        budget,
        cancellation,
    )?;
    Ok(ArchiveRecoveryOutcome::Completed)
}

fn checkpoint_cleanup(
    checkpoint: &musheen_ops::ArchiveCheckpoint,
    expected_kind: ArchiveCleanupKind,
) -> Result<
    (
        std::path::PathBuf,
        std::path::PathBuf,
        Option<ArchivePathIdentity>,
    ),
    ArchiveOperationError,
> {
    if checkpoint.cleanup_kind() != Some(expected_kind) {
        return Err(ArchiveOperationError::UnsafePath(
            "archive cleanup kind changed",
        ));
    }
    let source = checkpoint.cleanup().map(local_path).transpose()?.ok_or(
        ArchiveOperationError::UnsafePath("archive cleanup has no source path"),
    )?;
    let quarantine = checkpoint
        .cleanup_deletion()
        .map(local_path)
        .transpose()?
        .ok_or(ArchiveOperationError::UnsafePath(
            "archive cleanup has no quarantine path",
        ))?;
    Ok((source, quarantine, checkpoint.cleanup_identity()))
}

#[allow(clippy::too_many_arguments)]
fn finish_published<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    cleanup: &std::path::Path,
    cleanup_quarantine: &std::path::Path,
    expected_cleanup: Option<ArchivePathIdentity>,
    destination_after: Option<ArchivePathIdentity>,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let current_cleanup = path_identity_with_controls(cleanup, Some(budget), Some(cancellation))?;
    if current_cleanup.is_some() {
        verify_exact(expected_cleanup, current_cleanup)?;
    }
    if !matches!(
        record.phase(),
        JournalPhase::PublishedDestinationCleanupPlanned
            | JournalPhase::PublishedDestinationCleanupQuarantined
    ) {
        let cleanup_intent = ArchiveCleanupIntent::new(
            ArchiveCleanupKind::PublishedDestination,
            cleanup,
            cleanup_quarantine,
            expected_cleanup,
        );
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            cleanup_phase(
                JournalPhase::PublishedDestinationCleanupPlanned,
                cleanup_intent,
            ),
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            destination_after,
            Some(budget),
        )?;
    }
    {
        let cleanup_intent = ArchiveCleanupIntent::new(
            ArchiveCleanupKind::PublishedDestination,
            cleanup,
            cleanup_quarantine,
            expected_cleanup,
        );
        let mut quarantined = || {
            append_archive_phase(
                journal,
                record.job_id(),
                record.generation(),
                cleanup_phase(
                    JournalPhase::PublishedDestinationCleanupQuarantined,
                    cleanup_intent,
                ),
                checkpoint.plan(),
                staging,
                checkpoint.destination_before(),
                destination_after,
                Some(budget),
            )
        };
        remove_owned_journaled(
            cleanup,
            cleanup_quarantine,
            expected_cleanup,
            budget,
            cancellation,
            &mut quarantined,
        )?;
    }
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::StagingCleaned,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        destination_after,
        Some(budget),
    )?;
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::Completed,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        destination_after,
        Some(budget),
    )
}

fn recovery_budget(
    checkpoint: &musheen_ops::ArchiveCheckpoint,
    cancellation: &CancellationToken,
) -> ArchiveBudget {
    let limits = ArchiveOperationLimits {
        max_memory_bytes: checkpoint.identity_memory_limit(),
        max_identity_millis: checkpoint.identity_timeout_millis(),
        ..ArchiveOperationLimits::default()
    };
    ArchiveBudget::new(limits).with_identity_cancellation(cancellation.clone())
}

fn verify_exact(
    expected: Option<ArchivePathIdentity>,
    actual: Option<ArchivePathIdentity>,
) -> Result<(), ArchiveOperationError> {
    if expected == actual {
        Ok(())
    } else {
        Err(ArchiveOperationError::UnsafePath(
            "archive recovery path identity changed",
        ))
    }
}
