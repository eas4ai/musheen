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
    pub destination_is_writable: bool,
    pub destination_reason: Option<Box<str>>,
    pub is_local: bool,
    pub has_dot_name_semantics: bool,
    pub supports_provider_uris: bool,
    pub capabilities: CapabilityMatrix,
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
            destination_is_writable: false,
            destination_reason: None,
            is_local: false,
            has_dot_name_semantics: false,
            supports_provider_uris: false,
            capabilities: CapabilityMatrix::new(|_| {
                CapabilityState::Unknown(
                    CapabilityReason::new("the provider capability is not known")
                        .expect("the default capability reason is valid"),
                )
            }),
            show_hidden: false,
            directories_first: false,
            sidebar_visible: false,
            info_visible: false,
        }
    }
}

impl CommandContext {
    #[must_use]
    pub fn capability(&self, capability: CapabilityKind) -> &CapabilityState {
        self.capabilities.get(capability)
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
    #[must_use]
    pub fn new(id: ItemId, path: StorePath) -> Self {
        Self { id, path }
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
