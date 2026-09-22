use super::budget::ArchiveOperationError;
use super::create::{
    append_archive_phase, local_path, path_identity, publish_staging, remove_owned, sync_parent,
};
use musheen_ops::{
    ArchivePathIdentity, Journal, JournalPhase, JournalRecord, JournalStorage, StagingPath,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveRecoveryOutcome {
    RolledBack,
    Completed,
}

/// Recovers the latest durable checkpoint for each unfinished archive job.
pub fn recover_archive_operations<S: JournalStorage>(
    journal: &mut Journal<S>,
) -> Result<Vec<ArchiveRecoveryOutcome>, ArchiveOperationError> {
    let mut latest = std::collections::BTreeMap::new();
    for record in journal.records() {
        if record.archive_checkpoint().is_some() {
            latest.insert((record.job_id(), record.generation()), record.clone());
        }
    }
    let records = latest.into_values().collect::<Vec<_>>();
    let mut outcomes = Vec::new();
    for record in records {
        if let Some(outcome) = recover_record(journal, &record)? {
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}

fn recover_record<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
) -> Result<Option<ArchiveRecoveryOutcome>, ArchiveOperationError> {
    if matches!(
        record.phase(),
        JournalPhase::Completed | JournalPhase::RolledBack
    ) {
        return Ok(None);
    }
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    let plan = checkpoint.plan();
    let staging = local_path(checkpoint.staging())?;
    let expected_staging =
        StagingPath::for_destination(plan.destination(), record.job_id(), record.generation())
            .map_err(|_| {
                ArchiveOperationError::UnsafePath("invalid archive recovery staging path")
            })?;
    if expected_staging.path() != checkpoint.staging() {
        return Err(ArchiveOperationError::UnsafePath(
            "archive recovery staging path is not owned by Musheen",
        ));
    }
    let destination = local_path(plan.destination())?;

    match record.phase() {
        JournalPhase::Planned => {
            let _ = path_identity(&staging)?;
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
            )?;
            Ok(Some(ArchiveRecoveryOutcome::RolledBack))
        }
        JournalPhase::StagingCreated => {
            verify_stable_object(checkpoint.staging_identity(), path_identity(&staging)?)?;
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
            )?;
            Ok(Some(ArchiveRecoveryOutcome::RolledBack))
        }
        JournalPhase::DataCopied | JournalPhase::MetadataApplied => {
            let current_staging = path_identity(&staging)?;
            let current_destination = path_identity(&destination)?;
            let publish_already_happened = current_destination == checkpoint.staging_identity()
                && (current_staging == checkpoint.destination_before()
                    || (checkpoint.destination_before().is_none() && current_staging.is_none()));
            if publish_already_happened {
                append_archive_phase(
                    journal,
                    record.job_id(),
                    record.generation(),
                    JournalPhase::DestinationPublished,
                    plan,
                    &staging,
                    checkpoint.destination_before(),
                    current_destination,
                )?;
                finish_published(journal, record, &staging, current_destination)?;
                return Ok(Some(ArchiveRecoveryOutcome::Completed));
            }
            verify_exact(checkpoint.staging_identity(), current_staging)?;
            verify_exact(checkpoint.destination_before(), current_destination)?;
            let outcome = publish_staging(&staging, &destination, plan.conflict_policy())?;
            if matches!(outcome, super::create::ArchiveOperationOutcome::Skipped) {
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
                )?;
                return Ok(Some(ArchiveRecoveryOutcome::RolledBack));
            }
            sync_parent(&destination)?;
            let destination_after = path_identity(&destination)?;
            append_archive_phase(
                journal,
                record.job_id(),
                record.generation(),
                JournalPhase::DestinationPublished,
                plan,
                &staging,
                checkpoint.destination_before(),
                destination_after,
            )?;
            finish_published(journal, record, &staging, destination_after)?;
            Ok(Some(ArchiveRecoveryOutcome::Completed))
        }
        JournalPhase::DestinationPublished => {
            verify_exact(checkpoint.destination_after(), path_identity(&destination)?)?;
            let current_staging = path_identity(&staging)?;
            if current_staging.is_some() {
                verify_exact(checkpoint.staging_identity(), current_staging)?;
            }
            finish_published(journal, record, &staging, checkpoint.destination_after())?;
            Ok(Some(ArchiveRecoveryOutcome::Completed))
        }
        JournalPhase::StagingCleaned => {
            verify_exact(checkpoint.destination_after(), path_identity(&destination)?)?;
            verify_exact(None, path_identity(&staging)?)?;
            append_archive_phase(
                journal,
                record.job_id(),
                record.generation(),
                JournalPhase::Completed,
                plan,
                &staging,
                checkpoint.destination_before(),
                checkpoint.destination_after(),
            )?;
            Ok(Some(ArchiveRecoveryOutcome::Completed))
        }
        JournalPhase::SourceRemoved => Err(ArchiveOperationError::InvalidArchive),
        JournalPhase::Completed | JournalPhase::RolledBack => Ok(None),
    }
}

fn finish_published<S: JournalStorage>(
    journal: &mut Journal<S>,
    record: &JournalRecord,
    staging: &std::path::Path,
    destination_after: Option<ArchivePathIdentity>,
) -> Result<(), ArchiveOperationError> {
    let checkpoint = record
        .archive_checkpoint()
        .ok_or(ArchiveOperationError::InvalidArchive)?;
    remove_owned(staging)?;
    append_archive_phase(
        journal,
        record.job_id(),
        record.generation(),
        JournalPhase::StagingCleaned,
        checkpoint.plan(),
        staging,
        checkpoint.destination_before(),
        destination_after,
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

fn verify_stable_object(
    expected: Option<ArchivePathIdentity>,
    actual: Option<ArchivePathIdentity>,
) -> Result<(), ArchiveOperationError> {
    match (expected, actual) {
        (None, None) => Ok(()),
        (Some(expected), Some(actual))
            if expected.device() == actual.device()
                && expected.inode() == actual.inode()
                && expected.is_directory() == actual.is_directory() =>
        {
            Ok(())
        }
        _ => Err(ArchiveOperationError::UnsafePath(
            "archive recovery staging identity changed",
        )),
    }
}
