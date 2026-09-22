use super::ArchiveError;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveOperationLimits {
    pub max_entries: u64,
    pub max_expanded_bytes: u64,
    pub max_compression_ratio: u64,
    pub max_nesting: usize,
    pub max_path_bytes: usize,
    pub max_memory_bytes: u64,
    pub max_temporary_bytes: u64,
}

impl Default for ArchiveOperationLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_expanded_bytes: 20 * 1_024 * 1_024 * 1_024,
            max_compression_ratio: 1_000,
            max_nesting: 8,
            max_path_bytes: 4_096,
            max_memory_bytes: 512 * 1_024 * 1_024,
            max_temporary_bytes: 20 * 1_024 * 1_024 * 1_024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ArchiveBudgetCounters {
    pub entries: u64,
    pub expanded_bytes: u64,
    pub temporary_bytes: u64,
    pub memory_bytes: u64,
}

pub struct ArchiveBudget {
    limits: ArchiveOperationLimits,
    entries: u64,
    expanded_bytes: u64,
    temporary_bytes: u64,
    memory_bytes: Arc<AtomicU64>,
}

impl ArchiveBudget {
    #[must_use]
    pub fn new(limits: ArchiveOperationLimits) -> Self {
        Self {
            limits,
            entries: 0,
            expanded_bytes: 0,
            temporary_bytes: 0,
            memory_bytes: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn charge_entry(&mut self) -> Result<(), ArchiveOperationError> {
        self.entries = checked_charge("entries", self.entries, 1, self.limits.max_entries)?;
        Ok(())
    }

    pub fn check_path(&self, path: &[u8]) -> Result<(), ArchiveOperationError> {
        let value = u64::try_from(path.len()).unwrap_or(u64::MAX);
        let maximum = u64::try_from(self.limits.max_path_bytes).unwrap_or(u64::MAX);
        if value > maximum {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "path bytes",
                value,
                maximum,
            });
        }
        Ok(())
    }

    pub fn charge_expanded(
        &mut self,
        bytes: u64,
        compressed_bytes: u64,
    ) -> Result<(), ArchiveOperationError> {
        let next =
            self.expanded_bytes
                .checked_add(bytes)
                .ok_or(ArchiveOperationError::LimitExceeded {
                    resource: "expanded bytes",
                    value: u64::MAX,
                    maximum: self.limits.max_expanded_bytes,
                })?;
        if next > self.limits.max_expanded_bytes {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "expanded bytes",
                value: next,
                maximum: self.limits.max_expanded_bytes,
            });
        }
        let ratio_limit = compressed_bytes.saturating_mul(self.limits.max_compression_ratio);
        if next > ratio_limit {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "compression ratio",
                value: next,
                maximum: ratio_limit,
            });
        }
        self.expanded_bytes = next;
        Ok(())
    }

    pub fn check_nesting(&self, nesting: usize) -> Result<(), ArchiveOperationError> {
        if nesting > self.limits.max_nesting {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "archive nesting",
                value: u64::try_from(nesting).unwrap_or(u64::MAX),
                maximum: u64::try_from(self.limits.max_nesting).unwrap_or(u64::MAX),
            });
        }
        Ok(())
    }

    pub fn reserve_memory(&self, bytes: u64) -> Result<ArchiveMemoryLease, ArchiveOperationError> {
        self.memory_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= self.limits.max_memory_bytes)
            })
            .map_err(|current| ArchiveOperationError::LimitExceeded {
                resource: "memory bytes",
                value: current.saturating_add(bytes),
                maximum: self.limits.max_memory_bytes,
            })?;
        Ok(ArchiveMemoryLease {
            bytes,
            counter: Arc::clone(&self.memory_bytes),
        })
    }

    pub fn charge_temporary(&mut self, bytes: u64) -> Result<(), ArchiveOperationError> {
        self.temporary_bytes = checked_charge(
            "temporary bytes",
            self.temporary_bytes,
            bytes,
            self.limits.max_temporary_bytes,
        )?;
        Ok(())
    }

    #[must_use]
    pub fn counters(&self) -> ArchiveBudgetCounters {
        ArchiveBudgetCounters {
            entries: self.entries,
            expanded_bytes: self.expanded_bytes,
            temporary_bytes: self.temporary_bytes,
            memory_bytes: self.memory_bytes.load(Ordering::Acquire),
        }
    }
}

fn checked_charge(
    resource: &'static str,
    current: u64,
    amount: u64,
    maximum: u64,
) -> Result<u64, ArchiveOperationError> {
    let next = current
        .checked_add(amount)
        .ok_or(ArchiveOperationError::LimitExceeded {
            resource,
            value: u64::MAX,
            maximum,
        })?;
    if next > maximum {
        return Err(ArchiveOperationError::LimitExceeded {
            resource,
            value: next,
            maximum,
        });
    }
    Ok(next)
}

#[derive(Debug)]
pub struct ArchiveMemoryLease {
    bytes: u64,
    counter: Arc<AtomicU64>,
}

impl Drop for ArchiveMemoryLease {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ArchiveOperationError {
    LimitExceeded {
        resource: &'static str,
        value: u64,
        maximum: u64,
    },
    UnsafePath(&'static str),
    UnsupportedFileType,
    UnsupportedName,
    Conflict,
    PasswordRequired,
    InvalidPassword,
    Cancelled,
    NoSpace,
    InvalidArchive,
    Io,
    Journal,
}

impl fmt::Display for ArchiveOperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LimitExceeded {
                resource,
                value,
                maximum,
            } => write!(
                formatter,
                "{resource} limit exceeded: {value} is greater than {maximum}"
            ),
            Self::UnsafePath(reason) => formatter.write_str(reason),
            Self::UnsupportedFileType => {
                formatter.write_str("archives support regular files and directories only")
            }
            Self::UnsupportedName => {
                formatter.write_str("the archive format cannot represent this file name")
            }
            Self::Conflict => formatter.write_str("the archive destination already exists"),
            Self::PasswordRequired => formatter.write_str("the archive requires a password"),
            Self::InvalidPassword => formatter.write_str("the archive password is invalid"),
            Self::Cancelled => formatter.write_str("the archive operation was cancelled"),
            Self::NoSpace => formatter.write_str("the destination filesystem is out of space"),
            Self::InvalidArchive => formatter.write_str("the archive is invalid"),
            Self::Io => formatter.write_str("the archive operation failed"),
            Self::Journal => formatter.write_str("the archive journal could not be updated"),
        }
    }
}

impl Error for ArchiveOperationError {}

impl From<ArchiveError> for ArchiveOperationError {
    fn from(error: ArchiveError) -> Self {
        match error {
            ArchiveError::LimitExceeded {
                resource,
                value,
                maximum,
            } => Self::LimitExceeded {
                resource,
                value: u64::try_from(value).unwrap_or(u64::MAX),
                maximum: u64::try_from(maximum).unwrap_or(u64::MAX),
            },
            ArchiveError::UnsafePath(reason) => Self::UnsafePath(reason),
            ArchiveError::PasswordRequired => Self::PasswordRequired,
            ArchiveError::InvalidPassword => Self::InvalidPassword,
            ArchiveError::Cancelled => Self::Cancelled,
            ArchiveError::InvalidArchive | ArchiveError::DuplicatePath => Self::InvalidArchive,
            ArchiveError::NotArchiveEntry
            | ArchiveError::UnsupportedNestedFormat
            | ArchiveError::Io => Self::Io,
        }
    }
}

impl From<musheen_core::StoreError> for ArchiveOperationError {
    fn from(error: musheen_core::StoreError) -> Self {
        match error {
            musheen_core::StoreError::Cancelled => Self::Cancelled,
            _ => Self::Io,
        }
    }
}

pub(crate) fn map_io(error: &std::io::Error) -> ArchiveOperationError {
    if error.raw_os_error() == Some(28) {
        ArchiveOperationError::NoSpace
    } else if error.kind() == std::io::ErrorKind::Interrupted {
        ArchiveOperationError::Cancelled
    } else {
        ArchiveOperationError::Io
    }
}
