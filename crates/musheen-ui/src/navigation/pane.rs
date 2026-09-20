use super::{NavigationError, TabId, TabState};
use musheen_core::StorePath;
use serde::{Deserialize, Serialize};

const MAX_TABS_PER_PANE: usize = 128;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct PaneId(u64);

impl PaneId {
    pub(super) const fn new(value: u64) -> Self {
        Self(value)
    }

    pub(super) const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PaneState {
    id: PaneId,
    tabs: Vec<TabState>,
    active_tab: usize,
    next_tab_id: u64,
    closed_tabs: Vec<TabState>,
}

impl PaneState {
    pub(super) fn new(id: PaneId, initial: StorePath) -> Self {
        Self {
            id,
            tabs: vec![TabState::new(TabId::new(id.value(), 1), initial)],
            active_tab: 0,
            next_tab_id: 2,
            closed_tabs: Vec::new(),
        }
    }

    pub(super) fn from_tab(id: PaneId, tab: TabState) -> Self {
        Self {
            id,
            tabs: vec![tab.rehome(TabId::new(id.value(), 1))],
            active_tab: 0,
            next_tab_id: 2,
            closed_tabs: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> PaneId {
        self.id
    }

    #[must_use]
    pub fn tabs(&self) -> &[TabState] {
        &self.tabs
    }

    #[must_use]
    pub fn has_closed_tabs(&self) -> bool {
        !self.closed_tabs.is_empty()
    }

    pub(super) fn tabs_mut(&mut self) -> &mut [TabState] {
        &mut self.tabs
    }

    #[must_use]
    pub fn active_tab(&self) -> &TabState {
        &self.tabs[self.active_tab]
    }

    pub fn active_tab_mut(&mut self) -> &mut TabState {
        &mut self.tabs[self.active_tab]
    }

    pub fn create_tab(&mut self, location: StorePath) -> Result<TabId, NavigationError> {
        if self.tabs.len() >= MAX_TABS_PER_PANE {
            return Err(NavigationError::LimitReached("tabs per pane"));
        }
        let id = self.allocate_tab_id();
        self.tabs.push(TabState::new(id, location));
        self.active_tab = self.tabs.len() - 1;
        Ok(id)
    }

    pub fn duplicate_active_tab(&mut self) -> Result<TabId, NavigationError> {
        if self.tabs.len() >= MAX_TABS_PER_PANE {
            return Err(NavigationError::LimitReached("tabs per pane"));
        }
        let id = self.allocate_tab_id();
        let duplicate = self.active_tab().duplicate(id);
        self.tabs.insert(self.active_tab + 1, duplicate);
        self.active_tab += 1;
        Ok(id)
    }

    pub fn close_active_tab(&mut self) -> Result<TabId, NavigationError> {
        let tab = self.take_active_tab()?;
        let id = tab.id();
        self.closed_tabs.push(tab);
        Ok(id)
    }

    pub fn reopen_closed_tab(&mut self) -> Result<TabId, NavigationError> {
        let Some(tab) = self.closed_tabs.pop() else {
            return Err(NavigationError::NoClosedTab);
        };
        let id = tab.id();
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        Ok(id)
    }

    pub fn reorder_active_tab(&mut self, destination: usize) -> Result<(), NavigationError> {
        if destination >= self.tabs.len() {
            return Err(NavigationError::InvalidTabPosition);
        }
        let tab = self.tabs.remove(self.active_tab);
        self.tabs.insert(destination, tab);
        self.active_tab = destination;
        Ok(())
    }

    pub fn activate_tab(&mut self, id: TabId) -> Result<(), NavigationError> {
        let Some(index) = self.tabs.iter().position(|tab| tab.id() == id) else {
            return Err(NavigationError::UnknownTab);
        };
        self.active_tab = index;
        Ok(())
    }

    fn allocate_tab_id(&mut self) -> TabId {
        let id = TabId::new(self.id.value(), self.next_tab_id);
        self.next_tab_id = self.next_tab_id.saturating_add(1);
        id
    }

    pub(super) fn take_active_tab(&mut self) -> Result<TabState, NavigationError> {
        if self.tabs.len() == 1 {
            return Err(NavigationError::LastTab);
        }
        let tab = self.tabs.remove(self.active_tab);
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        Ok(tab)
    }

    pub(super) fn insert_tab(&mut self, tab: TabState) -> Result<TabId, NavigationError> {
        if self.tabs.len() >= MAX_TABS_PER_PANE {
            return Err(NavigationError::LimitReached("tabs per pane"));
        }
        let id = self.allocate_tab_id();
        self.tabs.push(tab.rehome(id));
        self.active_tab = self.tabs.len() - 1;
        Ok(id)
    }

    pub(super) fn validate(&self) -> Result<(), NavigationError> {
        if self.tabs.is_empty() || self.active_tab >= self.tabs.len() {
            return Err(NavigationError::InvalidDocument(
                "pane has no active tab".into(),
            ));
        }
        for tab in &self.tabs {
            tab.validate(self.id.value())?;
        }
        let mut ids = self.tabs.iter().map(TabState::id).collect::<Vec<_>>();
        ids.sort_by_key(|id| id.local());
        ids.dedup();
        if ids.len() != self.tabs.len() || ids.iter().any(|id| id.pane() != self.id.value()) {
            return Err(NavigationError::InvalidDocument(
                "pane contains duplicate or foreign tab IDs".into(),
            ));
        }
        Ok(())
    }

    pub(super) fn recover_missing(
        &mut self,
        exists: &impl Fn(&StorePath) -> bool,
        fallback: &StorePath,
    ) {
        for tab in &mut self.tabs {
            tab.recover_missing(exists, fallback);
        }
    }
}
