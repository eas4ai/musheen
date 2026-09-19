use super::{NavigationError, NavigationHistory};
use musheen_core::{ItemId, StorePath};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct TabId {
    pane: u64,
    local: u64,
}

impl TabId {
    pub(super) const fn new(pane: u64, local: u64) -> Self {
        Self { pane, local }
    }

    pub(super) const fn pane(self) -> u64 {
        self.pane
    }

    pub(super) const fn local(self) -> u64 {
        self.local
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TabState {
    id: TabId,
    history: NavigationHistory,
    selection: Vec<ItemId>,
}

impl TabState {
    pub(super) fn new(id: TabId, location: StorePath) -> Self {
        Self {
            id,
            history: NavigationHistory::new(location),
            selection: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> TabId {
        self.id
    }

    #[must_use]
    pub fn location(&self) -> &StorePath {
        self.history.current()
    }

    #[must_use]
    pub fn history(&self) -> &NavigationHistory {
        &self.history
    }

    pub(super) fn history_mut(&mut self) -> &mut NavigationHistory {
        &mut self.history
    }

    #[must_use]
    pub fn selection(&self) -> &[ItemId] {
        &self.selection
    }

    pub fn set_selection(&mut self, items: impl IntoIterator<Item = ItemId>) {
        self.selection = items.into_iter().collect();
    }

    pub(super) fn duplicate(&self, id: TabId) -> Self {
        let mut duplicate = self.clone();
        duplicate.id = id;
        duplicate
    }

    pub(super) fn rehome(mut self, id: TabId) -> Self {
        self.id = id;
        self
    }

    pub(super) fn validate(&self, pane: u64) -> Result<(), NavigationError> {
        if self.id.pane != pane {
            return Err(NavigationError::InvalidDocument(
                "tab belongs to a different pane".into(),
            ));
        }
        self.history.validate()
    }

    pub(super) fn recover_missing(
        &mut self,
        exists: &impl Fn(&StorePath) -> bool,
        fallback: &StorePath,
    ) {
        self.history.recover_missing(exists, fallback);
        self.selection.clear();
    }
}
