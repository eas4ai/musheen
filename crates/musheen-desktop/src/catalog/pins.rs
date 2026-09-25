use musheen_core::{ItemId, StorePath};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum PinState {
    Available,
    Unavailable(Box<str>),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PinnedLocation {
    item: ItemId,
    path_hint: StorePath,
    label: Box<str>,
    state: PinState,
}

impl PinnedLocation {
    #[must_use]
    pub fn item(&self) -> &ItemId {
        &self.item
    }

    #[must_use]
    pub fn path_hint(&self) -> &StorePath {
        &self.path_hint
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub const fn state(&self) -> &PinState {
        &self.state
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct PinCatalog {
    entries: Vec<PinnedLocation>,
}

impl PinCatalog {
    pub fn pin(
        &mut self,
        item: ItemId,
        path_hint: StorePath,
        label: impl Into<Box<str>>,
    ) -> Result<(), PinError> {
        if self.entries.iter().any(|entry| entry.item == item) {
            return Err(PinError::Duplicate);
        }
        let label = label.into();
        if label.trim().is_empty() {
            return Err(PinError::EmptyLabel);
        }
        self.entries.push(PinnedLocation {
            item,
            path_hint,
            label,
            state: PinState::Available,
        });
        Ok(())
    }

    pub fn unpin(&mut self, item: &ItemId) -> bool {
        let before = self.entries.len();
        self.entries.retain(|entry| &entry.item != item);
        self.entries.len() != before
    }

    #[must_use]
    pub fn contains(&self, item: &ItemId) -> bool {
        self.entries.iter().any(|entry| &entry.item == item)
    }

    #[must_use]
    pub fn entries(&self) -> &[PinnedLocation] {
        &self.entries
    }

    pub fn mark_unavailable(&mut self, item: &ItemId, reason: impl Into<Box<str>>) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| &entry.item == item) else {
            return false;
        };
        entry.state = PinState::Unavailable(reason.into());
        true
    }

    pub fn mark_available(&mut self, item: &ItemId, path_hint: StorePath) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|entry| &entry.item == item) else {
            return false;
        };
        entry.path_hint = path_hint;
        entry.state = PinState::Available;
        true
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PinError {
    Duplicate,
    EmptyLabel,
}

impl fmt::Display for PinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Duplicate => "the location is already pinned",
            Self::EmptyLabel => "pin labels must contain visible text",
        })
    }
}

impl Error for PinError {}
