//! Bounded, read-only archive browsing.

mod budget;
mod create;
mod extract;
mod format;
mod index;
mod io;
#[cfg(feature = "archive-libarchive")]
mod libarchive_codec;
mod path;
mod recovery;
mod seven_codec;
mod store;
mod tar_codec;
mod workspace;
mod zip_codec;

pub use budget::{
    ArchiveBudget, ArchiveBudgetCounters, ArchiveMemoryLease, ArchiveOperationAccounting,
    ArchiveOperationError, ArchiveOperationLimits,
};
pub use create::{
    ArchiveOperationOutcome, execute_archive_plan, execute_scheduled_archive_operation,
    execute_scheduled_archive_operation_with_accounting,
};
pub use format::ArchiveFormat;
#[cfg(feature = "archive-libarchive")]
#[doc(hidden)]
pub use libarchive_codec::run_worker as run_libarchive_worker;
pub use path::ArchivePath;
pub use recovery::{
    ArchiveRecoveryAction, ArchiveRecoveryOutcome, ArchiveRecoveryRequest, apply_archive_recovery,
    apply_archive_recovery_with_accounting, apply_archive_recovery_with_cancellation,
    recover_archive_operations, recover_archive_operations_with_cancellation,
};
pub use store::{
    ArchiveCounters, ArchiveError, ArchiveLimits, ArchivePassword, ArchivePasswordProvider,
    ArchiveStore, PasswordRequest,
};
