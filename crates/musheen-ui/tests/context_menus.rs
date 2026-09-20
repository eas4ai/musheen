use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, CommandAction,
    CommandContext, CommandDispatchError, CommandDispatcher, CommandParameters, CommandRegistry,
    CommandTarget, CommandTargetRef, ItemId, OpenWithIntent, ProviderActionMatrix, ProviderId,
    StorePath,
};
use musheen_ui::{
    AppearanceMode, ContextMenuDestinationResolver, ContextMenuRequest, ContextMenuSource,
    ContextMenuSurface, MenuDirection, MenuEntryKind, MenuFocus, MenuInvocation, MenuKeyRoute,
    MenuPresentation, MenuTarget, OpenWithApplication, SendToDestination, ShellModel, ThemeProfile,
};

fn path(value: &str) -> StorePath {
    StorePath::from_unix_path(value)
}

fn target(key: &[u8], value: &str) -> CommandTargetRef {
    CommandTargetRef::new(
        ItemId::new(ProviderId::new("local").unwrap(), key.to_vec()).unwrap(),
        path(value),
    )
    .unwrap()
}

fn supported_context(target: CommandTarget, selected: usize) -> CommandContext {
    CommandContext {
        target,
        selection_count: selected,
        item_count: 3,
        location_is_writable: true,
        mutation_is_supported: true,
        is_local: true,
        has_dot_name_semantics: true,
        executable_run_enabled: true,
        capabilities: CapabilityMatrix::new(|_| CapabilityState::Supported),
        provider_actions: ProviderActionMatrix::from_states(
            CapabilityState::Supported,
            CapabilityState::Supported,
            CapabilityState::Supported,
            CapabilityState::Supported,
        ),
        ..CommandContext::default()
    }
}

fn request(
    context: CommandContext,
    target: MenuTarget,
    selected: Vec<CommandTargetRef>,
) -> ContextMenuRequest {
    ContextMenuRequest::new(context, target, path("/work"), selected)
}

#[test]
fn target_matrix_projects_only_registry_commands_in_stable_groups() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let file = target(b"file", "/work/file.txt");
    let archive = target(b"archive", "/work/archive.tar");
    let executable = target(b"run", "/work/run");
    let cases = [
        (
            "background",
            request(
                supported_context(CommandTarget::Background, 0),
                MenuTarget::Background,
                vec![],
            ),
            vec![
                "create.directory",
                "clipboard.paste_into",
                "selection.select_all",
            ],
        ),
        (
            "file",
            request(
                supported_context(CommandTarget::File, 1),
                MenuTarget::Item,
                vec![file],
            ),
            vec![
                "file.open",
                "file.open_with",
                "clipboard.copy",
                "file.move_to_trash",
            ],
        ),
        (
            "archive",
            request(
                supported_context(CommandTarget::Archive, 1),
                MenuTarget::Item,
                vec![archive],
            ),
            vec!["archive.browse", "archive.extract", "archive.extract_here"],
        ),
        (
            "executable",
            request(
                supported_context(CommandTarget::ExecutableFile, 1),
                MenuTarget::Item,
                vec![executable],
            ),
            vec!["file.run", "file.run_as_administrator"],
        ),
        (
            "trash",
            request(
                supported_context(CommandTarget::TrashItem, 1),
                MenuTarget::Item,
                vec![target(b"trash", "/trash/file")],
            ),
            vec!["trash.restore", "file.delete_permanently"],
        ),
    ];

    for (name, request, expected) in cases {
        let menu = surface.compose(request);
        let actual = menu
            .entries()
            .iter()
            .filter_map(|entry| entry.command_id())
            .collect::<Vec<_>>();
        for id in expected {
            assert!(actual.contains(&id), "{name} is missing {id}");
            assert!(
                surface.registry().get(id).is_some(),
                "{id} bypassed the registry"
            );
        }
        assert!(menu.groups_are_stable(), "{name}");
        assert!(menu.destructive_group_is_isolated(), "{name}");
    }
}

#[test]
fn right_click_and_keyboard_target_the_current_pane_without_stale_selection() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let selected = target(b"selected", "/left/selected");
    let clicked = target(b"clicked", "/left/clicked");
    let result = surface.prepare_pointer_target(std::slice::from_ref(&selected), &clicked);
    assert_eq!(result.selection(), std::slice::from_ref(&clicked));

    let preserved = surface.prepare_pointer_target(&[selected.clone(), clicked.clone()], &clicked);
    assert_eq!(preserved.selection(), &[selected, clicked]);

    let keyboard = surface.prepare_keyboard_target(Some(target(b"right", "/right/focus")));
    assert_eq!(keyboard.source(), ContextMenuSource::Keyboard);
    assert_eq!(keyboard.target(), MenuTarget::Item);
    assert_eq!(keyboard.selection()[0].path(), &path("/right/focus"));
    assert_eq!(
        surface.prepare_keyboard_target(None).target(),
        MenuTarget::Background
    );
}

#[test]
fn disabled_provider_limits_remain_accessible_but_inapplicable_actions_are_absent() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let refusal = CapabilityReason::new("links are unavailable on this provider").unwrap();
    let context = CommandContext {
        capabilities: CapabilityMatrix::new(|kind| match kind {
            CapabilityKind::SymbolicLinks => CapabilityState::Unsupported(refusal.clone()),
            _ => CapabilityState::Supported,
        }),
        ..supported_context(CommandTarget::File, 1)
    };
    let menu = surface.compose(request(
        context,
        MenuTarget::Item,
        vec![target(b"a", "/work/a")],
    ));
    let link = menu.entry("file.create_symbolic_link").unwrap();
    assert!(!link.state().is_enabled());
    assert_eq!(
        link.accessible_disabled_reason(),
        Some("links are unavailable on this provider")
    );
    assert!(menu.entry("directory.open_as_administrator").is_none());
}

#[test]
fn open_with_and_send_to_keep_one_time_association_and_copy_only_destinations_separate() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let apps = [
        OpenWithApplication::compatible("Text Editor", "org.example.Text"),
        OpenWithApplication::incompatible("Image Viewer", "org.example.Image"),
    ];
    let destinations = [
        SendToDestination::pinned("Archive", path("/archive"), true),
        SendToDestination::remote("Read only", path("/remote"), false),
    ];
    let menu = surface.compose(
        request(
            supported_context(CommandTarget::File, 1),
            MenuTarget::Item,
            vec![target(b"a", "/work/a")],
        )
        .with_open_with(&apps)
        .with_send_to(&destinations),
    );
    let open_with = menu.entry("file.open_with").unwrap();
    assert_eq!(open_with.kind(), MenuEntryKind::Submenu);
    assert!(
        open_with
            .submenu()
            .unwrap()
            .entry("file.choose_application")
            .is_some()
    );
    assert!(
        open_with
            .submenu()
            .unwrap()
            .entry("file.set_default_application")
            .is_some()
    );
    assert!(
        open_with
            .submenu()
            .unwrap()
            .application("org.example.Image")
            .is_none()
    );

    let send_to = menu.entry("clipboard.send_to").unwrap().submenu().unwrap();
    assert!(send_to.destination("/archive").unwrap().copy_only());
    assert!(!send_to.destination("/remote").unwrap().state().is_enabled());
}

#[test]
fn open_with_rows_dispatch_validated_application_identity_and_default_intent() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let apps = [
        OpenWithApplication::compatible("Editor", "org.example.Editor"),
        OpenWithApplication::compatible("Viewer", "org.example.Viewer"),
    ];
    let menu = surface.compose(
        request(
            supported_context(CommandTarget::File, 1),
            MenuTarget::Item,
            vec![target(b"open-with", "/work/document.txt")],
        )
        .with_open_with(&apps),
    );
    let open_with = menu.entry("file.open_with").unwrap().submenu().unwrap();
    let defaults = open_with
        .entry("file.set_default_application")
        .unwrap()
        .submenu()
        .expect("set default is an application chooser");
    let mut dispatcher = RecordingDispatcher::default();

    assert!(
        surface
            .invoke(
                open_with.application("org.example.Editor").unwrap(),
                &mut dispatcher
            )
            .is_dispatched()
    );
    let set_default = surface.invoke(
        defaults.application("org.example.Viewer").unwrap(),
        &mut dispatcher,
    );
    assert!(set_default.needs_confirmation());
    surface.confirm(set_default, &mut dispatcher).unwrap();

    assert!(matches!(
        &dispatcher.calls[0].1,
        CommandParameters::OpenWith { application, intent: OpenWithIntent::OpenOnce, .. }
            if application.as_str() == "org.example.Editor"
    ));
    assert!(matches!(
        &dispatcher.calls[1].1,
        CommandParameters::OpenWith { application, intent: OpenWithIntent::SetAsDefault, .. }
            if application.as_str() == "org.example.Viewer"
    ));
}

#[test]
fn invocation_preserves_command_id_contracts_and_confirmation_routes() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let request = request(
        supported_context(CommandTarget::File, 1),
        MenuTarget::Item,
        vec![target(b"a", "/work/a")],
    );
    let menu = surface.compose(request);
    let mut dispatcher = RecordingDispatcher::default();

    let copy_to = surface.invoke(menu.entry("clipboard.copy_to").unwrap(), &mut dispatcher);
    assert!(copy_to.needs_destination_chooser());
    assert!(dispatcher.calls.is_empty());

    let delete = surface.invoke(
        menu.entry("file.delete_permanently").unwrap(),
        &mut dispatcher,
    );
    assert!(delete.needs_confirmation());
    assert!(dispatcher.calls.is_empty());
    surface.confirm(delete, &mut dispatcher).unwrap();
    assert_eq!(dispatcher.calls[0].0, CommandAction::DeletePermanently);
    assert!(matches!(
        dispatcher.calls[0].1,
        CommandParameters::Targets(_)
    ));
}

#[test]
fn presentation_has_compact_theme_tokens_shortcuts_checked_state_and_rtl_directional_chrome() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let mut context = supported_context(CommandTarget::Background, 0);
    context.show_hidden = true;
    let menu = surface.compose(request(context, MenuTarget::Background, vec![]));
    let hidden = menu.entry("view.hidden").unwrap();
    assert!(hidden.state().is_checked());
    assert_eq!(menu.presentation(), MenuPresentation::CompactNativeTheme);
    assert!(menu.entry("view.hidden").unwrap().shortcut().is_some());
    assert_eq!(
        menu.direction(MenuDirection::RightToLeft).submenu_arrow(),
        "←"
    );
    assert_eq!(
        menu.direction(MenuDirection::RightToLeft).path_direction(),
        MenuDirection::LeftToRight
    );
}

#[test]
fn chooser_resolution_rechecks_destination_policy_and_cancellation_never_dispatches() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let menu = surface.compose(request(
        supported_context(CommandTarget::File, 1),
        MenuTarget::Item,
        vec![target(b"a", "/work/a")],
    ));
    let mut dispatcher = RecordingDispatcher::default();
    assert!(surface.cancel_destination().is_cancelled());

    let MenuInvocation::NeedsDestinationChooser(pending) =
        surface.invoke(menu.entry("clipboard.copy_to").unwrap(), &mut dispatcher)
    else {
        panic!("copy to must request a destination");
    };
    assert!(
        surface
            .resolve_destination(
                pending,
                path("/read-only"),
                &DestinationResolver { writable: false },
                &mut dispatcher,
            )
            .is_rejected()
    );
    assert!(dispatcher.calls.is_empty());

    let MenuInvocation::NeedsDestinationChooser(pending) =
        surface.invoke(menu.entry("clipboard.copy_to").unwrap(), &mut dispatcher)
    else {
        panic!("copy to must request a destination");
    };
    assert!(
        surface
            .resolve_destination(
                pending,
                path("/copy-target"),
                &DestinationResolver { writable: true },
                &mut dispatcher,
            )
            .is_dispatched()
    );
    assert_eq!(dispatcher.calls[0].0, CommandAction::CopyTo);
    assert!(matches!(
        dispatcher.calls[0].1,
        CommandParameters::Destination { .. }
    ));
}

#[test]
fn sidebar_mount_tag_and_trash_background_project_their_exact_target_sets() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let cases = [
        (
            MenuTarget::SidebarLocation,
            CommandTarget::Sidebar,
            "directory.open_new_window",
        ),
        (MenuTarget::Mount, CommandTarget::Mount, "mount.unmount"),
        (MenuTarget::Tag, CommandTarget::Tag, "item.tags"),
        (
            MenuTarget::TrashBackground,
            CommandTarget::TrashBackground,
            "trash.empty",
        ),
    ];
    for (target_kind, command_target, expected) in cases {
        let selected = if matches!(target_kind, MenuTarget::TrashBackground) {
            vec![]
        } else {
            vec![target(expected.as_bytes(), "/work/target")]
        };
        let selected_count = selected.len();
        let menu = surface.compose(request(
            supported_context(command_target, selected_count),
            target_kind,
            selected,
        ));
        assert!(menu.entry(expected).is_some(), "{target_kind:?}");
    }
}

#[test]
fn context_menu_shape_rules_keep_file_and_directory_actions_exact() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let file = surface.compose(request(
        supported_context(CommandTarget::File, 1),
        MenuTarget::Item,
        vec![target(b"file", "/work/file")],
    ));
    assert!(file.entry("clipboard.paste_into").is_none());
    assert!(file.entry("file.unhide").is_none());

    let directory = surface.compose(request(
        supported_context(CommandTarget::Directory, 1),
        MenuTarget::Item,
        vec![target(b"directory", "/work/directory")],
    ));
    assert!(directory.entry("clipboard.paste_into").is_some());
    assert!(directory.entry("file.create_hard_link").is_none());
    assert!(directory.entry("file.hide").is_some());

    let mut hidden = supported_context(CommandTarget::File, 1);
    hidden.target_is_hidden = true;
    let hidden = surface.compose(request(
        hidden,
        MenuTarget::Item,
        vec![target(b"hidden", "/work/.hidden")],
    ));
    assert!(hidden.entry("file.hide").is_none());
    assert!(hidden.entry("file.unhide").is_some());

    let mount = surface.compose(request(
        supported_context(CommandTarget::Mount, 1),
        MenuTarget::Mount,
        vec![target(b"mount", "/mnt/drive")],
    ));
    assert!(
        mount
            .entry("directory.properties")
            .unwrap()
            .state()
            .is_enabled()
    );
}

#[test]
fn selected_directory_location_actions_use_the_directory_not_its_view() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let mut context = supported_context(CommandTarget::Directory, 1);
    context.clipboard_has_contents = true;
    context.resolved_destination = Some(musheen_core::ResolvedDestination::writable(path(
        "/work/selected",
    )));
    let menu = surface.compose(request(
        context,
        MenuTarget::Item,
        vec![target(b"directory", "/work/selected")],
    ));
    let mut dispatcher = RecordingDispatcher::default();
    assert!(
        surface
            .invoke(
                menu.entry("directory.open_terminal").unwrap(),
                &mut dispatcher
            )
            .is_dispatched()
    );
    assert!(matches!(
        &dispatcher.calls[0].1,
        CommandParameters::Location(location) if location == &path("/work/selected")
    ));

    let mut dispatcher = RecordingDispatcher::default();
    assert!(
        surface
            .invoke(menu.entry("clipboard.paste_into").unwrap(), &mut dispatcher)
            .is_dispatched()
    );
    assert!(matches!(
        &dispatcher.calls[0].1,
        CommandParameters::Location(location) if location == &path("/work/selected")
    ));
}

#[test]
fn compatible_open_with_overflow_retains_remaining_apps_as_a_keyboard_submenu() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let apps = (0..=musheen_ui::MAX_VARIABLE_CONTRIBUTIONS)
        .map(|index| {
            OpenWithApplication::compatible(
                format!("Editor {index}"),
                format!("org.example.Editor{index}"),
            )
        })
        .collect::<Vec<_>>();
    let menu = surface.compose(
        request(
            supported_context(CommandTarget::File, 1),
            MenuTarget::Item,
            vec![target(b"overflow", "/work/document")],
        )
        .with_open_with(&apps),
    );
    let submenu = menu.entry("file.open_with").unwrap().submenu().unwrap();
    let more = submenu
        .entries()
        .iter()
        .find(|entry| entry.label().contains("More"))
        .expect("remaining compatible applications are reachable");
    assert_eq!(more.kind(), MenuEntryKind::Submenu);
    assert!(
        more.submenu()
            .unwrap()
            .application("org.example.Editor8")
            .is_some()
    );
    assert!(
        submenu
            .accessibility_tree()
            .iter()
            .any(|node| node.name().contains("More")
                && matches!(node.role(), musheen_ui::MenuAccessibleRole::Submenu))
    );
}

#[test]
fn menu_accessibility_and_keyboard_follow_modal_focus_and_pseudo_locale_rules() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in())
        .with_locale(musheen_ui::Locale::EnXa)
        .with_theme_profile(ThemeProfile::new(AppearanceMode::HighContrast, true));
    let menu = surface.compose(request(
        supported_context(CommandTarget::Background, 0),
        MenuTarget::Background,
        vec![],
    ));
    let tree = menu.accessibility_tree();
    assert!(tree.iter().all(|node| !node.name().is_empty()));
    assert!(tree.iter().any(|node| node.name().starts_with('⟦')));
    assert_eq!(menu.first_keyboard_focus(), Some(MenuFocus::Entry(0)));
    assert_eq!(menu.key_route(true, "Escape"), MenuKeyRoute::Dialog);
    assert_eq!(menu.key_route(false, "Escape"), MenuKeyRoute::Browser);
    assert!(menu.theme_tokens().strong_boundaries());
    let file_menu = surface.compose(
        request(
            supported_context(CommandTarget::File, 1),
            MenuTarget::Item,
            vec![target(b"theme", "/work/theme")],
        )
        .with_open_with(&[OpenWithApplication::compatible(
            "Editor",
            "org.example.Editor",
        )]),
    );
    assert!(
        file_menu
            .entry("file.open_with")
            .unwrap()
            .submenu()
            .unwrap()
            .theme_tokens()
            .strong_boundaries()
    );

    let layout_menu = surface.compose(request(
        supported_context(CommandTarget::Background, 0),
        MenuTarget::Background,
        vec![],
    ));
    assert!(layout_menu.accessibility_tree().iter().any(|node| {
        matches!(node.role(), musheen_ui::MenuAccessibleRole::Radio) && node.name().starts_with('⟦')
    }));
    let unchecked_hidden = layout_menu.entry("view.hidden").expect("toggle is present");
    assert!(!unchecked_hidden.state().is_checked());
    assert_eq!(
        unchecked_hidden.accessible_role(),
        musheen_ui::MenuAccessibleRole::Checkbox
    );
}

#[test]
fn shell_owns_the_context_menu_surface_used_by_the_application() {
    let shell = ShellModel::new(false);
    let menu = shell.context_menus().compose(request(
        supported_context(CommandTarget::Background, 0),
        MenuTarget::Background,
        vec![],
    ));
    assert_eq!(menu.presentation(), MenuPresentation::CompactNativeTheme);
    assert!(menu.entry("create.directory").is_some());
}

#[derive(Default)]
struct RecordingDispatcher {
    calls: Vec<(CommandAction, CommandParameters)>,
}

struct DestinationResolver {
    writable: bool,
}

impl ContextMenuDestinationResolver for DestinationResolver {
    fn resolve_context_menu_destination(
        &self,
        destination: &StorePath,
    ) -> musheen_core::ResolvedDestination {
        if self.writable {
            musheen_core::ResolvedDestination::writable(destination.clone())
        } else {
            musheen_core::ResolvedDestination::read_only(
                destination.clone(),
                "the destination is read-only",
            )
        }
    }
}

impl CommandDispatcher for RecordingDispatcher {
    fn dispatch(
        &mut self,
        action: CommandAction,
        parameters: CommandParameters,
    ) -> Result<(), CommandDispatchError> {
        self.calls.push((action, parameters));
        Ok(())
    }
}
