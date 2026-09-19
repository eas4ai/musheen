use crate::create::{child_path, validate_local_name, validate_operation_path};
use crate::{MutationError, NameError};
use musheen_core::StorePath;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

pub trait LinkProvider {
    fn allows_symbolic_links(&mut self, parent: &StorePath) -> Result<bool, MutationError>;

    fn allows_hard_links(
        &mut self,
        source: &StorePath,
        parent: &StorePath,
    ) -> Result<bool, MutationError>;

    fn identity(&mut self, path: &StorePath) -> Result<Option<Box<[u8]>>, MutationError>;

    fn filesystem_id(&mut self, path: &StorePath) -> Result<u64, MutationError>;

    fn create_symbolic_link(
        &mut self,
        target: &OsStr,
        destination: &StorePath,
    ) -> Result<(), MutationError>;

    fn create_hard_link(
        &mut self,
        source: &StorePath,
        destination: &StorePath,
        expected_identity: &[u8],
    ) -> Result<(), MutationError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SymbolicLinkRequest {
    parent: StorePath,
    name: OsString,
    target: OsString,
}

impl SymbolicLinkRequest {
    #[must_use]
    pub const fn new(parent: StorePath, name: OsString, target: OsString) -> Self {
        Self {
            parent,
            name,
            target,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HardLinkRequest {
    source: StorePath,
    parent: StorePath,
    name: OsString,
    expected_identity: Box<[u8]>,
}

impl HardLinkRequest {
    #[must_use]
    pub fn new(
        source: StorePath,
        parent: StorePath,
        name: OsString,
        expected_identity: Vec<u8>,
    ) -> Self {
        Self {
            source,
            parent,
            name,
            expected_identity: expected_identity.into_boxed_slice(),
        }
    }
}

pub fn execute_symbolic_link(
    provider: &mut impl LinkProvider,
    request: &SymbolicLinkRequest,
) -> Result<StorePath, MutationError> {
    validate_local_name(&request.name).map_err(|_| MutationError::InvalidName)?;
    validate_symbolic_link_target(&request.target).map_err(|_| MutationError::InvalidName)?;
    let destination = child_path(&request.parent, &request.name)?;
    if !provider.allows_symbolic_links(&request.parent)? {
        return Err(MutationError::Unsupported);
    }
    if provider.identity(&destination)?.is_some() {
        return Err(MutationError::Conflict);
    }
    provider.create_symbolic_link(&request.target, &destination)?;
    Ok(destination)
}

pub fn execute_hard_link(
    provider: &mut impl LinkProvider,
    request: &HardLinkRequest,
) -> Result<StorePath, MutationError> {
    validate_local_name(&request.name).map_err(|_| MutationError::InvalidName)?;
    validate_operation_path(
        request
            .source
            .as_unix_path()
            .ok_or(MutationError::Unsupported)?,
    )?;
    let destination = child_path(&request.parent, &request.name)?;
    if !provider.allows_hard_links(&request.source, &request.parent)? {
        return Err(MutationError::Unsupported);
    }
    let Some(identity) = provider.identity(&request.source)? else {
        return Err(MutationError::Missing);
    };
    if identity.as_ref() != request.expected_identity.as_ref() {
        return Err(MutationError::SourceChanged);
    }
    if provider.identity(&destination)?.is_some() {
        return Err(MutationError::Conflict);
    }
    if provider.filesystem_id(&request.source)? != provider.filesystem_id(&request.parent)? {
        return Err(MutationError::CrossFilesystem);
    }
    provider.create_hard_link(&request.source, &destination, &request.expected_identity)?;
    Ok(destination)
}

fn validate_symbolic_link_target(target: &OsStr) -> Result<(), NameError> {
    let bytes = target.as_bytes();
    if bytes.is_empty() {
        return Err(NameError::Empty);
    }
    if bytes.contains(&0) {
        return Err(NameError::ContainsNul);
    }
    Ok(())
}
