use super::budget::ArchiveOperationError;
use super::create::{
    append_archive_phase, local_path, path_identity, publish_staging, remove_owned, sync_parent,
};
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
    latest_archive_records(journal)
        .into_iter()
        .filter(|record| {
            !matches!(
                record.phase(),
                JournalPhase::Completed | JournalPhase::RolledBack
            )
        })
        .map(recovery_request)
        .collect()
}

/// Applies one action that the caller obtained and explicitly approved.
pub fn apply_archive_recovery<S: JournalStorage>(
    journal: &mut Journal<S>,
    request: &ArchiveRecoveryRequest,
    action: ArchiveRecoveryAction,
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let record = latest_archive_records(journal)
        .into_iter()
        .find(|record| {
            record.job_id() == request.job_id
                && record.generation() == request.generation
                && record.phase() == request.phase
        })
        .ok_or(ArchiveOperationError::RecoveryConsentRequired)?;
    apply_record(journal, &record, action)
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
) -> Result<ArchiveRecoveryRequest, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let plan = checkpoint.plan();
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
    let staging_matches =
        owned_path && path_identity(&staging).ok() == Some(checkpoint.staging_identity());
    let destination_matches =
        path_identity(&destination).ok() == Some(checkpoint.destination_before());
    let recommended = match record.phase() {
        JournalPhase::Planned => RecoveryDecision::Rollback,
        JournalPhase::StagingCreated | JournalPhase::DataCopied | JournalPhase::MetadataApplied
            if staging_matches && destination_matches =>
        {
            RecoveryDecision::Rollback
        }
        JournalPhase::DestinationPublished | JournalPhase::StagingCleaned
            if path_identity(&destination).ok() == Some(checkpoint.destination_after()) =>
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
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let plan = checkpoint.plan();
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
                None,
            )?;
            Ok(ArchiveRecoveryOutcome::RolledBack)
        }
        (JournalPhase::StagingCreated, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::DataCopied, ArchiveRecoveryAction::Rollback)
        | (JournalPhase::MetadataApplied, ArchiveRecoveryAction::Rollback) => {
            verify_exact(checkpoint.staging_identity(), path_identity(&staging)?)?;
            verify_exact(
                checkpoint.destination_before(),
                path_identity(&destination)?,
            )?;
            remove_owned(&staging)?;
            append_archive_phase(
                journal,
                record.job_id(),
                record.generation(),
                JournalPhase::RolledBack,
                plan,
                &staging,
                checkpoint.destination_before(),
                None,
                None,
            )?;
            Ok(ArchiveRecoveryOutcome::RolledBack)
        }
        (JournalPhase::DataCopied, ArchiveRecoveryAction::Resume)
        | (JournalPhase::MetadataApplied, ArchiveRecoveryAction::Resume) => {
            resume_publish(journal, record, &staging, &destination)
        }
        (JournalPhase::DestinationPublished, ArchiveRecoveryAction::Resume) => {
            verify_exact(checkpoint.destination_after(), path_identity(&destination)?)?;
            finish_published(
                journal,
                record,
                &staging,
                checkpoint.staging_identity(),
                checkpoint.destination_after(),
            )?;
            Ok(ArchiveRecoveryOutcome::Completed)
        }
        (JournalPhase::StagingCleaned, ArchiveRecoveryAction::Resume) => {
            verify_exact(checkpoint.destination_after(), path_identity(&destination)?)?;
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
                None,
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
) -> Result<ArchiveRecoveryOutcome, ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let current_staging = path_identity(staging)?;
    let current_destination = path_identity(destination)?;
    let publish_already_happened = current_destination == checkpoint.staging_identity()
        && (current_staging == checkpoint.destination_before()
            || (checkpoint.destination_before().is_none() && current_staging.is_none()));
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
            None,
        )?;
        finish_published(
            journal,
            record,
            staging,
            current_staging,
            current_destination,
        )?;
        return Ok(ArchiveRecoveryOutcome::Completed);
    }
    verify_exact(checkpoint.staging_identity(), current_staging)?;
    verify_exact(checkpoint.destination_before(), current_destination)?;
    let outcome = publish_staging(
        staging,
        destination,
        checkpoint.plan().conflict_policy(),
        checkpoint.destination_before(),
    )?;
    if matches!(outcome, super::create::ArchiveOperationOutcome::Skipped) {
        remove_owned(staging)?;
        append_archive_phase(
            journal,
            record.job_id(),
            record.generation(),
            JournalPhase::RolledBack,
            checkpoint.plan(),
            staging,
            checkpoint.destination_before(),
            None,
            None,
        )?;
        return Ok(ArchiveRecoveryOutcome::RolledBack);
    }
    sync_parent(destination)?;
    let destination_after = path_identity(destination)?;
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::DestinationPublished,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        destination_after,
        None,
    )?;
    let staging_after = path_identity(staging)?;
    finish_published(journal, record, staging, staging_after, destination_after)?;
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

fn finish_published<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    expected_staging: Option<ArchivePathIdentity>,
    destination_after: Option<ArchivePathIdentity>,
) -> Result<(), ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let current_staging = path_identity(staging)?;
    if current_staging.is_some() {
        verify_exact(expected_staging, current_staging)?;
        remove_owned(staging)?;
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
        None,
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
        None,
    )
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
