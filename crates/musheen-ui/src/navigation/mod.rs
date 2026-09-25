mod breadcrumbs;
mod history;
mod omnibar;
mod pane;
mod session;
mod tab;

pub use breadcrumbs::{Breadcrumb, BreadcrumbTrail};
pub use history::NavigationHistory;
pub use omnibar::{
    OmnibarMode, OmnibarState, OmnibarSubmission, PathSuggestion, resolve_path_input,
    suggest_local_paths,
};
pub use pane::{PaneId, PaneState};
pub(crate) use session::MAX_WINDOWS;
pub use session::{
    ApplicationSession, NavigationFocus, NavigationOutcome, SessionSink, SessionWriteDebouncer,
    SessionWriteError, WindowSession,
};
pub use tab::{TabId, TabState};

use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum NavigationError {
    UnknownPane,
    UnknownTab,
    SamePane,
    LastTab,
    NoClosedTab,
    InvalidTabPosition,
    LimitReached(&'static str),
    InvalidDocument(Box<str>),
    Serialize(serde_json::Error),
}

impl fmt::Display for NavigationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPane => formatter.write_str("pane does not exist"),
            Self::UnknownTab => formatter.write_str("tab does not exist in the pane"),
            Self::SamePane => formatter.write_str("tab is already in the destination pane"),
            Self::LastTab => formatter.write_str("a pane must retain one tab"),
            Self::NoClosedTab => formatter.write_str("there is no closed tab to reopen"),
            Self::InvalidTabPosition => formatter.write_str("tab position is outside the pane"),
            Self::LimitReached(limit) => write!(formatter, "navigation limit reached: {limit}"),
            Self::InvalidDocument(message) => write!(formatter, "invalid session: {message}"),
            Self::Serialize(error) => write!(formatter, "session serialization failed: {error}"),
        }
    }
}

impl Error for NavigationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Serialize(error) => Some(error),
            _ => None,
        }
    }
}
