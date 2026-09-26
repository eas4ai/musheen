use crate::error::validate_bounded_bytes;
use crate::{CoreError, ProviderId};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{DisplayPath, StorePath};

/// A provider-scoped stable identity. Its byte representation is intentionally opaque.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ItemId {
    provider: ProviderId,
    key: Box<[u8]>,
}

#[derive(Deserialize, Serialize)]
struct ItemIdDocument {
    provider: ProviderId,
    key: Vec<u8>,
}

impl ItemId {
    pub const MAX_KEY_BYTES: usize = 4_096;

    pub fn new(provider: ProviderId, key: impl Into<Box<[u8]>>) -> Result<Self, CoreError> {
        let key = validate_bounded_bytes(key, Self::MAX_KEY_BYTES, CoreError::InvalidItemId)?;
        Ok(Self { provider, key })
    }

    #[must_use]
    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    /// Provider-owned stable bytes. Callers must treat these as opaque and
    /// compare them only for exact identity-bound workflows.
    #[must_use]
    pub fn opaque_key(&self) -> &[u8] {
        &self.key
    }
}

impl Serialize for ItemId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ItemIdDocument {
            provider: self.provider.clone(),
            key: self.key.to_vec(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ItemId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let document = ItemIdDocument::deserialize(deserializer)?;
        Self::new(document.provider, document.key).map_err(serde::de::Error::custom)
    }
}

impl std::fmt::Debug for ItemId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ItemId")
            .field("provider", &self.provider)
            .field("key_length", &self.key.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ItemKind {
    Directory,
    RegularFile,
    SymbolicLink,
    Other,
}

/// Provider metadata for one directory entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoreItem {
    id: ItemId,
    path: StorePath,
    display_name: DisplayPath,
    kind: ItemKind,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
}

impl StoreItem {
    #[must_use]
    pub fn new(
        id: ItemId,
        path: StorePath,
        display_name: DisplayPath,
        kind: ItemKind,
        size: Option<u64>,
    ) -> Self {
        Self {
            id,
            path,
            display_name,
            kind,
            size,
            modified_unix_seconds: None,
        }
    }

    #[must_use]
    pub fn with_modified_unix_seconds(mut self, modified_unix_seconds: i64) -> Self {
        self.modified_unix_seconds = Some(modified_unix_seconds);
        self
    }

    #[must_use]
    pub fn id(&self) -> &ItemId {
        &self.id
    }

    #[must_use]
    pub fn path(&self) -> &StorePath {
        &self.path
    }

    #[must_use]
    pub fn display_name(&self) -> &DisplayPath {
        &self.display_name
    }

    #[must_use]
    pub fn kind(&self) -> ItemKind {
        self.kind
    }

    #[must_use]
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    #[must_use]
    pub fn modified_unix_seconds(&self) -> Option<i64> {
        self.modified_unix_seconds
    }
}
