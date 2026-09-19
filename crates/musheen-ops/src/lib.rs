//! Recoverable file-operation engine.

mod copy;
mod event;
mod job;
mod journal;
mod metadata_copy;
mod r#move;
mod plan;
mod recovery;
mod scheduler;
mod staging;
mod state;
mod verify;

pub use event::{JobEvent, Progress, ProgressError, ProgressUnit};
pub use job::{EventGeneration, JobId};
pub use journal::{
    CorruptSource, Durability, Journal, JournalError, JournalPhase, JournalRecord, JournalStorage,
    StorageAction,
};
pub use metadata_copy::{MetadataKind, MetadataReport};
pub use r#move::{MoveOutcome, MoveStrategy, execute_move};
pub use plan::{
    InverseTemplate, OperationKind, OperationPlan, PlanError, ProviderLimits, ProviderSnapshot,
    WorkClass,
};
pub use recovery::{RecoveryContext, RecoveryDecision, decide_recovery};
pub use scheduler::{Clock, ScheduledJob, Scheduler, SchedulerError, SystemClock};
pub use staging::{StagingError, StagingPath};
pub use state::{JobState, JobStateMachine, StateError};
pub use verify::source_unchanged;

pub use copy::{
    CopyCapabilities, CopyOptions, CopyOutcome, CopyProvider, CopyRequest, CopySession,
    CopyStrategy, EntryKind, EntrySnapshot, FailureKind, OperationFailure, ProviderError,
    PublicationState, SourceMetadata, SourceState,
};
