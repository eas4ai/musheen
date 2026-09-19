use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, CommandAction,
    CommandContext, CommandRegistry, CommandTarget, DangerLevel,
};
use std::collections::{HashMap, HashSet};

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
        "directory.share",
        "file.preview",
        "archive.browse",
        "file.run",
        "mount.unmount",
        "mount.eject",
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
fn selection_and_destination_rules_are_enforced_before_dispatch() {
    let registry = CommandRegistry::built_in();
    let mut context = CommandContext::default();
    let rename = registry.get("file.rename").unwrap();
    let send_to = registry.get("clipboard.send_to").unwrap();

    assert!(!rename.state(&context).is_enabled());
    context.selection_count = 2;
    assert!(!rename.state(&context).is_enabled());
    context.selection_count = 1;
    assert!(rename.state(&context).is_enabled());
    assert!(!send_to.state(&context).is_enabled());
    assert_eq!(
        send_to.state(&context).disabled_reason(),
        Some("the destination is read-only")
    );
    context.destination_is_writable = true;
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
