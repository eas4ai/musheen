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

    pub fn for_destination_with_nonce(
        destination: &StorePath,
        job_id: JobId,
        generation: EventGeneration,
        nonce: [u8; 16],
    ) -> Result<Self, StagingError> {
        let destination = destination
            .as_unix_path()
            .ok_or(StagingError::UnsupportedProvider)?;
        let parent = destination.parent().ok_or(StagingError::MissingParent)?;
        let nonce = nonce
            .iter()
            .fold(String::with_capacity(32), |mut output, byte| {
                use std::fmt::Write as _;
                let _ = write!(output, "{byte:02x}");
                output
            });
        let name = format!(
            "{STAGING_PREFIX}{}-{}-{nonce}",
            job_id.get(),
            generation.get()
        );
        Ok(Self {
            path: StorePath::from_unix_path(parent.join(name).into_os_string()),
        })
    }

    #[must_use]
    pub fn nonce(path: &StorePath) -> Option<[u8; 16]> {
        let name = path.as_unix_path()?.file_name()?.as_bytes();
        let (_, _, encoded) = parse_staging_name(name)?;
        let encoded = encoded?;
        let mut nonce = [0_u8; 16];
        for (index, pair) in encoded.chunks_exact(2).enumerate() {
            nonce[index] = hex_digit(pair[0])?
                .checked_mul(16)?
                .checked_add(hex_digit(pair[1])?)?;
        }
        Some(nonce)
    }

    #[must_use]
    pub fn is_for_destination(
        path: &StorePath,
        destination: &StorePath,
        job_id: JobId,
        generation: EventGeneration,
        nonce: [u8; 16],
    ) -> bool {
        let Some(path_value) = path.as_unix_path() else {
            return false;
        };
        let Some(destination_value) = destination.as_unix_path() else {
            return false;
        };
        path_value.parent() == destination_value.parent()
            && Self::nonce(path) == Some(nonce)
            && path_value.file_name().is_some_and(|name| {
                name.as_bytes().starts_with(
                    format!("{STAGING_PREFIX}{}-{}-", job_id.get(), generation.get()).as_bytes(),
                )
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
        parse_staging_name(name.as_bytes()).is_some()
    }
}

fn parse_staging_name(name: &[u8]) -> Option<(u64, u64, Option<&[u8]>)> {
    let suffix = name.strip_prefix(STAGING_PREFIX.as_bytes())?;
    let mut parts = suffix.split(|byte| *byte == b'-');
    let job = parse_decimal(parts.next()?)?;
    let generation = parse_decimal(parts.next()?)?;
    let nonce = parts.next();
    if job == 0 || parts.next().is_some() || nonce.is_some_and(|value| value.len() != 32) {
        return None;
    }
    if let Some(encoded) = nonce {
        for pair in encoded.chunks_exact(2) {
            hex_digit(pair[0])?;
            hex_digit(pair[1])?;
        }
    }
    Some((job, generation, nonce))
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
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
