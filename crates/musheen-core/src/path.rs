use crate::CoreError;
use crate::error::validate_bounded_bytes;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::ffi::{OsStr, OsString};
use std::path::Path;

#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};

/// A stable identifier for a storage provider implementation or account.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProviderId(Box<str>);

impl ProviderId {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, CoreError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 64
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-_".contains(&byte)
            });

        valid
            .then_some(Self(value))
            .ok_or(CoreError::InvalidProviderId)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ProviderId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProviderId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A lossless operation target owned by a storage provider.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct StorePath(StorePathInner);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum StorePathInner {
    Unix(OsString),
    ProviderKey {
        provider: ProviderId,
        key: Box<[u8]>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum StorePathDocument {
    Unix { bytes: Vec<u8> },
    Provider { provider: ProviderId, key: Vec<u8> },
}

impl StorePath {
    pub const MAX_PROVIDER_KEY_BYTES: usize = 4_096;

    #[cfg(unix)]
    #[must_use]
    pub fn from_unix_bytes(bytes: Vec<u8>) -> Self {
        Self(StorePathInner::Unix(OsString::from_vec(bytes)))
    }

    #[must_use]
    pub fn from_unix_path(path: impl Into<OsString>) -> Self {
        Self(StorePathInner::Unix(path.into()))
    }

    pub fn from_provider_key(
        provider: ProviderId,
        key: impl Into<Box<[u8]>>,
    ) -> Result<Self, CoreError> {
        let key = validate_bounded_bytes(
            key,
            Self::MAX_PROVIDER_KEY_BYTES,
            CoreError::InvalidProviderKey,
        )?;
        Ok(Self(StorePathInner::ProviderKey { provider, key }))
    }

    #[must_use]
    pub fn as_unix_path(&self) -> Option<&Path> {
        match &self.0 {
            StorePathInner::Unix(path) => Some(Path::new(path)),
            StorePathInner::ProviderKey { .. } => None,
        }
    }

    #[cfg(unix)]
    #[must_use]
    pub fn unix_bytes(&self) -> Option<&[u8]> {
        match &self.0 {
            StorePathInner::Unix(path) => Some(path.as_os_str().as_bytes()),
            StorePathInner::ProviderKey { .. } => None,
        }
    }

    #[must_use]
    pub fn provider_key(&self) -> Option<(&ProviderId, &[u8])> {
        match &self.0 {
            StorePathInner::Unix(_) => None,
            StorePathInner::ProviderKey { provider, key } => Some((provider, key)),
        }
    }
}

impl Serialize for StorePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let document = match &self.0 {
            #[cfg(unix)]
            StorePathInner::Unix(path) => StorePathDocument::Unix {
                bytes: path.as_os_str().as_bytes().to_vec(),
            },
            StorePathInner::ProviderKey { provider, key } => StorePathDocument::Provider {
                provider: provider.clone(),
                key: key.to_vec(),
            },
        };
        document.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for StorePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match StorePathDocument::deserialize(deserializer)? {
            #[cfg(unix)]
            StorePathDocument::Unix { bytes } => Ok(Self::from_unix_bytes(bytes)),
            StorePathDocument::Provider { provider, key } => {
                Self::from_provider_key(provider, key).map_err(serde::de::Error::custom)
            }
        }
    }
}

/// Human-readable path text. It is never accepted as an operation target.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DisplayPath(Box<str>);

impl DisplayPath {
    #[must_use]
    pub fn new(value: impl Into<Box<str>>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn from_store_path(path: &StorePath) -> Self {
        match &path.0 {
            StorePathInner::Unix(path) => Self(path.to_string_lossy().into_owned().into()),
            StorePathInner::ProviderKey { key, .. } => {
                Self(String::from_utf8_lossy(key).into_owned().into())
            }
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&OsStr> for DisplayPath {
    fn from(value: &OsStr) -> Self {
        Self(value.to_string_lossy().into_owned().into())
    }
}
