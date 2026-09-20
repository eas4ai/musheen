use crate::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, ItemId, StorePath,
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

/// Provider operations whose availability is outside the portable filesystem
/// capability matrix, such as UDisks2 hardware controls and provider sharing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAction {
    Share,
    Unmount,
    Eject,
    PowerOff,
}

impl ProviderAction {
    pub const ALL: [Self; 4] = [Self::Share, Self::Unmount, Self::Eject, Self::PowerOff];
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
        Self([share, unmount, eject, power_off])
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
    pub supports_provider_uris: bool,
    pub capabilities: CapabilityMatrix,
    pub provider_actions: ProviderActionMatrix,
    pub show_hidden: bool,
    pub directories_first: bool,
    pub sidebar_visible: bool,
    pub info_visible: bool,
}

impl Default for CommandContext {
    fn default() -> Self {
        Self {
            can_go_back: false,
            can_go_forward: false,
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
        }
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
}

impl std::fmt::Display for CommandParameterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("command target item and path providers differ")
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
    Location(StorePath),
    CustomAction {
        targets: Vec<CommandTargetRef>,
        supports_provider_uris: bool,
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
}
