use musheen_core::{CommandRegistry, ShortcutMap, ShortcutScope, ToolbarLayout};
use musheen_desktop::{SettingsDocument, SettingsPage, SettingsStore};
use musheen_ui::settings::{SettingsBackends, SettingsState};

#[test]
fn edits_near_the_import_size_limit_fail_without_mutating_the_draft() {
    let registry = CommandRegistry::built_in();
    let mut toolbar =
        ToolbarLayout::import(&format!("v1;navigation.location;{}", "a".repeat(65_510))).unwrap();
    let original = toolbar.clone();
    assert!(toolbar.add("navigation.refresh", &registry).is_err());
    assert_eq!(toolbar, original);
    let mut map =
        ShortcutMap::import(&format!("v1;browser:{}:ctrl-alt-x", "a".repeat(65_500))).unwrap();
    let original = map.clone();
    assert!(
        map.assign(
            "navigation.refresh",
            ShortcutScope::Browser,
            "ctrl-alt-r",
            &registry
        )
        .is_err()
    );
    assert_eq!(map, original);
}

#[test]
fn punctuation_shortcuts_round_trip_without_corrupting_the_document() {
    let registry = CommandRegistry::built_in();
    let mut shortcuts = ShortcutMap::default();
    shortcuts
        .assign(
            "navigation.refresh",
            ShortcutScope::Browser,
            "Ctrl+;",
            &registry,
        )
        .unwrap();
    assert_eq!(ShortcutMap::import(&shortcuts.export()).unwrap(), shortcuts);
}

#[test]
fn toolbar_edits_preserve_identity_and_a_navigation_escape() {
    let registry = CommandRegistry::built_in();
    let mut toolbar = ToolbarLayout::default();
    toolbar.add("navigation.refresh", &registry).unwrap();
    assert!(toolbar.add("navigation.refresh", &registry).is_err());
    toolbar.move_to("navigation.refresh", 0).unwrap();
    assert_eq!(toolbar.ids()[0].as_str(), "navigation.refresh");
    toolbar.remove("navigation.refresh").unwrap();
    assert!(toolbar.remove("navigation.location").is_err());
    assert!(toolbar.add("missing.command", &registry).is_err());
}

#[test]
fn shortcuts_detect_active_scope_collisions_and_reserved_combinations() {
    let registry = CommandRegistry::built_in();
    let mut map = ShortcutMap::default();
    map.assign(
        "navigation.refresh",
        ShortcutScope::Browser,
        "Ctrl+Alt+R",
        &registry,
    )
    .unwrap();
    assert!(
        map.assign(
            "navigation.back",
            ShortcutScope::Global,
            "alt+ctrl+r",
            &registry
        )
        .is_err()
    );
    map.assign(
        "navigation.back",
        ShortcutScope::Dialog,
        "Ctrl+Alt+R",
        &registry,
    )
    .unwrap();
    assert!(
        map.assign(
            "navigation.parent",
            ShortcutScope::Dialog,
            "CTRL+ALT+R",
            &registry
        )
        .is_err()
    );
    for chord in ["Alt+F4", "Alt+Tab", "Super+L", "Ctrl+Alt+Delete", "Escape"] {
        assert!(
            map.assign(
                "navigation.refresh",
                ShortcutScope::Browser,
                chord,
                &registry
            )
            .is_err(),
            "{chord}"
        );
    }
    assert_eq!(
        map.resolve("ctrl-alt-r", ShortcutScope::Browser, &registry)
            .unwrap()
            .as_str(),
        "navigation.refresh"
    );
    assert_eq!(
        map.resolve("ctrl-alt-r", ShortcutScope::Dialog, &registry)
            .unwrap()
            .as_str(),
        "navigation.back"
    );
}

#[test]
fn import_export_is_versioned_lossless_and_retains_orphans() {
    let layout = ToolbarLayout::import("v1;navigation.location;future.command").unwrap();
    assert_eq!(layout.export(), "v1;navigation.location;future.command");
    assert!(ToolbarLayout::import("v2;navigation.location").is_err());
    assert!(ToolbarLayout::import("v1;navigation.location;navigation.location").is_err());
    assert!(ToolbarLayout::import("v1;future.command").is_err());
    let map = ShortcutMap::import("v1;browser:future.command:ctrl-alt-y").unwrap();
    assert_eq!(map.export(), "v1;browser:future.command:ctrl-alt-y");
    assert!(
        map.resolve(
            "ctrl-alt-y",
            ShortcutScope::Browser,
            &CommandRegistry::built_in()
        )
        .is_none()
    );
}

#[test]
fn settings_preview_cancel_reset_and_atomic_recovery_share_one_transaction() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(root.path());
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::default());
    let original = state.toolbar();
    let mut layout = original.clone();
    layout
        .add("navigation.refresh", &CommandRegistry::built_in())
        .unwrap();
    state.set_toolbar(layout.clone()).unwrap();
    assert_eq!(state.toolbar(), layout);
    state.cancel();
    assert_eq!(state.toolbar(), original);
    state.set_toolbar(layout.clone()).unwrap();
    state.apply(&store).unwrap();
    state.reset_page(SettingsPage::Layout);
    assert_eq!(state.toolbar(), original);
    state.apply(&store).unwrap();
    std::fs::write(store.path(), "broken").unwrap();
    let recovered = SettingsState::new(store.load().unwrap(), SettingsBackends::default());
    assert_eq!(recovered.toolbar(), layout);
    assert!(store.path().starts_with(root.path().join("musheen")));
    assert!(
        std::fs::read_dir(store.path().parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".tmp."))
    );
}

#[test]
fn shortcut_removal_reset_and_invalid_import_preserve_the_committed_map() {
    let registry = CommandRegistry::built_in();
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::default());
    let mut shortcuts = state.shortcuts();
    shortcuts
        .assign(
            "navigation.refresh",
            ShortcutScope::Browser,
            "ctrl-alt-r",
            &registry,
        )
        .unwrap();
    assert!(
        shortcuts
            .resolve("f5", ShortcutScope::Browser, &registry)
            .is_none()
    );
    state.set_shortcuts(shortcuts).unwrap();
    assert!(state.edit("shortcuts.bindings", "v9").is_err());
    assert_eq!(
        state
            .shortcuts()
            .resolve("ctrl-alt-r", ShortcutScope::Browser, &registry)
            .unwrap()
            .as_str(),
        "navigation.refresh"
    );
    state.reset_page(SettingsPage::Shortcuts);
    assert_eq!(state.shortcuts(), ShortcutMap::default());
    let mut map = state.shortcuts();
    map.clear("navigation.refresh", ShortcutScope::Browser, &registry)
        .unwrap();
    assert!(
        map.resolve("f5", ShortcutScope::Browser, &registry)
            .is_none()
    );
    assert_eq!(ShortcutMap::import(&map.export()).unwrap(), map);
    assert!(ShortcutMap::import("v1;global:navigation.refresh:ctrl-c").is_err());
    assert!(ShortcutMap::import("v1;global:navigation.refresh:ctrl-;").is_err());
}

#[test]
fn customized_toolbar_projects_current_registry_policy_in_every_locale() {
    for locale in [
        musheen_ui::Locale::EnUs,
        musheen_ui::Locale::EnXa,
        musheen_ui::Locale::Ar,
    ] {
        let catalog = musheen_ui::Catalog::load(locale).unwrap();
        let registry = CommandRegistry::built_in();
        let layout = ToolbarLayout::default();
        for id in layout.ids() {
            let command = registry.get(id.as_str()).unwrap();
            assert!(!catalog.message(command.label_key()).unwrap().is_empty());
            assert!(!command.icon_key().is_empty());
            let projection = registry
                .project(id, musheen_core::CommandPresentation::Toolbar)
                .unwrap();
            assert_eq!(projection.command().handler(), command.handler());
            assert_eq!(
                projection
                    .command()
                    .state(&musheen_core::CommandContext::default()),
                command.state(&musheen_core::CommandContext::default())
            );
        }
    }
}
