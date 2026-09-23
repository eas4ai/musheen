use super::{NavigationError, PaneId, PaneState, TabId, TabState};
use crate::views::{ViewPreferenceStore, ViewPreferences};
use musheen_core::StorePath;
use musheen_desktop::SESSION_SCHEMA_VERSION;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const MAX_PANES: usize = 2;
pub(crate) const MAX_WINDOWS: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NavigationFocus {
    Content,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NavigationOutcome {
    focus: NavigationFocus,
}

impl NavigationOutcome {
    #[must_use]
    pub const fn focus(self) -> NavigationFocus {
        self.focus
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WindowSession {
    panes: Vec<PaneState>,
    focused_pane: usize,
    next_pane_id: u64,
    #[serde(default)]
    view_preferences: ViewPreferenceStore,
}

#[derive(Deserialize, Serialize)]
struct SessionDocument {
    schema_version: u32,
    window: WindowSession,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationSession {
    windows: Vec<WindowSession>,
}

#[derive(Deserialize, Serialize)]
struct ApplicationSessionDocument {
    schema_version: u32,
    windows: Vec<WindowSession>,
}

impl ApplicationSession {
    pub fn new(windows: Vec<WindowSession>) -> Result<Self, NavigationError> {
        if windows.is_empty() || windows.len() > MAX_WINDOWS {
            return Err(NavigationError::InvalidDocument(
                "application session must contain 1 through 16 windows".into(),
            ));
        }
        for window in &windows {
            window.validate()?;
        }
        Ok(Self { windows })
    }

    #[must_use]
    pub fn windows(&self) -> &[WindowSession] {
        &self.windows
    }

    pub fn to_json(&self) -> Result<Vec<u8>, NavigationError> {
        serde_json::to_vec_pretty(&ApplicationSessionDocument {
            schema_version: SESSION_SCHEMA_VERSION,
            windows: self.windows.clone(),
        })
        .map_err(NavigationError::Serialize)
    }

    pub fn restore_json(
        bytes: &[u8],
        exists: impl Fn(&StorePath) -> bool,
        fallback: StorePath,
    ) -> Result<Self, NavigationError> {
        let document: ApplicationSessionDocument =
            serde_json::from_slice(bytes).map_err(NavigationError::Serialize)?;
        if document.schema_version != SESSION_SCHEMA_VERSION {
            return Err(NavigationError::InvalidDocument(
                "unsupported application session schema".into(),
            ));
        }
        let mut session = Self::new(document.windows)?;
        for window in &mut session.windows {
            for pane in &mut window.panes {
                pane.recover_missing(&exists, &fallback);
            }
        }
        Ok(session)
    }

    pub fn restore_compatible_json(
        bytes: &[u8],
        exists: impl Fn(&StorePath) -> bool,
        fallback: StorePath,
    ) -> Result<Self, NavigationError> {
        let shape: serde_json::Value =
            serde_json::from_slice(bytes).map_err(NavigationError::Serialize)?;
        if shape.get("windows").is_some() {
            return Self::restore_json(bytes, &exists, fallback);
        }
        if shape.get("window").is_some() {
            let window = WindowSession::restore_json(bytes, &exists, fallback)?;
            return Self::new(vec![window]);
        }
        Err(NavigationError::InvalidDocument(
            "session document has no window collection".into(),
        ))
    }
}

impl WindowSession {
    #[must_use]
    pub fn new(initial: StorePath) -> Self {
        Self {
            panes: vec![PaneState::new(PaneId::new(1), initial)],
            focused_pane: 0,
            next_pane_id: 2,
            view_preferences: ViewPreferenceStore::default(),
        }
    }

    #[must_use]
    pub fn panes(&self) -> &[PaneState] {
        &self.panes
    }

    #[must_use]
    pub fn focused_pane_id(&self) -> PaneId {
        self.focused_pane().id()
    }

    #[must_use]
    pub fn focused_pane(&self) -> &PaneState {
        &self.panes[self.focused_pane]
    }

    pub fn focused_pane_mut(&mut self) -> &mut PaneState {
        &mut self.panes[self.focused_pane]
    }

    #[must_use]
    pub fn preferences_for(&self, path: &StorePath) -> &ViewPreferences {
        self.view_preferences.for_path(path)
    }

    pub fn set_preferences_for(&mut self, path: StorePath, preferences: ViewPreferences) {
        self.view_preferences.set(path, preferences);
    }

    /// Applies startup defaults while retaining explicit per-directory choices.
    pub(crate) fn set_default_preferences(&mut self, preferences: ViewPreferences) {
        self.view_preferences.set_defaults(preferences);
        for pane in &mut self.panes {
            for tab in pane.tabs_mut() {
                tab.set_view_preferences(self.view_preferences.for_path(tab.location()).clone());
            }
        }
    }

    #[must_use]
    pub fn focused_tab(&self) -> &TabState {
        self.focused_pane().active_tab()
    }

    pub fn focused_tab_mut(&mut self) -> &mut TabState {
        self.focused_pane_mut().active_tab_mut()
    }

    #[must_use]
    pub fn tab(&self, id: TabId) -> Option<&TabState> {
        self.panes
            .iter()
            .flat_map(PaneState::tabs)
            .find(|tab| tab.id() == id)
    }

    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut TabState> {
        self.panes
            .iter_mut()
            .flat_map(|pane| pane.tabs_mut())
            .find(|tab| tab.id() == id)
    }

    pub fn focus_pane(&mut self, id: PaneId) -> Result<(), NavigationError> {
        let Some(index) = self.panes.iter().position(|pane| pane.id() == id) else {
            return Err(NavigationError::UnknownPane);
        };
        self.focused_pane = index;
        Ok(())
    }

    pub fn split_focused(&mut self, initial: StorePath) -> Result<PaneId, NavigationError> {
        if !self.can_split() {
            return Err(NavigationError::LimitReached("panes per window"));
        }
        let id = PaneId::new(self.next_pane_id);
        self.next_pane_id = self.next_pane_id.saturating_add(1);
        self.panes.push(PaneState::new(id, initial));
        self.focused_pane = self.panes.len() - 1;
        Ok(id)
    }

    #[must_use]
    pub fn can_split(&self) -> bool {
        self.panes.len() < MAX_PANES
    }

    pub fn duplicate_active_tab(&mut self) -> Result<TabId, NavigationError> {
        self.focused_pane_mut().duplicate_active_tab()
    }

    pub fn new_tab(&mut self, location: StorePath) -> Result<TabId, NavigationError> {
        self.focused_pane_mut().create_tab(location)
    }

    pub fn close_active_tab(&mut self) -> Result<TabId, NavigationError> {
        self.focused_pane_mut().close_active_tab()
    }

    pub fn reopen_closed_tab(&mut self) -> Result<TabId, NavigationError> {
        self.focused_pane_mut().reopen_closed_tab()
    }

    pub fn reorder_active_tab(&mut self, destination: usize) -> Result<(), NavigationError> {
        self.focused_pane_mut().reorder_active_tab(destination)
    }

    pub fn move_active_tab_to(&mut self, destination: PaneId) -> Result<TabId, NavigationError> {
        let Some(destination_index) = self.panes.iter().position(|pane| pane.id() == destination)
        else {
            return Err(NavigationError::UnknownPane);
        };
        if destination_index == self.focused_pane {
            return Err(NavigationError::SamePane);
        }
        // Validate the destination before taking ownership from the source.
        if !self.panes[destination_index].has_tab_capacity() {
            return Err(NavigationError::LimitReached("tabs per pane"));
        }
        let tab = self.focused_pane_mut().take_active_tab()?;
        self.panes[destination_index].insert_tab(tab)
    }

    pub fn tear_out_active_tab(&mut self) -> Result<Self, NavigationError> {
        let tab = self.focused_pane_mut().take_active_tab()?;
        Ok(Self {
            panes: vec![PaneState::from_tab(PaneId::new(1), tab)],
            focused_pane: 0,
            next_pane_id: 2,
            view_preferences: self.view_preferences.clone(),
        })
    }

    pub fn navigate_focused(&mut self, location: StorePath) -> NavigationOutcome {
        self.focused_tab_mut().history_mut().navigate(location);
        self.focused_tab_mut().set_selection([]);
        NavigationOutcome {
            focus: NavigationFocus::Content,
        }
    }

    pub fn go_back(&mut self) -> Option<&StorePath> {
        let tab = self.focused_tab_mut();
        tab.history_mut().back()?;
        tab.set_selection([]);
        Some(tab.location())
    }

    pub fn go_forward(&mut self) -> Option<&StorePath> {
        let tab = self.focused_tab_mut();
        tab.history_mut().forward()?;
        tab.set_selection([]);
        Some(tab.location())
    }

    pub fn to_json(&self) -> Result<Vec<u8>, NavigationError> {
        serde_json::to_vec_pretty(&SessionDocument {
            schema_version: SESSION_SCHEMA_VERSION,
            window: self.clone(),
        })
        .map_err(NavigationError::Serialize)
    }

    pub fn restore_json(
        bytes: &[u8],
        exists: impl Fn(&StorePath) -> bool,
        fallback: StorePath,
    ) -> Result<Self, NavigationError> {
        let document: SessionDocument =
            serde_json::from_slice(bytes).map_err(NavigationError::Serialize)?;
        if document.schema_version != SESSION_SCHEMA_VERSION {
            return Err(NavigationError::InvalidDocument(
                "unsupported session schema".into(),
            ));
        }
        let mut window = document.window;
        window.validate()?;
        for pane in &mut window.panes {
            pane.recover_missing(&exists, &fallback);
        }
        Ok(window)
    }

    fn validate(&self) -> Result<(), NavigationError> {
        if self.panes.is_empty()
            || self.panes.len() > MAX_PANES
            || self.focused_pane >= self.panes.len()
        {
            return Err(NavigationError::InvalidDocument(
                "window has no valid focused pane".into(),
            ));
        }
        for pane in &self.panes {
            pane.validate()?;
        }
        Ok(())
    }
}

pub trait SessionSink {
    type Error;

    fn save_session(&mut self, document: &[u8]) -> Result<(), Self::Error>;
}

impl SessionSink for musheen_desktop::SessionStore {
    type Error = musheen_desktop::SessionStoreError;

    fn save_session(&mut self, document: &[u8]) -> Result<(), Self::Error> {
        self.save(document)
    }
}

#[derive(Debug)]
pub enum SessionWriteError<E> {
    Serialize(NavigationError),
    Sink(E),
}

#[derive(Clone, Debug)]
pub struct SessionWriteDebouncer {
    delay: Duration,
    dirty_since: Option<Duration>,
}

impl SessionWriteDebouncer {
    #[must_use]
    pub const fn new(delay: Duration) -> Self {
        Self {
            delay,
            dirty_since: None,
        }
    }

    pub fn mark_dirty(&mut self, now: Duration) {
        self.dirty_since = Some(now);
    }

    pub fn flush_if_due<S: SessionSink>(
        &mut self,
        now: Duration,
        session: &WindowSession,
        sink: &mut S,
    ) -> Result<bool, SessionWriteError<S::Error>> {
        let Some(dirty_since) = self.dirty_since else {
            return Ok(false);
        };
        if now.saturating_sub(dirty_since) < self.delay {
            return Ok(false);
        }
        let document = session.to_json().map_err(SessionWriteError::Serialize)?;
        sink.save_session(&document)
            .map_err(SessionWriteError::Sink)?;
        self.dirty_since = None;
        Ok(true)
    }
}
