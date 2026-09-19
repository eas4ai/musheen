//! Recoverable file-operation engine.

mod event;
mod job;
mod plan;
mod scheduler;
mod state;

pub use event::{JobEvent, Progress, ProgressError, ProgressUnit};
pub use job::{EventGeneration, JobId};
pub use plan::{
    InverseTemplate, OperationKind, OperationPlan, PlanError, ProviderLimits, ProviderSnapshot,
    WorkClass,
};
pub use scheduler::{Clock, ScheduledJob, Scheduler, SchedulerError, SystemClock};
pub use state::{JobState, JobStateMachine, StateError};
