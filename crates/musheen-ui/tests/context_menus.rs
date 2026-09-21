use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, CommandAction,
    CommandContext, CommandDispatchError, CommandDispatcher, CommandParameters, CommandRegistry,
    CommandTarget, CommandTargetRef, ItemId, OpenWithIntent, ProviderActionMatrix, ProviderId,
    ShortcutMap, StorePath, ToolbarLayout, canonical_chord,
};
use musheen_ui::toolbar::{
    FixedCommandSurface, OMNIBAR_COMMANDS, STATIC_SHORTCUTS, customizable_commands,
    fixed_surface_ids, project_custom_toolbar, resolve_command_mode,
};
use musheen_ui::{
    AppearanceMode, ContextMenu, ContextMenuDestinationResolver, ContextMenuRequest,
    ContextMenuSource, ContextMenuSurface, Locale, MenuContribution, MenuDirection, MenuEntryKind,
    MenuFocus, MenuInvocation, MenuKeyRoute, MenuPresentation, MenuTarget, OmnibarMode,
    OpenWithApplication, SendToDestination, ShellModel, ThemeProfile,
    installed_static_command_actions, installed_static_shortcut_bindings,
};
use std::collections::{BTreeMap, BTreeSet};

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
    let registry = CommandRegistry::built_in();
    let surface = ContextMenuSurface::new(registry.clone());
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

    let mutation_menu = surface.compose(request(
        CommandContext {
            mutation_is_supported: false,
            ..supported_context(CommandTarget::File, 1)
        },
        MenuTarget::Item,
        vec![target(b"policy", "/remote/policy.txt")],
    ));
    let mutation_link = mutation_menu
        .entry("file.create_symbolic_link")
        .expect("applicable unsupported mutation remains explainable");
    assert_eq!(
        mutation_link.state(),
        &mutation_link
            .registry_policy_state(&registry, Locale::EnUs)
            .expect("command row captures its registry context")
    );
    assert_eq!(
        mutation_link.state().disabled_reason(),
        Some("the provider does not support this mutation")
    );
    assert!(mutation_menu.entry("file.compress").is_none());
}

#[test]
fn applicable_backend_actions_remain_visible_with_their_refusal_reason() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let context = CommandContext {
        backend_actions: Some(vec![(
            CommandAction::Restore,
            CapabilityState::Unsupported(
                CapabilityReason::new("the trash backend is unavailable").unwrap(),
            ),
        )]),
        ..supported_context(CommandTarget::TrashItem, 1)
    };
    let menu = surface.compose(request(
        context,
        MenuTarget::TrashItem,
        vec![target(b"trash-disabled", "/trash/document")],
    ));
    let restore = menu.entry("trash.restore").expect("applicable restore row");
    assert!(!restore.state().is_enabled());
    assert!(restore.accessible_disabled_reason().is_some());
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
    let writable_send = send_to.destination("/archive").unwrap();
    let read_only_send = send_to.destination("/remote").unwrap();
    assert!(writable_send.copy_only());
    assert!(!read_only_send.state().is_enabled());
}

#[test]
fn send_to_rows_capture_exact_sources_destinations_and_policy() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let selected = target(b"send", "/work/send");
    let menu = surface.compose(
        request(
            supported_context(CommandTarget::File, 1),
            MenuTarget::Item,
            vec![selected.clone()],
        )
        .with_send_to(&[
            SendToDestination::pinned("Archive", path("/archive"), true),
            SendToDestination::remote("Read only", path("/remote"), false),
        ]),
    );
    let submenu = menu.entry("clipboard.send_to").unwrap().submenu().unwrap();
    let send_to_parent = menu.entry("clipboard.send_to").unwrap();
    let writable_send = submenu.destination("/archive").unwrap();
    let read_only_send = submenu.destination("/remote").unwrap();
    let writable_parameters = CommandParameters::Destination {
        targets: vec![selected.clone()],
        destination: path("/archive"),
    };
    assert_eq!(
        send_to_parent.generated_parameters().unwrap(),
        writable_parameters,
        "the Send To parent must carry the first writable destination and exact sources",
    );
    assert_eq!(
        writable_send.generated_parameters().unwrap(),
        writable_parameters
    );
    assert_eq!(
        read_only_send.generated_parameters().unwrap(),
        CommandParameters::Destination {
            targets: vec![selected.clone()],
            destination: path("/remote"),
        }
    );

    let mut dispatcher = RecordingDispatcher::default();
    assert!(
        surface
            .invoke(writable_send, &mut dispatcher)
            .is_dispatched()
    );
    assert_eq!(
        dispatcher.calls,
        vec![(CommandAction::SendTo, writable_parameters,)]
    );
    assert!(
        surface
            .invoke(read_only_send, &mut dispatcher)
            .is_rejected()
    );
    assert_eq!(dispatcher.calls.len(), 1);
}

#[test]
fn open_with_rows_capture_exact_sources_and_association_intents() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let selected = target(b"open-with", "/work/open-with");
    let menu = surface.compose(
        request(
            supported_context(CommandTarget::File, 1),
            MenuTarget::Item,
            vec![selected.clone()],
        )
        .with_open_with(&[OpenWithApplication::compatible(
            "Text Editor",
            "org.example.Text",
        )]),
    );
    let open_with = menu.entry("file.open_with").unwrap();
    let mut dispatcher = RecordingDispatcher::default();
    let open_once = open_with
        .submenu()
        .unwrap()
        .application("org.example.Text")
        .expect("compatible application row");
    assert_eq!(
        open_once.generated_parameters().unwrap(),
        CommandParameters::OpenWith {
            targets: vec![selected.clone()],
            application: musheen_core::DesktopApplicationId::new("org.example.Text").unwrap(),
            intent: OpenWithIntent::OpenOnce,
        }
    );
    assert!(surface.invoke(open_once, &mut dispatcher).is_dispatched());
    assert_eq!(
        dispatcher.calls[0].1,
        CommandParameters::OpenWith {
            targets: vec![selected.clone()],
            application: musheen_core::DesktopApplicationId::new("org.example.Text").unwrap(),
            intent: OpenWithIntent::OpenOnce,
        }
    );
    let set_default = open_with
        .submenu()
        .unwrap()
        .entry("file.set_default_application")
        .unwrap()
        .submenu()
        .unwrap()
        .application("org.example.Text")
        .expect("set-default application row");
    assert_eq!(
        set_default.generated_parameters().unwrap(),
        CommandParameters::OpenWith {
            targets: vec![selected.clone()],
            application: musheen_core::DesktopApplicationId::new("org.example.Text").unwrap(),
            intent: OpenWithIntent::SetAsDefault,
        }
    );
    let confirmation = surface.invoke(set_default, &mut dispatcher);
    assert!(confirmation.needs_confirmation());
    surface.confirm(confirmation, &mut dispatcher).unwrap();
    assert_eq!(
        dispatcher.calls[1].1,
        CommandParameters::OpenWith {
            targets: vec![selected],
            application: musheen_core::DesktopApplicationId::new("org.example.Text").unwrap(),
            intent: OpenWithIntent::SetAsDefault,
        }
    );
}

#[test]
fn catalog_tags_project_through_the_registry_and_dispatch_manage_tags() {
    let selected = target(b"tagged", "/work/tagged");
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let menu = surface.compose(
        request(
            supported_context(CommandTarget::File, 1),
            MenuTarget::Item,
            vec![selected],
        )
        .with_catalog_tag_names([Box::<str>::from("Important"), Box::<str>::from("Work")]),
    );

    let submenu = menu
        .entry("item.tags")
        .unwrap()
        .submenu()
        .expect("catalog tags use the registry Tags submenu");
    assert_eq!(
        submenu
            .entries()
            .iter()
            .map(|entry| entry.label())
            .collect::<Vec<_>>(),
        ["Important", "Work"]
    );
    let mut dispatcher = RecordingDispatcher::default();
    assert!(
        surface
            .invoke(&submenu.entries()[0], &mut dispatcher)
            .is_dispatched()
    );
    assert_eq!(dispatcher.calls[0].0, CommandAction::ManageTags);
    assert!(matches!(
        dispatcher.calls[0].1,
        CommandParameters::Targets(_)
    ));
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
fn rtl_locale_mirrors_directional_menu_chrome_but_not_path_direction() {
    let menu = ContextMenuSurface::new(CommandRegistry::built_in())
        .with_locale(Locale::Ar)
        .compose(request(
            supported_context(CommandTarget::Background, 0),
            MenuTarget::Background,
            vec![],
        ));
    assert_eq!(menu.locale_direction(), MenuDirection::RightToLeft);
    let chrome = menu.direction(menu.locale_direction());
    assert_eq!(chrome.submenu_arrow(), "←");
    assert_eq!(chrome.path_direction(), MenuDirection::LeftToRight);
}

#[test]
fn rtl_locale_propagates_into_nested_submenus() {
    let menu = ContextMenuSurface::new(CommandRegistry::built_in())
        .with_locale(Locale::Ar)
        .compose(
            request(
                supported_context(CommandTarget::File, 1),
                MenuTarget::Item,
                vec![target(b"rtl-submenu", "/work/document")],
            )
            .with_open_with(&[OpenWithApplication::compatible(
                "Editor",
                "org.example.Editor",
            )]),
        );
    let submenu = menu
        .entry("file.open_with")
        .expect("open-with row")
        .submenu()
        .expect("open-with submenu");
    assert_eq!(submenu.locale_direction(), MenuDirection::RightToLeft);
    assert_eq!(
        submenu
            .direction(submenu.locale_direction())
            .submenu_arrow(),
        "←"
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
        (MenuTarget::Tag, CommandTarget::Tag, "tag.rename"),
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
fn tag_context_captures_exact_identity_for_rename_and_delete() {
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    let captured = target(b"tag:work", "/synthetic/work");
    let menu = surface.compose(request(
        supported_context(CommandTarget::Tag, 1),
        MenuTarget::Tag,
        vec![captured.clone()],
    ));

    for id in ["tag.rename", "tag.delete"] {
        let entry = menu.entry(id).expect("tag action is projected");
        assert_eq!(entry.captured_targets(), std::slice::from_ref(&captured));
        assert!(entry.state().is_enabled());
    }
    assert!(menu.entry("item.tags").is_none());
    assert!(menu.entry("item.properties").is_none());
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
    assert!(
        layout_menu
            .entry("view.list")
            .expect("active layout is present")
            .state()
            .is_checked()
    );
    assert!(
        !layout_menu
            .entry("view.details")
            .expect("inactive layout is present")
            .state()
            .is_checked()
    );
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

fn surface_matrix_requests() -> Vec<(&'static str, ContextMenuRequest)> {
    let file = target(b"file", "/work/file.txt");
    let variable_rows = request(
        supported_context(CommandTarget::File, 1),
        MenuTarget::Item,
        vec![file.clone()],
    )
    .with_open_with(&[OpenWithApplication::compatible(
        "Editor",
        "org.example.Editor",
    )])
    .with_send_to(&[SendToDestination::pinned("Archive", path("/archive"), true)])
    .with_tags(&[MenuContribution::new("Work", "item.tags")])
    .with_actions(&[MenuContribution::new("Inspect", "actions.custom")]);
    let mut hidden = supported_context(CommandTarget::File, 1);
    hidden.target_is_hidden = true;
    let mut pinned = supported_context(CommandTarget::Directory, 1);
    pinned.target_is_pinned = true;
    let mut read_only = supported_context(CommandTarget::File, 1);
    read_only.location_is_writable = false;
    let unsupported_reason =
        CapabilityReason::new("the provider does not implement this operation").unwrap();
    let unsupported_state = CapabilityState::Unsupported(unsupported_reason.clone());
    let mut unsupported = supported_context(CommandTarget::File, 1);
    unsupported.mutation_is_supported = false;
    unsupported.capabilities =
        CapabilityMatrix::new(|_| CapabilityState::Unsupported(unsupported_reason.clone()));
    unsupported.provider_actions = ProviderActionMatrix::from_states(
        unsupported_state.clone(),
        unsupported_state.clone(),
        unsupported_state.clone(),
        unsupported_state,
    );

    vec![
        (
            "background",
            request(
                supported_context(CommandTarget::Background, 0),
                MenuTarget::Background,
                vec![],
            ),
        ),
        ("file", variable_rows),
        ("hidden-file", request(hidden, MenuTarget::Item, vec![file])),
        (
            "read-only-file",
            request(
                read_only,
                MenuTarget::Item,
                vec![target(b"read-only", "/work/read-only.txt")],
            ),
        ),
        (
            "unsupported-file",
            request(
                unsupported,
                MenuTarget::Item,
                vec![target(b"unsupported", "/remote/unsupported.txt")],
            ),
        ),
        (
            "archive",
            request(
                supported_context(CommandTarget::Archive, 1),
                MenuTarget::Item,
                vec![target(b"archive", "/work/archive.tar")],
            ),
        ),
        (
            "executable",
            request(
                supported_context(CommandTarget::ExecutableFile, 1),
                MenuTarget::Item,
                vec![target(b"executable", "/work/tool")],
            ),
        ),
        (
            "directory",
            request(
                supported_context(CommandTarget::Directory, 1),
                MenuTarget::Item,
                vec![target(b"directory", "/work/folder")],
            ),
        ),
        (
            "pinned-directory",
            request(
                pinned,
                MenuTarget::Item,
                vec![target(b"pinned", "/work/pinned")],
            ),
        ),
        (
            "sidebar",
            request(
                supported_context(CommandTarget::Sidebar, 1),
                MenuTarget::SidebarLocation,
                vec![target(b"sidebar", "/work/sidebar")],
            ),
        ),
        (
            "mount",
            request(
                supported_context(CommandTarget::Mount, 1),
                MenuTarget::Mount,
                vec![target(b"mount", "/media/device")],
            ),
        ),
        (
            "tag",
            request(
                supported_context(CommandTarget::Tag, 1),
                MenuTarget::Tag,
                vec![target(b"tag:work", "/synthetic/work")],
            ),
        ),
        (
            "trash-item",
            request(
                supported_context(CommandTarget::TrashItem, 1),
                MenuTarget::TrashItem,
                vec![target(b"trash", "/trash/file")],
            ),
        ),
        (
            "trash-background",
            request(
                supported_context(CommandTarget::TrashBackground, 0),
                MenuTarget::TrashBackground,
                vec![],
            ),
        ),
    ]
}

fn audit_matrix_menu(
    menu: &ContextMenu,
    registry: &CommandRegistry,
    surface_name: &'static str,
    contexts: &mut BTreeMap<String, BTreeSet<&'static str>>,
) {
    for entry in menu.entries() {
        match entry.kind() {
            MenuEntryKind::Separator => assert!(entry.command_id().is_none()),
            MenuEntryKind::Command => {
                assert!(
                    entry.command_id().is_some(),
                    "command row on {surface_name}"
                );
            }
            MenuEntryKind::Submenu | MenuEntryKind::Overflow => {}
        }
        if let Some(id) = entry.command_id() {
            let command = registry
                .get(id)
                .unwrap_or_else(|| panic!("{surface_name} bypasses the registry with {id}"));
            assert_eq!(entry.icon_key(), Some(command.icon_key()));
            let expected = entry
                .registry_policy_state(registry, Locale::EnUs)
                .unwrap_or_else(|| panic!("{surface_name}:{id} has no captured registry policy"));
            assert_eq!(
                entry.state(),
                &expected,
                "{surface_name}:{id} bypasses its registry policy"
            );
            contexts
                .entry(id.to_owned())
                .or_default()
                .insert(surface_name);
        } else {
            assert!(
                matches!(entry.kind(), MenuEntryKind::Separator) || entry.submenu().is_some(),
                "non-command row on {surface_name} must be structural"
            );
        }
        if let Some(submenu) = entry.submenu() {
            audit_matrix_menu(submenu, registry, surface_name, contexts);
        }
    }
}

#[derive(Default)]
struct RecursivePolicy<'a> {
    present: BTreeSet<&'a str>,
    disabled: BTreeSet<&'a str>,
}

fn recursive_policy(menu: &ContextMenu) -> RecursivePolicy<'_> {
    let mut policy = RecursivePolicy::default();
    for entry in menu.entries() {
        if let Some(id) = entry.command_id() {
            policy.present.insert(id);
            if !entry.state().is_enabled() {
                policy.disabled.insert(id);
            }
        }
        if let Some(submenu) = entry.submenu() {
            let nested = recursive_policy(submenu);
            policy.present.extend(nested.present);
            policy.disabled.extend(nested.disabled);
        }
    }
    policy
}

type ApplicabilityOracle = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

fn applicability_oracles() -> ([ApplicabilityOracle; 14], BTreeSet<&'static str>) {
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
    const FILE: &[&str] = &[
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
        "clipboard.send_to",
        "clipboard.cut",
        "clipboard.copy",
        "clipboard.copy_to",
        "clipboard.move_to",
        "file.preview",
        "file.compress",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.create_hard_link",
        "file.hide",
        "item.tags",
        "actions.custom",
        "file.move_to_trash",
        "file.delete_permanently",
        "item.properties",
        "item.permissions",
        "item.copy_location",
    ];
    const DIRECTORY: &[&str] = &[
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
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
        "file.compress",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.hide",
        "directory.pin",
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
    const ARCHIVE: &[&str] = &[
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
        "clipboard.send_to",
        "clipboard.cut",
        "clipboard.copy",
        "clipboard.copy_to",
        "clipboard.move_to",
        "file.preview",
        "archive.browse",
        "archive.extract",
        "archive.extract_here",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.create_hard_link",
        "file.hide",
        "item.tags",
        "actions.custom",
        "file.move_to_trash",
        "file.delete_permanently",
        "item.properties",
        "item.permissions",
        "item.copy_location",
    ];
    const UNSUPPORTED_FILE: &[&str] = &[
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
        "clipboard.send_to",
        "clipboard.cut",
        "clipboard.copy",
        "clipboard.copy_to",
        "clipboard.move_to",
        "file.preview",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.create_hard_link",
        "file.hide",
        "item.tags",
        "actions.custom",
        "file.move_to_trash",
        "file.delete_permanently",
        "item.properties",
        "item.permissions",
        "item.copy_location",
    ];
    const HIDDEN_FILE: &[&str] = &[
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
        "clipboard.send_to",
        "clipboard.cut",
        "clipboard.copy",
        "clipboard.copy_to",
        "clipboard.move_to",
        "file.preview",
        "file.compress",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.create_hard_link",
        "file.unhide",
        "item.tags",
        "actions.custom",
        "file.move_to_trash",
        "file.delete_permanently",
        "item.properties",
        "item.permissions",
        "item.copy_location",
    ];
    const EXECUTABLE: &[&str] = &[
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
        "clipboard.send_to",
        "clipboard.cut",
        "clipboard.copy",
        "clipboard.copy_to",
        "clipboard.move_to",
        "file.preview",
        "file.run",
        "file.run_as_administrator",
        "file.compress",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.create_hard_link",
        "file.hide",
        "item.tags",
        "actions.custom",
        "file.move_to_trash",
        "file.delete_permanently",
        "item.properties",
        "item.permissions",
        "item.copy_location",
    ];
    const PINNED_DIRECTORY: &[&str] = &[
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
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
        "file.compress",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.hide",
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
    const ALL_CONTEXT_COMMANDS: &[&str] = &[
        "actions.custom",
        "archive.browse",
        "archive.extract",
        "archive.extract_here",
        "clipboard.copy",
        "clipboard.copy_to",
        "clipboard.cut",
        "clipboard.move_to",
        "clipboard.paste_into",
        "clipboard.send_to",
        "create.directory",
        "create.empty_file",
        "create.from_template",
        "directory.open_as_administrator",
        "directory.open_new_tab",
        "directory.open_new_window",
        "directory.open_other_pane",
        "directory.open_terminal",
        "directory.pin",
        "directory.properties",
        "directory.share",
        "directory.unpin",
        "file.choose_application",
        "file.compress",
        "file.create_hard_link",
        "file.create_symbolic_link",
        "file.delete_permanently",
        "file.duplicate",
        "file.hide",
        "file.move_to_trash",
        "file.open",
        "file.open_with",
        "file.preview",
        "file.rename",
        "file.run",
        "file.run_as_administrator",
        "file.set_default_application",
        "file.unhide",
        "item.copy_location",
        "item.permissions",
        "item.properties",
        "item.tags",
        "mount.eject",
        "mount.power_off",
        "mount.unmount",
        "selection.select_all",
        "tag.delete",
        "tag.rename",
        "trash.empty",
        "trash.restore",
        "view.adaptive",
        "view.cards",
        "view.columns",
        "view.details",
        "view.directories_first",
        "view.grid",
        "view.group",
        "view.hidden",
        "view.list",
        "view.sort",
    ];
    const NONE: &[&str] = &[];
    const SEND_TO_DISABLED: &[&str] = &["clipboard.send_to"];
    const READ_ONLY_DISABLED: &[&str] = &[
        "clipboard.move_to",
        "clipboard.send_to",
        "file.create_hard_link",
        "file.create_symbolic_link",
        "file.delete_permanently",
        "file.duplicate",
        "file.hide",
        "file.move_to_trash",
        "file.rename",
    ];
    const UNSUPPORTED_DISABLED: &[&str] = &[
        "clipboard.move_to",
        "clipboard.send_to",
        "file.create_hard_link",
        "file.create_symbolic_link",
        "file.delete_permanently",
        "file.duplicate",
        "file.hide",
        "file.move_to_trash",
        "file.rename",
        "item.permissions",
        "item.tags",
    ];
    const DIRECTORY_DISABLED: &[&str] = &["clipboard.paste_into", "clipboard.send_to"];

    let expected = [
        ("background", BACKGROUND, &["clipboard.paste_into"][..]),
        ("file", FILE, NONE),
        ("hidden-file", HIDDEN_FILE, SEND_TO_DISABLED),
        ("read-only-file", UNSUPPORTED_FILE, READ_ONLY_DISABLED),
        ("unsupported-file", UNSUPPORTED_FILE, UNSUPPORTED_DISABLED),
        ("archive", ARCHIVE, SEND_TO_DISABLED),
        ("executable", EXECUTABLE, SEND_TO_DISABLED),
        ("directory", DIRECTORY, DIRECTORY_DISABLED),
        ("pinned-directory", PINNED_DIRECTORY, DIRECTORY_DISABLED),
        ("sidebar", SIDEBAR, NONE),
        ("mount", MOUNT, NONE),
        ("tag", TAG, NONE),
        ("trash-item", TRASH_ITEM, NONE),
        ("trash-background", TRASH_BACKGROUND, NONE),
    ];
    let universe = ALL_CONTEXT_COMMANDS
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    (expected, universe)
}

#[test]
fn independent_target_applicability_matches_recursive_menu_composition() {
    let (expected, universe) = applicability_oracles();
    let requests = surface_matrix_requests();
    assert_eq!(expected.len(), requests.len());
    let surface = ContextMenuSurface::new(CommandRegistry::built_in());
    for ((actual_name, request), (expected_name, present, disabled)) in
        requests.into_iter().zip(expected)
    {
        assert_eq!(
            actual_name, expected_name,
            "canonical context order drifted"
        );
        let menu = surface.compose(request);
        let actual = recursive_policy(&menu);
        let actual_present = actual.present;
        let expected_present = present.iter().copied().collect::<BTreeSet<_>>();
        let expected_disabled = disabled.iter().copied().collect::<BTreeSet<_>>();
        let actual_omitted = universe
            .difference(&actual_present)
            .copied()
            .collect::<BTreeSet<_>>();
        let expected_omitted = universe
            .difference(&expected_present)
            .copied()
            .collect::<BTreeSet<_>>();

        assert_eq!(
            actual_present, expected_present,
            "{actual_name} present set"
        );
        assert_eq!(
            actual_omitted, expected_omitted,
            "{actual_name} omitted set"
        );
        assert_eq!(
            actual.disabled, expected_disabled,
            "{actual_name} disabled set"
        );
    }
}

fn add_surface_ids(
    surfaces: &mut BTreeMap<String, BTreeSet<&'static str>>,
    surface: &'static str,
    ids: impl IntoIterator<Item = impl AsRef<str>>,
) {
    for id in ids {
        surfaces
            .entry(id.as_ref().to_owned())
            .or_default()
            .insert(surface);
    }
}

fn live_surface_inventory(registry: &CommandRegistry) -> BTreeMap<String, BTreeSet<&'static str>> {
    let mut surfaces = BTreeMap::new();
    for surface in [
        FixedCommandSurface::NavigationToolbar,
        FixedCommandSurface::WideViewControls,
        FixedCommandSurface::CompactOverflow,
        FixedCommandSurface::TabStrip,
        FixedCommandSurface::StaticShortcut,
    ] {
        add_surface_ids(&mut surfaces, surface.id(), fixed_surface_ids(surface));
    }

    let default_layout = ToolbarLayout::default();
    let default_projection = project_custom_toolbar(&default_layout);
    add_surface_ids(
        &mut surfaces,
        "custom-visible",
        default_projection.visible().iter().map(|id| id.as_str()),
    );
    assert!(default_projection.overflow().is_empty());

    let mut overflow_layout = default_layout;
    overflow_layout
        .add("navigation.refresh", registry)
        .expect("eighth custom command");
    overflow_layout
        .add("tab.new", registry)
        .expect("ninth custom command");
    let overflow_projection = project_custom_toolbar(&overflow_layout);
    add_surface_ids(
        &mut surfaces,
        "custom-overflow",
        overflow_projection.overflow().iter().map(|id| id.as_str()),
    );
    add_surface_ids(
        &mut surfaces,
        "customizable",
        customizable_commands(registry).map(|command| command.id().as_str()),
    );
    add_surface_ids(
        &mut surfaces,
        "shortcut-default",
        ShortcutMap::default()
            .bindings(registry)
            .iter()
            .map(|binding| binding.command.as_str()),
    );
    for command in registry.commands() {
        let resolved = resolve_command_mode(registry, command.id().as_str())
            .expect("command mode resolves every registered command ID");
        assert!(std::ptr::eq(resolved, command));
        add_surface_ids(&mut surfaces, "command-mode", [command.id().as_str()]);
    }
    surfaces
}

fn render_surface_matrix(
    registry: &CommandRegistry,
    contexts: &BTreeMap<String, BTreeSet<&'static str>>,
    surfaces: &BTreeMap<String, BTreeSet<&'static str>>,
) -> String {
    let mut output = String::from(
        "# Command Surface Matrix\n\n\
         Generated by `cargo test -p musheen-ui --test context_menus`. Each row names the one registry predicate used by every live projection. `Context` lists canonical menu compositions, including read-only and unsupported providers.\n\n\
         Surface tokens cover the **Navigation toolbar**, wide controls, **Compact overflow**, **Tab strip**, default **Custom visible** commands, a deterministic **Custom overflow** layout, all customizable commands, each **Static shortcut**, default configurable shortcuts, and **Command mode**.\n\n\
         | Command ID | Capability policy | Context | Live surfaces |\n\
         |---|---|---|---|\n",
    );
    for command in registry.commands() {
        let context = contexts
            .get(command.id().as_str())
            .map(|values| values.iter().copied().collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| "—".to_owned());
        let live_surfaces = surfaces
            .get(command.id().as_str())
            .map(|values| values.iter().copied().collect::<Vec<_>>().join(", "))
            .unwrap_or_else(|| "—".to_owned());
        output.push_str(&format!(
            "| `{}` | `{:?}` | {} | {} |\n",
            command.id().as_str(),
            command.predicate(),
            context,
            live_surfaces,
        ));
    }
    output
}

#[test]
fn every_command_surface_uses_one_registry_definition_and_policy() {
    let registry = CommandRegistry::built_in();
    registry.audit().expect("built-in registry is unique");
    let surface = ContextMenuSurface::new(registry.clone());
    let mut contexts = BTreeMap::new();
    for (name, request) in surface_matrix_requests() {
        audit_matrix_menu(&surface.compose(request), &registry, name, &mut contexts);
    }
    let surfaces = live_surface_inventory(&registry);
    for (id, memberships) in &surfaces {
        assert!(
            registry.get(id).is_some(),
            "live surfaces {memberships:?} reference unregistered command {id}"
        );
    }
    assert_eq!(
        ShellModel::new(false).toolbar_command_ids(),
        fixed_surface_ids(FixedCommandSurface::NavigationToolbar)
    );
    for binding in STATIC_SHORTCUTS {
        let command = registry
            .get(binding.command_id())
            .expect("static shortcut command is registered");
        assert_eq!(
            command.action(),
            binding.action(),
            "{} handler action drifted from the registry",
            binding.command_id()
        );
        let chord = canonical_chord(binding.chord()).expect("static chord is canonicalizable");
        assert!(
            command.shortcuts().iter().any(|shortcut| {
                canonical_chord(shortcut.chord()).as_deref() == Ok(chord.as_str())
            }),
            "{} installed chord {} drifted from its registry definition",
            binding.command_id(),
            binding.chord()
        );
    }

    let expected = render_surface_matrix(&registry, &contexts, &surfaces);
    let actual =
        std::fs::read_to_string("../../docs/command-surface-matrix.md").unwrap_or_else(|error| {
            panic!("the checked-in command-surface matrix exists: {error}\n\n{expected}")
        });
    assert_eq!(actual, expected, "regenerate the command-surface matrix");
}

#[test]
fn omnibar_modes_and_static_handlers_are_complete_live_surface_inventories() {
    assert_eq!(
        OMNIBAR_COMMANDS
            .iter()
            .map(|command| (command.command_id(), command.mode()))
            .collect::<Vec<_>>(),
        vec![
            ("navigation.location", OmnibarMode::Path),
            ("view.search", OmnibarMode::Search),
            ("view.filter", OmnibarMode::Filter),
            ("view.command", OmnibarMode::Command),
        ]
    );
    let navigation = fixed_surface_ids(FixedCommandSurface::NavigationToolbar);
    for command in OMNIBAR_COMMANDS {
        assert!(navigation.contains(&command.command_id()));
    }

    let declared = STATIC_SHORTCUTS
        .iter()
        .map(|shortcut| shortcut.action())
        .collect::<Vec<_>>();
    let installed = installed_static_command_actions().to_vec();
    assert_eq!(
        installed, declared,
        "every static shortcut is installed and handled"
    );
    assert_eq!(installed.len(), STATIC_SHORTCUTS.len());
    assert_eq!(
        installed_static_shortcut_bindings(),
        STATIC_SHORTCUTS
            .iter()
            .map(|shortcut| (shortcut.command_id(), shortcut.chord(), shortcut.action()))
            .collect::<Vec<_>>(),
        "installed chord, command ID, and action coverage must share one declaration",
    );
}

#[test]
fn command_surface_matrix_names_every_live_shell_projection() {
    let matrix = std::fs::read_to_string("../../docs/command-surface-matrix.md").unwrap();
    for heading in [
        "Navigation toolbar",
        "Compact overflow",
        "Tab strip",
        "Custom visible",
        "Custom overflow",
        "Static shortcut",
        "Command mode",
    ] {
        assert!(matrix.contains(heading), "matrix is missing {heading}");
    }
    for id in ShellModel::new(false).toolbar_command_ids() {
        let row = matrix
            .lines()
            .find(|line| line.starts_with(&format!("| `{id}` |")))
            .unwrap_or_else(|| panic!("matrix is missing live toolbar command {id}"));
        assert!(
            row.contains("navigation-toolbar"),
            "matrix omits live toolbar command {id}: {row}"
        );
    }
    for (id, surface) in [
        ("tab.new", "tab-strip"),
        ("view.details", "compact-overflow"),
        ("tab.close", "static-shortcut"),
        ("app.settings", "custom-visible"),
        ("view.command", "command-mode"),
    ] {
        let row = matrix
            .lines()
            .find(|line| line.starts_with(&format!("| `{id}` |")))
            .unwrap_or_else(|| panic!("matrix is missing {id}"));
        assert!(
            row.contains(surface),
            "matrix omits {id} from {surface}: {row}"
        );
    }
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
