use musheen_core::{CommandContext, CommandTargetRef, StorePath};
use std::sync::Arc;

/// The user-visible surface that supplied the menu target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuTarget {
    Item,
    Background,
    SidebarLocation,
    Mount,
    Tag,
    TrashItem,
    TrashBackground,
}

/// Pointer and keyboard menus deliberately share one target preparation path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextMenuSource {
    Pointer,
    Keyboard,
}

/// Exact, pane-local facts required to project a context menu.
#[derive(Clone, Debug)]
pub struct ContextMenuRequest {
    context: CommandContext,
    target: MenuTarget,
    location: StorePath,
    selection: Arc<[CommandTargetRef]>,
    trash_contents: Arc<[CommandTargetRef]>,
    source: ContextMenuSource,
    origin_tab: Option<crate::navigation::TabId>,
    pub(crate) open_with: Vec<crate::menus::OpenWithApplication>,
    pub(crate) send_to: Vec<crate::menus::SendToDestination>,
    pub(crate) tags: Vec<crate::menus::MenuContribution>,
    pub(crate) actions: Vec<crate::menus::MenuContribution>,
}

impl ContextMenuRequest {
    #[must_use]
    pub fn new(
        mut context: CommandContext,
        target: MenuTarget,
        location: StorePath,
        mut selection: Vec<CommandTargetRef>,
    ) -> Self {
        if matches!(target, MenuTarget::Background | MenuTarget::TrashBackground) {
            selection.clear();
        }
        if target == MenuTarget::SidebarLocation
            && context.target == musheen_core::CommandTarget::Sidebar
        {
            // Sidebar locations are navigable directory references, not a
            // separate filesystem object kind for registry evaluation. A
            // location the application resolved to a file keeps the file
            // target it was given, so the directory commands are not
            // offered for it.
            context.target = musheen_core::CommandTarget::Directory;
        }
        context.selection_count = selection.len();
        Self {
            context,
            target,
            location,
            selection: selection.into(),
            trash_contents: Arc::from([]),
            source: ContextMenuSource::Pointer,
            origin_tab: None,
            open_with: Vec::new(),
            send_to: Vec::new(),
            tags: Vec::new(),
            actions: Vec::new(),
        }
    }

    /// Empty Trash captures its contents without turning a background menu
    /// into a selection command. Confirmation retains this exact snapshot.
    pub(crate) fn with_trash_contents(mut self, contents: Vec<CommandTargetRef>) -> Self {
        if self.target == MenuTarget::TrashBackground {
            self.trash_contents = contents.into();
        }
        self
    }

    #[cfg(test)]
    pub(crate) fn captured_targets(&self) -> &[CommandTargetRef] {
        if self.target == MenuTarget::TrashBackground {
            &self.trash_contents
        } else {
            &self.selection
        }
    }

    pub(crate) fn captured_targets_arc(&self) -> Arc<[CommandTargetRef]> {
        if self.target == MenuTarget::TrashBackground {
            Arc::clone(&self.trash_contents)
        } else {
            Arc::clone(&self.selection)
        }
    }

    #[must_use]
    pub fn with_open_with(mut self, applications: &[crate::menus::OpenWithApplication]) -> Self {
        self.open_with = applications.to_vec();
        self
    }

    #[must_use]
    pub fn with_send_to(mut self, destinations: &[crate::menus::SendToDestination]) -> Self {
        self.send_to = destinations.to_vec();
        self
    }

    #[must_use]
    pub fn with_tags(mut self, tags: &[crate::menus::MenuContribution]) -> Self {
        self.tags = tags.to_vec();
        self
    }

    #[must_use]
    pub fn with_catalog_tag_names(mut self, tags: impl IntoIterator<Item = Box<str>>) -> Self {
        self.tags = tags
            .into_iter()
            .map(|tag| crate::menus::MenuContribution::new(tag, "item.tags"))
            .collect();
        self
    }

    #[must_use]
    pub fn with_actions(mut self, actions: &[crate::menus::MenuContribution]) -> Self {
        self.actions = actions.to_vec();
        self
    }

    #[must_use]
    pub const fn context(&self) -> &CommandContext {
        &self.context
    }

    #[must_use]
    pub const fn target(&self) -> MenuTarget {
        self.target
    }

    #[must_use]
    pub const fn location(&self) -> &StorePath {
        &self.location
    }

    #[must_use]
    pub fn selection(&self) -> &[CommandTargetRef] {
        &self.selection
    }

    #[must_use]
    pub const fn source(&self) -> ContextMenuSource {
        self.source
    }

    /// Binds all menu rows to the originating tab instead of whichever pane
    /// happens to be focused when a delayed menu item is activated.
    #[must_use]
    pub const fn with_origin_tab(mut self, tab: crate::navigation::TabId) -> Self {
        self.origin_tab = Some(tab);
        self
    }

    #[must_use]
    pub const fn origin_tab(&self) -> Option<crate::navigation::TabId> {
        self.origin_tab
    }

    pub(crate) fn context_with_destination(
        &self,
        destination: StorePath,
        writable: bool,
        read_only_reason: &str,
    ) -> CommandContext {
        let mut context = self.context.clone();
        context.resolved_destination = Some(if writable {
            musheen_core::ResolvedDestination::writable(destination)
        } else {
            musheen_core::ResolvedDestination::read_only(destination, read_only_reason)
        });
        context
    }
}

/// The selection captured at menu-open time, before later pane changes can make it stale.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreparedContextTarget {
    source: ContextMenuSource,
    target: MenuTarget,
    selection: Vec<CommandTargetRef>,
}

impl PreparedContextTarget {
    #[must_use]
    pub const fn source(&self) -> ContextMenuSource {
        self.source
    }

    #[must_use]
    pub const fn target(&self) -> MenuTarget {
        self.target
    }

    #[must_use]
    pub fn selection(&self) -> &[CommandTargetRef] {
        &self.selection
    }
}

pub(crate) fn pointer_target(
    selection: &[CommandTargetRef],
    clicked: &CommandTargetRef,
) -> PreparedContextTarget {
    let selection = if selection.iter().any(|item| item.id() == clicked.id()) {
        selection.to_vec()
    } else {
        vec![clicked.clone()]
    };
    PreparedContextTarget {
        source: ContextMenuSource::Pointer,
        target: MenuTarget::Item,
        selection,
    }
}

pub(crate) fn keyboard_target(focused: Option<CommandTargetRef>) -> PreparedContextTarget {
    match focused {
        Some(item) => PreparedContextTarget {
            source: ContextMenuSource::Keyboard,
            target: MenuTarget::Item,
            selection: vec![item],
        },
        None => PreparedContextTarget {
            source: ContextMenuSource::Keyboard,
            target: MenuTarget::Background,
            selection: Vec::new(),
        },
    }
}
