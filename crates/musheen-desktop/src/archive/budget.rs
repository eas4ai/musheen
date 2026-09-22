use super::ArchiveError;
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};

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
    pub compressed_bytes: u64,
    pub compression_ratio_checks: u64,
    pub temporary_bytes: u64,
    pub memory_bytes: u64,
    pub peak_memory_bytes: u64,
    pub max_nesting: u64,
    pub max_path_bytes: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ArchiveOperationAccounting {
    evidence: Arc<BudgetEvidence>,
}

#[derive(Debug, Default)]
struct BudgetEvidence {
    entries: AtomicU64,
    expanded_bytes: AtomicU64,
    compressed_bytes: AtomicU64,
    compression_ratio_checks: AtomicU64,
    temporary_bytes: AtomicU64,
    memory_bytes: AtomicU64,
    peak_memory_bytes: AtomicU64,
    max_nesting: AtomicU64,
    max_path_bytes: AtomicU64,
    stage_write_remaining: AtomicU64,
    stage_write_errno: AtomicI32,
}

impl ArchiveOperationAccounting {
    #[must_use]
    pub fn counters(&self) -> ArchiveBudgetCounters {
        ArchiveBudgetCounters {
            entries: self.evidence.entries.load(Ordering::Acquire),
            expanded_bytes: self.evidence.expanded_bytes.load(Ordering::Acquire),
            compressed_bytes: self.evidence.compressed_bytes.load(Ordering::Acquire),
            compression_ratio_checks: self
                .evidence
                .compression_ratio_checks
                .load(Ordering::Acquire),
            temporary_bytes: self.evidence.temporary_bytes.load(Ordering::Acquire),
            memory_bytes: self.evidence.memory_bytes.load(Ordering::Acquire),
            peak_memory_bytes: self.evidence.peak_memory_bytes.load(Ordering::Acquire),
            max_nesting: self.evidence.max_nesting.load(Ordering::Acquire),
            max_path_bytes: self.evidence.max_path_bytes.load(Ordering::Acquire),
        }
    }

    /// Configures a deterministic storage-boundary failure for integration tests.
    #[doc(hidden)]
    pub fn inject_stage_write_error_after(&self, bytes: u64, raw_os_error: i32) {
        self.evidence
            .stage_write_remaining
            .store(bytes, Ordering::Release);
        self.evidence
            .stage_write_errno
            .store(raw_os_error, Ordering::Release);
    }

    fn check_stage_write(&self, bytes: u64) -> std::io::Result<()> {
        let errno = self.evidence.stage_write_errno.load(Ordering::Acquire);
        if errno == 0 {
            return Ok(());
        }
        self.evidence
            .stage_write_remaining
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                remaining.checked_sub(bytes)
            })
            .map(|_| ())
            .map_err(|_| std::io::Error::from_raw_os_error(errno))
    }
}

pub struct ArchiveBudget {
    limits: ArchiveOperationLimits,
    entries: u64,
    expanded_bytes: u64,
    temporary_bytes: u64,
    memory: SharedMemoryBudget,
    accounting: ArchiveOperationAccounting,
}

#[derive(Clone, Debug)]
pub(crate) struct SharedMemoryBudget {
    evidence: Arc<BudgetEvidence>,
    maximum: u64,
}

impl SharedMemoryBudget {
    fn new(maximum: u64, evidence: Arc<BudgetEvidence>) -> Self {
        Self { evidence, maximum }
    }

    pub(crate) fn reserve(&self, bytes: u64) -> Result<ArchiveMemoryLease, ArchiveOperationError> {
        self.reserve_raw(bytes)?;
        Ok(ArchiveMemoryLease {
            bytes,
            memory: self.clone(),
        })
    }

    pub(crate) fn reserve_raw(&self, bytes: u64) -> Result<(), ArchiveOperationError> {
        let previous = self
            .evidence
            .memory_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                current
                    .checked_add(bytes)
                    .filter(|next| *next <= self.maximum)
            })
            .map_err(|current| ArchiveOperationError::LimitExceeded {
                resource: "memory bytes",
                value: current.saturating_add(bytes),
                maximum: self.maximum,
            })?;
        self.evidence
            .peak_memory_bytes
            .fetch_max(previous.saturating_add(bytes), Ordering::AcqRel);
        Ok(())
    }

    pub(crate) fn release_raw(&self, bytes: u64) {
        self.evidence
            .memory_bytes
            .fetch_sub(bytes, Ordering::AcqRel);
    }

    fn current(&self) -> u64 {
        self.evidence.memory_bytes.load(Ordering::Acquire)
    }

    fn peak(&self) -> u64 {
        self.evidence.peak_memory_bytes.load(Ordering::Acquire)
    }
}

impl ArchiveBudget {
    #[must_use]
    pub fn new(limits: ArchiveOperationLimits) -> Self {
        Self::with_accounting(limits, ArchiveOperationAccounting::default())
    }

    #[must_use]
    pub fn with_accounting(
        limits: ArchiveOperationLimits,
        accounting: ArchiveOperationAccounting,
    ) -> Self {
        let maximum_memory = limits.max_memory_bytes;
        Self {
            limits,
            entries: 0,
            expanded_bytes: 0,
            temporary_bytes: 0,
            memory: SharedMemoryBudget::new(maximum_memory, Arc::clone(&accounting.evidence)),
            accounting,
        }
    }

    /// Starts a new accounting phase while retaining the shared live-memory total.
    #[must_use]
    pub fn next_phase(&self) -> Self {
        Self {
            limits: self.limits.clone(),
            entries: 0,
            expanded_bytes: 0,
            temporary_bytes: 0,
            memory: self.memory.clone(),
            accounting: self.accounting.clone(),
        }
    }

    pub fn charge_entry(&mut self) -> Result<(), ArchiveOperationError> {
        self.entries = checked_charge("entries", self.entries, 1, self.limits.max_entries)?;
        self.accounting
            .evidence
            .entries
            .fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    pub fn check_path(&self, path: &[u8]) -> Result<(), ArchiveOperationError> {
        self.check_path_len(path.len())
    }

    pub fn check_path_len(&self, length: usize) -> Result<(), ArchiveOperationError> {
        let value = u64::try_from(length).unwrap_or(u64::MAX);
        self.accounting
            .evidence
            .max_path_bytes
            .fetch_max(value, Ordering::AcqRel);
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

    pub fn check_temporary(&self, bytes: u64) -> Result<(), ArchiveOperationError> {
        self.accounting
            .evidence
            .temporary_bytes
            .fetch_max(bytes, Ordering::AcqRel);
        if bytes > self.limits.max_temporary_bytes {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "temporary bytes",
                value: bytes,
                maximum: self.limits.max_temporary_bytes,
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
        self.accounting
            .evidence
            .compression_ratio_checks
            .fetch_add(1, Ordering::AcqRel);
        if next > ratio_limit {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "compression ratio",
                value: next,
                maximum: ratio_limit,
            });
        }
        self.expanded_bytes = next;
        self.accounting
            .evidence
            .expanded_bytes
            .fetch_max(next, Ordering::AcqRel);
        self.accounting
            .evidence
            .compressed_bytes
            .fetch_max(compressed_bytes, Ordering::AcqRel);
        Ok(())
    }

    pub fn check_expanded(&self, bytes: u64) -> Result<(), ArchiveOperationError> {
        self.accounting
            .evidence
            .expanded_bytes
            .fetch_max(bytes, Ordering::AcqRel);
        if bytes > self.limits.max_expanded_bytes {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "expanded bytes",
                value: bytes,
                maximum: self.limits.max_expanded_bytes,
            });
        }
        Ok(())
    }

    pub fn charge_expanded_bytes(&mut self, bytes: u64) -> Result<(), ArchiveOperationError> {
        let next =
            self.expanded_bytes
                .checked_add(bytes)
                .ok_or(ArchiveOperationError::LimitExceeded {
                    resource: "expanded bytes",
                    value: u64::MAX,
                    maximum: self.limits.max_expanded_bytes,
                })?;
        self.check_expanded(next)?;
        self.expanded_bytes = next;
        self.accounting
            .evidence
            .expanded_bytes
            .fetch_max(next, Ordering::AcqRel);
        Ok(())
    }

    pub fn check_compression_ratio(
        &self,
        expanded_bytes: u64,
        compressed_bytes: u64,
    ) -> Result<(), ArchiveOperationError> {
        self.accounting
            .evidence
            .compression_ratio_checks
            .fetch_add(1, Ordering::AcqRel);
        self.accounting
            .evidence
            .expanded_bytes
            .fetch_max(expanded_bytes, Ordering::AcqRel);
        self.accounting
            .evidence
            .compressed_bytes
            .fetch_max(compressed_bytes, Ordering::AcqRel);
        let maximum = compressed_bytes.saturating_mul(self.limits.max_compression_ratio);
        if expanded_bytes > maximum {
            return Err(ArchiveOperationError::LimitExceeded {
                resource: "compression ratio",
                value: expanded_bytes,
                maximum,
            });
        }
        Ok(())
    }

    pub fn check_nesting(&self, nesting: usize) -> Result<(), ArchiveOperationError> {
        self.accounting
            .evidence
            .max_nesting
            .fetch_max(u64::try_from(nesting).unwrap_or(u64::MAX), Ordering::AcqRel);
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
        self.memory.reserve(bytes)
    }

    pub(crate) fn shared_memory(&self) -> SharedMemoryBudget {
        self.memory.clone()
    }

    pub fn charge_temporary(&mut self, bytes: u64) -> Result<(), ArchiveOperationError> {
        self.accounting
            .evidence
            .temporary_bytes
            .fetch_max(self.temporary_bytes.saturating_add(bytes), Ordering::AcqRel);
        self.temporary_bytes = checked_charge(
            "temporary bytes",
            self.temporary_bytes,
            bytes,
            self.limits.max_temporary_bytes,
        )?;
        self.accounting
            .evidence
            .temporary_bytes
            .fetch_max(self.temporary_bytes, Ordering::AcqRel);
        Ok(())
    }

    pub(crate) fn check_stage_write(&self, bytes: u64) -> std::io::Result<()> {
        self.accounting.check_stage_write(bytes)
    }

    #[must_use]
    pub fn counters(&self) -> ArchiveBudgetCounters {
        let mut counters = self.accounting.counters();
        counters.entries = self.entries;
        counters.expanded_bytes = self.expanded_bytes;
        counters.temporary_bytes = self.temporary_bytes;
        counters.memory_bytes = self.memory.current();
        counters.peak_memory_bytes = self.memory.peak();
        counters
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
    memory: SharedMemoryBudget,
}

impl Drop for ArchiveMemoryLease {
    fn drop(&mut self) {
        self.memory.release_raw(self.bytes);
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
    Engine,
    RecoveryConsentRequired,
    RecoveryRequired,
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
            Self::Engine => formatter.write_str("the operation engine rejected the archive event"),
            Self::RecoveryConsentRequired => {
                formatter.write_str("archive recovery requires an explicit approved action")
            }
            Self::RecoveryRequired => {
                formatter.write_str("archive staging requires explicit recovery")
            }
        }
    }
}

impl Error for ArchiveOperationError {}

impl ArchiveOperationError {
    /// Maps an operating-system I/O failure at an archive filesystem boundary.
    #[must_use]
    pub fn from_io_error(error: &std::io::Error) -> Self {
        if error.raw_os_error() == Some(28) {
            Self::NoSpace
        } else if error.kind() == std::io::ErrorKind::Interrupted {
            Self::Cancelled
        } else {
            Self::Io
        }
    }
}

impl From<ArchiveError> for ArchiveOperationError {
    fn from(error: ArchiveError) -> Self {
        match error {
            ArchiveError::LimitExceeded {
                resource,
                value,
                maximum,
            } => Self::LimitExceeded {
                resource: match resource {
                    "nested archive bytes" => "temporary bytes",
                    "nested archives" => "archive nesting",
                    other => other,
                },
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
    ArchiveOperationError::from_io_error(error)
}
