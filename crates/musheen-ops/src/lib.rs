//! Recoverable file-operation engine.

mod batch_rename;
mod conflict;
mod copy;
mod create;
mod delete;
mod event;
mod job;
mod journal;
mod link;
mod metadata;
mod metadata_copy;
mod r#move;
mod mutation;
mod plan;
mod recovery;
mod remote;
mod rename;
mod scheduler;
mod staging;
mod state;
mod verify;

pub use batch_rename::{BatchRenameJournal, BatchRenamePlan, BatchRenameStep, RenameMapping};
pub use create::{CreateKind, CreateRequest, NameError, execute_create, validate_local_name};
pub use delete::{
    DeleteFailure, DeleteOutcome, DeleteProvider, DeleteTarget, PermanentDeleteChallenge,
    PermanentDeleteConfirmation, PermanentDeleteRequest, TrashReceipt, execute_delete,
    execute_permanent_delete, execute_restore,
};
pub use event::{ArchiveEventPhase, JobEvent, Progress, ProgressError, ProgressUnit};
pub use job::{EventGeneration, JobId};
pub use journal::{
    ArchiveCheckpoint, ArchiveCleanupKind, ArchivePathIdentity, CorruptSource, Durability, Journal,
    JournalError, JournalPhase, JournalRecord, JournalStorage, StorageAction,
};
pub use link::{
    HardLinkRequest, LinkProvider, SymbolicLinkRequest, execute_hard_link, execute_symbolic_link,
};
pub use metadata::{
    AclChange, AclEntry, AclQualifier, MetadataChange, MetadataEntry, MetadataEntryKind,
    MetadataPlan, MetadataProvider, MetadataScope, ResolvedMetadataChange,
};
pub use metadata_copy::{MetadataKind, MetadataReport};
pub use r#move::{
    MoveMetadataReview, MoveOutcome, MoveStrategy, complete_move_after_metadata_review,
    execute_move,
};
pub use mutation::{MutationError, MutationProvider};
pub use plan::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveOperationPlan, ExtractMerge, InverseTemplate,
    OperationKind, OperationPlan, PlanError, ProviderLimits, ProviderSnapshot, WorkClass,
};
pub use recovery::{RecoveryContext, RecoveryDecision, decide_recovery};
pub use remote::{
    RemoteTransferCapabilities, RemoteTransferGap, RemoteTransferPlan, RemoteTransferPlanError,
    RemoteTransferStrategy, ResumePolicy,
};
pub use rename::{RenameRequest, execute_rename};
pub use scheduler::{Clock, ScheduledJob, Scheduler, SchedulerError, SystemClock};
pub use staging::{StagingError, StagingPath};
pub use state::{JobState, JobStateMachine, StateError};
pub use verify::source_unchanged;

pub use conflict::{
    ApplyScope, ConflictChoice, ConflictDecision, ConflictDecisionJournal, ConflictError,
    ConflictItemKind, ConflictPolicies, ConflictRecord,
};
pub use copy::{
    CopyCapabilities, CopyOptions, CopyOutcome, CopyProvider, CopyRequest, CopySession,
    CopyStrategy, EntryKind, EntrySnapshot, FailureKind, OperationFailure, ProviderError,
    PublicationState, SourceMetadata, SourceRemovalToken, SourceState,
};
