use crate::{EventGeneration, JobId};
use musheen_core::StorePath;
use std::error::Error;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const STAGING_PREFIX: &str = ".musheen-stage-v1-";
static NEXT_STAGING_NONCE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagingPath {
    path: StorePath,
}

impl StagingPath {
    #[must_use]
    pub fn unique_nonce() -> [u8; 16] {
        let sequence = NEXT_STAGING_NONCE.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0_u128, |duration| duration.as_nanos());
        let mut input = Vec::with_capacity(36);
        input.extend_from_slice(&timestamp.to_ne_bytes());
        input.extend_from_slice(&std::process::id().to_ne_bytes());
        input.extend_from_slice(&sequence.to_ne_bytes());
        let hash = blake3::hash(&input);
        let mut nonce = [0_u8; 16];
        nonce.copy_from_slice(&hash.as_bytes()[..16]);
        nonce
    }

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

    /// Use only for providers whose keys are absolute slash-separated paths.
    /// Opaque provider keys have no portable parent or sibling relationship.
    pub fn for_slash_key_destination_with_nonce(
        destination: &StorePath,
        job_id: JobId,
        generation: EventGeneration,
        nonce: [u8; 16],
    ) -> Result<Self, StagingError> {
        let (provider, key) = destination
            .provider_key()
            .ok_or(StagingError::UnsupportedProvider)?;
        let parent = slash_key_parent(key).ok_or(StagingError::InvalidSlashKey)?;
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
        let mut staging = parent.to_vec();
        if staging != b"/" {
            staging.push(b'/');
        }
        staging.extend_from_slice(name.as_bytes());
        let path = StorePath::from_provider_key(provider.clone(), staging)
            .map_err(|_| StagingError::InvalidSlashKey)?;
        Ok(Self { path })
    }

    #[must_use]
    pub fn nonce(path: &StorePath) -> Option<[u8; 16]> {
        let name = path_name(path)?;
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
        let same_parent = match (path.as_unix_path(), destination.as_unix_path()) {
            (Some(path), Some(destination)) => path.parent() == destination.parent(),
            _ => match (path.provider_key(), destination.provider_key()) {
                (
                    Some((path_provider, path_key)),
                    Some((destination_provider, destination_key)),
                ) => {
                    path_provider == destination_provider
                        && slash_key_parent(path_key) == slash_key_parent(destination_key)
                        && slash_key_parent(path_key).is_some()
                }
                _ => false,
            },
        };
        same_parent
            && Self::nonce(path) == Some(nonce)
            && path_name(path).is_some_and(|name| {
                name.starts_with(
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
        let Some(name) = path_name(path) else {
            return false;
        };
        parse_staging_name(name).is_some()
    }
}

fn path_name(path: &StorePath) -> Option<&[u8]> {
    if let Some(path) = path.as_unix_path() {
        return path.file_name().map(OsStrExt::as_bytes);
    }
    let (_, key) = path.provider_key()?;
    slash_key_parent(key)?;
    key.rsplit(|byte| *byte == b'/').next()
}

fn slash_key_parent(key: &[u8]) -> Option<&[u8]> {
    if key.len() < 2 || key[0] != b'/' || key.last() == Some(&b'/') || key.contains(&0) {
        return None;
    }
    if key[1..]
        .split(|byte| *byte == b'/')
        .any(|segment| segment.is_empty() || segment == b"." || segment == b"..")
    {
        return None;
    }
    let separator = key.iter().rposition(|byte| *byte == b'/')?;
    Some(if separator == 0 {
        &key[..1]
    } else {
        &key[..separator]
    })
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
    InvalidSlashKey,
}

impl fmt::Display for StagingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProvider => {
                formatter.write_str("provider does not expose sibling staging paths")
            }
            Self::MissingParent => formatter.write_str("destination has no parent for staging"),
            Self::InvalidSlashKey => formatter.write_str("provider path is not a valid slash key"),
        }
    }
}

impl Error for StagingError {}
