use crate::JournalPhase;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryDecision {
    /// Offer continuation after the user reviews the interrupted job.
    Resume,
    /// Offer removal of app-owned work after the user reviews what remains.
    Rollback,
    /// Require manual review because no safe action can be recommended.
    Ask,
    NoAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryContext {
    pub continuation_verified: bool,
    pub staging_owned: bool,
    pub destination_verified: bool,
    pub source_identity_current: bool,
}

/// Determines which recovery action is safe to offer.
///
/// A decision does not authorize storage mutation. Restarted jobs remain
/// interrupted until the user explicitly chooses the offered action.
#[must_use]
pub const fn decide_recovery(phase: JournalPhase, context: RecoveryContext) -> RecoveryDecision {
    match phase {
        JournalPhase::Planned => RecoveryDecision::Rollback,
        JournalPhase::StagingCreated | JournalPhase::DataCopied | JournalPhase::MetadataApplied => {
            if context.staging_owned {
                RecoveryDecision::Rollback
            } else {
                RecoveryDecision::Ask
            }
        }
        JournalPhase::CleanupPlanned | JournalPhase::RecoveryRequired => RecoveryDecision::Ask,
        JournalPhase::DestinationPublished => {
            if context.continuation_verified
                && context.destination_verified
                && context.source_identity_current
            {
                RecoveryDecision::Resume
            } else {
                RecoveryDecision::Ask
            }
        }
        JournalPhase::CleanupQuarantined
        | JournalPhase::SourceRemoved
        | JournalPhase::StagingCleaned => {
            if context.continuation_verified && context.destination_verified {
                RecoveryDecision::Resume
            } else {
                RecoveryDecision::Ask
            }
        }
        JournalPhase::Completed | JournalPhase::RolledBack => RecoveryDecision::NoAction,
    }
}
