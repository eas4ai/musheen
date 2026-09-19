use crate::{CoreError, ProviderId};

/// A provider-scoped stable identity. Its byte representation is intentionally opaque.
#[derive(Clone, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ItemId {
    provider: ProviderId,
    key: Box<[u8]>,
}

impl ItemId {
    pub const MAX_KEY_BYTES: usize = 4_096;

    pub fn new(provider: ProviderId, key: impl Into<Box<[u8]>>) -> Result<Self, CoreError> {
        let key = key.into();
        if key.is_empty() || key.len() > Self::MAX_KEY_BYTES {
            return Err(CoreError::InvalidItemId);
        }

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
