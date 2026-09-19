use crate::toolbar::COMMAND_IDS;
use musheen_core::CommandRegistry;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticRegion {
    TabStrip,
    NavigationToolbar,
    Sidebar,
    DirectoryContent,
    Info,
    StatusBar,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FocusTarget {
    Tabs,
    Back,
    Forward,
    Parent,
    Refresh,
    Location,
    Search,
    ViewMode,
    InfoToggle,
    PaneSplit,
    Settings,
    Sidebar,
    Directory,
    Info,
}

const BASE_REGIONS: [SemanticRegion; 5] = [
    SemanticRegion::TabStrip,
    SemanticRegion::NavigationToolbar,
    SemanticRegion::Sidebar,
    SemanticRegion::DirectoryContent,
    SemanticRegion::StatusBar,
];

const INFO_REGIONS: [SemanticRegion; 6] = [
    SemanticRegion::TabStrip,
    SemanticRegion::NavigationToolbar,
    SemanticRegion::Sidebar,
    SemanticRegion::DirectoryContent,
    SemanticRegion::Info,
    SemanticRegion::StatusBar,
];

const BASE_FOCUS: [FocusTarget; 13] = [
    FocusTarget::Tabs,
    FocusTarget::Back,
    FocusTarget::Forward,
    FocusTarget::Parent,
    FocusTarget::Refresh,
    FocusTarget::Location,
    FocusTarget::Search,
    FocusTarget::ViewMode,
    FocusTarget::InfoToggle,
    FocusTarget::PaneSplit,
    FocusTarget::Settings,
    FocusTarget::Sidebar,
    FocusTarget::Directory,
];

const INFO_FOCUS: [FocusTarget; 14] = [
    FocusTarget::Tabs,
    FocusTarget::Back,
    FocusTarget::Forward,
    FocusTarget::Parent,
    FocusTarget::Refresh,
    FocusTarget::Location,
    FocusTarget::Search,
    FocusTarget::ViewMode,
    FocusTarget::InfoToggle,
    FocusTarget::PaneSplit,
    FocusTarget::Settings,
    FocusTarget::Sidebar,
    FocusTarget::Directory,
    FocusTarget::Info,
];

#[derive(Clone, Debug)]
pub struct ShellModel {
    commands: CommandRegistry,
    info_visible: bool,
}

impl ShellModel {
    #[must_use]
    pub fn new(info_visible: bool) -> Self {
        Self {
            commands: CommandRegistry::built_in(),
            info_visible,
        }
    }

    #[must_use]
    pub fn semantic_regions(&self) -> &'static [SemanticRegion] {
        if self.info_visible {
            &INFO_REGIONS
        } else {
            &BASE_REGIONS
        }
    }

    #[must_use]
    pub fn focus_order(&self) -> &'static [FocusTarget] {
        if self.info_visible {
            &INFO_FOCUS
        } else {
            &BASE_FOCUS
        }
    }

    #[must_use]
    pub fn toolbar_command_ids(&self) -> &'static [&'static str] {
        &COMMAND_IDS
    }

    #[must_use]
    pub fn commands(&self) -> &CommandRegistry {
        &self.commands
    }

    #[must_use]
    pub const fn info_visible(&self) -> bool {
        self.info_visible
    }

    pub fn toggle_info(&mut self) {
        self.info_visible = !self.info_visible;
    }
}

impl Default for ShellModel {
    fn default() -> Self {
        Self::new(false)
    }
}
