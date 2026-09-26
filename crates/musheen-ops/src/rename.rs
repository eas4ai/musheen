use crate::create::{sibling_path, validate_local_name};
use crate::{MutationError, MutationProvider};
use musheen_core::StorePath;
use std::ffi::{OsStr, OsString};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RenameRequest {
    source: StorePath,
    target_name: OsString,
    expected_identity: Box<[u8]>,
}

impl RenameRequest {
    #[must_use]
    pub fn new(source: StorePath, target_name: OsString, expected_identity: Vec<u8>) -> Self {
        Self {
            source,
            target_name,
            expected_identity: expected_identity.into_boxed_slice(),
        }
    }

    #[must_use]
    pub const fn source(&self) -> &StorePath {
        &self.source
    }

    #[must_use]
    pub fn target_name(&self) -> &OsStr {
        &self.target_name
    }

    #[must_use]
    pub const fn expected_identity(&self) -> &[u8] {
        &self.expected_identity
    }

    pub fn destination(&self) -> Result<StorePath, MutationError> {
        validate_local_name(self.target_name()).map_err(|_| MutationError::InvalidName)?;
        sibling_path(self.source(), self.target_name())
    }
}

pub fn execute_rename(
    provider: &mut impl MutationProvider,
    request: &RenameRequest,
) -> Result<StorePath, MutationError> {
    let destination = request.destination()?;
    if !provider.allows_rename(request.source())? {
        return Err(MutationError::Unsupported);
    }
    validate_source(provider, request.source(), request.expected_identity())?;

    if destination == *request.source() {
        return Ok(destination);
    }
    if let Some(identity) = provider.identity(&destination)?
        && identity.as_ref() != request.expected_identity()
    {
        return Err(MutationError::Conflict);
    }

    provider.rename_no_replace(request.source(), &destination, request.expected_identity())?;
    Ok(destination)
}

pub(crate) fn validate_source(
    provider: &mut impl MutationProvider,
    source: &StorePath,
    expected_identity: &[u8],
) -> Result<(), MutationError> {
    let Some(identity) = provider.identity(source)? else {
        return Err(MutationError::Missing);
    };
    if identity.as_ref() != expected_identity {
        return Err(MutationError::SourceChanged);
    }
    Ok(())
}
