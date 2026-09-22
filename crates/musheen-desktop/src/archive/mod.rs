//! Bounded, read-only archive browsing.

mod format;
mod index;
mod io;
#[cfg(feature = "archive-libarchive")]
mod libarchive_codec;
mod path;
mod seven_codec;
mod store;
mod tar_codec;
mod zip_codec;

pub use format::ArchiveFormat;
#[cfg(feature = "archive-libarchive")]
#[doc(hidden)]
pub use libarchive_codec::run_worker as run_libarchive_worker;
pub use path::ArchivePath;
pub use store::{
    ArchiveCounters, ArchiveError, ArchiveLimits, ArchivePassword, ArchivePasswordProvider,
    ArchiveStore, PasswordRequest,
};
