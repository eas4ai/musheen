use crate::CreateKind;
use musheen_core::StorePath;
use std::error::Error;
use std::fmt;

/// Storage mutations used by the provider-independent operation planner.
///
/// Implementations must revalidate `expected_identity` at the syscall boundary.
/// `rename_no_replace` may accept an existing destination only when it is an
/// alternate spelling of the same source identity (for example, a case-only
/// rename on a case-insensitive filesystem).
pub trait MutationProvider {
    fn allows_create(
        &mut self,
        parent: &StorePath,
        kind: CreateKind,
    ) -> Result<bool, MutationError>;

    fn allows_rename(&mut self, source: &StorePath) -> Result<bool, MutationError>;

    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError>;

    fn create(&mut self, path: &StorePath, kind: CreateKind) -> Result<(), MutationError>;

    fn rename_no_replace(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
        expected_identity: &[u8],
    ) -> Result<(), MutationError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationError {
    InvalidName,
    Conflict,
    BatchCollision,
    Missing,
    SourceChanged,
    CrossFilesystem,
    Unsupported,
    PermissionDenied,
    ScopeNotReviewed,
    NoChanges,
    InvalidMetadata,
    TrashUnsupported,
    ConfirmationRequired,
    InvalidScope,
    Provider(Box<str>),
}

impl fmt::Display for MutationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName => formatter.write_str("the name is not a valid local file name"),
            Self::Conflict => formatter.write_str("the destination already exists"),
            Self::BatchCollision => formatter.write_str("the batch contains colliding paths"),
            Self::Missing => formatter.write_str("the source does not exist"),
            Self::SourceChanged => formatter.write_str("the source identity changed"),
            Self::CrossFilesystem => {
                formatter.write_str("hard links cannot cross filesystem boundaries")
            }
            Self::Unsupported => formatter.write_str("the provider does not support this mutation"),
            Self::PermissionDenied => formatter.write_str("permission denied"),
            Self::ScopeNotReviewed => formatter.write_str("recursive scope was not reviewed"),
            Self::NoChanges => formatter.write_str("the metadata plan has no changes"),
            Self::InvalidMetadata => formatter.write_str("the metadata change is invalid"),
            Self::TrashUnsupported => formatter.write_str("trash is unavailable at this location"),
            Self::ConfirmationRequired => {
                formatter.write_str("permanent deletion requires an exact confirmation")
            }
            Self::InvalidScope => formatter.write_str("the operation scope is invalid"),
            Self::Provider(message) => formatter.write_str(message),
        }
    }
}

impl Error for MutationError {}
