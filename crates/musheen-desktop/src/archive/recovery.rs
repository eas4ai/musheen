use super::budget::{ArchiveBudget, ArchiveOperationError, ArchiveOperationLimits};
use super::create::{
    append_archive_phase, cleanup_path, deletion_path, local_path, path_identity_with_controls,
    publish_staging, remove_owned_with_controls, sync_parent,
};
use musheen_core::CancellationToken;
use musheen_ops::{
    ArchivePathIdentity, EventGeneration, JobId, Journal, JournalPhase, JournalRecord,
    JournalStorage, RecoveryDecision, StagingPath,
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
        | JournalPhase::CleanupQuarantined
        | JournalPhase::StagingCleaned
            if current_destination == checkpoint.destination_after() =>
        {
            RecoveryDecision::Resume
        }
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
            remove_owned_with_controls(
                &staging,
                checkpoint.staging_identity(),
                &budget,
                cancellation,
            )?;
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
        (JournalPhase::DataCopied, ArchiveRecoveryAction::Resume)
        | (JournalPhase::MetadataApplied, ArchiveRecoveryAction::Resume)
        | (JournalPhase::CleanupPlanned, ArchiveRecoveryAction::Resume)
        | (JournalPhase::RecoveryRequired, ArchiveRecoveryAction::Resume) => resume_publish(
            journal,
            record,
            &staging,
            &destination,
            &budget,
            cancellation,
        ),
        (JournalPhase::RecoveryRequired, ArchiveRecoveryAction::Rollback) => {
            verify_exact(
                checkpoint.destination_before(),
                path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?,
            )?;
            let cleanup = checkpoint
                .cleanup()
                .map(local_path)
                .transpose()?
                .unwrap_or(cleanup_path(&staging)?);
            remove_owned_with_controls(
                &cleanup,
                checkpoint.cleanup_identity(),
                &budget,
                cancellation,
            )?;
            let current_staging =
                path_identity_with_controls(&staging, Some(&budget), Some(cancellation))?;
            if current_staging == checkpoint.staging_identity() {
                remove_owned_with_controls(
                    &staging,
                    checkpoint.staging_identity(),
                    &budget,
                    cancellation,
                )?;
            }
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
        (JournalPhase::DestinationPublished, ArchiveRecoveryAction::Resume)
        | (JournalPhase::CleanupQuarantined, ArchiveRecoveryAction::Resume) => {
            verify_exact(
                checkpoint.destination_after(),
                path_identity_with_controls(&destination, Some(&budget), Some(cancellation))?,
            )?;
            let cleanup = checkpoint
                .cleanup()
                .map(local_path)
                .transpose()?
                .unwrap_or_else(|| staging.clone());
            finish_published(
                journal,
                record,
                &staging,
                &cleanup,
                checkpoint
                    .cleanup_identity()
                    .or(checkpoint.staging_identity()),
                checkpoint.destination_after(),
                &budget,
                cancellation,
            )?;
            Ok(ArchiveRecoveryOutcome::Completed)
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
    let cleanup = checkpoint
        .cleanup()
        .map(local_path)
        .transpose()?
        .unwrap_or(cleanup_path(staging)?);
    let current_cleanup = path_identity_with_controls(&cleanup, Some(budget), Some(cancellation))?;
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
            current_cleanup,
            current_destination,
            budget,
            cancellation,
        )?;
        return Ok(ArchiveRecoveryOutcome::Completed);
    }
    verify_exact(checkpoint.staging_identity(), current_staging)?;
    verify_exact(checkpoint.destination_before(), current_destination)?;
    verify_exact(None, current_cleanup)?;
    let outcome = publish_staging(
        staging,
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
    )?;
    if matches!(outcome, super::create::ArchiveOperationOutcome::Skipped) {
        remove_owned_with_controls(staging, checkpoint.staging_identity(), budget, cancellation)?;
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::RolledBack,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            Some(budget),
        )?;
        return Ok(ArchiveRecoveryOutcome::RolledBack);
    }
    sync_parent(destination)?;
    let destination_after =
        path_identity_with_controls(destination, Some(budget), Some(cancellation))?;
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::DestinationPublished,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        destination_after,
        Some(budget),
    )?;
    let cleanup_after = path_identity_with_controls(&cleanup, Some(budget), Some(cancellation))?;
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::CleanupQuarantined,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        destination_after,
        Some(budget),
    )?;
    finish_published(
        journal,
        record,
        staging,
        &cleanup,
        cleanup_after,
        destination_after,
        budget,
        cancellation,
    )?;
    Ok(ArchiveRecoveryOutcome::Completed)
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

#[allow(clippy::too_many_arguments)]
fn finish_published<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    cleanup: &std::path::Path,
    expected_cleanup: Option<ArchivePathIdentity>,
    destination_after: Option<ArchivePathIdentity>,
    budget: &ArchiveBudget,
    cancellation: &CancellationToken,
) -> Result<(), ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let current_cleanup = path_identity_with_controls(cleanup, Some(budget), Some(cancellation))?;
    if let Some(recorded) = checkpoint.cleanup_deletion()
        && local_path(recorded)? != deletion_path(cleanup)?
    {
        return Err(ArchiveOperationError::UnsafePath(
            "archive cleanup quarantine target changed",
        ));
    }
    if current_cleanup.is_some() {
        verify_exact(expected_cleanup, current_cleanup)?;
    }
    remove_owned_with_controls(cleanup, expected_cleanup, budget, cancellation)?;
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
