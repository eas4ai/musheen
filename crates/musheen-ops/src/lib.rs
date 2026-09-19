//! Recoverable file-operation engine.

mod event;
mod job;
mod journal;
mod plan;
mod recovery;
mod scheduler;
mod staging;
mod state;

pub use event::{JobEvent, Progress, ProgressError, ProgressUnit};
pub use job::{EventGeneration, JobId};
pub use journal::{
    CorruptSource, Durability, Journal, JournalError, JournalPhase, JournalRecord, JournalStorage,
    StorageAction,
};
pub use plan::{
    InverseTemplate, OperationKind, OperationPlan, PlanError, ProviderLimits, ProviderSnapshot,
    WorkClass,
};
pub use recovery::{RecoveryContext, RecoveryDecision, decide_recovery};
pub use scheduler::{Clock, ScheduledJob, Scheduler, SchedulerError, SystemClock};
pub use staging::{StagingError, StagingPath};
pub use state::{JobState, JobStateMachine, StateError};
