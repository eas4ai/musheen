use crate::{MutationError, MutationProvider};
use musheen_core::StorePath;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CreateKind {
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NameError {
    Empty,
    Dot,
    ContainsSeparator,
    ContainsNul,
}

pub fn validate_local_name(name: &OsStr) -> Result<(), NameError> {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return Err(NameError::Empty);
    }
    if bytes == b"." || bytes == b".." {
        return Err(NameError::Dot);
    }
    if bytes.contains(&b'/') {
        return Err(NameError::ContainsSeparator);
    }
    if bytes.contains(&0) {
        return Err(NameError::ContainsNul);
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateRequest {
    parent: StorePath,
    name: OsString,
    kind: CreateKind,
}

impl CreateRequest {
    #[must_use]
    pub const fn new(parent: StorePath, name: OsString, kind: CreateKind) -> Self {
        Self { parent, name, kind }
    }

    #[must_use]
    pub const fn parent(&self) -> &StorePath {
        &self.parent
    }

    #[must_use]
    pub fn name(&self) -> &OsStr {
        &self.name
    }

    #[must_use]
    pub const fn kind(&self) -> CreateKind {
        self.kind
    }
}

pub fn execute_create(
    provider: &mut impl MutationProvider,
    request: &CreateRequest,
) -> Result<StorePath, MutationError> {
    validate_local_name(request.name()).map_err(|_| MutationError::InvalidName)?;
    let destination = child_path(request.parent(), request.name())?;
    if !provider.allows_create(request.parent(), request.kind())? {
        return Err(MutationError::Unsupported);
    }
    if provider.identity(&destination)?.is_some() {
        return Err(MutationError::Conflict);
    }
    provider.create(&destination, request.kind())?;
    Ok(destination)
}

pub(crate) fn child_path(parent: &StorePath, name: &OsStr) -> Result<StorePath, MutationError> {
    let parent = parent.as_unix_path().ok_or(MutationError::Unsupported)?;
    validate_operation_path(parent)?;
    Ok(StorePath::from_unix_path(
        parent.join(name).into_os_string(),
    ))
}

pub(crate) fn sibling_path(source: &StorePath, name: &OsStr) -> Result<StorePath, MutationError> {
    let parent = source
        .as_unix_path()
        .filter(|path| validate_operation_path(path).is_ok())
        .and_then(std::path::Path::parent)
        .ok_or(MutationError::InvalidScope)?;
    Ok(StorePath::from_unix_path(
        parent.join(name).into_os_string(),
    ))
}

pub(crate) fn validate_operation_path(path: &Path) -> Result<(), MutationError> {
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err(MutationError::InvalidScope);
    }
    Ok(())
}
