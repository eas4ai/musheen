use std::borrow::Borrow;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CommandId(Box<str>);

impl CommandId {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, CommandRegistryError> {
        let value = value.into();
        if value.is_empty()
            || !value.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._".contains(&byte)
            })
        {
            return Err(CommandRegistryError::InvalidId(value));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for CommandId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ShortcutScope {
    Global,
    Browser,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct Shortcut {
    chord: Box<str>,
    scope: ShortcutScope,
}

impl Shortcut {
    #[must_use]
    pub fn new(chord: &'static str, scope: ShortcutScope) -> Self {
        Self {
            chord: chord.into(),
            scope,
        }
    }

    #[must_use]
    pub fn chord(&self) -> &str {
        &self.chord
    }

    #[must_use]
    pub fn scope(&self) -> ShortcutScope {
        self.scope
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandAction {
    NavigateBack,
    NavigateForward,
    NavigateParent,
    Refresh,
    FocusLocation,
    Search,
    FocusCommand,
    ViewDetails,
    ViewList,
    ViewCards,
    ViewGrid,
    ViewColumns,
    ViewAdaptive,
    CycleSort,
    CycleGroup,
    ToggleDirectoriesFirst,
    ToggleHidden,
    ToggleSidebar,
    ToggleInfo,
    NewTab,
    CloseTab,
    DuplicateTab,
    ReopenClosedTab,
    MoveTabOtherPane,
    MoveTabLeft,
    MoveTabRight,
    TearOutTab,
    SplitPane,
    FocusNextPane,
    SelectAll,
    ClearSelection,
    OpenSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandHandler(CommandAction);

impl CommandHandler {
    #[must_use]
    pub fn action(self) -> CommandAction {
        self.0
    }

    pub fn invoke(
        self,
        dispatcher: &mut dyn CommandDispatcher,
    ) -> Result<(), CommandDispatchError> {
        dispatcher.dispatch(self.0)
    }
}

pub trait CommandDispatcher {
    fn dispatch(&mut self, action: CommandAction) -> Result<(), CommandDispatchError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandDispatchError(Box<str>);

impl CommandDispatchError {
    #[must_use]
    pub fn new(message: impl Into<Box<str>>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for CommandDispatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for CommandDispatchError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CommandContext {
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub has_parent: bool,
    pub item_count: usize,
    pub selection_count: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPredicate {
    Always,
    CanGoBack,
    CanGoForward,
    HasParent,
    HasItems,
    HasSelection,
}

impl CommandPredicate {
    fn evaluate(self, context: &CommandContext) -> CommandState {
        match self {
            Self::Always => CommandState::enabled(),
            Self::CanGoBack if context.can_go_back => CommandState::enabled(),
            Self::CanGoForward if context.can_go_forward => CommandState::enabled(),
            Self::HasParent if context.has_parent => CommandState::enabled(),
            Self::HasItems if context.item_count > 0 => CommandState::enabled(),
            Self::HasSelection if context.selection_count > 0 => CommandState::enabled(),
            Self::CanGoBack => CommandState::disabled("there is no earlier history entry"),
            Self::CanGoForward => CommandState::disabled("there is no later history entry"),
            Self::HasParent => CommandState::disabled("the current location has no parent"),
            Self::HasItems => CommandState::disabled("the current view has no items"),
            Self::HasSelection => CommandState::disabled("no items are selected"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CommandState {
    enabled: bool,
    disabled_reason: Option<&'static str>,
}

impl CommandState {
    const fn enabled() -> Self {
        Self {
            enabled: true,
            disabled_reason: None,
        }
    }

    const fn disabled(reason: &'static str) -> Self {
        Self {
            enabled: false,
            disabled_reason: Some(reason),
        }
    }

    #[must_use]
    pub fn is_enabled(self) -> bool {
        self.enabled
    }

    #[must_use]
    pub fn disabled_reason(self) -> Option<&'static str> {
        self.disabled_reason
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandDefinition {
    id: CommandId,
    label_key: Box<str>,
    icon_key: Box<str>,
    shortcuts: Vec<Shortcut>,
    predicate: CommandPredicate,
    handler: CommandHandler,
}

impl CommandDefinition {
    #[must_use]
    pub fn id(&self) -> &CommandId {
        &self.id
    }

    #[must_use]
    pub fn label_key(&self) -> &str {
        &self.label_key
    }

    #[must_use]
    pub fn icon_key(&self) -> &str {
        &self.icon_key
    }

    #[must_use]
    pub fn shortcuts(&self) -> &[Shortcut] {
        &self.shortcuts
    }

    #[must_use]
    pub fn predicate(&self) -> CommandPredicate {
        self.predicate
    }

    #[must_use]
    pub fn handler(&self) -> CommandHandler {
        self.handler
    }

    #[must_use]
    pub fn action(&self) -> CommandAction {
        self.handler.action()
    }

    #[must_use]
    pub fn state(&self, context: &CommandContext) -> CommandState {
        self.predicate.evaluate(context)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPresentation {
    Toolbar,
    Menu,
    Palette,
}

impl CommandPresentation {
    pub const ALL: [Self; 3] = [Self::Toolbar, Self::Menu, Self::Palette];
}

#[derive(Clone, Copy, Debug)]
pub struct CommandProjection<'a> {
    command: &'a CommandDefinition,
    presentation: CommandPresentation,
}

impl CommandProjection<'_> {
    #[must_use]
    pub fn command(&self) -> &CommandDefinition {
        self.command
    }

    #[must_use]
    pub fn presentation(&self) -> CommandPresentation {
        self.presentation
    }
}

#[derive(Clone, Debug)]
pub struct CommandRegistry {
    commands: Vec<CommandDefinition>,
    by_id: HashMap<CommandId, usize>,
}

impl CommandRegistry {
    #[must_use]
    pub fn built_in() -> Self {
        Self::try_new(built_in_commands()).expect("built-in command metadata must be valid")
    }

    pub fn try_new(commands: Vec<CommandDefinition>) -> Result<Self, CommandRegistryError> {
        let mut by_id = HashMap::with_capacity(commands.len());
        let mut shortcuts = HashSet::new();
        for (index, command) in commands.iter().enumerate() {
            if by_id.insert(command.id.clone(), index).is_some() {
                return Err(CommandRegistryError::DuplicateId(command.id.clone()));
            }
            for shortcut in &command.shortcuts {
                let key = (shortcut.scope, shortcut.chord.to_ascii_lowercase());
                if !shortcuts.insert(key) {
                    return Err(CommandRegistryError::DuplicateShortcut {
                        scope: shortcut.scope,
                        chord: shortcut.chord.clone(),
                    });
                }
            }
        }
        Ok(Self { commands, by_id })
    }

    #[must_use]
    pub fn commands(&self) -> &[CommandDefinition] {
        &self.commands
    }

    #[must_use]
    pub fn get(&self, id: &str) -> Option<&CommandDefinition> {
        self.by_id.get(id).map(|index| &self.commands[*index])
    }

    #[must_use]
    pub fn project(
        &self,
        id: &CommandId,
        presentation: CommandPresentation,
    ) -> Option<CommandProjection<'_>> {
        self.get(id.as_str()).map(|command| CommandProjection {
            command,
            presentation,
        })
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::built_in()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandRegistryError {
    InvalidId(Box<str>),
    DuplicateId(CommandId),
    DuplicateShortcut {
        scope: ShortcutScope,
        chord: Box<str>,
    },
}

impl fmt::Display for CommandRegistryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(formatter, "invalid command ID {id:?}"),
            Self::DuplicateId(id) => write!(formatter, "duplicate command ID {}", id.as_str()),
            Self::DuplicateShortcut { scope, chord } => {
                write!(formatter, "duplicate {scope:?} shortcut {chord}")
            }
        }
    }
}

impl Error for CommandRegistryError {}

fn built_in_commands() -> Vec<CommandDefinition> {
    use CommandAction as Action;
    use CommandPredicate as Predicate;
    use ShortcutScope::{Browser, Global};

    vec![
        command(
            "navigation.back",
            "command.back",
            "arrow-left",
            &[("Alt+Left", Browser)],
            Predicate::CanGoBack,
            Action::NavigateBack,
        ),
        command(
            "navigation.forward",
            "command.forward",
            "arrow-right",
            &[("Alt+Right", Browser)],
            Predicate::CanGoForward,
            Action::NavigateForward,
        ),
        command(
            "navigation.parent",
            "command.parent",
            "arrow-up",
            &[("Alt+Up", Browser)],
            Predicate::HasParent,
            Action::NavigateParent,
        ),
        command(
            "navigation.refresh",
            "command.refresh",
            "refresh-cw",
            &[("F5", Browser)],
            Predicate::Always,
            Action::Refresh,
        ),
        command(
            "navigation.location",
            "command.location",
            "text-cursor-input",
            &[("Ctrl+L", Browser)],
            Predicate::Always,
            Action::FocusLocation,
        ),
        command(
            "view.search",
            "command.search",
            "search",
            &[("Ctrl+F", Browser)],
            Predicate::Always,
            Action::Search,
        ),
        command(
            "view.command",
            "command.command-mode",
            "text-cursor-input",
            &[("Ctrl+Shift+P", Browser)],
            Predicate::Always,
            Action::FocusCommand,
        ),
        command(
            "view.details",
            "command.view-details",
            "list-checks",
            &[("Ctrl+1", Browser)],
            Predicate::Always,
            Action::ViewDetails,
        ),
        command(
            "view.list",
            "command.view-list",
            "list",
            &[("Ctrl+2", Browser)],
            Predicate::Always,
            Action::ViewList,
        ),
        command(
            "view.cards",
            "command.view-cards",
            "grid-2x2",
            &[("Ctrl+3", Browser)],
            Predicate::Always,
            Action::ViewCards,
        ),
        command(
            "view.grid",
            "command.view-grid",
            "grid-2x2",
            &[("Ctrl+4", Browser)],
            Predicate::Always,
            Action::ViewGrid,
        ),
        command(
            "view.columns",
            "command.view-columns",
            "columns-2",
            &[("Ctrl+5", Browser)],
            Predicate::Always,
            Action::ViewColumns,
        ),
        command(
            "view.adaptive",
            "command.view-adaptive",
            "panel-right",
            &[("Ctrl+6", Browser)],
            Predicate::Always,
            Action::ViewAdaptive,
        ),
        command(
            "view.sort",
            "command.sort",
            "list",
            &[],
            Predicate::Always,
            Action::CycleSort,
        ),
        command(
            "view.group",
            "command.group",
            "list-checks",
            &[],
            Predicate::Always,
            Action::CycleGroup,
        ),
        command(
            "view.directories_first",
            "command.directories-first",
            "folder",
            &[],
            Predicate::Always,
            Action::ToggleDirectoriesFirst,
        ),
        command(
            "view.hidden",
            "command.show-hidden",
            "text-cursor-input",
            &[("Ctrl+H", Browser)],
            Predicate::Always,
            Action::ToggleHidden,
        ),
        command(
            "view.sidebar",
            "command.sidebar",
            "panel-right",
            &[("Ctrl+B", Browser)],
            Predicate::Always,
            Action::ToggleSidebar,
        ),
        command(
            "view.info",
            "command.info-pane",
            "info",
            &[],
            Predicate::Always,
            Action::ToggleInfo,
        ),
        command(
            "tab.new",
            "command.new-tab",
            "plus",
            &[("Ctrl+T", Browser)],
            Predicate::Always,
            Action::NewTab,
        ),
        command(
            "tab.close",
            "command.close-tab",
            "x",
            &[("Ctrl+W", Browser)],
            Predicate::Always,
            Action::CloseTab,
        ),
        command(
            "tab.duplicate",
            "command.duplicate-tab",
            "copy",
            &[],
            Predicate::Always,
            Action::DuplicateTab,
        ),
        command(
            "tab.reopen_closed",
            "command.reopen-closed-tab",
            "rotate-ccw",
            &[("Ctrl+Shift+T", Browser)],
            Predicate::Always,
            Action::ReopenClosedTab,
        ),
        command(
            "tab.move_other_pane",
            "command.move-tab-other-pane",
            "panel-right",
            &[],
            Predicate::Always,
            Action::MoveTabOtherPane,
        ),
        command(
            "tab.move_left",
            "command.move-tab-left",
            "arrow-left",
            &[],
            Predicate::Always,
            Action::MoveTabLeft,
        ),
        command(
            "tab.move_right",
            "command.move-tab-right",
            "arrow-right",
            &[],
            Predicate::Always,
            Action::MoveTabRight,
        ),
        command(
            "tab.tear_out",
            "command.tear-out-tab",
            "copy",
            &[],
            Predicate::Always,
            Action::TearOutTab,
        ),
        command(
            "pane.split",
            "command.split-pane",
            "columns-2",
            &[("F3", Browser)],
            Predicate::Always,
            Action::SplitPane,
        ),
        command(
            "pane.focus_next",
            "command.focus-next-pane",
            "panel-right",
            &[("F6", Browser)],
            Predicate::Always,
            Action::FocusNextPane,
        ),
        command(
            "selection.select_all",
            "command.select-all",
            "list-checks",
            &[("Ctrl+A", Browser)],
            Predicate::HasItems,
            Action::SelectAll,
        ),
        command(
            "selection.clear",
            "command.clear-selection",
            "x",
            &[("Escape", Browser)],
            Predicate::HasSelection,
            Action::ClearSelection,
        ),
        command(
            "app.settings",
            "command.settings",
            "settings",
            &[("Ctrl+,", Global)],
            Predicate::Always,
            Action::OpenSettings,
        ),
    ]
}

fn command(
    id: &'static str,
    label_key: &'static str,
    icon_key: &'static str,
    shortcuts: &[(&'static str, ShortcutScope)],
    predicate: CommandPredicate,
    action: CommandAction,
) -> CommandDefinition {
    CommandDefinition {
        id: CommandId::new(id).expect("a built-in command ID is valid"),
        label_key: label_key.into(),
        icon_key: icon_key.into(),
        shortcuts: shortcuts
            .iter()
            .map(|(chord, scope)| Shortcut::new(chord, *scope))
            .collect(),
        predicate,
        handler: CommandHandler(action),
    }
}
