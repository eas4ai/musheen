use gpui_kit::test::TestWindowExt;
use musheen_core::{BoxFuture, CancellationToken, ItemId, ProviderId, StorePath};
use musheen_desktop::{
    CatalogDocument, CatalogStore, ConnectionProbe, ConnectionProfile, ConnectionProfiles,
    FolderIdentity, ProfileConnectionTest, ProfileConnectionTester, RemoteError,
    RemoteErrorCategory, RemoteProtocol, SecurityPolicy, SettingsDocument, SettingsPage,
    SettingsStore, TLS_PIN_BYTES, TlsPolicy, settings_schema,
};
use musheen_ui::settings::{SettingsBackends, SettingsState, clear_recent_locations};
use musheen_ui::{AppearanceMode, Catalog, Locale, ThemeProfile};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::{Arc, Mutex};

struct FailingConnectionTester;

impl ProfileConnectionTest for FailingConnectionTester {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            Err(RemoteError::new(
                profile.protocol(),
                RemoteErrorCategory::Network,
                Some(profile.host().clone()),
            ))
        })
    }
}

#[derive(Clone, Default)]
struct RecordingConnectionProbe {
    calls: Arc<Mutex<Vec<(String, u16)>>>,
}

impl ConnectionProbe for RecordingConnectionProbe {
    fn connect<'a>(
        &'a self,
        profile: &'a musheen_desktop::ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteErrorCategory>> {
        let port = profile.port().unwrap_or_else(|| match profile.protocol() {
            RemoteProtocol::Ftp => 21,
            RemoteProtocol::Ftps => 990,
            RemoteProtocol::Sftp => 22,
            RemoteProtocol::WebDav | RemoteProtocol::Http => {
                if matches!(profile.security(), SecurityPolicy::PlaintextConfirmed) {
                    80
                } else {
                    443
                }
            }
            RemoteProtocol::Smb => 445,
            RemoteProtocol::Nfs => 2049,
        });
        self.calls
            .lock()
            .unwrap()
            .push((profile.host().as_str().to_owned(), port));
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn general_settings_clear_only_recent_location_history() {
    let root = tempfile::tempdir().unwrap();
    let store = CatalogStore::at(root.path().join("catalog.json"));
    let provider = ProviderId::new("local").unwrap();
    let recent = FolderIdentity::new(provider.clone(), b"recent".to_vec()).unwrap();
    let pinned = ItemId::new(provider, b"pinned".to_vec()).unwrap();
    let mut catalog = CatalogDocument::default();
    catalog
        .recents_mut()
        .record(recent, StorePath::from_unix_path("/recent"), "Recent");
    catalog
        .pins_mut()
        .pin(pinned, StorePath::from_unix_path("/pinned"), "Pinned")
        .unwrap();
    store.save(&catalog).unwrap();

    clear_recent_locations(&store).unwrap();

    let catalog = store.load().unwrap();
    assert!(catalog.recents().entries().is_empty());
    assert_eq!(catalog.pins().entries().len(), 1);
}

#[test]
fn tag_xattrs_are_an_explicit_catalog_opt_in() {
    let setting = settings_schema()
        .iter()
        .find(|setting| setting.key == "general.store_tags_in_files")
        .expect("tag metadata opt-in is exposed in General settings");
    assert_eq!(setting.default, "false");
    assert_eq!(setting.feature, musheen_desktop::SettingsFeature::Catalog);
}

#[test]
fn all_settings_have_one_searchable_localized_owner() {
    let mut keys = std::collections::HashSet::new();
    for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
        let catalog = Catalog::load(locale).unwrap();
        let state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
        for spec in settings_schema() {
            assert!(
                state
                    .search(catalog.message(spec.label).unwrap(), &catalog)
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
    // The original resource-limit schema remains supported.
    fs::write(store.path(), include_str!("fixtures/settings-v1.conf")).unwrap();
    let document = store.load().unwrap();
    assert_eq!(
        document.schema_version(),
        musheen_desktop::SETTINGS_SCHEMA_VERSION
    );
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
fn malformed_neighbors_recover() {
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
        "remote.connections",
        "integrations.privilege",
    ] {
        assert!(state.edit(key, "true").is_err());
        assert!(!state.search("", &catalog).iter().any(|hit| hit.key == key));
    }
    for page in SettingsPage::ALL {
        state.select_page(page);
        for spec in state.page_controls() {
            assert!(
                state.available(spec),
                "unavailable control {} rendered",
                spec.key
            );
            assert!(
                state
                    .search("", &catalog)
                    .iter()
                    .any(|hit| hit.key == spec.key)
            );
        }
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
fn search_for_a_password_finds_the_connections() {
    let catalog = Catalog::load(Locale::EnUs).unwrap();
    let state = SettingsState::new(SettingsDocument::default(), SettingsBackends::all());
    for query in ["password", "credential", "ssh key"] {
        assert!(
            state
                .search(query, &catalog)
                .iter()
                .any(|hit| hit.key == "remote.connections"),
            "a search for {query:?} leads to the connections, where passwords are set"
        );
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

#[gpui_kit::test]
async fn remote_connection_editor_tests_before_save_and_confirms_failed_tests(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext, Role, px, size};
    use std::sync::Arc;

    let root = tempfile::tempdir().unwrap();
    cx.update(gpui_kit::init);
    let mut view = None;
    let handle = cx.open_window(size(px(760.), px(900.)), |window, cx| {
        let settings = cx.new(|cx| {
            musheen_ui::settings::SettingsWindow::new_with_connection_tester(
                SettingsStore::from_config_home(root.path()),
                SettingsBackends::all(),
                Catalog::load(Locale::EnUs).unwrap(),
                Arc::new(FailingConnectionTester),
                window,
                cx,
            )
        });
        view = Some(settings.clone());
        Root::new(settings, window, cx)
    });
    let view = view.unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-page-integrations", cx);
        window.render_frame(cx);
        window.click("settings-search", cx);
        window.input("remote.connections", cx);
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("result-remote.connections", cx);
    })
    .unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-add", cx);
        window.render_frame(cx);
        for (id, value) in [
            ("remote-profile-id", "work-sftp"),
            ("remote-profile-name", "Work files"),
            ("remote-profile-host", "files.example.test"),
            ("remote-profile-path", "/home/alice"),
            ("remote-profile-username", "alice"),
        ] {
            window.click(id, cx);
            window.input(value, cx);
        }
        assert_eq!(
            window.find("settings-remote-test").role(),
            Some(Role::Button)
        );
        assert_eq!(
            window.find("settings-remote-save").role(),
            Some(Role::Button)
        );
        window.click("settings-remote-test", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            window.find("settings-remote-test-failed").role(),
            Some(Role::Alert)
        );
        window.click("settings-remote-save", cx);
        window.render_frame(cx);
        assert_eq!(
            window.find("settings-remote-confirm-save").role(),
            Some(Role::Button)
        );
        window.click("settings-remote-confirm-save", cx);
    })
    .unwrap();

    let encoded = cx.update(|cx| {
        view.read(cx)
            .state()
            .draft()
            .value("remote.connections")
            .unwrap()
    });
    let profiles = ConnectionProfiles::import(&encoded).unwrap();
    assert_eq!(profiles.profiles().len(), 1);
    assert_eq!(profiles.profiles()[0].id().as_str(), "work-sftp");
    assert_eq!(profiles.profiles()[0].credential(), None);
}

#[gpui_kit::test]
async fn remote_editor_configures_and_preserves_pinned_security(cx: &mut gpui_kit::TestAppContext) {
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext, ScrollDelta, point, px, size};

    let root = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(root.path());
    cx.update(gpui_kit::init);
    let probe = RecordingConnectionProbe::default();
    let mut view = None;
    let handle = cx.open_window(size(px(900.), px(1400.)), |window, cx| {
        let settings = cx.new(|cx| {
            musheen_ui::settings::SettingsWindow::new_with_connection_tester(
                store.clone(),
                SettingsBackends::all(),
                Catalog::load(Locale::EnUs).unwrap(),
                Arc::new(ProfileConnectionTester::new(probe.clone())),
                window,
                cx,
            )
        });
        view = Some(settings.clone());
        Root::new(settings, window, cx)
    });
    let view = view.unwrap();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-page-integrations", cx);
        window.render_frame(cx);
        window.click("settings-remote-add", cx);
        window.render_frame(cx);
        for (id, value) in [
            ("remote-profile-id", "secure-webdav"),
            ("remote-profile-name", "Secure WebDAV"),
            ("remote-profile-host", "dav.example.test"),
            ("remote-profile-path", "files "),
            ("remote-profile-username", "dav-user "),
        ] {
            window.click(id, cx);
            window.input(value, cx);
        }
        window.click("settings-remote-protocol-webdav", cx);
        window.render_frame(cx);
        window.scroll("settings-controls", ScrollDelta::Lines(point(0., -40.)), cx);
        window.render_frame(cx);
        window.click("settings-remote-security-tls-pinned", cx);
        window.render_frame(cx);
        window.click("remote-security-pin", cx);
        window.input(&"5a".repeat(TLS_PIN_BYTES), cx);
        assert!(
            window.try_find("settings-remote-proxy-socks5").is_none(),
            "the editor offers no proxy, which browsing refuses"
        );
        window.scroll("settings-controls", ScrollDelta::Lines(point(0., 40.)), cx);
        window.render_frame(cx);
        window.click("settings-remote-test", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-save", cx);
    })
    .unwrap();

    let encoded = cx.update(|cx| {
        view.read(cx)
            .state()
            .draft()
            .value("remote.connections")
            .unwrap()
    });
    let profiles = ConnectionProfiles::import(&encoded).unwrap();
    let profile = &profiles.profiles()[0];
    assert_eq!(
        profile.security(),
        &SecurityPolicy::Tls(TlsPolicy::PinnedSha256([0x5a; TLS_PIN_BYTES]))
    );
    assert!(profile.proxy().is_none());
    assert_eq!(profile.path(), "/files ");
    assert_eq!(profile.username(), Some("dav-user "));
    assert_eq!(
        &*probe.calls.lock().unwrap(),
        &[("dav.example.test".to_owned(), 443)]
    );

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("remote-profile-username", cx);
        window.input("changed-user ", cx);
        window.render_frame(cx);
        window.click("settings-remote-save", cx);
    })
    .unwrap();
    let unchanged = cx.update(|cx| {
        ConnectionProfiles::import(
            &view
                .read(cx)
                .state()
                .draft()
                .value("remote.connections")
                .unwrap(),
        )
        .unwrap()
        .profiles()[0]
            .username()
            .unwrap()
            .to_owned()
    });
    assert_eq!(unchanged, "dav-user ");
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-test", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-save", cx);
    })
    .unwrap();

    let original = cx.update(|cx| {
        ConnectionProfiles::import(
            &view
                .read(cx)
                .state()
                .draft()
                .value("remote.connections")
                .unwrap(),
        )
        .unwrap()
        .profiles()[0]
            .clone()
    });
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-close-editor", cx);
        window.render_frame(cx);
        window.click("settings-remote-edit-secure-webdav", cx);
        window.render_frame(cx);
        window.click("settings-remote-test", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-save", cx);
    })
    .unwrap();
    let preserved = cx.update(|cx| {
        ConnectionProfiles::import(
            &view
                .read(cx)
                .state()
                .draft()
                .value("remote.connections")
                .unwrap(),
        )
        .unwrap()
        .profiles()[0]
            .clone()
    });
    assert_eq!(preserved, original);

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.scroll("settings-controls", ScrollDelta::Lines(point(0., -40.)), cx);
        window.render_frame(cx);
        window.click("settings-remote-security-plaintext", cx);
        window.scroll("settings-controls", ScrollDelta::Lines(point(0., 40.)), cx);
        window.render_frame(cx);
        window.click("settings-remote-test", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-save", cx);
        window.render_frame(cx);
        assert!(window.find("settings-remote-confirm-save").visible());
        window.click("settings-remote-confirm-save", cx);
    })
    .unwrap();

    let plaintext = cx.update(|cx| {
        ConnectionProfiles::import(
            &view
                .read(cx)
                .state()
                .draft()
                .value("remote.connections")
                .unwrap(),
        )
        .unwrap()
        .profiles()[0]
            .clone()
    });
    assert_eq!(plaintext.security(), &SecurityPolicy::PlaintextConfirmed);
    assert!(plaintext.proxy().is_none());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-close-editor", cx);
        window.render_frame(cx);
        window.click("settings-remote-add", cx);
        window.render_frame(cx);
        for (id, value) in [
            ("remote-profile-id", "secure-sftp"),
            ("remote-profile-name", "Secure SFTP"),
            ("remote-profile-host", "sftp.example.test"),
            ("remote-profile-path", "/home/alice"),
        ] {
            window.click(id, cx);
            window.input(value, cx);
        }
        window.scroll("settings-controls", ScrollDelta::Lines(point(0., -40.)), cx);
        window.render_frame(cx);
        window.click("settings-remote-security-ssh-pinned", cx);
        window.render_frame(cx);
        window.click("remote-security-pin", cx);
        window.input(&"6b".repeat(TLS_PIN_BYTES), cx);
        window.scroll("settings-controls", ScrollDelta::Lines(point(0., 40.)), cx);
        window.render_frame(cx);
        window.click("settings-remote-test", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-save", cx);
    })
    .unwrap();

    let sftp = cx.update(|cx| {
        ConnectionProfiles::import(
            &view
                .read(cx)
                .state()
                .draft()
                .value("remote.connections")
                .unwrap(),
        )
        .unwrap()
        .profiles()[1]
            .clone()
    });
    assert_eq!(
        sftp.security(),
        &SecurityPolicy::Ssh(musheen_desktop::HostKeyPolicy::PinnedSha256(
            [0x6b; TLS_PIN_BYTES]
        ))
    );
    assert!(sftp.proxy().is_none());

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-close-editor", cx);
        window.render_frame(cx);
        window.click("settings-remote-edit-secure-sftp", cx);
        window.render_frame(cx);
        window.click("settings-remote-test", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-remote-save", cx);
    })
    .unwrap();
    let preserved_sftp = cx.update(|cx| {
        ConnectionProfiles::import(
            &view
                .read(cx)
                .state()
                .draft()
                .value("remote.connections")
                .unwrap(),
        )
        .unwrap()
        .profiles()[1]
            .clone()
    });
    assert_eq!(preserved_sftp, sftp);
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
async fn settings_gallery_checks_rendered_controls_labels_and_confirmation_at_double_scale(
    cx: &mut gpui_kit::TestAppContext,
) {
    use gpui_kit::component::Root;
    use gpui_kit::{AppContext, Role, px, size};
    use musheen_desktop::SettingKind;
    let root = tempfile::tempdir().unwrap();
    for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
        let catalog = Catalog::load(locale).unwrap();
        for (dark, contrast) in [(false, false), (true, false), (false, true)] {
            // Exercise every locale and theme mode without nine full schema walks.
            if !matches!(
                (locale, dark, contrast),
                (Locale::EnUs, false, false)
                    | (Locale::EnXa, true, false)
                    | (Locale::Ar, false, true)
            ) {
                continue;
            }
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
                        SettingsBackends::all(),
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
                    let sidebar = window.find("settings-sidebar").bounds();
                    let label = window.find(format!("text-{}", page.label()));
                    assert_eq!(label.label(), Some(catalog.message(page.label()).unwrap()));
                    assert!(label.bounds().origin.x >= sidebar.origin.x);
                    assert!(label.bounds().bottom_right().x <= sidebar.bottom_right().x);
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
            })
            .unwrap();
            // Every schema control is reached through the real searchable sidebar.
            // Check actual rendered labels, choice buttons, inputs and help text,
            // not a model-only list or just the fixed footer's bounds.
            for spec in settings_schema() {
                cx.update_window(handle.into(), |_, window, cx| {
                    window.click("settings-search", cx);
                    window.press("ctrl-a", cx);
                    window.input(spec.key, cx);
                })
                .unwrap();
                cx.update_window(handle.into(), |_, window, cx| {
                    window.render_frame(cx);
                    let result = format!("result-{}", spec.key);
                    let label = window.find(format!("text-{result}"));
                    let sidebar = window.find("settings-sidebar").bounds();
                    assert!(label.bounds().origin.x >= sidebar.origin.x);
                    assert!(
                        label.bounds().bottom_right().x <= sidebar.bottom_right().x,
                        "{locale:?} {result}"
                    );
                    window.click(result, cx);
                    window.render_frame(cx);
                    assert_eq!(view.read(cx).state().page(), spec.page);
                    assert_eq!(view.read(cx).state().focused_key(), Some(spec.key));
                    let panel = window.find("settings-controls").bounds();
                    let row = window.find(format!("row-{}", spec.key)).bounds();
                    let control = window.find(spec.key);
                    assert!(control.visible(), "{locale:?} {}", spec.key);
                    let expected_role = match spec.kind {
                        SettingKind::Boolean | SettingKind::Choice(_) => Role::Button,
                        SettingKind::Toolbar
                        | SettingKind::Shortcuts
                        | SettingKind::CustomActions
                        | SettingKind::ConnectionProfiles => Role::Group,
                        _ => Role::TextInput,
                    };
                    assert_eq!(control.role(), Some(expected_role), "{}", spec.key);
                    let mut ids = vec![
                        spec.key.to_owned(),
                        format!("label-{}", spec.key),
                        format!("help-{}", spec.key),
                    ];
                    if spec.restart_required {
                        ids.push(format!("restart-{}", spec.key));
                    }
                    let options: &[&str] = match spec.kind {
                        SettingKind::Boolean => &["false", "true"],
                        SettingKind::Choice(values) => values,
                        _ => &[],
                    };
                    for value in options {
                        let id = if *value == spec.default {
                            spec.key.to_owned()
                        } else {
                            format!("{}:{value}", spec.key)
                        };
                        let option = window.find(id.clone());
                        let expected = catalog
                            .message(&format!("settings-value-{value}"))
                            .unwrap()
                            .to_string();
                        assert_eq!(option.label(), Some(expected.as_str()));
                        ids.push(id.clone());
                        ids.push(format!("text-{id}"));
                    }
                    for id in ids {
                        let bounds = window.find(id.clone()).bounds();
                        assert!(
                            bounds.size.width > px(0.) && bounds.size.height > px(0.),
                            "{locale:?} {id}"
                        );
                        assert!(
                            bounds.origin.x >= panel.origin.x
                                && bounds.bottom_right().x <= panel.bottom_right().x,
                            "{locale:?} {id}: {bounds:?} outside {panel:?}"
                        );
                        assert!(
                            bounds.origin.y >= row.origin.y
                                && bounds.bottom_right().y <= row.bottom_right().y,
                            "{locale:?} {id}: content escapes row"
                        );
                    }
                    assert!(
                        row.size.height <= panel.size.height,
                        "{locale:?} {} cannot fit a scroll viewport",
                        spec.key
                    );
                    assert!(
                        row.origin.y >= panel.origin.y
                            && row.bottom_right().y <= panel.bottom_right().y,
                        "{locale:?} {}: target row clipped {row:?} in {panel:?}",
                        spec.key
                    );
                })
                .unwrap();
            }
            cx.update_window(handle.into(), |_, window, cx| {
                window.click("settings-reset-all", cx);
                window.render_frame(cx);
                let bounds = window.find("settings-window").bounds();
                for id in [
                    "settings-reset-summary",
                    "settings-reset-cancel",
                    "settings-confirm",
                ] {
                    let item = window.find(id);
                    assert!(item.visible());
                    assert!(
                        item.bounds().origin.x >= bounds.origin.x
                            && item.bounds().bottom_right().x <= bounds.bottom_right().x,
                        "{locale:?} {id}"
                    );
                    assert!(
                        item.bounds().origin.y >= bounds.origin.y
                            && item.bounds().bottom_right().y <= bounds.bottom_right().y,
                        "{locale:?} {id}"
                    );
                }
                assert!(window.find("settings-reset-cancel").focused().unwrap());
                window.press("escape", cx);
                window.remove_window();
            })
            .unwrap();
        }
    }
}
