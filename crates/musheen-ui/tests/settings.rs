use gpui_kit::test::TestWindowExt;
use musheen_desktop::{SettingsDocument, SettingsPage, SettingsStore, settings_schema};
use musheen_ui::settings::{SettingsBackends, SettingsState};
use musheen_ui::{AppearanceMode, Catalog, Locale, ThemeProfile};
use std::fs;
use std::os::unix::fs::PermissionsExt;

#[test]
fn all_settings_have_one_searchable_localized_owner() {
    let mut keys = std::collections::HashSet::new();
    for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
        let catalog = Catalog::load(locale).unwrap();
        let state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
        for spec in settings_schema() {
            assert!(
                state
                    .search(&catalog.message(spec.label).unwrap(), &catalog)
                    .iter()
                    .any(|hit| hit.key == spec.key)
            );
            assert!(catalog.message(spec.page.label()).is_ok());
            assert!(catalog.message(spec.group).is_ok());
        }
    }
    for spec in settings_schema() {
        assert!(keys.insert(spec.key));
    }
}

#[test]
fn apply_is_transactional_cancel_and_reset_are_scoped() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(root.path());
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
    state.edit("appearance.mode", "dark").unwrap();
    state.edit("files.hidden", "true").unwrap();
    assert!(state.is_dirty());
    state.reset_page(SettingsPage::Appearance);
    assert_eq!(state.draft().value("appearance.mode").unwrap(), "system");
    assert_eq!(state.draft().value("files.hidden").unwrap(), "true");
    state.apply(&store).unwrap();
    assert!(!state.is_dirty());
    assert_eq!(store.load().unwrap().value("files.hidden").unwrap(), "true");
    state.edit("files.hidden", "false").unwrap();
    state.cancel();
    assert_eq!(state.draft().value("files.hidden").unwrap(), "true");
}

#[test]
fn invalid_drafts_never_change_committed_state_or_disk() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(root.path());
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
    state.edit("files.hidden", "true").unwrap();
    state.apply(&store).unwrap();
    let before = fs::read(store.path()).unwrap();
    for (key, invalid) in [
        ("directory_page_items", "0"),
        ("operation_data_mutations", "33"),
        ("files.hidden", "maybe"),
        ("appearance.mode", "purple"),
        ("remote.credential", "plaintext-password"),
    ] {
        assert!(state.edit(key, invalid).is_err());
        assert!(!state.errors().is_empty());
        assert!(state.apply(&store).is_err());
        assert_eq!(fs::read(store.path()).unwrap(), before);
        state.cancel();
    }
}

#[test]
fn appearance_preview_rolls_back_and_follows_native_theme() {
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
    for native in [
        AppearanceMode::Light,
        AppearanceMode::Dark,
        AppearanceMode::HighContrast,
    ] {
        let baseline = ThemeProfile::new(native, false);
        assert_eq!(state.appearance(baseline), baseline);
        state.edit("appearance.mode", "dark").unwrap();
        assert_eq!(
            state.appearance(baseline),
            ThemeProfile::new(AppearanceMode::Dark, false)
        );
        state.cancel();
        assert_eq!(state.appearance(baseline), baseline);
    }
}

#[test]
fn prior_schema_fixture_migrates_preserving_unknown_fields() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::at(root.path().join("settings.conf"));
    // v1 is the sole schema published before this change.
    fs::write(store.path(), include_str!("fixtures/settings-v1.conf")).unwrap();
    let document = store.load().unwrap();
    assert_eq!(document.schema_version(), 2);
    assert_eq!(document.resource_limits().directory_page_items, 128);
    assert_eq!(document.value("appearance.mode").unwrap(), "system");
    store.save(&document).unwrap();
    assert!(
        fs::read_to_string(store.path())
            .unwrap()
            .contains("future.option=opaque-value")
    );
}

#[test]
fn malformed_neighbors_recover_and_secret_references_are_validated() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(root.path());
    store.save(&SettingsDocument::default()).unwrap();
    fs::write(store.path(), "schema_version=2\nfiles.hidden=wrong\nappearance.mode=dark\nremote.credential=secret-service:account-1\n").unwrap();
    let loaded = store.load().unwrap();
    assert_eq!(loaded.value("files.hidden").unwrap(), "false");
    assert_eq!(loaded.value("appearance.mode").unwrap(), "dark");
    store.save(&loaded).unwrap();
    fs::write(store.path(), "corrupt").unwrap();
    assert_eq!(store.load().unwrap(), loaded);
    assert_eq!(
        fs::metadata(store.path()).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(store.path().parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn unavailable_backends_do_not_accept_edits_or_appear_in_search() {
    let catalog = Catalog::load(Locale::EnUs).unwrap();
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::default());
    for key in [
        "terminal.program",
        "remote.credential",
        "integrations.privilege",
    ] {
        assert!(state.edit(key, "true").is_err());
        assert!(!state.search("", &catalog).iter().any(|hit| hit.key == key));
    }
}

#[test]
fn search_navigation_focuses_owner_and_resource_controls_expose_contract() {
    let catalog = Catalog::load(Locale::EnUs).unwrap();
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
    let hit = state
        .search("concurrency", &catalog)
        .into_iter()
        .next()
        .unwrap();
    state.navigate_to(hit.key).unwrap();
    assert_eq!(state.page(), hit.page);
    assert_eq!(state.focused_key(), Some(hit.key));
    for spec in settings_schema()
        .iter()
        .filter(|spec| spec.key.starts_with("directory_") || spec.key.starts_with("operation_"))
    {
        assert!(spec.restart_required);
        assert!(spec.maximum().unwrap() >= spec.default.parse::<usize>().unwrap());
        assert!(!spec.units().unwrap().is_empty());
    }
}

#[test]
fn failed_save_keeps_draft_and_committed_values_for_retry() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("file");
    fs::write(&file, "file").unwrap();
    let store = SettingsStore::at(file.join("settings.conf"));
    let mut state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
    state.edit("files.hidden", "true").unwrap();
    assert!(state.apply(&store).is_err());
    assert!(state.is_dirty());
    state.cancel();
    assert_eq!(state.draft().value("files.hidden").unwrap(), "false");
}

#[test]
fn future_schema_refusal_preserves_both_files() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::at(root.path().join("settings.conf"));
    let future = "schema_version=999\nfuture=keep\n";
    fs::write(store.path(), future).unwrap();
    assert!(store.load().is_err());
    assert!(store.save(&SettingsDocument::default()).is_err());
    assert_eq!(fs::read_to_string(store.path()).unwrap(), future);
}

#[test]
fn reset_all_needs_confirmation_and_preserves_unknown_fields() {
    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::at(root.path().join("settings.conf"));
    fs::write(store.path(), "schema_version=2\nfuture.keep=opaque\n").unwrap();
    let mut state = SettingsState::new(store.load().unwrap(), SettingsBackends::all());
    state.edit("files.hidden", "true").unwrap();
    state.confirm_reset_all(true);
    assert_eq!(state.draft().value("files.hidden").unwrap(), "true");
    state.request_reset_all();
    state.confirm_reset_all(false);
    assert_eq!(state.draft().value("files.hidden").unwrap(), "true");
    state.request_reset_all();
    state.confirm_reset_all(true);
    state.apply(&store).unwrap();
    assert!(
        fs::read_to_string(store.path())
            .unwrap()
            .contains("future.keep=opaque")
    );
    for spec in settings_schema() {
        assert_eq!(
            state.draft().value(spec.key).unwrap(),
            spec.default,
            "{}",
            spec.key
        );
    }
}

#[gpui_kit::test]
async fn settings_gallery_keeps_controls_reachable_at_double_scale(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext, px, size};
    let root = tempfile::tempdir().unwrap();
    for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
        for (dark, contrast) in [(false, false), (true, false), (false, true)] {
            cx.update(|cx| {
                gpui_kit::init(cx);
                let preferences = native_theme::AccessibilityPreferences {
                    high_contrast: contrast,
                    ..Default::default()
                };
                let (theme, resolved) =
                    native_theme_gpui::from_preset("adwaita", dark, &preferences).unwrap();
                native_theme_gpui::apply(theme, &resolved, &preferences, cx);
            });
            let mut view = None;
            let handle = cx.open_window(size(px(720.), px(580.)), |window, cx| {
                let settings = cx.new(|cx| {
                    musheen_ui::settings::SettingsWindow::new(
                        SettingsStore::from_config_home(root.path()),
                        SettingsBackends::default(),
                        Catalog::load(locale).unwrap(),
                        window,
                        cx,
                    )
                });
                view = Some(settings.clone());
                Root::new(settings, window, cx)
            });
            let view = view.unwrap();
            cx.update_window(handle.into(), |_, window, cx| {
                window.set_scale_factor(2.0);
                for page in SettingsPage::ALL {
                    window.render_frame(cx);
                    window.click(page.label(), cx);
                    window.render_frame(cx);
                    assert_eq!(view.read(cx).state().page(), page);
                    let bounds = window.find("settings-window").bounds();
                    for id in ["settings-apply", "settings-cancel", "settings-reset-all"] {
                        let control = window.find(id);
                        assert!(control.visible());
                        assert!(
                            control.bounds().bottom_right().x <= bounds.bottom_right().x,
                            "{locale:?} {id}"
                        );
                        assert!(
                            control.bounds().bottom_right().y <= bounds.bottom_right().y,
                            "{locale:?} {id}"
                        );
                    }
                }
                window.remove_window();
            })
            .unwrap();
        }
    }
}
