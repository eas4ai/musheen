use super::context::ContextMenuRequest;
use super::{MenuInvocationError, MenuTarget, OpenWithApplication, SendToDestination};
use crate::{Catalog, Locale};
use musheen_core::{
    CommandAction, CommandContext, CommandDefinition, CommandGroup, CommandId, CommandParameters,
    CommandPredicate, CommandRegistry, CommandState, CommandSubmenu, CommandTarget,
    CommandTargetRef, DangerLevel, StorePath,
};
use std::fmt;

pub const MAX_VARIABLE_CONTRIBUTIONS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuPresentation {
    CompactNativeTheme,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuThemeTokens {
    compact_rows: bool,
    strong_boundaries: bool,
    reduced_motion: bool,
}

impl MenuThemeTokens {
    #[must_use]
    pub const fn from_profile(profile: crate::ThemeProfile) -> Self {
        Self {
            compact_rows: true,
            strong_boundaries: profile.has_strong_boundaries(),
            reduced_motion: matches!(profile.motion(), crate::MotionPolicy::Reduced),
        }
    }

    #[must_use]
    pub const fn compact_rows(self) -> bool {
        self.compact_rows
    }
    #[must_use]
    pub const fn strong_boundaries(self) -> bool {
        self.strong_boundaries
    }
    #[must_use]
    pub const fn reduced_motion(self) -> bool {
        self.reduced_motion
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuEntryKind {
    Command,
    Submenu,
    Separator,
    Overflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuFocus {
    Entry(usize),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuKeyRoute {
    Dialog,
    Browser,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAccessibleRole {
    MenuItem,
    Checkbox,
    Radio,
    Submenu,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuAccessibilityNode {
    name: Box<str>,
    role: MenuAccessibleRole,
    checked: bool,
    disabled_reason: Option<Box<str>>,
    children: Vec<MenuAccessibilityNode>,
}

impl MenuAccessibilityNode {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    #[must_use]
    pub const fn role(&self) -> MenuAccessibleRole {
        self.role
    }
    #[must_use]
    pub const fn checked(&self) -> bool {
        self.checked
    }
    #[must_use]
    pub fn disabled_reason(&self) -> Option<&str> {
        self.disabled_reason.as_deref()
    }

    /// Submenu descendants are retained in the same semantic tree rather
    /// than becoming an inaccessible flat list when a native popup opens.
    #[must_use]
    pub fn children(&self) -> &[MenuAccessibilityNode] {
        &self.children
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuDirection {
    LeftToRight,
    RightToLeft,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuChrome {
    direction: MenuDirection,
}

impl MenuChrome {
    #[must_use]
    pub const fn submenu_arrow(self) -> &'static str {
        match self.direction {
            MenuDirection::LeftToRight => "→",
            MenuDirection::RightToLeft => "←",
        }
    }

    #[must_use]
    pub const fn path_direction(self) -> MenuDirection {
        MenuDirection::LeftToRight
    }
}

#[derive(Clone, Debug)]
pub struct ContextMenu {
    entries: Vec<MenuEntry>,
    presentation: MenuPresentation,
    theme_tokens: MenuThemeTokens,
    direction: MenuDirection,
}

impl ContextMenu {
    #[must_use]
    pub fn entries(&self) -> &[MenuEntry] {
        &self.entries
    }

    #[must_use]
    pub const fn presentation(&self) -> MenuPresentation {
        self.presentation
    }

    #[must_use]
    pub const fn theme_tokens(&self) -> MenuThemeTokens {
        self.theme_tokens
    }

    #[must_use]
    pub fn accessibility_tree(&self) -> Vec<MenuAccessibilityNode> {
        self.entries
            .iter()
            .filter(|entry| !matches!(entry.kind, MenuEntryKind::Separator))
            .map(|entry| MenuAccessibilityNode {
                name: entry.label.clone(),
                role: entry.accessible_role(),
                checked: entry.state.is_checked(),
                disabled_reason: entry.state.disabled_reason().map(Into::into),
                children: entry
                    .submenu
                    .as_deref()
                    .map_or_else(Vec::new, ContextMenu::accessibility_tree),
            })
            .collect()
    }

    #[must_use]
    pub fn first_keyboard_focus(&self) -> Option<MenuFocus> {
        self.entries.iter().enumerate().find_map(|(index, entry)| {
            matches!(entry.kind, MenuEntryKind::Command | MenuEntryKind::Submenu)
                .then_some(MenuFocus::Entry(index))
        })
    }

    #[must_use]
    pub const fn key_route(&self, modal_dialog_active: bool, _key: &str) -> MenuKeyRoute {
        if modal_dialog_active {
            MenuKeyRoute::Dialog
        } else {
            MenuKeyRoute::Browser
        }
    }

    #[must_use]
    pub fn entry(&self, id: &str) -> Option<&MenuEntry> {
        self.entries
            .iter()
            .find(|entry| entry.command_id() == Some(id))
    }

    #[must_use]
    pub fn groups_are_stable(&self) -> bool {
        let mut previous = None;
        for entry in &self.entries {
            let Some(group) = entry.group else {
                continue;
            };
            let rank = group_rank(group);
            if previous.is_some_and(|previous| previous > rank) {
                return false;
            }
            previous = Some(rank);
        }
        true
    }

    #[must_use]
    pub fn destructive_group_is_isolated(&self) -> bool {
        let Some(index) = self
            .entries
            .iter()
            .position(|entry| entry.group == Some(CommandGroup::Destructive))
        else {
            return true;
        };
        index == 0 || self.entries[index - 1].kind == MenuEntryKind::Separator
    }

    #[must_use]
    pub const fn direction(&self, direction: MenuDirection) -> MenuChrome {
        MenuChrome { direction }
    }

    #[must_use]
    pub const fn locale_direction(&self) -> MenuDirection {
        self.direction
    }
}

fn is_layout_choice(command_id: Option<&str>) -> bool {
    matches!(
        command_id,
        Some(
            "view.details"
                | "view.list"
                | "view.cards"
                | "view.grid"
                | "view.columns"
                | "view.adaptive"
        )
    )
}

const fn locale_direction(locale: Locale) -> MenuDirection {
    if matches!(locale, Locale::Ar) {
        MenuDirection::RightToLeft
    } else {
        MenuDirection::LeftToRight
    }
}

fn is_toggle_command(command_id: Option<&str>) -> bool {
    matches!(
        command_id,
        Some("view.directories_first" | "view.hidden" | "view.sidebar" | "view.info")
    )
}

#[derive(Clone, Debug)]
pub struct MenuEntry {
    kind: MenuEntryKind,
    command_id: Option<CommandId>,
    label: Box<str>,
    icon_key: Option<Box<str>>,
    state: CommandState,
    shortcut: Option<Box<str>>,
    group: Option<CommandGroup>,
    danger: DangerLevel,
    submenu: Option<Box<ContextMenu>>,
    application: Option<OpenWithApplication>,
    destination: Option<SendToDestination>,
    pub(crate) invocation: Option<InvocationData>,
}

impl MenuEntry {
    #[must_use]
    pub const fn kind(&self) -> MenuEntryKind {
        self.kind
    }

    #[must_use]
    pub fn command_id(&self) -> Option<&str> {
        self.command_id.as_ref().map(CommandId::as_str)
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn icon_key(&self) -> Option<&str> {
        self.icon_key.as_deref()
    }

    #[must_use]
    pub const fn state(&self) -> &CommandState {
        &self.state
    }

    #[must_use]
    pub fn shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }

    #[must_use]
    pub fn accessible_disabled_reason(&self) -> Option<&str> {
        self.state.disabled_reason()
    }

    /// Role and checked state are derived from the same registry projection
    /// that feeds the native popup, so assistive technology cannot observe a
    /// different command policy than keyboard and pointer users.
    #[must_use]
    pub fn accessible_role(&self) -> MenuAccessibleRole {
        if self.kind == MenuEntryKind::Submenu {
            MenuAccessibleRole::Submenu
        } else if is_layout_choice(self.command_id()) {
            MenuAccessibleRole::Radio
        } else if is_toggle_command(self.command_id()) {
            MenuAccessibleRole::Checkbox
        } else {
            MenuAccessibleRole::MenuItem
        }
    }

    #[must_use]
    pub const fn danger_level(&self) -> DangerLevel {
        self.danger
    }

    #[must_use]
    pub fn submenu(&self) -> Option<&ContextMenu> {
        self.submenu.as_deref()
    }

    #[must_use]
    pub fn application(&self) -> Option<&OpenWithApplication> {
        self.application.as_ref()
    }

    #[must_use]
    pub fn destination(&self) -> Option<&SendToDestination> {
        self.destination.as_ref()
    }

    #[must_use]
    pub fn copy_only(&self) -> bool {
        self.command_id() == Some("clipboard.send_to") && self.destination.is_some()
    }

    #[must_use]
    pub(crate) fn origin_tab(&self) -> Option<crate::navigation::TabId> {
        self.invocation
            .as_ref()
            .and_then(|invocation| invocation.origin_tab)
    }

    #[must_use]
    pub fn captured_targets(&self) -> &[CommandTargetRef] {
        self.invocation
            .as_ref()
            .map(|invocation| invocation.selection.as_slice())
            .unwrap_or_default()
    }

    /// Re-evaluates the registry predicate against the exact context captured
    /// for this row. Surface audits use this to prove that nested and variable
    /// menu entries cannot substitute their own enabled, checked, or refusal
    /// state for the command registry's policy.
    #[must_use]
    pub fn registry_policy_state(
        &self,
        registry: &CommandRegistry,
        locale: Locale,
    ) -> Option<CommandState> {
        let invocation = self.invocation.as_ref()?;
        let command = registry.get(invocation.id.as_str())?;
        let catalog = Catalog::load(locale).expect("built-in menu locale is valid");
        Some(
            command
                .state(&invocation.context)
                .map_disabled_reason(|reason| catalog.localize_reason(reason)),
        )
    }

    /// Generates the exact typed payload this nested row will dispatch without
    /// bypassing enablement or confirmation policy. Audits can therefore prove
    /// that variable submenu labels retain their captured targets, destination,
    /// application, and association intent.
    pub fn generated_parameters(&self) -> Result<CommandParameters, MenuInvocationError> {
        let invocation = self
            .invocation
            .clone()
            .ok_or(MenuInvocationError::NotInvokable)?;
        Ok(super::pending_with_parameters(invocation, self.application())?.parameters)
    }
}

impl ContextMenu {
    #[must_use]
    pub fn application(&self, desktop_id: &str) -> Option<&MenuEntry> {
        self.entries.iter().find(|entry| {
            entry
                .application()
                .is_some_and(|application| application.desktop_id() == desktop_id)
        })
    }

    #[must_use]
    pub fn destination(&self, path: &str) -> Option<&MenuEntry> {
        self.entries.iter().find(|entry| {
            entry.destination().is_some_and(|destination| {
                destination
                    .path()
                    .as_unix_path()
                    .is_some_and(|candidate| candidate == std::path::Path::new(path))
            })
        })
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InvocationData {
    pub(crate) id: CommandId,
    pub(crate) action: CommandAction,
    pub(crate) context: CommandContext,
    pub(crate) selection: Vec<CommandTargetRef>,
    pub(crate) location: StorePath,
    pub(crate) destination: Option<StorePath>,
    pub(crate) origin_tab: Option<crate::navigation::TabId>,
    pub(crate) custom_action: Option<musheen_desktop::CustomAction>,
}

pub(crate) fn compose(
    registry: &CommandRegistry,
    locale: Locale,
    theme: crate::ThemeProfile,
    request: ContextMenuRequest,
) -> ContextMenu {
    let catalog = Catalog::load(locale).expect("built-in menu locale is valid");
    let direction = locale_direction(locale);
    let mut candidates = command_ids_for(effective_target(&request))
        .iter()
        .enumerate()
        .filter_map(|(index, id)| {
            registry
                .get(id)
                .filter(|command| is_presentable(registry, command, &request))
                .map(|command| (index, command))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(index, command)| (group_rank(command.group()), *index));

    let mut entries = Vec::new();
    let mut previous_group = None;
    for (_, command) in candidates {
        if previous_group.is_some_and(|group| group != command.group()) {
            entries.push(separator());
        }
        previous_group = Some(command.group());
        entries.push(command_entry(
            registry,
            command,
            &catalog,
            &request,
            request.context(),
            theme,
            direction,
        ));
    }
    ContextMenu {
        entries,
        presentation: MenuPresentation::CompactNativeTheme,
        theme_tokens: MenuThemeTokens::from_profile(theme),
        direction,
    }
}

fn effective_target(request: &ContextMenuRequest) -> MenuTarget {
    match request.target() {
        MenuTarget::Item => match request.context().target {
            CommandTarget::Mount => MenuTarget::Mount,
            CommandTarget::TrashItem => MenuTarget::TrashItem,
            CommandTarget::TrashBackground => MenuTarget::TrashBackground,
            CommandTarget::Tag => MenuTarget::Tag,
            CommandTarget::Sidebar => MenuTarget::SidebarLocation,
            _ => MenuTarget::Item,
        },
        target => target,
    }
}

fn is_presentable(
    registry: &CommandRegistry,
    command: &CommandDefinition,
    request: &ContextMenuRequest,
) -> bool {
    let target = request.context().target;
    match command.id().as_str() {
        // These are target-shape applicability rules, not capability policy.
        // The registry remains the authority for all capability-limited states.
        "clipboard.paste_into"
            if matches!(
                target,
                CommandTarget::File | CommandTarget::Archive | CommandTarget::ExecutableFile
            ) =>
        {
            return false;
        }
        "file.create_hard_link" if target == CommandTarget::Directory => return false,
        "file.hide" if request.context().target_is_hidden => return false,
        "file.unhide" if !request.context().target_is_hidden => return false,
        "archive.browse" | "archive.extract" | "archive.extract_here"
            if target != CommandTarget::Archive =>
        {
            return false;
        }
        "file.compress" if target == CommandTarget::Archive => return false,
        "file.run" | "file.run_as_administrator" if target != CommandTarget::ExecutableFile => {
            return false;
        }
        "directory.open_as_administrator"
        | "directory.open_other_pane"
        | "directory.pin"
        | "directory.unpin"
        | "directory.share"
            if target != CommandTarget::Directory =>
        {
            return false;
        }
        "directory.open_new_tab" | "directory.open_new_window"
            if !matches!(target, CommandTarget::Directory | CommandTarget::Mount) =>
        {
            return false;
        }
        "mount.unmount" | "mount.eject" | "mount.power_off" if target != CommandTarget::Mount => {
            return false;
        }
        "trash.restore" if target != CommandTarget::TrashItem => return false,
        "clipboard.send_to" if request.send_to.is_empty() => return false,
        "actions.custom"
            if !request.actions.iter().any(|contribution| {
                valid_contribution_command(registry, contribution, CommandSubmenu::Actions)
                    .is_some()
            }) =>
        {
            return false;
        }
        _ => {}
    }
    if command.state(request.context()).is_enabled() {
        return true;
    }
    // A desktop/backend refusal does not make an otherwise applicable action
    // disappear. Keep its disabled row so the provider's explanation reaches
    // the user instead of looking like Musheen has no Restore/Empty Trash
    // action at all.
    let mut without_backend_refusal = request.context().clone();
    without_backend_refusal.backend_actions = None;
    if command.state(&without_backend_refusal).is_enabled() {
        return true;
    }
    match command.predicate() {
        CommandPredicate::Capability(_)
        | CommandPredicate::WritableLocation
        | CommandPredicate::WritableDestination
        | CommandPredicate::WritableSelection
        | CommandPredicate::WritableExactlyOneSelection
        | CommandPredicate::WritableSelectionCapability(_)
        | CommandPredicate::WritableExactlyOneSelectionCapability(_)
        | CommandPredicate::WritableExactlyOneFileCapability(_)
        | CommandPredicate::Hide
        | CommandPredicate::Unhide
        | CommandPredicate::PasteInto
        | CommandPredicate::DestinationCopy
        | CommandPredicate::DestinationMove
        | CommandPredicate::DestinationExtract
        | CommandPredicate::ProviderAction(_) => true,
        CommandPredicate::PinnedDirectory => request.context().target_is_pinned,
        CommandPredicate::UnpinnedDirectory => !request.context().target_is_pinned,
        _ => false,
    }
}

fn command_entry(
    registry: &CommandRegistry,
    command: &CommandDefinition,
    catalog: &Catalog,
    request: &ContextMenuRequest,
    context: &CommandContext,
    theme: crate::ThemeProfile,
    direction: MenuDirection,
) -> MenuEntry {
    let mut entry = plain_command_entry(command, catalog, request, context);
    match command.id().as_str() {
        "file.open_with" => {
            add_open_with_submenu(registry, catalog, request, &mut entry, theme, direction)
        }
        "clipboard.send_to" => {
            add_send_to_submenu(registry, catalog, request, &mut entry, theme, direction)
        }
        "item.tags" => add_contribution_submenu(
            registry, catalog, request, &mut entry, true, theme, direction,
        ),
        "actions.custom" => add_contribution_submenu(
            registry, catalog, request, &mut entry, false, theme, direction,
        ),
        _ => {}
    }
    entry
}

fn plain_command_entry(
    command: &CommandDefinition,
    catalog: &Catalog,
    request: &ContextMenuRequest,
    context: &CommandContext,
) -> MenuEntry {
    let label = catalog
        .message(command.label_key())
        .unwrap_or(command.label_key())
        .into();
    MenuEntry {
        kind: MenuEntryKind::Command,
        command_id: Some(command.id().clone()),
        label,
        icon_key: Some(command.icon_key().into()),
        state: command
            .state(context)
            .map_disabled_reason(|reason| catalog.localize_reason(reason)),
        shortcut: command
            .shortcuts()
            .first()
            .map(|shortcut| shortcut.chord().into()),
        group: Some(command.group()),
        danger: command.danger_level(),
        submenu: None,
        application: None,
        destination: None,
        invocation: Some(InvocationData {
            id: command.id().clone(),
            action: command.action(),
            context: context.clone(),
            selection: request.captured_targets().to_vec(),
            location: selected_directory_location(command.action(), context, request),
            destination: None,
            origin_tab: request.origin_tab(),
            custom_action: None,
        }),
    }
}

fn selected_directory_location(
    action: CommandAction,
    context: &CommandContext,
    request: &ContextMenuRequest,
) -> StorePath {
    if matches!(
        action,
        CommandAction::OpenTerminalHere
            | CommandAction::DirectoryProperties
            | CommandAction::PasteInto
    ) && matches!(
        context.target,
        CommandTarget::Directory | CommandTarget::Mount
    ) {
        return request.selection().first().map_or_else(
            || request.location().clone(),
            |target| target.path().clone(),
        );
    }
    request.location().clone()
}

fn add_open_with_submenu(
    registry: &CommandRegistry,
    catalog: &Catalog,
    request: &ContextMenuRequest,
    entry: &mut MenuEntry,
    theme: crate::ThemeProfile,
    direction: MenuDirection,
) {
    let Some(open_with) = registry.get("file.open_with") else {
        return;
    };
    let compatible = request
        .open_with
        .iter()
        .filter(|application| application.is_compatible())
        .collect::<Vec<_>>();
    let mut status_entries = request
        .open_with
        .iter()
        .filter(|application| !application.is_compatible())
        .map(|application| {
            let mut disabled_context = request.context().clone();
            disabled_context.selection_count = 0;
            let mut child = plain_command_entry(open_with, catalog, request, &disabled_context);
            child.label = application.label().into();
            child
        })
        .collect::<Vec<_>>();
    let mut app_entries = compatible
        .iter()
        .map(|application| {
            let mut child = plain_command_entry(open_with, catalog, request, request.context());
            child.label = application.label().into();
            child.application = Some((*application).clone());
            child
        })
        .collect::<Vec<_>>();
    let overflow = app_entries.split_off(app_entries.len().min(MAX_VARIABLE_CONTRIBUTIONS));
    status_entries.extend(app_entries);
    let mut entries = status_entries;
    append_overflow_submenu(
        &mut entries,
        overflow,
        theme,
        direction,
        menu_more_label(catalog),
    );
    if let Some(command) = registry.get("file.choose_application") {
        entries.push(command_entry(
            registry,
            command,
            catalog,
            request,
            request.context(),
            theme,
            direction,
        ));
    }
    if let Some(set_default) = registry.get("file.set_default_application") {
        let mut defaults = compatible
            .iter()
            .map(|application| {
                let mut child =
                    plain_command_entry(set_default, catalog, request, request.context());
                child.label = application.label().into();
                child.application = Some((*application).clone());
                child
            })
            .collect::<Vec<_>>();
        let overflow = defaults.split_off(defaults.len().min(MAX_VARIABLE_CONTRIBUTIONS));
        append_overflow_submenu(
            &mut defaults,
            overflow,
            theme,
            direction,
            menu_more_label(catalog),
        );
        let mut default_entry =
            plain_command_entry(set_default, catalog, request, request.context());
        default_entry.kind = MenuEntryKind::Submenu;
        default_entry.submenu = Some(Box::new(ContextMenu {
            entries: defaults,
            presentation: MenuPresentation::CompactNativeTheme,
            theme_tokens: MenuThemeTokens::from_profile(theme),
            direction,
        }));
        entries.push(default_entry);
    }
    entry.kind = MenuEntryKind::Submenu;
    entry.submenu = Some(Box::new(ContextMenu {
        entries,
        presentation: MenuPresentation::CompactNativeTheme,
        theme_tokens: MenuThemeTokens::from_profile(theme),
        direction,
    }));
}

fn add_send_to_submenu(
    registry: &CommandRegistry,
    catalog: &Catalog,
    request: &ContextMenuRequest,
    entry: &mut MenuEntry,
    theme: crate::ThemeProfile,
    direction: MenuDirection,
) {
    let Some(send_to) = registry.get("clipboard.send_to") else {
        return;
    };
    let read_only_reason = catalog
        .message("command-refusal-6")
        .expect("the read-only destination refusal is localized");
    let mut destination_entries = request
        .send_to
        .iter()
        .map(|destination| {
            let context = request.context_with_destination(
                destination.path().clone(),
                destination.writable(),
                read_only_reason,
            );
            let mut child = plain_command_entry(send_to, catalog, request, &context);
            child.label = destination.label().into();
            child.destination = Some(destination.clone());
            child
                .invocation
                .as_mut()
                .expect("command rows are invokable")
                .destination = Some(destination.path().clone());
            child
        })
        .collect::<Vec<_>>();
    let overflow =
        destination_entries.split_off(destination_entries.len().min(MAX_VARIABLE_CONTRIBUTIONS));
    let mut entries = destination_entries;
    append_overflow_submenu(
        &mut entries,
        overflow,
        theme,
        direction,
        menu_more_label(catalog),
    );
    if let Some(destination) = request
        .send_to
        .iter()
        .find(|destination| destination.writable())
        .or_else(|| request.send_to.first())
    {
        let context = request.context_with_destination(
            destination.path().clone(),
            destination.writable(),
            read_only_reason,
        );
        entry.state = send_to
            .state(&context)
            .map_disabled_reason(|reason| catalog.localize_reason(reason));
        let invocation = entry
            .invocation
            .as_mut()
            .expect("command rows are invokable");
        invocation.context = context;
        invocation.destination = Some(destination.path().clone());
    }
    entry.kind = MenuEntryKind::Submenu;
    entry.submenu = Some(Box::new(ContextMenu {
        entries,
        presentation: MenuPresentation::CompactNativeTheme,
        theme_tokens: MenuThemeTokens::from_profile(theme),
        direction,
    }));
}

fn add_contribution_submenu(
    registry: &CommandRegistry,
    catalog: &Catalog,
    request: &ContextMenuRequest,
    entry: &mut MenuEntry,
    tags: bool,
    theme: crate::ThemeProfile,
    direction: MenuDirection,
) {
    let contributions = if tags {
        &request.tags
    } else {
        &request.actions
    };
    if contributions.is_empty() {
        return;
    }
    let expected_submenu = if tags {
        CommandSubmenu::Tags
    } else {
        CommandSubmenu::Actions
    };
    let mut entries = contributions
        .iter()
        .filter_map(|contribution| {
            valid_contribution_command(registry, contribution, expected_submenu)
                .map(|command| (contribution, command))
        })
        .map(|(contribution, command)| contribution_entry(contribution, command, catalog, request))
        .collect::<Vec<_>>();
    let overflow = entries.split_off(entries.len().min(MAX_VARIABLE_CONTRIBUTIONS));
    append_overflow_submenu(
        &mut entries,
        overflow,
        theme,
        direction,
        menu_more_label(catalog),
    );
    entry.kind = MenuEntryKind::Submenu;
    entry.submenu = Some(Box::new(ContextMenu {
        entries,
        presentation: MenuPresentation::CompactNativeTheme,
        theme_tokens: MenuThemeTokens::from_profile(theme),
        direction,
    }));
}

fn valid_contribution_command<'a>(
    registry: &'a CommandRegistry,
    contribution: &super::MenuContribution,
    expected_submenu: CommandSubmenu,
) -> Option<&'a CommandDefinition> {
    let command = registry.get(contribution.command_id())?;
    (command.submenu() == Some(expected_submenu)
        && (expected_submenu != CommandSubmenu::Actions
            || contribution
                .custom_action
                .as_ref()
                .is_some_and(|action| action.validate().is_ok())))
    .then_some(command)
}

fn set_custom_action_availability(
    context: &mut CommandContext,
    catalog: &Catalog,
    availability: Result<(), &'static str>,
) {
    let states = context.backend_actions.get_or_insert_with(Default::default);
    states.retain(|(action, _)| *action != CommandAction::CustomAction);
    let state = match availability {
        Ok(()) => musheen_core::CapabilityState::Supported,
        Err(key) => musheen_core::CapabilityState::Unsupported(
            musheen_core::CapabilityReason::new(
                catalog.message(key).expect("localized action state"),
            )
            .expect("nonempty action state"),
        ),
    };
    states.push((CommandAction::CustomAction, state));
}

fn contribution_entry(
    contribution: &super::MenuContribution,
    command: &CommandDefinition,
    catalog: &Catalog,
    request: &ContextMenuRequest,
) -> MenuEntry {
    let mut context = request.context().clone();
    if let Some(action) = &contribution.custom_action {
        context.supports_provider_uris = action.supports_provider_uris;
        set_custom_action_availability(
            &mut context,
            catalog,
            contribution
                .custom_action_availability
                .unwrap_or(Err("custom-action-checking")),
        );
    }
    let mut child = plain_command_entry(command, catalog, request, &context);
    child.label = contribution.label().into();
    let Some(action) = &contribution.custom_action else {
        return child;
    };
    if matches!(
        action.execution,
        musheen_desktop::ActionExecution::Shell { .. }
    ) {
        child.label = format!(
            "{} — {}",
            child.label,
            catalog
                .message("custom-action-shell")
                .expect("localized shell label")
        )
        .into();
    }
    child.danger = match action.confirmation {
        musheen_desktop::ActionConfirmation::Never => DangerLevel::None,
        musheen_desktop::ActionConfirmation::Always => DangerLevel::Review,
        musheen_desktop::ActionConfirmation::Destructive => DangerLevel::Destructive,
    };
    if let Some(data) = &mut child.invocation {
        data.custom_action = Some(action.clone());
    }
    child
}

fn append_overflow_submenu(
    entries: &mut Vec<MenuEntry>,
    mut remaining: Vec<MenuEntry>,
    theme: crate::ThemeProfile,
    direction: MenuDirection,
    more_label: &str,
) {
    if remaining.is_empty() {
        return;
    }
    let tail = remaining.split_off(remaining.len().min(MAX_VARIABLE_CONTRIBUTIONS));
    append_overflow_submenu(&mut remaining, tail, theme, direction, more_label);
    entries.push(MenuEntry {
        kind: MenuEntryKind::Submenu,
        command_id: None,
        label: more_label.into(),
        icon_key: None,
        state: CommandRegistry::built_in()
            .get("navigation.refresh")
            .expect("built-in command exists")
            .state(&CommandContext::default()),
        shortcut: None,
        group: None,
        danger: DangerLevel::None,
        submenu: Some(Box::new(ContextMenu {
            entries: remaining,
            presentation: MenuPresentation::CompactNativeTheme,
            theme_tokens: MenuThemeTokens::from_profile(theme),
            direction,
        })),
        application: None,
        destination: None,
        invocation: None,
    });
}

fn menu_more_label(catalog: &Catalog) -> &str {
    catalog.message("menu-more").unwrap_or("More…")
}

fn separator() -> MenuEntry {
    MenuEntry {
        kind: MenuEntryKind::Separator,
        command_id: None,
        label: Box::default(),
        icon_key: None,
        state: CommandRegistry::built_in()
            .get("navigation.refresh")
            .expect("built-in command exists")
            .state(&CommandContext::default()),
        shortcut: None,
        group: None,
        danger: DangerLevel::None,
        submenu: None,
        application: None,
        destination: None,
        invocation: None,
    }
}

const fn group_rank(group: CommandGroup) -> u8 {
    match group {
        CommandGroup::Open => 0,
        CommandGroup::Navigation => 1,
        CommandGroup::Clipboard => 2,
        CommandGroup::Creation => 3,
        CommandGroup::FileType => 4,
        CommandGroup::Organization => 5,
        CommandGroup::Destructive => 6,
        CommandGroup::Details => 7,
    }
}

fn command_ids_for(target: MenuTarget) -> &'static [&'static str] {
    match target {
        MenuTarget::Background => BACKGROUND,
        MenuTarget::Item => ITEM,
        MenuTarget::SidebarLocation => SIDEBAR,
        MenuTarget::Mount => MOUNT,
        MenuTarget::Tag => TAG,
        MenuTarget::TrashItem => TRASH_ITEM,
        MenuTarget::TrashBackground => TRASH_BACKGROUND,
    }
}

const BACKGROUND: &[&str] = &[
    "clipboard.paste_into",
    "selection.select_all",
    "create.directory",
    "create.empty_file",
    "create.from_template",
    "directory.open_terminal",
    "view.hidden",
    "view.details",
    "view.list",
    "view.cards",
    "view.grid",
    "view.columns",
    "view.adaptive",
    "view.sort",
    "view.group",
    "view.directories_first",
    "directory.properties",
];
const ITEM: &[&str] = &[
    "file.open",
    "file.open_with",
    "file.run",
    "file.run_as_administrator",
    "directory.open_as_administrator",
    "directory.open_new_tab",
    "directory.open_new_window",
    "directory.open_other_pane",
    "clipboard.cut",
    "clipboard.copy",
    "clipboard.copy_to",
    "clipboard.move_to",
    "clipboard.paste_into",
    "clipboard.send_to",
    "file.preview",
    "archive.browse",
    "archive.extract",
    "archive.extract_here",
    "file.compress",
    "file.rename",
    "file.duplicate",
    "file.create_symbolic_link",
    "file.create_hard_link",
    "file.hide",
    "file.unhide",
    "directory.pin",
    "directory.unpin",
    "item.tags",
    "actions.custom",
    "directory.share",
    "file.move_to_trash",
    "file.delete_permanently",
    "item.properties",
    "item.permissions",
    "item.copy_location",
    "directory.open_terminal",
    "directory.properties",
];
const SIDEBAR: &[&str] = &[
    "file.open",
    "directory.open_new_tab",
    "directory.open_new_window",
    "directory.pin",
    "directory.unpin",
    "item.copy_location",
    "directory.properties",
];
const MOUNT: &[&str] = &[
    "file.open",
    "directory.open_new_tab",
    "directory.open_new_window",
    "mount.unmount",
    "mount.eject",
    "mount.power_off",
    "directory.properties",
];
const TAG: &[&str] = &["tag.rename", "tag.delete"];
const TRASH_ITEM: &[&str] = &[
    "trash.restore",
    "file.delete_permanently",
    "item.properties",
    "item.copy_location",
];
const TRASH_BACKGROUND: &[&str] = &["trash.empty", "view.hidden", "view.sort", "view.group"];

impl fmt::Display for MenuDirection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LeftToRight => formatter.write_str("ltr"),
            Self::RightToLeft => formatter.write_str("rtl"),
        }
    }
}
