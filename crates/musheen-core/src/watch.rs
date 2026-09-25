use crate::{BoxFuture, CancellationToken, ItemId, StoreError, StoreItem, StorePath};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchSemantics {
    Live,
    Polling(Duration),
    ManualRefresh,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchFailure {
    Overflow,
    EventGap,
    Reconnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WatchEvent {
    Created(StoreItem),
    Changed(StoreItem),
    Renamed {
        previous_path: StorePath,
        item: StoreItem,
    },
    Removed(ItemId),
    Invalidated {
        location: StorePath,
        cause: WatchFailure,
    },
}

impl WatchEvent {
    #[must_use]
    pub fn invalidation(location: StorePath, cause: WatchFailure) -> Self {
        Self::Invalidated { location, cause }
    }
}

pub trait DirectoryWatch: Send {
    fn semantics(&self) -> WatchSemantics;

    fn next_event<'a>(
        &'a mut self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<WatchEvent, StoreError>>;
}

/// A bounded stable-ID window used while watch invalidations are reconciled.
#[derive(Clone, Debug)]
pub struct ReconcileBuffer {
    maximum: NonZeroUsize,
    items: BTreeMap<ItemId, StoreItem>,
    identities_by_path: HashMap<StorePath, ItemId>,
}

impl ReconcileBuffer {
    pub fn new(maximum: usize) -> Result<Self, StoreError> {
        let maximum = NonZeroUsize::new(maximum).ok_or(StoreError::InvalidLimit {
            resource: "reconciliation item models",
            value: 0,
            minimum: 1,
            maximum: usize::MAX,
        })?;
        Ok(Self {
            maximum,
            items: BTreeMap::new(),
            identities_by_path: HashMap::new(),
        })
    }

    pub fn apply(&mut self, item: StoreItem) -> Result<(), StoreError> {
        let id = item.id().clone();
        let path = StoreItem::path(&item).clone();
        let replaced_id = self.identities_by_path.get(&path).cloned();
        let adds_item = !self.items.contains_key(&id) && replaced_id.is_none();
        if adds_item && self.items.len() == self.maximum.get() {
            return Err(StoreError::ResourceLimit {
                resource: "reconciliation item models",
                value: self.items.len() + 1,
                maximum: self.maximum.get(),
            });
        }

        if let Some(previous) = self.items.remove(&id) {
            self.identities_by_path.remove(StoreItem::path(&previous));
        }
        if let Some(replaced_id) = replaced_id {
            self.items.remove(&replaced_id);
        }
        self.identities_by_path.insert(path, id.clone());
        self.items.insert(id, item);
        Ok(())
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub fn get(&self, id: &ItemId) -> Option<&StoreItem> {
        self.items.get(id)
    }

    pub fn remove(&mut self, id: &ItemId) -> Option<StoreItem> {
        let item = self.items.remove(id)?;
        self.identities_by_path.remove(StoreItem::path(&item));
        Some(item)
    }
}
