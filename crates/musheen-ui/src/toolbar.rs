//! Command IDs consumed by the live shell surfaces.
//!
//! Renderers and input handlers use this inventory directly. It is therefore
//! independent evidence of which registry commands each surface projects.

use musheen_core::{CommandAction, CommandDefinition, CommandId, CommandRegistry, ToolbarLayout};

pub const COMMAND_IDS: [&str; 21] = [
    "navigation.back",
    "navigation.forward",
    "navigation.parent",
    "navigation.refresh",
    "navigation.location",
    "view.search",
    "view.details",
    "view.list",
    "view.cards",
    "view.grid",
    "view.columns",
    "view.adaptive",
    "view.sort",
    "view.group",
    "view.directories_first",
    "view.hidden",
    "view.sidebar",
    "view.info",
    "pane.split",
    "pane.focus_next",
    "app.settings",
];

pub const NAVIGATION_LEADING_IDS: [&str; 4] = [
    "navigation.back",
    "navigation.forward",
    "navigation.parent",
    "navigation.refresh",
];
pub const OMNIBAR_COMMAND_ID: &str = "navigation.location";
pub const SEARCH_COMMAND_ID: &str = "view.search";
pub const VIEW_COMMAND_IDS: [&str; 10] = [
    "view.details",
    "view.list",
    "view.cards",
    "view.grid",
    "view.columns",
    "view.adaptive",
    "view.sort",
    "view.group",
    "view.directories_first",
    "view.hidden",
];
pub const NAVIGATION_TRAILING_IDS: [&str; 5] = [
    "view.sidebar",
    "view.info",
    "pane.split",
    "pane.focus_next",
    "app.settings",
];
pub const TAB_STRIP_COMMAND_IDS: [&str; 1] = ["tab.new"];
pub const CUSTOM_TOOLBAR_VISIBLE_LIMIT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum FixedCommandSurface {
    NavigationToolbar,
    WideViewControls,
    CompactOverflow,
    TabStrip,
    StaticShortcut,
}

impl FixedCommandSurface {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::NavigationToolbar => "navigation-toolbar",
            Self::WideViewControls => "wide-view-controls",
            Self::CompactOverflow => "compact-overflow",
            Self::TabStrip => "tab-strip",
            Self::StaticShortcut => "static-shortcut",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StaticShortcut {
    chord: &'static str,
    command_id: &'static str,
    action: CommandAction,
}

impl StaticShortcut {
    const fn new(chord: &'static str, command_id: &'static str, action: CommandAction) -> Self {
        Self {
            chord,
            command_id,
            action,
        }
    }

    #[must_use]
    pub const fn chord(self) -> &'static str {
        self.chord
    }

    #[must_use]
    pub const fn command_id(self) -> &'static str {
        self.command_id
    }

    #[must_use]
    pub const fn action(self) -> CommandAction {
        self.action
    }
}

pub const STATIC_SHORTCUTS: [StaticShortcut; 23] = [
    StaticShortcut::new("alt-left", "navigation.back", CommandAction::NavigateBack),
    StaticShortcut::new(
        "alt-right",
        "navigation.forward",
        CommandAction::NavigateForward,
    ),
    StaticShortcut::new("alt-up", "navigation.parent", CommandAction::NavigateParent),
    StaticShortcut::new("f5", "navigation.refresh", CommandAction::Refresh),
    StaticShortcut::new(
        "ctrl-l",
        "navigation.location",
        CommandAction::FocusLocation,
    ),
    StaticShortcut::new("ctrl-f", "view.search", CommandAction::Search),
    StaticShortcut::new("ctrl-shift-f", "view.filter", CommandAction::Filter),
    StaticShortcut::new("ctrl-shift-p", "view.command", CommandAction::FocusCommand),
    StaticShortcut::new("ctrl-t", "tab.new", CommandAction::NewTab),
    StaticShortcut::new("ctrl-w", "tab.close", CommandAction::CloseTab),
    StaticShortcut::new(
        "ctrl-shift-t",
        "tab.reopen_closed",
        CommandAction::ReopenClosedTab,
    ),
    StaticShortcut::new("f3", "pane.split", CommandAction::SplitPane),
    StaticShortcut::new("f6", "pane.focus_next", CommandAction::FocusNextPane),
    StaticShortcut::new("ctrl-a", "selection.select_all", CommandAction::SelectAll),
    StaticShortcut::new("ctrl-h", "view.hidden", CommandAction::ToggleHidden),
    StaticShortcut::new("ctrl-1", "view.details", CommandAction::ViewDetails),
    StaticShortcut::new("ctrl-2", "view.list", CommandAction::ViewList),
    StaticShortcut::new("ctrl-3", "view.cards", CommandAction::ViewCards),
    StaticShortcut::new("ctrl-4", "view.grid", CommandAction::ViewGrid),
    StaticShortcut::new("ctrl-5", "view.columns", CommandAction::ViewColumns),
    StaticShortcut::new("ctrl-6", "view.adaptive", CommandAction::ViewAdaptive),
    StaticShortcut::new("ctrl-b", "view.sidebar", CommandAction::ToggleSidebar),
    StaticShortcut::new(
        "alt-enter",
        "item.properties",
        CommandAction::OpenProperties,
    ),
];

#[must_use]
pub fn fixed_surface_ids(surface: FixedCommandSurface) -> Vec<&'static str> {
    match surface {
        FixedCommandSurface::NavigationToolbar => NAVIGATION_LEADING_IDS
            .into_iter()
            .chain([OMNIBAR_COMMAND_ID, SEARCH_COMMAND_ID])
            .chain(VIEW_COMMAND_IDS)
            .chain(NAVIGATION_TRAILING_IDS)
            .collect(),
        FixedCommandSurface::WideViewControls | FixedCommandSurface::CompactOverflow => {
            VIEW_COMMAND_IDS.to_vec()
        }
        FixedCommandSurface::TabStrip => TAB_STRIP_COMMAND_IDS.to_vec(),
        FixedCommandSurface::StaticShortcut => STATIC_SHORTCUTS
            .iter()
            .map(|binding| binding.command_id())
            .collect(),
    }
}

#[must_use]
pub fn static_shortcut(action: CommandAction) -> Option<StaticShortcut> {
    STATIC_SHORTCUTS
        .iter()
        .copied()
        .find(|binding| binding.action == action)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CustomToolbarProjection {
    visible: Vec<CommandId>,
    overflow: Vec<CommandId>,
}

impl CustomToolbarProjection {
    #[must_use]
    pub fn visible(&self) -> &[CommandId] {
        &self.visible
    }

    #[must_use]
    pub fn overflow(&self) -> &[CommandId] {
        &self.overflow
    }
}

#[must_use]
pub fn project_custom_toolbar(layout: &ToolbarLayout) -> CustomToolbarProjection {
    let visible = layout
        .ids()
        .iter()
        .take(CUSTOM_TOOLBAR_VISIBLE_LIMIT)
        .cloned()
        .collect();
    let overflow = layout
        .ids()
        .iter()
        .skip(CUSTOM_TOOLBAR_VISIBLE_LIMIT)
        .cloned()
        .collect();
    CustomToolbarProjection { visible, overflow }
}

pub fn customizable_commands(
    registry: &CommandRegistry,
) -> impl Iterator<Item = &CommandDefinition> {
    registry.commands().iter()
}

#[must_use]
pub fn resolve_command_mode<'a>(
    registry: &'a CommandRegistry,
    query: &str,
) -> Option<&'a CommandDefinition> {
    let query = query.trim();
    registry.get(query).or_else(|| {
        registry.commands().iter().find(|command| {
            command
                .label_key()
                .rsplit('.')
                .next()
                .is_some_and(|label| label == query)
        })
    })
}
