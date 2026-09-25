use std::path::{Path, PathBuf};

use musheen_core::StorePath;
use musheen_desktop::PasteDisposition;

const MIN_HEIGHT: f32 = 120.0;
const MAX_HEIGHT_FRACTION: f32 = 0.8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalDrawerAction {
    None,
    FocusTerminal,
    RestoreBrowserFocus,
    ConfirmTerminate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PasteRequest {
    text: String,
    disposition: PasteDisposition,
}

impl PasteRequest {
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    #[must_use]
    pub fn requires_confirmation(&self) -> bool {
        self.disposition == PasteDisposition::ConfirmationRequired
    }
}

#[derive(Clone, Debug)]
pub struct TerminalDrawer {
    open: bool,
    height: f32,
    follow_active_pane: bool,
    cwd: Option<PathBuf>,
    child_exited: bool,
    foreground_job: bool,
}

impl TerminalDrawer {
    #[must_use]
    pub fn new(open: bool, height: f32) -> Self {
        Self {
            open,
            height: height.max(MIN_HEIGHT),
            follow_active_pane: true,
            cwd: None,
            child_exited: false,
            foreground_job: false,
        }
    }

    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub const fn height(&self) -> f32 {
        self.height
    }

    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    pub fn set_active_location(&mut self, location: StorePath) {
        if self.follow_active_pane
            && let Some(path) = location.as_unix_path()
        {
            self.cwd = Some(path.to_path_buf());
        }
    }

    pub fn set_follow_active_pane(&mut self, follow: bool) {
        self.follow_active_pane = follow;
    }

    pub fn toggle(&mut self) -> TerminalDrawerAction {
        self.open = !self.open;
        if self.open {
            TerminalDrawerAction::FocusTerminal
        } else {
            TerminalDrawerAction::RestoreBrowserFocus
        }
    }

    pub fn close(&mut self) -> TerminalDrawerAction {
        if !self.open {
            return TerminalDrawerAction::None;
        }
        self.open = false;
        TerminalDrawerAction::RestoreBrowserFocus
    }

    pub fn resize(&mut self, requested_height: f32, available_height: f32) {
        self.height = requested_height.clamp(MIN_HEIGHT, available_height * MAX_HEIGHT_FRACTION);
    }

    #[must_use]
    pub fn request_paste(&self, text: impl Into<String>) -> PasteRequest {
        let text = text.into();
        let disposition = if text.contains(['\n', '\r'])
            || text
                .chars()
                .any(|character| character.is_control() && character != '\t')
        {
            PasteDisposition::ConfirmationRequired
        } else {
            PasteDisposition::Safe
        };
        PasteRequest { text, disposition }
    }

    pub fn mark_child_exited(&mut self) {
        self.child_exited = true;
    }

    pub fn mark_restarted(&mut self) {
        self.child_exited = false;
        self.foreground_job = false;
    }

    #[must_use]
    pub const fn restart_available(&self) -> bool {
        self.child_exited
    }

    pub fn mark_foreground_job(&mut self, active: bool) {
        self.foreground_job = active;
    }

    pub fn request_close(&mut self) -> TerminalDrawerAction {
        if self.foreground_job {
            TerminalDrawerAction::ConfirmTerminate
        } else {
            self.close()
        }
    }
}
