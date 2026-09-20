use crate::{
    CapabilityKind, CapabilityState, CommandContext, CommandParameters, CommandTarget,
    OpenWithIntent, ProviderAction,
};
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

fn writable_location_state(context: &CommandContext) -> CommandState {
    if !context.location_is_writable {
        return CommandState::disabled("the current location is read-only");
    }
    if !context.mutation_is_supported {
        return CommandState::disabled(
            context
                .mutation_reason
                .as_deref()
                .unwrap_or("the provider does not support this mutation"),
        );
    }
    CommandState::enabled()
}

fn writable_destination_state(context: &CommandContext) -> CommandState {
    let state = selection_state(context);
    if !state.is_enabled() {
        return state;
    }
    let state = resolved_destination_state(context);
    if !state.is_enabled() {
        return state;
    }
    if !context.mutation_is_supported {
        return CommandState::disabled(
            context
                .mutation_reason
                .as_deref()
                .unwrap_or("the provider does not support this mutation"),
        );
    }
    CommandState::enabled()
}

fn selection_state(context: &CommandContext) -> CommandState {
    if !context.has_target_selection() {
        return CommandState::disabled(if context.selection_count > 0 {
            "background commands do not use a selection"
        } else {
            "no items are selected"
        });
    }
    CommandState::enabled()
}

fn resolved_destination_state(context: &CommandContext) -> CommandState {
    match &context.resolved_destination {
        None => CommandState::disabled("choose a destination first"),
        Some(destination) if destination.is_writable => CommandState::enabled(),
        Some(destination) => CommandState::disabled(
            destination
                .refusal_reason
                .as_deref()
                .unwrap_or("the destination is read-only"),
        ),
    }
}

fn destination_operation_state(
    context: &CommandContext,
    requires_source_mutation: bool,
) -> CommandState {
    let state = selection_state(context);
    if !state.is_enabled() {
        return state;
    }
    if requires_source_mutation {
        let state = writable_location_state(context);
        if !state.is_enabled() {
            return state;
        }
    }
    match context.resolved_destination {
        None => CommandState::enabled(),
        Some(_) => resolved_destination_state(context),
    }
}

fn writable_selection_state(context: &CommandContext) -> CommandState {
    let state = selection_state(context);
    if !state.is_enabled() {
        return state;
    }
    if !context.location_is_writable {
        return CommandState::disabled("the current location is read-only");
    }
    if !context.mutation_is_supported {
        return CommandState::disabled(
            context
                .mutation_reason
                .as_deref()
                .unwrap_or("the provider does not support this mutation"),
        );
    }
    CommandState::enabled()
}

fn writable_exactly_one_selection_state(context: &CommandContext) -> CommandState {
    if !context.has_target_selection() && context.selection_count > 0 {
        return CommandState::disabled("background commands do not use a selection");
    }
    if context.selection_count != 1 {
        return CommandState::disabled("select exactly one item");
    }
    writable_location_state(context)
}

fn writable_exactly_one_file_capability_state(
    context: &CommandContext,
    capability: CapabilityKind,
) -> CommandState {
    if context.selection_count != 1
        || !matches!(
            context.target,
            CommandTarget::File | CommandTarget::Archive | CommandTarget::ExecutableFile
        )
    {
        return CommandState::disabled("exactly one file must be selected");
    }
    writable_location_state(context).and_capability(context.capability(capability))
}

fn writable_exactly_one_selection_capability_state(
    context: &CommandContext,
    capability: CapabilityKind,
) -> CommandState {
    writable_exactly_one_selection_state(context).and_capability(context.capability(capability))
}

fn hide_state(context: &CommandContext, target_is_hidden: bool) -> CommandState {
    let state = writable_selection_state(context);
    if !state.is_enabled() {
        return state;
    }
    if !context.has_dot_name_semantics {
        return CommandState::disabled("the provider does not support dot-name semantics");
    }
    if context.target_is_hidden != target_is_hidden {
        return CommandState::disabled(if context.target_is_hidden {
            "the selected item is already hidden"
        } else {
            "the selected item is not hidden"
        });
    }
    CommandState::enabled()
}

fn archive_mutation_state(context: &CommandContext) -> CommandState {
    if context.selection_count != 1 || context.target != CommandTarget::Archive {
        return CommandState::disabled("exactly one archive must be selected");
    }
    writable_selection_state(context)
}

fn archive_destination_state(context: &CommandContext) -> CommandState {
    if context.selection_count != 1 || context.target != CommandTarget::Archive {
        return CommandState::disabled("exactly one archive must be selected");
    }
    destination_operation_state(context, false)
}

fn non_archive_mutation_state(context: &CommandContext) -> CommandState {
    let state = writable_selection_state(context);
    if !state.is_enabled() {
        return state;
    }
    if context.target == CommandTarget::Archive {
        return CommandState::disabled("archive items must be extracted instead");
    }
    CommandState::enabled()
}

fn paste_state(context: &CommandContext) -> CommandState {
    match context.target {
        CommandTarget::Background if context.selection_count == 0 => {}
        CommandTarget::Directory if context.selection_count == 1 => {}
        CommandTarget::Background => {
            return CommandState::disabled("background commands do not use a selection");
        }
        CommandTarget::Directory => {
            return CommandState::disabled("exactly one directory must be selected");
        }
        _ => return CommandState::disabled("paste requires a directory or directory background"),
    }
    if !context.clipboard_has_contents {
        return CommandState::disabled("the clipboard has no pasteable items");
    }
    let state = resolved_destination_state(context);
    if !state.is_enabled() {
        return state;
    }
    if !context.mutation_is_supported {
        return CommandState::disabled(
            context
                .mutation_reason
                .as_deref()
                .unwrap_or("the provider does not support this mutation"),
        );
    }
    CommandState::enabled()
}

fn provider_action_state(context: &CommandContext, action: ProviderAction) -> CommandState {
    let expected_target = match action {
        ProviderAction::Share => CommandTarget::Directory,
        ProviderAction::Unmount | ProviderAction::Eject | ProviderAction::PowerOff => {
            CommandTarget::Mount
        }
    };
    if context.selection_count != 1 || context.target != expected_target {
        return CommandState::disabled(match action {
            ProviderAction::Share => "exactly one directory must be selected",
            ProviderAction::Unmount | ProviderAction::Eject | ProviderAction::PowerOff => {
                "exactly one mount must be selected"
            }
        });
    }
    match context.provider_action(action) {
        CapabilityState::Supported => CommandState::enabled(),
        CapabilityState::Unsupported(reason) | CapabilityState::Unknown(reason) => {
            CommandState::disabled(reason.as_str())
        }
    }
}

fn pin_state(context: &CommandContext, target_is_pinned: bool) -> CommandState {
    if context.selection_count != 1 || context.target != CommandTarget::Directory {
        return CommandState::disabled("exactly one directory must be selected");
    }
    if context.target_is_pinned != target_is_pinned {
        return CommandState::disabled(if context.target_is_pinned {
            "the selected directory is already pinned"
        } else {
            "the selected directory is not pinned"
        });
    }
    CommandState::enabled()
}

fn executable_run_state(context: &CommandContext) -> CommandState {
    if context.selection_count != 1
        || !context.is_local
        || context.target != CommandTarget::ExecutableFile
    {
        return CommandState::disabled("only local executable files can run");
    }
    if !context.executable_run_enabled {
        return CommandState::disabled("the executable run preference is disabled");
    }
    CommandState::enabled()
}

fn directory_or_mount_state(context: &CommandContext) -> CommandState {
    if context.selection_count == 1
        && matches!(
            context.target,
            CommandTarget::Directory | CommandTarget::Mount
        )
    {
        CommandState::enabled()
    } else {
        CommandState::disabled("exactly one directory or mount must be selected")
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
    ExtractHere,
}
impl CommandAction {
    pub const ALL: [Self; 80] = [
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
        Self::ExtractHere,
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
pub enum CommandSubmenu {
    OpenWith,
    SendTo,
    Tags,
    Actions,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandContributionPolicy {
    Fixed,
    Variable(CommandSubmenu),
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
        validate_parameters(self.0, &parameters)?;
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
pub enum TargetCardinality {
    ExactlyOne,
    OneOrMore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandParameterContract {
    None,
    Targets(TargetCardinality),
    Destination(TargetCardinality),
    DestinationWorkflow(TargetCardinality),
    Location,
    CustomAction(TargetCardinality),
    OpenWith(TargetCardinality),
}

impl CommandAction {
    #[must_use]
    pub const fn parameter_contract(self) -> CommandParameterContract {
        match self {
            CommandAction::NavigateBack
            | CommandAction::NavigateForward
            | CommandAction::NavigateParent
            | CommandAction::Refresh
            | CommandAction::FocusLocation
            | CommandAction::Search
            | CommandAction::Filter
            | CommandAction::FocusCommand
            | CommandAction::ViewDetails
            | CommandAction::ViewList
            | CommandAction::ViewCards
            | CommandAction::ViewGrid
            | CommandAction::ViewColumns
            | CommandAction::ViewAdaptive
            | CommandAction::CycleSort
            | CommandAction::CycleGroup
            | CommandAction::ToggleDirectoriesFirst
            | CommandAction::ToggleHidden
            | CommandAction::ToggleSidebar
            | CommandAction::ToggleInfo
            | CommandAction::NewTab
            | CommandAction::CloseTab
            | CommandAction::DuplicateTab
            | CommandAction::ReopenClosedTab
            | CommandAction::MoveTabOtherPane
            | CommandAction::MoveTabLeft
            | CommandAction::MoveTabRight
            | CommandAction::TearOutTab
            | CommandAction::SplitPane
            | CommandAction::FocusNextPane
            | CommandAction::SelectAll
            | CommandAction::ClearSelection
            | CommandAction::OpenSettings => CommandParameterContract::None,
            CommandAction::NewDirectory
            | CommandAction::NewEmptyFile
            | CommandAction::NewFromTemplate
            | CommandAction::OpenTerminalHere
            | CommandAction::PasteInto
            | CommandAction::DirectoryProperties
            | CommandAction::EmptyTrash => CommandParameterContract::Location,
            CommandAction::SendTo => {
                CommandParameterContract::Destination(TargetCardinality::OneOrMore)
            }
            CommandAction::CopyTo | CommandAction::MoveTo => {
                CommandParameterContract::DestinationWorkflow(TargetCardinality::OneOrMore)
            }
            CommandAction::Extract => {
                CommandParameterContract::DestinationWorkflow(TargetCardinality::ExactlyOne)
            }
            CommandAction::OpenWith | CommandAction::SetDefaultApplication => {
                CommandParameterContract::OpenWith(TargetCardinality::ExactlyOne)
            }
            CommandAction::CustomAction => {
                CommandParameterContract::CustomAction(TargetCardinality::OneOrMore)
            }
            CommandAction::Cut
            | CommandAction::Copy
            | CommandAction::Duplicate
            | CommandAction::Compress
            | CommandAction::Hide
            | CommandAction::Unhide
            | CommandAction::MoveToTrash
            | CommandAction::DeletePermanently
            | CommandAction::OpenProperties
            | CommandAction::Permissions
            | CommandAction::ManageTags => {
                CommandParameterContract::Targets(TargetCardinality::OneOrMore)
            }
            _ => CommandParameterContract::Targets(TargetCardinality::ExactlyOne),
        }
    }
}

fn validate_parameters(
    action: CommandAction,
    parameters: &CommandParameters,
) -> Result<(), CommandDispatchError> {
    let contract = action.parameter_contract();
    let valid = match (contract, parameters) {
        (CommandParameterContract::None, CommandParameters::None)
        | (CommandParameterContract::Location, CommandParameters::Location(_)) => true,
        (CommandParameterContract::Targets(cardinality), CommandParameters::Targets(targets)) => {
            cardinality_matches(cardinality, targets.len())
        }
        (
            CommandParameterContract::Destination(cardinality),
            CommandParameters::Destination { targets, .. },
        ) => cardinality_matches(cardinality, targets.len()),
        (
            CommandParameterContract::DestinationWorkflow(cardinality),
            CommandParameters::DestinationRequest(targets),
        ) => cardinality_matches(cardinality, targets.len()),
        (
            CommandParameterContract::DestinationWorkflow(cardinality),
            CommandParameters::Destination { targets, .. },
        ) => cardinality_matches(cardinality, targets.len()),
        (
            CommandParameterContract::CustomAction(cardinality),
            CommandParameters::CustomAction { targets, .. },
        ) => cardinality_matches(cardinality, targets.len()),
        (
            CommandParameterContract::OpenWith(cardinality),
            CommandParameters::OpenWith {
                targets, intent, ..
            },
        ) => {
            cardinality_matches(cardinality, targets.len())
                && matches!(
                    (action, intent),
                    (CommandAction::OpenWith, OpenWithIntent::OpenOnce)
                        | (
                            CommandAction::SetDefaultApplication,
                            OpenWithIntent::SetAsDefault
                        )
                )
        }
        _ => false,
    };
    valid.then_some(()).ok_or_else(|| {
        CommandDispatchError::new(format!("invalid parameters for command action {action:?}"))
    })
}

const fn cardinality_matches(cardinality: TargetCardinality, count: usize) -> bool {
    match cardinality {
        TargetCardinality::ExactlyOne => count == 1,
        TargetCardinality::OneOrMore => count > 0,
    }
}

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
    DirectoryMountOrBackground,
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
    WritableSelection,
    WritableExactlyOneSelection,
    WritableSelectionCapability(CapabilityKind),
    WritableExactlyOneSelectionCapability(CapabilityKind),
    WritableExactlyOneFileCapability(CapabilityKind),
    Hide,
    Unhide,
    WritableArchive,
    WritableNonArchive,
    PasteInto,
    DestinationCopy,
    DestinationMove,
    DestinationExtract,
    ProviderAction(ProviderAction),
    PinnedDirectory,
    UnpinnedDirectory,
    ExecutableRun,
    DirectoryOrMount,
}
impl CommandPredicate {
    fn evaluate(self, context: &CommandContext) -> CommandState {
        if let Some(state) = self.policy_state(context) {
            return state;
        }
        match self {
            Self::Always => CommandState::enabled(),
            Self::CanGoBack if context.can_go_back => CommandState::enabled(),
            Self::CanGoForward if context.can_go_forward => CommandState::enabled(),
            Self::HasParent if context.has_parent => CommandState::enabled(),
            Self::HasItems if context.item_count > 0 => CommandState::enabled(),
            Self::HasSelection if context.has_target_selection() => CommandState::enabled(),
            Self::ExactlyOneSelection
                if context.selection_count == 1 && context.has_target_selection() =>
            {
                CommandState::enabled()
            }
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
            Self::DirectoryMountOrBackground
                if context.target == CommandTarget::Background
                    || context.target == CommandTarget::Mount
                    || (context.selection_count == 1
                        && matches!(
                            context.target,
                            CommandTarget::Directory | CommandTarget::Mount
                        )) =>
            {
                CommandState::enabled()
            }
            Self::WritableLocation if context.location_is_writable => CommandState::enabled(),
            Self::Capability(kind) if context.has_target_selection() => {
                match context.capability(kind) {
                    CapabilityState::Supported => CommandState::enabled(),
                    CapabilityState::Unsupported(reason) | CapabilityState::Unknown(reason) => {
                        CommandState::disabled(reason.as_str())
                    }
                }
            }
            Self::DotNameSemantics
                if context.has_target_selection() && context.has_dot_name_semantics =>
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
                if context.has_target_selection()
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
            Self::DirectoryMountOrBackground => {
                CommandState::disabled("a directory, mount, or its background must be targeted")
            }
            Self::WritableLocation => CommandState::disabled("the current location is read-only"),
            Self::WritableDestination => CommandState::disabled("choose a destination first"),
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
            _ => unreachable!("policy predicates return before this match"),
        }
    }

    fn policy_state(self, context: &CommandContext) -> Option<CommandState> {
        match self {
            Self::WritableLocation => Some(writable_location_state(context)),
            Self::WritableDestination => Some(writable_destination_state(context)),
            Self::WritableSelection => Some(writable_selection_state(context)),
            Self::WritableExactlyOneSelection => {
                Some(writable_exactly_one_selection_state(context))
            }
            Self::WritableSelectionCapability(capability) => Some(
                writable_selection_state(context).and_capability(context.capability(capability)),
            ),
            Self::WritableExactlyOneSelectionCapability(capability) => Some(
                writable_exactly_one_selection_capability_state(context, capability),
            ),
            Self::WritableExactlyOneFileCapability(capability) => Some(
                writable_exactly_one_file_capability_state(context, capability),
            ),
            Self::Hide => Some(hide_state(context, false)),
            Self::Unhide => Some(hide_state(context, true)),
            Self::WritableArchive => Some(archive_mutation_state(context)),
            Self::WritableNonArchive => Some(non_archive_mutation_state(context)),
            Self::PasteInto => Some(paste_state(context)),
            Self::DestinationCopy => Some(destination_operation_state(context, false)),
            Self::DestinationMove => Some(destination_operation_state(context, true)),
            Self::DestinationExtract => Some(archive_destination_state(context)),
            Self::ProviderAction(action) => Some(provider_action_state(context, action)),
            Self::PinnedDirectory => Some(pin_state(context, true)),
            Self::UnpinnedDirectory => Some(pin_state(context, false)),
            Self::ExecutableRun => Some(executable_run_state(context)),
            Self::DirectoryOrMount => Some(directory_or_mount_state(context)),
            _ => None,
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
    fn and_capability(self, capability: &CapabilityState) -> Self {
        if !self.is_enabled() {
            return self;
        }
        match capability {
            CapabilityState::Supported => self,
            CapabilityState::Unsupported(reason) | CapabilityState::Unknown(reason) => {
                Self::disabled(reason.as_str())
            }
        }
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

    /// A projection may translate the refusal without changing command policy.
    #[must_use]
    pub fn map_disabled_reason(mut self, translate: impl FnOnce(&str) -> String) -> Self {
        if let Some(reason) = self.disabled_reason.as_ref() {
            self.disabled_reason = Some(translate(reason).into());
        }
        self
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
    contribution_policy: CommandContributionPolicy,
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
    pub fn parameter_contract(&self) -> CommandParameterContract {
        self.handler.action().parameter_contract()
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
    pub fn contribution_policy(&self) -> CommandContributionPolicy {
        self.contribution_policy
    }
    #[must_use]
    pub fn submenu(&self) -> Option<CommandSubmenu> {
        match self.contribution_policy {
            CommandContributionPolicy::Fixed => None,
            CommandContributionPolicy::Variable(submenu) => Some(submenu),
        }
    }
    #[must_use]
    pub fn state(&self, context: &CommandContext) -> CommandState {
        if let Some(state) = context.backend_action_state(self.action())
            && !matches!(state, CapabilityState::Supported)
        {
            return CommandState::disabled(
                state
                    .reason()
                    .unwrap_or("the desktop backend cannot run this command"),
            );
        }
        self.predicate
            .evaluate(context)
            .with_checked(match self.action() {
                CommandAction::ViewDetails => {
                    matches!(context.active_layout, crate::ActiveLayout::Details)
                }
                CommandAction::ViewList => {
                    matches!(context.active_layout, crate::ActiveLayout::List)
                }
                CommandAction::ViewCards => {
                    matches!(context.active_layout, crate::ActiveLayout::Cards)
                }
                CommandAction::ViewGrid => {
                    matches!(context.active_layout, crate::ActiveLayout::Grid)
                }
                CommandAction::ViewColumns => {
                    matches!(context.active_layout, crate::ActiveLayout::Columns)
                }
                CommandAction::ViewAdaptive => {
                    matches!(context.active_layout, crate::ActiveLayout::Adaptive)
                }
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
            P::DestinationCopy,
            A::CopyTo,
            G::Clipboard,
            D::None,
        ),
        command(
            "clipboard.move_to",
            "command.move-to",
            "arrow-right",
            &[],
            P::DestinationMove,
            A::MoveTo,
            G::Clipboard,
            D::Review,
        ),
        command(
            "clipboard.paste_into",
            "command.paste-into",
            "clipboard-paste",
            &[("Ctrl+V", Browser)],
            P::PasteInto,
            A::PasteInto,
            G::Clipboard,
            D::None,
        ),
        command(
            "file.rename",
            "command.rename",
            "pencil",
            &[("F2", Browser)],
            P::WritableExactlyOneSelection,
            A::Rename,
            G::Organization,
            D::Review,
        ),
        command(
            "file.duplicate",
            "command.duplicate",
            "copy-plus",
            &[],
            P::WritableSelection,
            A::Duplicate,
            G::Organization,
            D::None,
        ),
        command(
            "file.create_symbolic_link",
            "command.create-symbolic-link",
            "file-symlink",
            &[],
            P::WritableExactlyOneSelectionCapability(CapabilityKind::SymbolicLinks),
            A::CreateSymbolicLink,
            G::Organization,
            D::None,
        ),
        command(
            "file.create_hard_link",
            "command.create-hard-link",
            "link-2",
            &[],
            P::WritableExactlyOneFileCapability(CapabilityKind::HardLinks),
            A::CreateHardLink,
            G::Organization,
            D::None,
        ),
        command(
            "file.compress",
            "command.compress",
            "archive",
            &[],
            P::WritableNonArchive,
            A::Compress,
            G::FileType,
            D::None,
        ),
        command(
            "archive.extract",
            "command.extract",
            "archive-restore",
            &[],
            P::DestinationExtract,
            A::Extract,
            G::FileType,
            D::None,
        ),
        command(
            "archive.extract_here",
            "command.extract-here",
            "archive-restore",
            &[],
            P::WritableArchive,
            A::ExtractHere,
            G::FileType,
            D::None,
        ),
        command(
            "file.hide",
            "command.hide",
            "eye-off",
            &[],
            P::Hide,
            A::Hide,
            G::Organization,
            D::Review,
        ),
        command(
            "file.unhide",
            "command.unhide",
            "eye",
            &[],
            P::Unhide,
            A::Unhide,
            G::Organization,
            D::Review,
        ),
        command(
            "file.move_to_trash",
            "command.move-to-trash",
            "trash-2",
            &[],
            P::WritableSelectionCapability(CapabilityKind::Trash),
            A::MoveToTrash,
            G::Destructive,
            D::Review,
        ),
        command(
            "file.delete_permanently",
            "command.delete-permanently",
            "trash",
            &[("Shift+Delete", Browser)],
            P::WritableSelection,
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
            P::DirectoryMountOrBackground,
            A::DirectoryProperties,
            G::Details,
            D::None,
        ),
        command(
            "directory.open_new_tab",
            "command.open-new-tab",
            "plus",
            &[],
            P::DirectoryOrMount,
            A::OpenInNewTab,
            G::Open,
            D::None,
        ),
        command(
            "directory.open_new_window",
            "command.open-new-window",
            "app-window",
            &[],
            P::DirectoryOrMount,
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
            P::UnpinnedDirectory,
            A::Pin,
            G::Organization,
            D::None,
        ),
        command(
            "directory.unpin",
            "command.unpin",
            "pin-off",
            &[],
            P::PinnedDirectory,
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
            P::ProviderAction(ProviderAction::Share),
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
            P::ExecutableRun,
            A::Run,
            G::Open,
            D::Review,
        ),
        command(
            "mount.unmount",
            "command.unmount",
            "eject",
            &[],
            P::ProviderAction(ProviderAction::Unmount),
            A::Unmount,
            G::Destructive,
            D::Review,
        ),
        command(
            "mount.eject",
            "command.eject",
            "eject",
            &[],
            P::ProviderAction(ProviderAction::Eject),
            A::Eject,
            G::Destructive,
            D::Review,
        ),
        command(
            "mount.power_off",
            "command.power-off",
            "power",
            &[],
            P::ProviderAction(ProviderAction::PowerOff),
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
        contribution_policy: contribution_policy(action),
    }
}

const fn contribution_policy(action: CommandAction) -> CommandContributionPolicy {
    match action {
        CommandAction::OpenWith => CommandContributionPolicy::Variable(CommandSubmenu::OpenWith),
        CommandAction::SendTo => CommandContributionPolicy::Variable(CommandSubmenu::SendTo),
        CommandAction::ManageTags => CommandContributionPolicy::Variable(CommandSubmenu::Tags),
        CommandAction::CustomAction => CommandContributionPolicy::Variable(CommandSubmenu::Actions),
        _ => CommandContributionPolicy::Fixed,
    }
}
