use crate::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, CommandAction, ItemId,
    StorePath,
};

/// The exact target for a command projection. Context menus set this from the
/// item that was invoked, rather than from a display string or stale selection.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CommandTarget {
    #[default]
    Background,
    File,
    Directory,
    MultiSelection,
    Archive,
    ExecutableFile,
    Mount,
    TrashItem,
    TrashBackground,
    Tag,
    Sidebar,
}

/// What a local regular file could run as, read from its first bytes
/// (SYS-035, SYS-036). Whether the user may execute it is a separate fact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunKind {
    /// A compiled program: an ELF file.
    Program,
    /// A desktop entry of type Application.
    DesktopEntry,
    /// A script: a file that starts with `#!`.
    Script,
}

/// The active presentation layout, kept in the command context so radio menu
/// state is a registry projection rather than renderer-local policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ActiveLayout {
    Details,
    #[default]
    List,
    Cards,
    Grid,
    Columns,
    Adaptive,
}

/// Provider operations whose availability is outside the portable filesystem
/// capability matrix, such as UDisks2 hardware controls and provider sharing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAction {
    Share,
    Mount,
    Unmount,
    Eject,
    Unlock,
    PowerOff,
}

impl ProviderAction {
    pub const ALL: [Self; 6] = [
        Self::Share,
        Self::Mount,
        Self::Unmount,
        Self::Eject,
        Self::Unlock,
        Self::PowerOff,
    ];
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProviderActionMatrix([CapabilityState; ProviderAction::ALL.len()]);

impl ProviderActionMatrix {
    #[must_use]
    pub fn from_states(
        share: CapabilityState,
        unmount: CapabilityState,
        eject: CapabilityState,
        power_off: CapabilityState,
    ) -> Self {
        Self([
            share,
            unknown_provider_action(),
            unmount,
            eject,
            unknown_provider_action(),
            power_off,
        ])
    }

    #[must_use]
    pub fn from_volume_states(
        share: CapabilityState,
        mount: CapabilityState,
        unmount: CapabilityState,
        eject: CapabilityState,
        unlock: CapabilityState,
        power_off: CapabilityState,
    ) -> Self {
        Self([share, mount, unmount, eject, unlock, power_off])
    }

    #[must_use]
    pub fn get(&self, action: ProviderAction) -> &CapabilityState {
        &self.0[action as usize]
    }
}

/// The mutable and capability facts used to decide whether a command is safe.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandContext {
    pub can_go_back: bool,
    pub can_go_forward: bool,
    pub can_close_tab: bool,
    pub can_reopen_closed_tab: bool,
    pub can_move_tab_left: bool,
    pub can_move_tab_right: bool,
    pub can_create_tab: bool,
    pub can_move_tab_other_pane: bool,
    pub can_tear_out_tab: bool,
    pub can_split_pane: bool,
    pub can_focus_next_pane: bool,
    pub has_parent: bool,
    pub item_count: usize,
    pub selection_count: usize,
    pub target: CommandTarget,
    pub location_is_writable: bool,
    pub resolved_destination: Option<ResolvedDestination>,
    pub clipboard_has_contents: bool,
    pub mutation_is_supported: bool,
    pub mutation_reason: Option<Box<str>>,
    pub is_local: bool,
    pub has_dot_name_semantics: bool,
    pub target_is_hidden: bool,
    pub target_is_pinned: bool,
    pub executable_run_enabled: bool,
    /// What the single selected local file could run as, when known.
    pub run_kind: Option<RunKind>,
    pub supports_provider_uris: bool,
    pub capabilities: CapabilityMatrix,
    pub provider_actions: ProviderActionMatrix,
    pub show_hidden: bool,
    pub directories_first: bool,
    pub sidebar_visible: bool,
    pub info_visible: bool,
    pub active_layout: ActiveLayout,
    /// Optional desktop-boundary state. When supplied, every command surface
    /// receives the same unavailable reason from the registry definition.
    pub backend_actions: Option<Vec<(CommandAction, CapabilityState)>>,
}

impl Default for CommandContext {
    fn default() -> Self {
        Self {
            can_go_back: false,
            can_go_forward: false,
            can_close_tab: false,
            can_reopen_closed_tab: false,
            can_move_tab_left: false,
            can_move_tab_right: false,
            can_create_tab: false,
            can_move_tab_other_pane: false,
            can_tear_out_tab: false,
            can_split_pane: false,
            can_focus_next_pane: false,
            has_parent: false,
            item_count: 0,
            selection_count: 0,
            target: CommandTarget::Background,
            location_is_writable: false,
            resolved_destination: None,
            clipboard_has_contents: false,
            mutation_is_supported: false,
            mutation_reason: None,
            is_local: false,
            has_dot_name_semantics: false,
            target_is_hidden: false,
            target_is_pinned: false,
            executable_run_enabled: false,
            run_kind: None,
            supports_provider_uris: false,
            capabilities: CapabilityMatrix::new(|_| {
                CapabilityState::Unknown(
                    CapabilityReason::new("the provider capability is not known")
                        .expect("the default capability reason is valid"),
                )
            }),
            provider_actions: ProviderActionMatrix::from_states(
                unknown_provider_action(),
                unknown_provider_action(),
                unknown_provider_action(),
                unknown_provider_action(),
            ),
            show_hidden: false,
            directories_first: false,
            sidebar_visible: false,
            info_visible: false,
            active_layout: ActiveLayout::default(),
            backend_actions: None,
        }
    }
}

impl CommandContext {
    #[must_use]
    pub fn backend_action_state(&self, action: CommandAction) -> Option<&CapabilityState> {
        self.backend_actions.as_ref().and_then(|actions| {
            actions
                .iter()
                .find(|(candidate, _)| *candidate == action)
                .map(|(_, state)| state)
        })
    }
}

fn unknown_provider_action() -> CapabilityState {
    CapabilityState::Unknown(
        CapabilityReason::new("the provider action is not known")
            .expect("the default provider action reason is valid"),
    )
}

impl CommandContext {
    #[must_use]
    pub fn capability(&self, capability: CapabilityKind) -> &CapabilityState {
        self.capabilities.get(capability)
    }

    #[must_use]
    pub fn provider_action(&self, action: ProviderAction) -> &CapabilityState {
        self.provider_actions.get(action)
    }

    #[must_use]
    pub fn has_target_selection(&self) -> bool {
        self.selection_count > 0
            && !matches!(
                self.target,
                CommandTarget::Background | CommandTarget::TrashBackground
            )
    }
}

/// A destination chosen by the user and resolved by a provider before an
/// operation is allowed to mutate it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedDestination {
    pub path: StorePath,
    pub is_writable: bool,
    pub refusal_reason: Option<Box<str>>,
}

impl ResolvedDestination {
    #[must_use]
    pub fn writable(path: StorePath) -> Self {
        Self {
            path,
            is_writable: true,
            refusal_reason: None,
        }
    }

    #[must_use]
    pub fn read_only(path: StorePath, reason: impl Into<Box<str>>) -> Self {
        Self {
            path,
            is_writable: false,
            refusal_reason: Some(reason.into()),
        }
    }
}

/// A validated, provider-scoped command target. It deliberately contains no
/// display text, so handlers cannot accidentally operate on lossy path labels.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandTargetRef {
    id: ItemId,
    path: StorePath,
}

impl CommandTargetRef {
    pub fn new(id: ItemId, path: StorePath) -> Result<Self, CommandParameterError> {
        let provider_matches = match path.provider_key() {
            Some((provider, _)) => provider == id.provider(),
            None => id.provider().as_str() == "local",
        };
        provider_matches
            .then_some(Self { id, path })
            .ok_or(CommandParameterError::ProviderMismatch)
    }

    #[must_use]
    pub fn id(&self) -> &ItemId {
        &self.id
    }

    #[must_use]
    pub fn path(&self) -> &StorePath {
        &self.path
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandParameterError {
    ProviderMismatch,
    InvalidApplicationId,
}

impl std::fmt::Display for CommandParameterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProviderMismatch => {
                formatter.write_str("command target item and path providers differ")
            }
            Self::InvalidApplicationId => formatter.write_str("desktop application ID is invalid"),
        }
    }
}

impl std::error::Error for CommandParameterError {}

/// Inputs accepted by command handlers. All filesystem values are lossless
/// `StorePath`s and all selected items retain their validated provider IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandParameters {
    None,
    Targets(Vec<CommandTargetRef>),
    Destination {
        targets: Vec<CommandTargetRef>,
        destination: StorePath,
    },
    DestinationRequest(Vec<CommandTargetRef>),
    Location(StorePath),
    CustomAction {
        targets: Vec<CommandTargetRef>,
        supports_provider_uris: bool,
        action_id: Option<Box<str>>,
        definition: Option<Box<str>>,
        location: StorePath,
    },
    OpenWith {
        targets: Vec<CommandTargetRef>,
        application: DesktopApplicationId,
        intent: OpenWithIntent,
    },
}

impl CommandParameters {
    #[must_use]
    pub fn targets(targets: Vec<CommandTargetRef>) -> Self {
        Self::Targets(targets)
    }

    #[must_use]
    pub fn destination(targets: Vec<CommandTargetRef>, destination: StorePath) -> Self {
        Self::Destination {
            targets,
            destination,
        }
    }

    #[must_use]
    pub fn destination_request(targets: Vec<CommandTargetRef>) -> Self {
        Self::DestinationRequest(targets)
    }

    #[must_use]
    pub fn open_with(
        targets: Vec<CommandTargetRef>,
        application: DesktopApplicationId,
        intent: OpenWithIntent,
    ) -> Self {
        Self::OpenWith {
            targets,
            application,
            intent,
        }
    }
}

/// A validated freedesktop desktop-file identity. It is never a display label.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DesktopApplicationId(Box<str>);

impl DesktopApplicationId {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, CommandParameterError> {
        let value = value.into();
        if value.is_empty()
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            return Err(CommandParameterError::InvalidApplicationId);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Association changes are explicit; a one-time launch cannot alter defaults.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenWithIntent {
    OpenOnce,
    SetAsDefault,
}
