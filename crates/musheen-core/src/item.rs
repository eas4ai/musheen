use crate::error::validate_bounded_bytes;
use crate::{CoreError, ProviderId};

use crate::{DisplayPath, StorePath};

/// A provider-scoped stable identity. Its byte representation is intentionally opaque.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ItemId {
    provider: ProviderId,
    key: Box<[u8]>,
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
        }
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
}
