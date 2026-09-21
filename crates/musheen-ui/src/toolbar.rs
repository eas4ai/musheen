//! Command IDs consumed by the live shell surfaces.
//!
//! Renderers and input handlers use this inventory directly. It is therefore
//! independent evidence of which registry commands each surface projects.

use crate::navigation::OmnibarMode;
use musheen_core::{CommandAction, CommandDefinition, CommandId, CommandRegistry, ToolbarLayout};

macro_rules! static_shortcut_declarations {
    ($consumer:ident $(, $argument:ident)*) => {
        $consumer! {$($argument,)* [
            NavigateBack => ("navigation.back", "alt-left", GoBack, GoBack, None),
            NavigateForward => ("navigation.forward", "alt-right", GoForward, GoForward, None),
            NavigateParent => ("navigation.parent", "alt-up", GoParent, GoParent, None),
            Refresh => ("navigation.refresh", "f5", Reload, Reload, None),
            FocusLocation => ("navigation.location", "ctrl-l", EditLocation, EditLocation, None),
            Search => ("view.search", "ctrl-f", SearchLocation, SearchLocation, Some("!Input")),
            Filter => ("view.filter", "ctrl-shift-f", FilterLocation, FilterLocation, None),
            FocusCommand => ("view.command", "ctrl-shift-p", OpenCommandMode, OpenCommandMode, None),
            NewTab => ("tab.new", "ctrl-t", NewTabShortcut, NewTabShortcut, None),
            CloseTab => ("tab.close", "ctrl-w", CloseTabShortcut, CloseTabShortcut, None),
            ReopenClosedTab => ("tab.reopen_closed", "ctrl-shift-t", ReopenClosedTabShortcut, ReopenClosedTabShortcut, None),
            SplitPane => ("pane.split", "f3", SplitPaneShortcut, SplitPaneShortcut, None),
            FocusNextPane => ("pane.focus_next", "f6", FocusNextPaneShortcut, FocusNextPaneShortcut, None),
            SelectAll => ("selection.select_all", "ctrl-a", SelectAllShortcut, SelectAllShortcut, Some("!Input")),
            ToggleHidden => ("view.hidden", "ctrl-h", ToggleHiddenShortcut, ToggleHiddenShortcut, Some("!Input")),
            ViewDetails => ("view.details", "ctrl-1", ViewDetailsShortcut, ViewDetailsShortcut, None),
            ViewList => ("view.list", "ctrl-2", ViewListShortcut, ViewListShortcut, None),
            ViewCards => ("view.cards", "ctrl-3", ViewCardsShortcut, ViewCardsShortcut, None),
            ViewGrid => ("view.grid", "ctrl-4", ViewGridShortcut, ViewGridShortcut, None),
            ViewColumns => ("view.columns", "ctrl-5", ViewColumnsShortcut, ViewColumnsShortcut, None),
            ViewAdaptive => ("view.adaptive", "ctrl-6", ViewAdaptiveShortcut, ViewAdaptiveShortcut, None),
            ToggleSidebar => ("view.sidebar", "ctrl-b", ToggleSidebarShortcut, ToggleSidebarShortcut, None),
            OpenProperties => ("item.properties", "alt-enter", OpenPropertiesShortcut, OpenPropertiesShortcut, None),
        ]}
    };
}

pub(crate) use static_shortcut_declarations;

pub const NAVIGATION_LEADING_IDS: [&str; 4] = [
    "navigation.back",
    "navigation.forward",
    "navigation.parent",
    "navigation.refresh",
];
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OmnibarCommand {
    command_id: &'static str,
    mode: OmnibarMode,
    button_id: &'static str,
}

impl OmnibarCommand {
    const fn new(command_id: &'static str, mode: OmnibarMode, button_id: &'static str) -> Self {
        Self {
            command_id,
            mode,
            button_id,
        }
    }

    #[must_use]
    pub const fn command_id(self) -> &'static str {
        self.command_id
    }

    #[must_use]
    pub const fn mode(self) -> OmnibarMode {
        self.mode
    }

    #[must_use]
    pub const fn button_id(self) -> &'static str {
        self.button_id
    }
}

pub const OMNIBAR_COMMANDS: [OmnibarCommand; 4] = [
    OmnibarCommand::new("navigation.location", OmnibarMode::Path, "omnibar-path"),
    OmnibarCommand::new("view.search", OmnibarMode::Search, "omnibar-search"),
    OmnibarCommand::new("view.filter", OmnibarMode::Filter, "omnibar-filter"),
    OmnibarCommand::new("view.command", OmnibarMode::Command, "omnibar-command"),
];

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

macro_rules! define_static_shortcuts {
    ([$($command:ident => ($command_id:literal, $chord:literal, $action_type:ident, $action:expr, $context:expr),)*]) => {
        pub const STATIC_SHORTCUTS: [StaticShortcut; 23] = [
            $(StaticShortcut {
                chord: $chord,
                command_id: $command_id,
                action: CommandAction::$command,
            },)*
        ];
    };
}

static_shortcut_declarations!(define_static_shortcuts);

#[must_use]
pub fn fixed_surface_ids(surface: FixedCommandSurface) -> Vec<&'static str> {
    match surface {
        FixedCommandSurface::NavigationToolbar => {
            let mut ids = Vec::new();
            for id in NAVIGATION_LEADING_IDS
                .into_iter()
                .chain(OMNIBAR_COMMANDS.map(OmnibarCommand::command_id))
                .chain([SEARCH_COMMAND_ID])
                .chain(VIEW_COMMAND_IDS)
                .chain(NAVIGATION_TRAILING_IDS)
            {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            ids
        }
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
pub fn omnibar_command_for_action(
    registry: &CommandRegistry,
    action: CommandAction,
) -> Option<OmnibarCommand> {
    OMNIBAR_COMMANDS.iter().copied().find(|binding| {
        registry
            .get(binding.command_id())
            .is_some_and(|command| command.action() == action)
    })
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
    registry
        .commands()
        .iter()
        .filter(|command| is_direct_surface_command(command))
}

#[must_use]
pub fn is_direct_surface_command(command: &CommandDefinition) -> bool {
    !matches!(
        command.action(),
        CommandAction::OpenWith
            | CommandAction::SetDefaultApplication
            | CommandAction::SendTo
            | CommandAction::CustomAction
    )
}

#[must_use]
pub fn resolve_command_mode<'a>(
    registry: &'a CommandRegistry,
    query: &str,
) -> Option<&'a CommandDefinition> {
    let query = query.trim();
    registry
        .get(query)
        .filter(|command| is_direct_surface_command(command))
        .or_else(|| {
            registry.commands().iter().find(|command| {
                is_direct_surface_command(command)
                    && command
                        .label_key()
                        .rsplit('.')
                        .next()
                        .is_some_and(|label| label == query)
            })
        })
}
