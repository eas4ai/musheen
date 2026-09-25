use super::ArchiveError;
use musheen_core::{ProviderId, StorePath};

/// A normalized archive path bound to the provider that issued it.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArchivePath {
    provider: ProviderId,
    key: Box<[u8]>,
}

impl ArchivePath {
    pub const MAX_BYTES: usize = 4_096;

    pub fn new(provider: ProviderId, value: impl AsRef<[u8]>) -> Result<Self, ArchiveError> {
        Self::with_limit(provider, value.as_ref(), Self::MAX_BYTES)
    }

    pub(crate) fn with_limit(
        provider: ProviderId,
        value: &[u8],
        maximum: usize,
    ) -> Result<Self, ArchiveError> {
        let key = normalize(value, maximum)?;
        Ok(Self { provider, key })
    }

    pub(crate) fn from_normalized(provider: ProviderId, key: Vec<u8>) -> Self {
        Self {
            provider,
            key: key.into_boxed_slice(),
        }
    }

    pub(crate) fn normalize_bytes(value: &[u8], maximum: usize) -> Result<Vec<u8>, ArchiveError> {
        normalize(value, maximum).map(Into::into)
    }

    pub(crate) fn root(provider: ProviderId) -> Self {
        Self {
            provider,
            key: Box::default(),
        }
    }

    #[must_use]
    pub fn provider_id(&self) -> &ProviderId {
        &self.provider
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.key
    }

    #[must_use]
    pub fn is_root(&self) -> bool {
        self.key.is_empty()
    }

    pub fn to_store_path(&self) -> Result<StorePath, ArchiveError> {
        let key = if self.is_root() {
            b".".to_vec().into_boxed_slice()
        } else {
            self.key.clone()
        };
        StorePath::from_provider_key(self.provider.clone(), key)
            .map_err(|_| ArchiveError::InvalidArchive)
    }

    pub(crate) fn parent(&self) -> Option<Self> {
        let split = self.key.iter().rposition(|byte| *byte == b'/');
        match split {
            Some(index) => Some(Self {
                provider: self.provider.clone(),
                key: self.key[..index].to_vec().into_boxed_slice(),
            }),
            None if !self.is_root() => Some(Self::root(self.provider.clone())),
            None => None,
        }
    }

    pub(crate) fn file_name(&self) -> &[u8] {
        self.key
            .iter()
            .rposition(|byte| *byte == b'/')
            .map_or(&self.key, |index| &self.key[index + 1..])
    }

    pub(crate) fn ancestors(&self) -> impl Iterator<Item = Self> + '_ {
        self.key
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == b'/')
            .map(|(index, _)| Self {
                provider: self.provider.clone(),
                key: self.key[..index].to_vec().into_boxed_slice(),
            })
    }
}

fn normalize(value: &[u8], maximum: usize) -> Result<Box<[u8]>, ArchiveError> {
    if value.len() > maximum {
        return Err(ArchiveError::LimitExceeded {
            resource: "path bytes",
            value: value.len(),
            maximum,
        });
    }
    if value.contains(&0) {
        return Err(ArchiveError::UnsafePath("archive paths cannot contain NUL"));
    }
    if value
        .first()
        .is_some_and(|byte| matches!(byte, b'/' | b'\\'))
        || value
            .get(0..2)
            .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':')
    {
        return Err(ArchiveError::UnsafePath(
            "archive paths must be relative to the archive root",
        ));
    }

    let mut normalized = Vec::with_capacity(value.len());
    for component in value.split(|byte| matches!(byte, b'/' | b'\\')) {
        if component.is_empty() || component == b"." {
            continue;
        }
        if component == b".." {
            return Err(ArchiveError::UnsafePath(
                "archive paths cannot contain a parent component",
            ));
        }
        if normalized.is_empty()
            && component.len() >= 2
            && component[0].is_ascii_alphabetic()
            && component[1] == b':'
        {
            return Err(ArchiveError::UnsafePath(
                "archive paths must not contain a Windows drive prefix",
            ));
        }
        if !normalized.is_empty() {
            normalized.push(b'/');
        }
        normalized.extend_from_slice(component);
    }
    Ok(normalized.into_boxed_slice())
}
