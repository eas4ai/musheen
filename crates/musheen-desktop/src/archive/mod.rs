//! Bounded, read-only archive browsing.

mod format;
mod path;
mod store;

pub use format::ArchiveFormat;
pub use path::ArchivePath;
pub use store::{
    ArchiveCounters, ArchiveError, ArchiveLimits, ArchivePassword, ArchivePasswordProvider,
    ArchiveStore, PasswordRequest,
};
