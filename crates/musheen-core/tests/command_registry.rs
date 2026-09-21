use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, CommandAction,
    CommandContext, CommandContributionPolicy, CommandDispatchError, CommandDispatcher,
    CommandParameterContract, CommandParameters, CommandRegistry, CommandSubmenu, CommandTarget,
    CommandTargetRef, DangerLevel, ItemId, ProviderAction, ProviderActionMatrix, ProviderId,
    ResolvedDestination, StorePath, TargetCardinality,
};
use std::collections::{HashMap, HashSet};

#[test]
fn custom_actions_require_a_bounded_stable_identity_before_dispatch() {
    let registry = CommandRegistry::built_in();
    let handler = registry.get("actions.custom").unwrap().handler();
    let mut dispatcher = RecordingDispatcher::default();
    for (id, definition) in [
        (None, Some("{}")),
        (Some("valid"), None),
        (Some("../invalid"), Some("{}")),
        (Some(""), Some("{}")),
        (Some("valid"), Some("")),
    ] {
        assert!(
            handler
                .invoke(
                    &mut dispatcher,
                    CommandParameters::CustomAction {
                        targets: vec![local_target()],
                        supports_provider_uris: false,
                        action_id: id.map(Into::into),
                        definition: definition.map(Into::into),
                        location: StorePath::from_unix_path("/tmp"),
                    }
                )
                .is_err()
        );
    }
    assert!(dispatcher.actions.is_empty());
}

#[test]
fn tab_commands_refuse_unavailable_navigation_transitions() {
    let registry = CommandRegistry::built_in();
    for id in [
        "tab.close",
        "tab.reopen_closed",
        "tab.move_left",
        "tab.move_right",
        "tab.new",
        "tab.duplicate",
        "tab.move_other_pane",
        "tab.tear_out",
        "pane.split",
        "pane.focus_next",
    ] {
        let state = registry.get(id).unwrap().state(&CommandContext::default());
        assert!(!state.is_enabled(), "{id} must require live tab facts");
        assert!(state.disabled_reason().is_some());
    }
    for (id, context) in [
        (
            "tab.close",
            CommandContext {
                can_close_tab: true,
                ..CommandContext::default()
            },
        ),
        (
            "tab.reopen_closed",
            CommandContext {
                can_reopen_closed_tab: true,
                ..CommandContext::default()
            },
        ),
        (
            "tab.move_left",
            CommandContext {
                can_move_tab_left: true,
                ..CommandContext::default()
            },
        ),
        (
            "tab.move_right",
            CommandContext {
                can_move_tab_right: true,
                ..CommandContext::default()
            },
        ),
    ] {
        for other in [
            "tab.close",
            "tab.reopen_closed",
            "tab.move_left",
            "tab.move_right",
        ] {
            let state = registry.get(other).unwrap().state(&context);
            assert_eq!(
                state.is_enabled(),
                id == other,
                "only {id} is possible, checking {other}"
            );
        }
    }
}

#[test]
fn open_with_dispatches_a_validated_application_identity_and_explicit_intent() {
    let registry = CommandRegistry::built_in();
    let target = local_target();
    let editor = musheen_core::DesktopApplicationId::new("org.example.Editor").unwrap();
    let viewer = musheen_core::DesktopApplicationId::new("org.example.Viewer").unwrap();
    let mut dispatcher = RecordingDispatcher::default();

    registry
        .get("file.open_with")
        .unwrap()
        .handler()
        .invoke(
            &mut dispatcher,
            CommandParameters::open_with(
                vec![target.clone()],
                editor.clone(),
                musheen_core::OpenWithIntent::OpenOnce,
            ),
        )
        .unwrap();
    registry
        .get("file.set_default_application")
        .unwrap()
        .handler()
        .invoke(
            &mut dispatcher,
            CommandParameters::open_with(
                vec![target],
                viewer.clone(),
                musheen_core::OpenWithIntent::SetAsDefault,
            ),
        )
        .unwrap();

    assert_ne!(editor, viewer);
    assert!(matches!(
        dispatcher.parameters[0],
        CommandParameters::OpenWith {
            intent: musheen_core::OpenWithIntent::OpenOnce,
            ..
        }
    ));
    assert!(matches!(
        dispatcher.parameters[1],
        CommandParameters::OpenWith {
            intent: musheen_core::OpenWithIntent::SetAsDefault,
            ..
        }
    ));
}

#[test]
fn application_commands_accept_one_or_more_selected_targets() {
    let registry = CommandRegistry::built_in();
    let context = CommandContext {
        selection_count: 2,
        target: CommandTarget::File,
        ..CommandContext::default()
    };

    for (id, contract) in [
        (
            "file.open",
            CommandParameterContract::Targets(TargetCardinality::OneOrMore),
        ),
        (
            "file.open_with",
            CommandParameterContract::OpenWith(TargetCardinality::OneOrMore),
        ),
        (
            "file.choose_application",
            CommandParameterContract::Targets(TargetCardinality::OneOrMore),
        ),
        (
            "file.set_default_application",
            CommandParameterContract::OpenWith(TargetCardinality::OneOrMore),
        ),
    ] {
        let command = registry.get(id).expect("application command is registered");
        assert_eq!(command.parameter_contract(), contract, "{id}");
        assert!(command.state(&context).is_enabled(), "{id}");
    }
}

#[test]
fn registry_contains_the_mutation_actions_that_context_menus_project() {
    let registry = CommandRegistry::built_in();

    for id in [
        "file.open",
        "clipboard.cut",
        "clipboard.copy",
        "clipboard.paste_into",
        "file.rename",
        "file.move_to_trash",
        "file.delete_permanently",
        "trash.empty",
    ] {
        assert!(registry.get(id).is_some(), "{id} must be registered");
    }
}

#[test]
fn tag_context_has_exact_registry_backed_rename_and_delete_actions() {
    let registry = CommandRegistry::built_in();
    let tag_context = CommandContext {
        selection_count: 1,
        target: CommandTarget::Tag,
        ..CommandContext::default()
    };

    for (id, action) in [
        ("tag.rename", CommandAction::RenameTag),
        ("tag.delete", CommandAction::DeleteTag),
    ] {
        let command = registry.get(id).expect("tag command is registered");
        assert_eq!(command.action(), action);
        assert!(command.state(&tag_context).is_enabled());
        assert!(!command.state(&CommandContext::default()).is_enabled());
        assert_eq!(
            action.parameter_contract(),
            CommandParameterContract::Targets(TargetCardinality::ExactlyOne)
        );
    }
}

#[test]
fn registry_audit_has_unique_ids_shortcuts_and_exactly_one_entry_per_action() {
    let registry = CommandRegistry::built_in();
    let audit = registry
        .audit()
        .expect("the built-in registry audits cleanly");
    let ids = registry
        .commands()
        .iter()
        .map(|command| command.id().as_str())
        .collect::<HashSet<_>>();
    let shortcuts = registry
        .commands()
        .iter()
        .flat_map(|command| command.shortcuts())
        .map(|shortcut| (shortcut.scope(), shortcut.chord().to_ascii_lowercase()))
        .collect::<HashSet<_>>();
    let actions = registry
        .commands()
        .iter()
        .fold(HashMap::new(), |mut counts, command| {
            *counts.entry(command.action()).or_insert(0) += 1;
            counts
        });

    assert_eq!(ids.len(), registry.commands().len());
    assert_eq!(
        shortcuts.len(),
        registry
            .commands()
            .iter()
            .map(|command| command.shortcuts().len())
            .sum()
    );
    assert_eq!(actions.len(), CommandAction::ALL.len());
    assert!(
        CommandAction::ALL
            .iter()
            .all(|action| actions.get(action) == Some(&1))
    );
    assert_eq!(
        audit
            .public_ids()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>(),
        registry
            .commands()
            .iter()
            .map(|command| command.id().as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn registry_audit_snapshots_stable_public_ids() {
    let registry = CommandRegistry::built_in();
    let audit = registry.audit().unwrap();
    let actual = audit
        .public_ids()
        .iter()
        .map(|id| id.as_str())
        .collect::<Vec<_>>();
    let expected = [
        "navigation.back",
        "navigation.forward",
        "navigation.parent",
        "navigation.refresh",
        "navigation.location",
        "view.search",
        "view.filter",
        "view.command",
        "view.details",
        "view.list",
        "view.cards",
        "view.grid",
        "view.columns",
        "view.adaptive",
        "view.sort",
        "view.group",
        "view.directories_first",
        "view.hidden",
        "view.sidebar",
        "view.info",
        "tab.new",
        "tab.close",
        "tab.duplicate",
        "tab.reopen_closed",
        "tab.move_other_pane",
        "tab.move_left",
        "tab.move_right",
        "tab.tear_out",
        "pane.split",
        "pane.focus_next",
        "selection.select_all",
        "selection.clear",
        "item.properties",
        "app.settings",
        "file.open",
        "file.open_with",
        "file.choose_application",
        "file.set_default_application",
        "clipboard.send_to",
        "clipboard.cut",
        "clipboard.copy",
        "clipboard.copy_to",
        "clipboard.move_to",
        "clipboard.paste_into",
        "file.rename",
        "file.duplicate",
        "file.create_symbolic_link",
        "file.create_hard_link",
        "file.compress",
        "archive.extract",
        "archive.extract_here",
        "file.hide",
        "file.unhide",
        "file.move_to_trash",
        "file.delete_permanently",
        "item.permissions",
        "directory.open_as_administrator",
        "file.run_as_administrator",
        "create.directory",
        "create.empty_file",
        "create.from_template",
        "directory.open_terminal",
        "directory.properties",
        "directory.open_new_tab",
        "directory.open_new_window",
        "directory.open_other_pane",
        "directory.pin",
        "directory.unpin",
        "item.copy_location",
        "item.tags",
        "tag.rename",
        "tag.delete",
        "directory.share",
        "file.preview",
        "archive.browse",
        "file.run",
        "mount.mount",
        "mount.unmount",
        "mount.eject",
        "mount.unlock",
        "mount.power_off",
        "trash.restore",
        "trash.empty",
        "actions.custom",
    ];

    assert_eq!(actual, expected);
}

#[test]
fn command_enablement_is_deterministic_and_explains_capability_refusal() {
    let registry = CommandRegistry::built_in();
    let reason = CapabilityReason::new("symbolic links are disabled by this provider").unwrap();
    let context = CommandContext {
        selection_count: 1,
        target: CommandTarget::File,
        location_is_writable: true,
        mutation_is_supported: true,
        capabilities: CapabilityMatrix::new(|kind| match kind {
            CapabilityKind::SymbolicLinks => CapabilityState::Unsupported(reason.clone()),
            _ => CapabilityState::Supported,
        }),
        ..CommandContext::default()
    };
    let command = registry.get("file.create_symbolic_link").unwrap();

    assert_eq!(command.state(&context), command.state(&context));
    assert!(!command.state(&context).is_enabled());
    assert_eq!(
        command.state(&context).disabled_reason(),
        Some("symbolic links are disabled by this provider")
    );
}

#[test]
fn tag_management_requires_the_provider_tags_capability() {
    let registry = CommandRegistry::built_in();
    let command = registry.get("item.tags").unwrap();
    let unsupported = CommandContext {
        selection_count: 1,
        target: CommandTarget::File,
        capabilities: CapabilityMatrix::new(|kind| {
            if kind == CapabilityKind::Tags {
                CapabilityState::Unsupported(
                    CapabilityReason::new("this provider does not support tags").unwrap(),
                )
            } else {
                CapabilityState::Supported
            }
        }),
        ..CommandContext::default()
    };

    assert!(!command.state(&unsupported).is_enabled());
    assert_eq!(
        command.state(&unsupported).disabled_reason(),
        Some("this provider does not support tags")
    );
    assert!(
        command
            .state(&CommandContext {
                capabilities: CapabilityMatrix::new(|_| CapabilityState::Supported),
                ..unsupported
            })
            .is_enabled()
    );
}

#[test]
fn selection_and_destination_rules_are_enforced_before_dispatch() {
    let registry = CommandRegistry::built_in();
    let mut context = CommandContext {
        target: CommandTarget::File,
        ..CommandContext::default()
    };
    let rename = registry.get("file.rename").unwrap();
    let send_to = registry.get("clipboard.send_to").unwrap();

    assert!(!rename.state(&context).is_enabled());
    context.selection_count = 2;
    assert!(!rename.state(&context).is_enabled());
    context.selection_count = 1;
    assert!(!rename.state(&context).is_enabled());
    assert_eq!(
        rename.state(&context).disabled_reason(),
        Some("the current location is read-only")
    );
    context.location_is_writable = true;
    context.mutation_is_supported = true;
    assert!(rename.state(&context).is_enabled());
    assert!(!send_to.state(&context).is_enabled());
    assert_eq!(
        send_to.state(&context).disabled_reason(),
        Some("choose a destination first")
    );
    context.resolved_destination = Some(writable_destination());
    assert!(send_to.state(&context).is_enabled());
}

#[test]
fn checked_and_dangerous_actions_have_explicit_metadata() {
    let registry = CommandRegistry::built_in();
    let context = CommandContext {
        show_hidden: true,
        ..CommandContext::default()
    };

    assert!(
        registry
            .get("view.hidden")
            .unwrap()
            .state(&context)
            .is_checked()
    );
    assert_eq!(
        registry
            .get("file.delete_permanently")
            .unwrap()
            .danger_level(),
        DangerLevel::Destructive
    );
    assert_eq!(
        registry.get("trash.empty").unwrap().danger_level(),
        DangerLevel::Destructive
    );
    assert_eq!(
        registry
            .get("directory.open_as_administrator")
            .unwrap()
            .danger_level(),
        DangerLevel::Review
    );
}

#[test]
fn target_specific_commands_refuse_inapplicable_local_and_remote_targets() {
    let registry = CommandRegistry::built_in();
    let directory = CommandContext {
        selection_count: 1,
        target: CommandTarget::Directory,
        is_local: true,
        ..CommandContext::default()
    };
    let executable = CommandContext {
        selection_count: 1,
        target: CommandTarget::ExecutableFile,
        is_local: true,
        ..CommandContext::default()
    };
    let remote_file = CommandContext {
        selection_count: 1,
        target: CommandTarget::File,
        is_local: false,
        ..CommandContext::default()
    };

    assert!(
        registry
            .get("directory.open_as_administrator")
            .unwrap()
            .state(&directory)
            .is_enabled()
    );
    assert!(
        !registry
            .get("file.run_as_administrator")
            .unwrap()
            .state(&directory)
            .is_enabled()
    );
    assert!(
        registry
            .get("file.run_as_administrator")
            .unwrap()
            .state(&executable)
            .is_enabled()
    );
    assert!(
        !registry
            .get("file.run_as_administrator")
            .unwrap()
            .state(&remote_file)
            .is_enabled()
    );
    assert!(
        !registry
            .get("actions.custom")
            .unwrap()
            .state(&remote_file)
            .is_enabled()
    );
}

#[test]
fn localization_and_icon_metadata_are_complete_for_every_command() {
    let registry = CommandRegistry::built_in();
    let english = include_str!("../../../locales/en-US.ftl");
    let pseudo = include_str!("../../../locales/en-XA.ftl");

    for command in registry.commands() {
        let localization_key = command.label_key().replace('.', "-");
        assert!(!command.icon_key().is_empty());
        assert!(
            english.contains(&format!("{localization_key} =")),
            "missing English {localization_key}"
        );
        assert!(
            pseudo.contains(&format!("{localization_key} =")),
            "missing pseudo-locale {localization_key}"
        );
    }
}

#[test]
fn provider_actions_expose_supported_unsupported_and_unknown_states() {
    let registry = CommandRegistry::built_in();
    let unsupported = CapabilityReason::new("the device does not support ejection").unwrap();
    let unknown = CapabilityReason::new("the provider did not report power state").unwrap();

    for (action, id, target) in [
        (
            ProviderAction::Share,
            "directory.share",
            CommandTarget::Directory,
        ),
        (ProviderAction::Mount, "mount.mount", CommandTarget::Mount),
        (
            ProviderAction::Unmount,
            "mount.unmount",
            CommandTarget::Mount,
        ),
        (ProviderAction::Eject, "mount.eject", CommandTarget::Mount),
        (ProviderAction::Unlock, "mount.unlock", CommandTarget::Mount),
        (
            ProviderAction::PowerOff,
            "mount.power_off",
            CommandTarget::Mount,
        ),
    ] {
        let supported = CommandContext {
            selection_count: 1,
            target,
            provider_actions: provider_actions(ProviderAction::Share, CapabilityState::Supported),
            ..CommandContext::default()
        };
        assert!(
            registry.get(id).unwrap().state(&supported).is_enabled(),
            "{id}"
        );

        let refused = CommandContext {
            provider_actions: provider_actions(
                action,
                CapabilityState::Unsupported(unsupported.clone()),
            ),
            ..supported.clone()
        };
        assert_eq!(
            registry.get(id).unwrap().state(&refused).disabled_reason(),
            Some(unsupported.as_str())
        );

        let unresolved = CommandContext {
            provider_actions: provider_actions(action, CapabilityState::Unknown(unknown.clone())),
            ..supported
        };
        assert_eq!(
            registry
                .get(id)
                .unwrap()
                .state(&unresolved)
                .disabled_reason(),
            Some(unknown.as_str())
        );
    }
}

fn provider_actions(action: ProviderAction, state: CapabilityState) -> ProviderActionMatrix {
    let mut states: [CapabilityState; 6] = std::array::from_fn(|_| CapabilityState::Supported);
    states[action as usize] = state;
    ProviderActionMatrix::from_volume_states(
        states[0].clone(),
        states[1].clone(),
        states[2].clone(),
        states[3].clone(),
        states[4].clone(),
        states[5].clone(),
    )
}

#[test]
fn mutation_commands_refuse_read_only_and_stale_selection_contexts() {
    let registry = CommandRegistry::built_in();
    let read_only = CommandContext {
        selection_count: 1,
        target: CommandTarget::File,
        ..CommandContext::default()
    };

    for id in [
        "file.rename",
        "file.duplicate",
        "file.delete_permanently",
        "file.create_symbolic_link",
        "file.create_hard_link",
        "file.compress",
        "file.hide",
        "file.unhide",
    ] {
        assert_eq!(
            registry
                .get(id)
                .unwrap()
                .state(&read_only)
                .disabled_reason(),
            Some("the current location is read-only"),
            "{id}"
        );
    }

    let stale_background = CommandContext {
        selection_count: 1,
        target: CommandTarget::Background,
        location_is_writable: true,
        mutation_is_supported: true,
        ..CommandContext::default()
    };
    assert!(
        !registry
            .get("clipboard.cut")
            .unwrap()
            .state(&stale_background)
            .is_enabled()
    );
    assert_eq!(
        registry
            .get("file.rename")
            .unwrap()
            .state(&stale_background)
            .disabled_reason(),
        Some("background commands do not use a selection")
    );
}

#[test]
fn target_and_saved_state_policies_are_mutually_exclusive() {
    let registry = CommandRegistry::built_in();
    let writable_file = CommandContext {
        selection_count: 1,
        target: CommandTarget::File,
        location_is_writable: true,
        mutation_is_supported: true,
        has_dot_name_semantics: true,
        ..CommandContext::default()
    };
    assert!(
        registry
            .get("file.hide")
            .unwrap()
            .state(&writable_file)
            .is_enabled()
    );
    assert!(
        !registry
            .get("file.unhide")
            .unwrap()
            .state(&writable_file)
            .is_enabled()
    );
    let hidden = CommandContext {
        target_is_hidden: true,
        ..writable_file
    };
    assert!(
        !registry
            .get("file.hide")
            .unwrap()
            .state(&hidden)
            .is_enabled()
    );
    assert!(
        registry
            .get("file.unhide")
            .unwrap()
            .state(&hidden)
            .is_enabled()
    );

    let directory = CommandContext {
        selection_count: 1,
        target: CommandTarget::Directory,
        ..CommandContext::default()
    };
    assert!(
        registry
            .get("directory.pin")
            .unwrap()
            .state(&directory)
            .is_enabled()
    );
    assert!(
        !registry
            .get("directory.unpin")
            .unwrap()
            .state(&directory)
            .is_enabled()
    );
    let pinned = CommandContext {
        target_is_pinned: true,
        ..directory
    };
    assert!(
        !registry
            .get("directory.pin")
            .unwrap()
            .state(&pinned)
            .is_enabled()
    );
    assert!(
        registry
            .get("directory.unpin")
            .unwrap()
            .state(&pinned)
            .is_enabled()
    );

    let executable = CommandContext {
        selection_count: 1,
        target: CommandTarget::ExecutableFile,
        is_local: true,
        ..CommandContext::default()
    };
    assert!(
        !registry
            .get("file.run")
            .unwrap()
            .state(&executable)
            .is_enabled()
    );
    assert!(
        registry
            .get("file.run")
            .unwrap()
            .state(&CommandContext {
                executable_run_enabled: true,
                ..executable
            })
            .is_enabled()
    );
}

#[test]
fn paste_and_mount_navigation_require_their_exact_targets() {
    let registry = CommandRegistry::built_in();
    let paste = CommandContext {
        selection_count: 1,
        target: CommandTarget::Directory,
        clipboard_has_contents: true,
        resolved_destination: Some(writable_destination()),
        mutation_is_supported: true,
        ..CommandContext::default()
    };
    assert!(
        registry
            .get("clipboard.paste_into")
            .unwrap()
            .state(&paste)
            .is_enabled()
    );
    assert!(
        !registry
            .get("clipboard.paste_into")
            .unwrap()
            .state(&CommandContext {
                target: CommandTarget::File,
                ..paste.clone()
            })
            .is_enabled()
    );
    assert!(
        !registry
            .get("clipboard.paste_into")
            .unwrap()
            .state(&CommandContext {
                clipboard_has_contents: false,
                ..paste
            })
            .is_enabled()
    );

    let mount = CommandContext {
        selection_count: 1,
        target: CommandTarget::Mount,
        ..CommandContext::default()
    };
    assert!(
        registry
            .get("file.open")
            .unwrap()
            .state(&mount)
            .is_enabled()
    );
    assert!(
        registry
            .get("directory.open_new_tab")
            .unwrap()
            .state(&mount)
            .is_enabled()
    );
    assert!(
        registry
            .get("directory.open_new_window")
            .unwrap()
            .state(&mount)
            .is_enabled()
    );
}

#[test]
fn registry_owns_variable_submenu_contributions() {
    let registry = CommandRegistry::built_in();
    for (id, submenu) in [
        ("file.open_with", CommandSubmenu::OpenWith),
        ("clipboard.send_to", CommandSubmenu::SendTo),
        ("item.tags", CommandSubmenu::Tags),
        ("actions.custom", CommandSubmenu::Actions),
    ] {
        let command = registry.get(id).unwrap();
        assert_eq!(command.submenu(), Some(submenu));
        assert_eq!(
            command.contribution_policy(),
            CommandContributionPolicy::Variable(submenu)
        );
    }
}

#[test]
fn handlers_reject_invalid_parameter_shapes_before_dispatch() {
    let registry = CommandRegistry::built_in();
    let mut dispatcher = RecordingDispatcher::default();
    let rename = registry.get("file.rename").unwrap().handler();

    assert!(
        rename
            .invoke(&mut dispatcher, CommandParameters::None)
            .is_err()
    );
    assert!(
        rename
            .invoke(&mut dispatcher, CommandParameters::targets(Vec::new()))
            .is_err()
    );
    assert!(dispatcher.actions.is_empty());
    assert!(
        rename
            .invoke(
                &mut dispatcher,
                CommandParameters::targets(vec![local_target(), local_target()]),
            )
            .is_err()
    );
    rename
        .invoke(
            &mut dispatcher,
            CommandParameters::targets(vec![local_target()]),
        )
        .unwrap();
    assert_eq!(dispatcher.actions, vec![CommandAction::Rename]);

    let paste = registry.get("clipboard.paste_into").unwrap().handler();
    assert!(
        paste
            .invoke(
                &mut dispatcher,
                CommandParameters::targets(vec![local_target()])
            )
            .is_err()
    );
    paste
        .invoke(
            &mut dispatcher,
            CommandParameters::Location(StorePath::from_unix_path("/tmp")),
        )
        .unwrap();
    assert_eq!(
        dispatcher.actions,
        vec![CommandAction::Rename, CommandAction::PasteInto]
    );

    let directory_properties = registry.get("directory.properties").unwrap().handler();
    directory_properties
        .invoke(
            &mut dispatcher,
            CommandParameters::Location(StorePath::from_unix_path("/tmp")),
        )
        .unwrap();
    assert!(
        directory_properties
            .invoke(
                &mut dispatcher,
                CommandParameters::targets(vec![local_target()])
            )
            .is_err()
    );

    let empty_trash = registry.get("trash.empty").unwrap().handler();
    empty_trash
        .invoke(
            &mut dispatcher,
            CommandParameters::Location(StorePath::from_unix_path("/trash")),
        )
        .unwrap();
    assert!(
        empty_trash
            .invoke(
                &mut dispatcher,
                CommandParameters::targets(vec![local_target()])
            )
            .is_err()
    );

    let extract_here = registry.get("archive.extract_here").unwrap().handler();
    extract_here
        .invoke(
            &mut dispatcher,
            CommandParameters::targets(vec![local_target()]),
        )
        .unwrap();
    assert!(
        extract_here
            .invoke(
                &mut dispatcher,
                CommandParameters::destination(
                    vec![local_target()],
                    StorePath::from_unix_path("/tmp")
                )
            )
            .is_err()
    );
    assert_eq!(
        dispatcher.actions,
        vec![
            CommandAction::Rename,
            CommandAction::PasteInto,
            CommandAction::DirectoryProperties,
            CommandAction::EmptyTrash,
            CommandAction::ExtractHere
        ]
    );

    let remote = ProviderId::new("remote").unwrap();
    let remote_id = ItemId::new(remote, b"entry".to_vec()).unwrap();
    assert!(CommandTargetRef::new(remote_id, StorePath::from_unix_path("/tmp")).is_err());
}

#[test]
fn destination_workflows_distinguish_chooser_requests_from_resolved_execution() {
    let registry = CommandRegistry::built_in();
    let mut dispatcher = RecordingDispatcher::default();

    for (id, action, targets, chooser_context) in [
        (
            "clipboard.copy_to",
            CommandAction::CopyTo,
            vec![local_target(), local_target()],
            CommandContext {
                selection_count: 2,
                target: CommandTarget::MultiSelection,
                resolved_destination: None,
                ..CommandContext::default()
            },
        ),
        (
            "clipboard.move_to",
            CommandAction::MoveTo,
            vec![local_target(), local_target()],
            CommandContext {
                selection_count: 2,
                target: CommandTarget::MultiSelection,
                location_is_writable: true,
                mutation_is_supported: true,
                resolved_destination: None,
                ..CommandContext::default()
            },
        ),
        (
            "archive.extract",
            CommandAction::Extract,
            vec![local_target()],
            CommandContext {
                selection_count: 1,
                target: CommandTarget::Archive,
                resolved_destination: None,
                ..CommandContext::default()
            },
        ),
    ] {
        let command = registry.get(id).unwrap();
        assert!(command.state(&chooser_context).is_enabled(), "{id}");
        let handler = command.handler();
        handler
            .invoke(
                &mut dispatcher,
                CommandParameters::destination_request(targets.clone()),
            )
            .unwrap();
        handler
            .invoke(
                &mut dispatcher,
                CommandParameters::destination(targets.clone(), StorePath::from_unix_path("/to")),
            )
            .unwrap();
        assert!(
            handler
                .invoke(&mut dispatcher, CommandParameters::targets(targets))
                .is_err()
        );
        assert_eq!(
            dispatcher.actions[dispatcher.actions.len() - 2..],
            [action, action]
        );
    }

    let extract = registry.get("archive.extract").unwrap().handler();
    assert!(
        extract
            .invoke(
                &mut dispatcher,
                CommandParameters::destination_request(vec![local_target(), local_target()]),
            )
            .is_err()
    );
}

#[test]
fn destination_and_source_policies_distinguish_choosing_from_executing() {
    let registry = CommandRegistry::built_in();
    let copy_to = registry.get("clipboard.copy_to").unwrap();
    let move_to = registry.get("clipboard.move_to").unwrap();
    let extract = registry.get("archive.extract").unwrap();
    let extract_here = registry.get("archive.extract_here").unwrap();

    let mut file = CommandContext {
        selection_count: 1,
        target: CommandTarget::File,
        location_is_writable: false,
        mutation_is_supported: true,
        resolved_destination: Some(writable_destination()),
        ..CommandContext::default()
    };
    let chooser = CommandContext {
        resolved_destination: None,
        ..file.clone()
    };
    assert!(copy_to.state(&chooser).is_enabled());
    assert!(!move_to.state(&chooser).is_enabled());
    assert!(copy_to.state(&file).is_enabled());
    assert!(!move_to.state(&file).is_enabled());
    file.location_is_writable = true;
    file.resolved_destination = Some(ResolvedDestination::read_only(
        StorePath::from_unix_path("/read-only"),
        "the destination is read-only",
    ));
    assert!(!copy_to.state(&file).is_enabled());
    assert!(!move_to.state(&file).is_enabled());

    let mut archive = CommandContext {
        selection_count: 1,
        target: CommandTarget::Archive,
        location_is_writable: false,
        mutation_is_supported: true,
        resolved_destination: Some(writable_destination()),
        ..CommandContext::default()
    };
    let extract_chooser = CommandContext {
        resolved_destination: None,
        ..archive.clone()
    };
    assert!(extract.state(&extract_chooser).is_enabled());
    assert!(extract.state(&archive).is_enabled());
    assert!(!extract_here.state(&archive).is_enabled());
    archive.location_is_writable = true;
    archive.resolved_destination = Some(ResolvedDestination::read_only(
        StorePath::from_unix_path("/read-only"),
        "the destination is read-only",
    ));
    assert!(!extract.state(&archive).is_enabled());
    assert!(extract_here.state(&archive).is_enabled());
}

#[test]
fn selection_targets_and_paste_destinations_reject_stale_or_inexact_contexts() {
    let registry = CommandRegistry::built_in();
    let copy = registry.get("clipboard.copy").unwrap();
    let open = registry.get("file.open").unwrap();
    let hard_link = registry.get("file.create_hard_link").unwrap();
    let paste = registry.get("clipboard.paste_into").unwrap();

    let stale_trash_background = CommandContext {
        selection_count: 1,
        target: CommandTarget::TrashBackground,
        ..CommandContext::default()
    };
    assert!(!copy.state(&stale_trash_background).is_enabled());
    assert!(!open.state(&stale_trash_background).is_enabled());

    let directory = CommandContext {
        selection_count: 1,
        target: CommandTarget::Directory,
        location_is_writable: true,
        mutation_is_supported: true,
        capabilities: CapabilityMatrix::new(|kind| match kind {
            CapabilityKind::HardLinks => CapabilityState::Supported,
            _ => CapabilityState::Unknown(CapabilityReason::new("not applicable").unwrap()),
        }),
        ..CommandContext::default()
    };
    assert!(!hard_link.state(&directory).is_enabled());
    assert!(
        hard_link
            .state(&CommandContext {
                target: CommandTarget::File,
                ..directory.clone()
            })
            .is_enabled()
    );
    assert!(
        !hard_link
            .state(&CommandContext {
                target: CommandTarget::File,
                capabilities: CapabilityMatrix::new(|_| {
                    CapabilityState::Unsupported(
                        CapabilityReason::new("hard links are unavailable").unwrap(),
                    )
                }),
                ..directory
            })
            .is_enabled()
    );

    let ready_to_paste = CommandContext {
        clipboard_has_contents: true,
        resolved_destination: Some(writable_destination()),
        mutation_is_supported: true,
        ..CommandContext::default()
    };
    assert!(paste.state(&ready_to_paste).is_enabled());
    let directory_without_item = CommandContext {
        target: CommandTarget::Directory,
        ..ready_to_paste.clone()
    };
    assert!(!paste.state(&directory_without_item).is_enabled());
    let background_with_stale_selection = CommandContext {
        selection_count: 1,
        ..ready_to_paste
    };
    assert!(!paste.state(&background_with_stale_selection).is_enabled());
}

#[test]
fn every_single_target_contract_rejects_multiple_targets_before_dispatch() {
    let registry = CommandRegistry::built_in();
    let mut dispatcher = RecordingDispatcher::default();

    for command in registry.commands() {
        let parameters = match command.parameter_contract() {
            CommandParameterContract::Targets(TargetCardinality::ExactlyOne) => {
                CommandParameters::targets(vec![local_target(), local_target()])
            }
            CommandParameterContract::Destination(TargetCardinality::ExactlyOne)
            | CommandParameterContract::DestinationWorkflow(TargetCardinality::ExactlyOne) => {
                CommandParameters::destination(
                    vec![local_target(), local_target()],
                    StorePath::from_unix_path("/destination"),
                )
            }
            _ => continue,
        };
        assert!(
            command
                .handler()
                .invoke(&mut dispatcher, parameters)
                .is_err(),
            "{} accepted multiple targets",
            command.id().as_str()
        );
    }
    assert!(dispatcher.actions.is_empty());
}

#[test]
fn mount_and_unlock_are_registry_owned_provider_actions() {
    let registry = CommandRegistry::built_in();
    let context = CommandContext {
        target: CommandTarget::Mount,
        selection_count: 1,
        provider_actions: ProviderActionMatrix::from_volume_states(
            CapabilityState::Unsupported(CapabilityReason::new("not shareable").unwrap()),
            CapabilityState::Supported,
            CapabilityState::Supported,
            CapabilityState::Supported,
            CapabilityState::Supported,
            CapabilityState::Supported,
        ),
        ..CommandContext::default()
    };
    for (id, action) in [
        ("mount.mount", CommandAction::Mount),
        ("mount.unlock", CommandAction::Unlock),
        ("mount.unmount", CommandAction::Unmount),
        ("mount.eject", CommandAction::Eject),
        ("mount.power_off", CommandAction::PowerOff),
    ] {
        let command = registry.get(id).expect("volume command is registered");
        assert_eq!(command.handler().action(), action);
        assert!(command.state(&context).is_enabled(), "{id}");
    }
}

#[derive(Default)]
struct RecordingDispatcher {
    actions: Vec<CommandAction>,
    parameters: Vec<CommandParameters>,
}

impl CommandDispatcher for RecordingDispatcher {
    fn dispatch(
        &mut self,
        action: CommandAction,
        parameters: CommandParameters,
    ) -> Result<(), CommandDispatchError> {
        self.actions.push(action);
        self.parameters.push(parameters);
        Ok(())
    }
}

fn local_target() -> CommandTargetRef {
    let local = ProviderId::new("local").unwrap();
    let id = ItemId::new(local, b"entry".to_vec()).unwrap();
    CommandTargetRef::new(id, StorePath::from_unix_path("/tmp/entry")).unwrap()
}

fn writable_destination() -> ResolvedDestination {
    ResolvedDestination::writable(StorePath::from_unix_path("/destination"))
}
