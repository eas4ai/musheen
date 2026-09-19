use crate::{EventGeneration, JobId};
use musheen_core::StorePath;
use std::error::Error;
use std::fmt;
use std::os::unix::ffi::OsStrExt;

const STAGING_PREFIX: &str = ".musheen-stage-v1-";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagingPath {
    path: StorePath,
}

impl StagingPath {
    pub fn for_destination(
        destination: &StorePath,
        job_id: JobId,
        generation: EventGeneration,
    ) -> Result<Self, StagingError> {
        let destination = destination
            .as_unix_path()
            .ok_or(StagingError::UnsupportedProvider)?;
        let parent = destination.parent().ok_or(StagingError::MissingParent)?;
        let name = format!("{STAGING_PREFIX}{}-{}", job_id.get(), generation.get());
        Ok(Self {
            path: StorePath::from_unix_path(parent.join(name).into_os_string()),
        })
    }

    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    #[must_use]
    pub fn is_app_owned(&self) -> bool {
        Self::is_owned_path(&self.path)
    }

    #[must_use]
    pub fn is_owned_path(path: &StorePath) -> bool {
        let Some(name) = path.as_unix_path().and_then(std::path::Path::file_name) else {
            return false;
        };
        let Some(suffix) = name.as_bytes().strip_prefix(STAGING_PREFIX.as_bytes()) else {
            return false;
        };
        let Some(separator) = suffix.iter().position(|byte| *byte == b'-') else {
            return false;
        };
        let (job, generation_with_separator) = suffix.split_at(separator);
        let generation = &generation_with_separator[1..];
        parse_decimal(job).is_some_and(|value| value > 0) && parse_decimal(generation).is_some()
    }
}

fn parse_decimal(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        let digit = byte.checked_sub(b'0').filter(|digit| *digit <= 9)?;
        value.checked_mul(10)?.checked_add(u64::from(digit))
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StagingError {
    UnsupportedProvider,
    MissingParent,
}

impl fmt::Display for StagingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProvider => {
                formatter.write_str("provider does not expose sibling staging paths")
            }
            Self::MissingParent => formatter.write_str("destination has no parent for staging"),
        }
    }
}

impl Error for StagingError {}
