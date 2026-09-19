use crate::{CapabilityKind, CapabilityState, CommandContext, CommandParameters, CommandTarget};
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
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._".contains(&b))
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

/// Stable behaviors projected by every menu, toolbar, and keyboard surface.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum CommandAction {
    NavigateBack,
    NavigateForward,
    NavigateParent,
    Refresh,
    FocusLocation,
    Search,
    Filter,
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
    OpenProperties,
    OpenSettings,
    Open,
    OpenWith,
    ChooseApplication,
    SetDefaultApplication,
    SendTo,
    Cut,
    Copy,
    CopyTo,
    MoveTo,
    PasteInto,
    Rename,
    Duplicate,
    CreateSymbolicLink,
    CreateHardLink,
    Compress,
    Extract,
    Hide,
    Unhide,
    MoveToTrash,
    DeletePermanently,
    Permissions,
    OpenAsAdministrator,
    RunAsAdministrator,
    NewDirectory,
    NewEmptyFile,
    NewFromTemplate,
    OpenTerminalHere,
    DirectoryProperties,
    OpenInNewTab,
    OpenInNewWindow,
    OpenInOtherPane,
    Pin,
    Unpin,
    CopyLocation,
    ManageTags,
    Share,
    Preview,
    BrowseArchive,
    Run,
    Unmount,
    Eject,
    PowerOff,
    Restore,
    EmptyTrash,
    CustomAction,
}
impl CommandAction {
    pub const ALL: [Self; 79] = [
        Self::NavigateBack,
        Self::NavigateForward,
        Self::NavigateParent,
        Self::Refresh,
        Self::FocusLocation,
        Self::Search,
        Self::Filter,
        Self::FocusCommand,
        Self::ViewDetails,
        Self::ViewList,
        Self::ViewCards,
        Self::ViewGrid,
        Self::ViewColumns,
        Self::ViewAdaptive,
        Self::CycleSort,
        Self::CycleGroup,
        Self::ToggleDirectoriesFirst,
        Self::ToggleHidden,
        Self::ToggleSidebar,
        Self::ToggleInfo,
        Self::NewTab,
        Self::CloseTab,
        Self::DuplicateTab,
        Self::ReopenClosedTab,
        Self::MoveTabOtherPane,
        Self::MoveTabLeft,
        Self::MoveTabRight,
        Self::TearOutTab,
        Self::SplitPane,
        Self::FocusNextPane,
        Self::SelectAll,
        Self::ClearSelection,
        Self::OpenProperties,
        Self::OpenSettings,
        Self::Open,
        Self::OpenWith,
        Self::ChooseApplication,
        Self::SetDefaultApplication,
        Self::SendTo,
        Self::Cut,
        Self::Copy,
        Self::CopyTo,
        Self::MoveTo,
        Self::PasteInto,
        Self::Rename,
        Self::Duplicate,
        Self::CreateSymbolicLink,
        Self::CreateHardLink,
        Self::Compress,
        Self::Extract,
        Self::Hide,
        Self::Unhide,
        Self::MoveToTrash,
        Self::DeletePermanently,
        Self::Permissions,
        Self::OpenAsAdministrator,
        Self::RunAsAdministrator,
        Self::NewDirectory,
        Self::NewEmptyFile,
        Self::NewFromTemplate,
        Self::OpenTerminalHere,
        Self::DirectoryProperties,
        Self::OpenInNewTab,
        Self::OpenInNewWindow,
        Self::OpenInOtherPane,
        Self::Pin,
        Self::Unpin,
        Self::CopyLocation,
        Self::ManageTags,
        Self::Share,
        Self::Preview,
        Self::BrowseArchive,
        Self::Run,
        Self::Unmount,
        Self::Eject,
        Self::PowerOff,
        Self::Restore,
        Self::EmptyTrash,
        Self::CustomAction,
    ];
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandGroup {
    Open,
    Navigation,
    Clipboard,
    Creation,
    FileType,
    Organization,
    Destructive,
    Details,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DangerLevel {
    None,
    Review,
    Destructive,
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
        parameters: CommandParameters,
    ) -> Result<(), CommandDispatchError> {
        dispatcher.dispatch(self.0, parameters)
    }
}
pub trait CommandDispatcher {
    fn dispatch(
        &mut self,
        action: CommandAction,
        parameters: CommandParameters,
    ) -> Result<(), CommandDispatchError>;
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
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl Error for CommandDispatchError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandPredicate {
    Always,
    CanGoBack,
    CanGoForward,
    HasParent,
    HasItems,
    HasSelection,
    ExactlyOneSelection,
    ExactlyOneDirectory,
    ExactlyOneFile,
    DirectoryOrBackground,
    WritableLocation,
    WritableDestination,
    Capability(CapabilityKind),
    DotNameSemantics,
    LocalDirectory,
    LocalExecutable,
    Archive,
    NonArchive,
    Mount,
    TrashItem,
    TrashBackground,
    CustomActionSupportsRemote,
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
            Self::ExactlyOneSelection if context.selection_count == 1 => CommandState::enabled(),
            Self::ExactlyOneDirectory
                if context.selection_count == 1 && context.target == CommandTarget::Directory =>
            {
                CommandState::enabled()
            }
            Self::ExactlyOneFile
                if context.selection_count == 1
                    && matches!(
                        context.target,
                        CommandTarget::File
                            | CommandTarget::Archive
                            | CommandTarget::ExecutableFile
                    ) =>
            {
                CommandState::enabled()
            }
            Self::DirectoryOrBackground
                if context.target == CommandTarget::Background
                    || (context.selection_count == 1
                        && context.target == CommandTarget::Directory) =>
            {
                CommandState::enabled()
            }
            Self::WritableLocation if context.location_is_writable => CommandState::enabled(),
            Self::WritableDestination
                if context.selection_count > 0 && context.destination_is_writable =>
            {
                CommandState::enabled()
            }
            Self::Capability(kind) if context.selection_count > 0 => match context.capability(kind)
            {
                CapabilityState::Supported => CommandState::enabled(),
                CapabilityState::Unsupported(reason) | CapabilityState::Unknown(reason) => {
                    CommandState::disabled(reason.as_str())
                }
            },
            Self::DotNameSemantics
                if context.selection_count > 0 && context.has_dot_name_semantics =>
            {
                CommandState::enabled()
            }
            Self::LocalDirectory
                if context.selection_count == 1
                    && context.is_local
                    && context.target == CommandTarget::Directory =>
            {
                CommandState::enabled()
            }
            Self::LocalExecutable
                if context.selection_count == 1
                    && context.is_local
                    && context.target == CommandTarget::ExecutableFile =>
            {
                CommandState::enabled()
            }
            Self::Archive
                if context.selection_count == 1 && context.target == CommandTarget::Archive =>
            {
                CommandState::enabled()
            }
            Self::NonArchive
                if context.selection_count > 0 && context.target != CommandTarget::Archive =>
            {
                CommandState::enabled()
            }
            Self::Mount
                if context.selection_count == 1 && context.target == CommandTarget::Mount =>
            {
                CommandState::enabled()
            }
            Self::TrashItem
                if context.selection_count == 1 && context.target == CommandTarget::TrashItem =>
            {
                CommandState::enabled()
            }
            Self::TrashBackground if context.target == CommandTarget::TrashBackground => {
                CommandState::enabled()
            }
            Self::CustomActionSupportsRemote
                if context.selection_count > 0
                    && (context.is_local || context.supports_provider_uris) =>
            {
                CommandState::enabled()
            }
            Self::CanGoBack => CommandState::disabled("there is no earlier history entry"),
            Self::CanGoForward => CommandState::disabled("there is no later history entry"),
            Self::HasParent => CommandState::disabled("the current location has no parent"),
            Self::HasItems => CommandState::disabled("the current view has no items"),
            Self::HasSelection => CommandState::disabled("no items are selected"),
            Self::ExactlyOneSelection => {
                CommandState::disabled("exactly one item must be selected")
            }
            Self::ExactlyOneDirectory => {
                CommandState::disabled("exactly one directory must be selected")
            }
            Self::ExactlyOneFile => CommandState::disabled("exactly one file must be selected"),
            Self::DirectoryOrBackground => {
                CommandState::disabled("a directory or its background must be targeted")
            }
            Self::WritableLocation => CommandState::disabled("the current location is read-only"),
            Self::WritableDestination => CommandState::disabled(
                context
                    .destination_reason
                    .as_deref()
                    .unwrap_or("the destination is read-only"),
            ),
            Self::Capability(_) => CommandState::disabled("no items are selected"),
            Self::DotNameSemantics => {
                CommandState::disabled("the provider does not support dot-name semantics")
            }
            Self::LocalDirectory => {
                CommandState::disabled("only local directories can be opened as administrator")
            }
            Self::LocalExecutable => {
                CommandState::disabled("only local executable files can run as administrator")
            }
            Self::Archive => CommandState::disabled("the selected item is not an archive"),
            Self::NonArchive => CommandState::disabled("archive items must be extracted instead"),
            Self::Mount => CommandState::disabled("the selected item is not a mount"),
            Self::TrashItem => CommandState::disabled("the selected item is not in trash"),
            Self::TrashBackground => CommandState::disabled("the current location is not trash"),
            Self::CustomActionSupportsRemote => {
                CommandState::disabled("the action does not support provider URIs")
            }
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandState {
    enabled: bool,
    disabled_reason: Option<Box<str>>,
    checked: bool,
}
impl CommandState {
    const fn enabled() -> Self {
        Self {
            enabled: true,
            disabled_reason: None,
            checked: false,
        }
    }
    fn disabled(reason: impl Into<Box<str>>) -> Self {
        Self {
            enabled: false,
            disabled_reason: Some(reason.into()),
            checked: false,
        }
    }
    fn with_checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
    #[must_use]
    pub fn disabled_reason(&self) -> Option<&str> {
        self.disabled_reason.as_deref()
    }
    #[must_use]
    pub fn is_checked(&self) -> bool {
        self.checked
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
    group: CommandGroup,
    danger: DangerLevel,
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
    pub fn group(&self) -> CommandGroup {
        self.group
    }
    #[must_use]
    pub fn danger_level(&self) -> DangerLevel {
        self.danger
    }
    #[must_use]
    pub fn state(&self, context: &CommandContext) -> CommandState {
        self.predicate
            .evaluate(context)
            .with_checked(match self.action() {
                CommandAction::ToggleHidden => context.show_hidden,
                CommandAction::ToggleDirectoriesFirst => context.directories_first,
                CommandAction::ToggleSidebar => context.sidebar_visible,
                CommandAction::ToggleInfo => context.info_visible,
                _ => false,
            })
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRegistryAudit {
    public_ids: Vec<CommandId>,
}
impl CommandRegistryAudit {
    #[must_use]
    pub fn public_ids(&self) -> &[CommandId] {
        &self.public_ids
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
        let mut actions = HashSet::new();
        for (index, command) in commands.iter().enumerate() {
            if command.label_key.is_empty() {
                return Err(CommandRegistryError::EmptyLabel(command.id.clone()));
            }
            if command.icon_key.is_empty() {
                return Err(CommandRegistryError::EmptyIcon(command.id.clone()));
            }
            if by_id.insert(command.id.clone(), index).is_some() {
                return Err(CommandRegistryError::DuplicateId(command.id.clone()));
            }
            if !actions.insert(command.action()) {
                return Err(CommandRegistryError::DuplicateAction(command.action()));
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
    pub fn audit(&self) -> Result<CommandRegistryAudit, CommandRegistryError> {
        let rebuilt = Self::try_new(self.commands.clone())?;
        Ok(CommandRegistryAudit {
            public_ids: rebuilt
                .commands
                .into_iter()
                .map(|command| command.id)
                .collect(),
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
    DuplicateAction(CommandAction),
    EmptyLabel(CommandId),
    EmptyIcon(CommandId),
    DuplicateShortcut {
        scope: ShortcutScope,
        chord: Box<str>,
    },
}
impl fmt::Display for CommandRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidId(id) => write!(f, "invalid command ID {id:?}"),
            Self::DuplicateId(id) => write!(f, "duplicate command ID {}", id.as_str()),
            Self::DuplicateAction(action) => write!(f, "duplicate command action {action:?}"),
            Self::EmptyLabel(id) => write!(f, "command {} has an empty label key", id.as_str()),
            Self::EmptyIcon(id) => write!(f, "command {} has an empty icon key", id.as_str()),
            Self::DuplicateShortcut { scope, chord } => {
                write!(f, "duplicate {scope:?} shortcut {chord}")
            }
        }
    }
}
impl Error for CommandRegistryError {}

fn built_in_commands() -> Vec<CommandDefinition> {
    use CommandAction as A;
    use CommandGroup as G;
    use CommandPredicate as P;
    use DangerLevel as D;
    use ShortcutScope::{Browser, Global};
    let mut commands = vec![
        command(
            "navigation.back",
            "command.back",
            "arrow-left",
            &[("Alt+Left", Browser)],
            P::CanGoBack,
            A::NavigateBack,
            G::Navigation,
            D::None,
        ),
        command(
            "navigation.forward",
            "command.forward",
            "arrow-right",
            &[("Alt+Right", Browser)],
            P::CanGoForward,
            A::NavigateForward,
            G::Navigation,
            D::None,
        ),
        command(
            "navigation.parent",
            "command.parent",
            "arrow-up",
            &[("Alt+Up", Browser)],
            P::HasParent,
            A::NavigateParent,
            G::Navigation,
            D::None,
        ),
        command(
            "navigation.refresh",
            "command.refresh",
            "refresh-cw",
            &[("F5", Browser)],
            P::Always,
            A::Refresh,
            G::Navigation,
            D::None,
        ),
        command(
            "navigation.location",
            "command.location",
            "text-cursor-input",
            &[("Ctrl+L", Browser)],
            P::Always,
            A::FocusLocation,
            G::Navigation,
            D::None,
        ),
        command(
            "view.search",
            "command.search",
            "search",
            &[("Ctrl+F", Browser)],
            P::Always,
            A::Search,
            G::Navigation,
            D::None,
        ),
        command(
            "view.filter",
            "command.filter",
            "list-filter",
            &[("Ctrl+Shift+F", Browser)],
            P::Always,
            A::Filter,
            G::Navigation,
            D::None,
        ),
        command(
            "view.command",
            "command.command-mode",
            "text-cursor-input",
            &[("Ctrl+Shift+P", Browser)],
            P::Always,
            A::FocusCommand,
            G::Navigation,
            D::None,
        ),
        command(
            "view.details",
            "command.view-details",
            "list-checks",
            &[("Ctrl+1", Browser)],
            P::Always,
            A::ViewDetails,
            G::Details,
            D::None,
        ),
        command(
            "view.list",
            "command.view-list",
            "list",
            &[("Ctrl+2", Browser)],
            P::Always,
            A::ViewList,
            G::Details,
            D::None,
        ),
        command(
            "view.cards",
            "command.view-cards",
            "grid-2x2",
            &[("Ctrl+3", Browser)],
            P::Always,
            A::ViewCards,
            G::Details,
            D::None,
        ),
        command(
            "view.grid",
            "command.view-grid",
            "grid-2x2",
            &[("Ctrl+4", Browser)],
            P::Always,
            A::ViewGrid,
            G::Details,
            D::None,
        ),
        command(
            "view.columns",
            "command.view-columns",
            "columns-2",
            &[("Ctrl+5", Browser)],
            P::Always,
            A::ViewColumns,
            G::Details,
            D::None,
        ),
        command(
            "view.adaptive",
            "command.view-adaptive",
            "panel-right",
            &[("Ctrl+6", Browser)],
            P::Always,
            A::ViewAdaptive,
            G::Details,
            D::None,
        ),
        command(
            "view.sort",
            "command.sort",
            "list",
            &[],
            P::Always,
            A::CycleSort,
            G::Details,
            D::None,
        ),
        command(
            "view.group",
            "command.group",
            "list-checks",
            &[],
            P::Always,
            A::CycleGroup,
            G::Details,
            D::None,
        ),
        command(
            "view.directories_first",
            "command.directories-first",
            "folder",
            &[],
            P::Always,
            A::ToggleDirectoriesFirst,
            G::Details,
            D::None,
        ),
        command(
            "view.hidden",
            "command.show-hidden",
            "text-cursor-input",
            &[("Ctrl+H", Browser)],
            P::Always,
            A::ToggleHidden,
            G::Details,
            D::None,
        ),
        command(
            "view.sidebar",
            "command.sidebar",
            "panel-right",
            &[("Ctrl+B", Browser)],
            P::Always,
            A::ToggleSidebar,
            G::Details,
            D::None,
        ),
        command(
            "view.info",
            "command.info-pane",
            "info",
            &[],
            P::Always,
            A::ToggleInfo,
            G::Details,
            D::None,
        ),
        command(
            "tab.new",
            "command.new-tab",
            "plus",
            &[("Ctrl+T", Browser)],
            P::Always,
            A::NewTab,
            G::Navigation,
            D::None,
        ),
        command(
            "tab.close",
            "command.close-tab",
            "x",
            &[("Ctrl+W", Browser)],
            P::Always,
            A::CloseTab,
            G::Navigation,
            D::None,
        ),
        command(
            "tab.duplicate",
            "command.duplicate-tab",
            "copy",
            &[],
            P::Always,
            A::DuplicateTab,
            G::Navigation,
            D::None,
        ),
        command(
            "tab.reopen_closed",
            "command.reopen-closed-tab",
            "rotate-ccw",
            &[("Ctrl+Shift+T", Browser)],
            P::Always,
            A::ReopenClosedTab,
            G::Navigation,
            D::None,
        ),
        command(
            "tab.move_other_pane",
            "command.move-tab-other-pane",
            "panel-right",
            &[],
            P::Always,
            A::MoveTabOtherPane,
            G::Navigation,
            D::None,
        ),
        command(
            "tab.move_left",
            "command.move-tab-left",
            "arrow-left",
            &[],
            P::Always,
            A::MoveTabLeft,
            G::Navigation,
            D::None,
        ),
        command(
            "tab.move_right",
            "command.move-tab-right",
            "arrow-right",
            &[],
            P::Always,
            A::MoveTabRight,
            G::Navigation,
            D::None,
        ),
        command(
            "tab.tear_out",
            "command.tear-out-tab",
            "copy",
            &[],
            P::Always,
            A::TearOutTab,
            G::Navigation,
            D::None,
        ),
        command(
            "pane.split",
            "command.split-pane",
            "columns-2",
            &[("F3", Browser)],
            P::Always,
            A::SplitPane,
            G::Navigation,
            D::None,
        ),
        command(
            "pane.focus_next",
            "command.focus-next-pane",
            "panel-right",
            &[("F6", Browser)],
            P::Always,
            A::FocusNextPane,
            G::Navigation,
            D::None,
        ),
        command(
            "selection.select_all",
            "command.select-all",
            "list-checks",
            &[("Ctrl+A", Browser)],
            P::HasItems,
            A::SelectAll,
            G::Navigation,
            D::None,
        ),
        command(
            "selection.clear",
            "command.clear-selection",
            "x",
            &[("Escape", Browser)],
            P::HasSelection,
            A::ClearSelection,
            G::Navigation,
            D::None,
        ),
        command(
            "item.properties",
            "command.properties",
            "info",
            &[("Alt+Enter", Browser)],
            P::HasSelection,
            A::OpenProperties,
            G::Details,
            D::None,
        ),
        command(
            "app.settings",
            "command.settings",
            "settings",
            &[("Ctrl+,", Global)],
            P::Always,
            A::OpenSettings,
            G::Details,
            D::None,
        ),
    ];
    commands.extend([
        command(
            "file.open",
            "command.open",
            "folder-open",
            &[],
            P::ExactlyOneSelection,
            A::Open,
            G::Open,
            D::None,
        ),
        command(
            "file.open_with",
            "command.open-with",
            "app-window",
            &[],
            P::ExactlyOneSelection,
            A::OpenWith,
            G::Open,
            D::None,
        ),
        command(
            "file.choose_application",
            "command.choose-application",
            "app-window",
            &[],
            P::ExactlyOneSelection,
            A::ChooseApplication,
            G::Open,
            D::None,
        ),
        command(
            "file.set_default_application",
            "command.set-default-application",
            "star",
            &[],
            P::ExactlyOneSelection,
            A::SetDefaultApplication,
            G::Open,
            D::Review,
        ),
        command(
            "clipboard.send_to",
            "command.send-to",
            "send",
            &[],
            P::WritableDestination,
            A::SendTo,
            G::Clipboard,
            D::None,
        ),
        command(
            "clipboard.cut",
            "command.cut",
            "scissors",
            &[("Ctrl+X", Browser)],
            P::HasSelection,
            A::Cut,
            G::Clipboard,
            D::None,
        ),
        command(
            "clipboard.copy",
            "command.copy",
            "copy",
            &[("Ctrl+C", Browser)],
            P::HasSelection,
            A::Copy,
            G::Clipboard,
            D::None,
        ),
        command(
            "clipboard.copy_to",
            "command.copy-to",
            "copy",
            &[],
            P::HasSelection,
            A::CopyTo,
            G::Clipboard,
            D::None,
        ),
        command(
            "clipboard.move_to",
            "command.move-to",
            "arrow-right",
            &[],
            P::HasSelection,
            A::MoveTo,
            G::Clipboard,
            D::Review,
        ),
        command(
            "clipboard.paste_into",
            "command.paste-into",
            "clipboard-paste",
            &[("Ctrl+V", Browser)],
            P::WritableLocation,
            A::PasteInto,
            G::Clipboard,
            D::None,
        ),
        command(
            "file.rename",
            "command.rename",
            "pencil",
            &[("F2", Browser)],
            P::ExactlyOneSelection,
            A::Rename,
            G::Organization,
            D::Review,
        ),
        command(
            "file.duplicate",
            "command.duplicate",
            "copy-plus",
            &[],
            P::HasSelection,
            A::Duplicate,
            G::Organization,
            D::None,
        ),
        command(
            "file.create_symbolic_link",
            "command.create-symbolic-link",
            "link",
            &[],
            P::Capability(CapabilityKind::SymbolicLinks),
            A::CreateSymbolicLink,
            G::Organization,
            D::None,
        ),
        command(
            "file.create_hard_link",
            "command.create-hard-link",
            "link",
            &[],
            P::Capability(CapabilityKind::HardLinks),
            A::CreateHardLink,
            G::Organization,
            D::None,
        ),
        command(
            "file.compress",
            "command.compress",
            "archive",
            &[],
            P::NonArchive,
            A::Compress,
            G::FileType,
            D::None,
        ),
        command(
            "archive.extract",
            "command.extract",
            "archive-restore",
            &[],
            P::Archive,
            A::Extract,
            G::FileType,
            D::None,
        ),
        command(
            "file.hide",
            "command.hide",
            "eye-off",
            &[],
            P::DotNameSemantics,
            A::Hide,
            G::Organization,
            D::Review,
        ),
        command(
            "file.unhide",
            "command.unhide",
            "eye",
            &[],
            P::DotNameSemantics,
            A::Unhide,
            G::Organization,
            D::Review,
        ),
        command(
            "file.move_to_trash",
            "command.move-to-trash",
            "trash-2",
            &[],
            P::Capability(CapabilityKind::Trash),
            A::MoveToTrash,
            G::Destructive,
            D::Review,
        ),
        command(
            "file.delete_permanently",
            "command.delete-permanently",
            "trash",
            &[("Shift+Delete", Browser)],
            P::HasSelection,
            A::DeletePermanently,
            G::Destructive,
            D::Destructive,
        ),
        command(
            "item.permissions",
            "command.permissions",
            "shield-check",
            &[],
            P::Capability(CapabilityKind::Permissions),
            A::Permissions,
            G::Details,
            D::None,
        ),
        command(
            "directory.open_as_administrator",
            "command.open-as-administrator",
            "shield",
            &[],
            P::LocalDirectory,
            A::OpenAsAdministrator,
            G::Open,
            D::Review,
        ),
        command(
            "file.run_as_administrator",
            "command.run-as-administrator",
            "shield",
            &[],
            P::LocalExecutable,
            A::RunAsAdministrator,
            G::Open,
            D::Review,
        ),
        command(
            "create.directory",
            "command.new-directory",
            "folder-plus",
            &[],
            P::WritableLocation,
            A::NewDirectory,
            G::Creation,
            D::None,
        ),
        command(
            "create.empty_file",
            "command.new-empty-file",
            "file-plus",
            &[],
            P::WritableLocation,
            A::NewEmptyFile,
            G::Creation,
            D::None,
        ),
        command(
            "create.from_template",
            "command.new-from-template",
            "file-stack",
            &[],
            P::WritableLocation,
            A::NewFromTemplate,
            G::Creation,
            D::None,
        ),
        command(
            "directory.open_terminal",
            "command.open-terminal",
            "terminal",
            &[],
            P::DirectoryOrBackground,
            A::OpenTerminalHere,
            G::Navigation,
            D::None,
        ),
        command(
            "directory.properties",
            "command.directory-properties",
            "info",
            &[],
            P::DirectoryOrBackground,
            A::DirectoryProperties,
            G::Details,
            D::None,
        ),
        command(
            "directory.open_new_tab",
            "command.open-new-tab",
            "plus",
            &[],
            P::ExactlyOneDirectory,
            A::OpenInNewTab,
            G::Open,
            D::None,
        ),
        command(
            "directory.open_new_window",
            "command.open-new-window",
            "app-window",
            &[],
            P::ExactlyOneDirectory,
            A::OpenInNewWindow,
            G::Open,
            D::None,
        ),
        command(
            "directory.open_other_pane",
            "command.open-other-pane",
            "panel-right",
            &[],
            P::ExactlyOneDirectory,
            A::OpenInOtherPane,
            G::Open,
            D::None,
        ),
        command(
            "directory.pin",
            "command.pin",
            "pin",
            &[],
            P::ExactlyOneDirectory,
            A::Pin,
            G::Organization,
            D::None,
        ),
        command(
            "directory.unpin",
            "command.unpin",
            "pin-off",
            &[],
            P::ExactlyOneDirectory,
            A::Unpin,
            G::Organization,
            D::None,
        ),
        command(
            "item.copy_location",
            "command.copy-location",
            "map-pin",
            &[],
            P::ExactlyOneSelection,
            A::CopyLocation,
            G::Details,
            D::None,
        ),
        command(
            "item.tags",
            "command.tags",
            "tag",
            &[],
            P::HasSelection,
            A::ManageTags,
            G::Organization,
            D::None,
        ),
        command(
            "directory.share",
            "command.share",
            "share-2",
            &[],
            P::ExactlyOneDirectory,
            A::Share,
            G::Organization,
            D::Review,
        ),
        command(
            "file.preview",
            "command.preview",
            "eye",
            &[],
            P::ExactlyOneFile,
            A::Preview,
            G::FileType,
            D::None,
        ),
        command(
            "archive.browse",
            "command.browse-archive",
            "archive",
            &[],
            P::Archive,
            A::BrowseArchive,
            G::FileType,
            D::None,
        ),
        command(
            "file.run",
            "command.run",
            "play",
            &[],
            P::LocalExecutable,
            A::Run,
            G::Open,
            D::Review,
        ),
        command(
            "mount.unmount",
            "command.unmount",
            "eject",
            &[],
            P::Mount,
            A::Unmount,
            G::Destructive,
            D::Review,
        ),
        command(
            "mount.eject",
            "command.eject",
            "eject",
            &[],
            P::Mount,
            A::Eject,
            G::Destructive,
            D::Review,
        ),
        command(
            "mount.power_off",
            "command.power-off",
            "power",
            &[],
            P::Mount,
            A::PowerOff,
            G::Destructive,
            D::Review,
        ),
        command(
            "trash.restore",
            "command.restore",
            "rotate-ccw",
            &[],
            P::TrashItem,
            A::Restore,
            G::Organization,
            D::None,
        ),
        command(
            "trash.empty",
            "command.empty-trash",
            "trash",
            &[],
            P::TrashBackground,
            A::EmptyTrash,
            G::Destructive,
            D::Destructive,
        ),
        command(
            "actions.custom",
            "command.actions",
            "terminal-square",
            &[],
            P::CustomActionSupportsRemote,
            A::CustomAction,
            G::Organization,
            D::Review,
        ),
    ]);
    commands
}
#[allow(clippy::too_many_arguments)]
fn command(
    id: &'static str,
    label_key: &'static str,
    icon_key: &'static str,
    shortcuts: &[(&'static str, ShortcutScope)],
    predicate: CommandPredicate,
    action: CommandAction,
    group: CommandGroup,
    danger: DangerLevel,
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
        group,
        danger,
    }
}
