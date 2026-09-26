use super::*;
use crate::providers::{ProviderRuntime, network_root_path};
use crate::settings::{RemoteCredentialStore, SettingsBackends, SettingsWindow};
use crate::test_fixtures::{FakeFtp, FtpServerOptions, MemoryKeyring};
use crate::{Catalog, Locale};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, ScrollDelta, TestAppContext, point, px, size};
use musheen_core::{BoxFuture, CancellationToken, PageRequest, StoreError};
use musheen_desktop::{
    RemoteCredentials, RemoteErrorCategory, SettingsDocument, SettingsStore, SshLogin,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Editor {
    view: Entity<SettingsWindow>,
    handle: AnyWindowHandle,
    store: SettingsStore,
    _config: tempfile::TempDir,
}

/// Opens Settings on Integrations with the connection editor open. A given
/// keyring stands in for the desktop secret service.
fn open_editor(
    cx: &mut TestAppContext,
    keyring: Option<MemoryKeyring>,
    tester: Option<Arc<dyn ConnectionTestService>>,
) -> Editor {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
        if let Some(keyring) = keyring {
            cx.set_global(RemoteCredentialStore(Arc::new(RemoteCredentials::new(
                Arc::new(keyring),
            ))));
        }
    });
    let config = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(config.path());
    let tester = tester.unwrap_or_else(default_connection_tester);
    let mut view = None;
    let handle = cx.open_window(size(px(900.), px(1400.)), |window, cx| {
        let settings = cx.new(|cx| {
            SettingsWindow::new_with_connection_tester(
                store.clone(),
                SettingsBackends::all(),
                Catalog::load(Locale::EnUs).unwrap(),
                tester,
                window,
                cx,
            )
        });
        view = Some(settings.clone());
        Root::new(settings, window, cx)
    });
    let handle: AnyWindowHandle = handle.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click("settings-page-integrations", cx);
        window.render_frame(cx);
        window.click("settings-remote-add", cx);
        window.render_frame(cx);
    })
    .unwrap();
    Editor {
        view: view.unwrap(),
        handle,
        store,
        _config: config,
    }
}

/// Whether `id` is shown with its centre inside the scrolled Settings
/// controls, where a click lands on it.
fn clickable(window: &gpui_kit::Window, id: &str) -> bool {
    let (Some(target), Some(controls)) = (
        window.try_find(id.to_owned()),
        window.try_find("settings-controls"),
    ) else {
        return false;
    };
    target.visible() && controls.bounds().contains(&target.bounds().center())
}

/// Scrolls the Settings controls until `id` can be clicked, from the top down.
fn reveal(window: &mut gpui_kit::Window, id: &str, cx: &mut gpui_kit::App) {
    window.render_frame(cx);
    if clickable(window, id) {
        return;
    }
    window.scroll("settings-controls", ScrollDelta::Lines(point(0., 400.)), cx);
    for _ in 0..80 {
        window.render_frame(cx);
        if clickable(window, id) {
            return;
        }
        window.scroll("settings-controls", ScrollDelta::Lines(point(0., -3.)), cx);
    }
}

impl Editor {
    fn fill(&self, cx: &mut TestAppContext, fields: &[(&str, &str)]) {
        cx.update_window(self.handle, |_, window, cx| {
            for (id, value) in fields {
                reveal(window, id, cx);
                window.click(id.to_string(), cx);
                window.input(value, cx);
            }
            window.render_frame(cx);
        })
        .unwrap();
    }

    fn click(&self, cx: &mut TestAppContext, id: &str) {
        let id = id.to_owned();
        cx.update_window(self.handle, |_, window, cx| {
            reveal(window, &id, cx);
            window.click(id, cx);
            window.render_frame(cx);
        })
        .unwrap();
    }

    fn shows(&self, cx: &mut TestAppContext, id: &str) -> bool {
        let id = id.to_owned();
        cx.update_window(self.handle, |_, window, cx| {
            window.render_frame(cx);
            window.try_find(id).is_some()
        })
        .unwrap()
    }

    /// Types `password` into the editor's password field when it has one.
    fn type_password(&self, cx: &mut TestAppContext, password: &str) {
        if self.shows(cx, "remote-profile-password") {
            self.fill(cx, &[("remote-profile-password", password)]);
        }
    }

    fn saved_connections(&self, cx: &mut TestAppContext) -> String {
        cx.read(|cx| {
            self.view
                .read(cx)
                .state
                .draft()
                .value("remote.connections")
                .unwrap()
        })
    }

    fn test_connection(&self, cx: &mut TestAppContext) {
        self.click(cx, "settings-remote-test");
        let view = self.view.clone();
        settle(cx, |cx| !view.read(cx).remote_testing);
    }

    /// Saves the connection in the editor and waits until it is in the draft.
    fn save_connection(&self, cx: &mut TestAppContext, id: &str) {
        self.click(cx, "settings-remote-save");
        let view = self.view.clone();
        let id = format!("\"{id}\"");
        assert!(
            settle(cx, |cx| {
                !view.read(cx).remote_saving
                    && view
                        .read(cx)
                        .state
                        .draft()
                        .value("remote.connections")
                        .is_some_and(|saved| saved.contains(&id))
            }),
            "the connection is saved"
        );
    }

    /// Applies the draft and waits until Apply ends.
    fn apply(&self, cx: &mut TestAppContext) {
        self.click(cx, "settings-apply");
        let view = self.view.clone();
        assert!(settle(cx, |cx| !view.read(cx).saving));
    }

    fn failure(&self, cx: &mut TestAppContext) -> Option<&'static str> {
        cx.read(|cx| self.view.read(cx).failure)
    }
}

/// Runs the app until `ready` holds, waiting in real time for the fake
/// servers' threads. Returns whether it held within five seconds.
fn settle(cx: &mut TestAppContext, mut ready: impl FnMut(&mut gpui_kit::App) -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        cx.run_until_parked();
        if cx.update(&mut ready) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn fill_ftp_connection(editor: &Editor, cx: &mut TestAppContext, port: u16) {
    editor.fill(
        cx,
        &[
            ("remote-profile-id", "team-ftp"),
            ("remote-profile-name", "Team FTP"),
            ("remote-profile-host", "127.0.0.1"),
            ("remote-profile-port", &port.to_string()),
            ("remote-profile-username", "alice"),
        ],
    );
    editor.click(cx, "settings-remote-protocol-ftp");
}

/// Lists the root of the one saved connection the way browsing does, with
/// the app's shared credentials.
fn browse_saved_connection(
    document: &str,
    cx: &mut TestAppContext,
) -> Result<Vec<String>, StoreError> {
    let mut settings = SettingsDocument::default();
    settings.set_value("remote.connections", document).unwrap();
    let credentials = cx.update(crate::settings::remote_credentials);
    let runtime = ProviderRuntime::from_settings_with_credentials(&settings, credentials).unwrap();
    let store = runtime.store();
    let entries = futures_lite::future::block_on(store.read_directory(
        &network_root_path(),
        PageRequest::new(16, None).unwrap(),
        CancellationToken::new(),
    ))
    .unwrap();
    let root = entries.items()[0].path().clone();
    futures_lite::future::block_on(store.read_directory(
        &root,
        PageRequest::new(16, None).unwrap(),
        CancellationToken::new(),
    ))
    .map(|page| {
        page.items()
            .iter()
            .map(|item| item.display_name().as_str().to_owned())
            .collect()
    })
}

#[gpui_kit::test]
async fn remote_parity_the_editor_offers_no_choice_browsing_refuses(cx: &mut TestAppContext) {
    let editor = open_editor(cx, Some(MemoryKeyring::default()), None);

    for refused in [
        "settings-remote-protocol-smb",
        "settings-remote-protocol-nfs",
        "settings-remote-protocol-http",
    ] {
        assert!(
            !editor.shows(cx, refused),
            "the editor offers no protocol browsing refuses: {refused}"
        );
    }
    for protocol in ["ftp", "ftps", "sftp", "webdav"] {
        editor.click(cx, &format!("settings-remote-protocol-{protocol}"));
        for proxy in [
            "settings-remote-proxy-socks5",
            "settings-remote-proxy-http-connect",
        ] {
            assert!(
                !editor.shows(cx, proxy),
                "{protocol} offers no proxy, which browsing refuses: {proxy}"
            );
        }
    }
    editor.click(cx, "settings-remote-protocol-ftps");
    assert!(
        !editor.shows(cx, "settings-remote-security-tls-pinned"),
        "FTPS offers no certificate pin, which browsing refuses"
    );
}

/// Whether browsing refused a connection outright, rather than failing to
/// reach its server.
fn refused(error: &StoreError) -> bool {
    let text = format!("{error:?} {error}");
    text.contains("nsupported")
        || text.contains("InvalidProfile")
        || text.contains("not yet available")
        || text.contains("must be mounted")
}

#[test]
fn remote_parity_every_offered_choice_reaches_the_server() {
    let closed_port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // SFTP's offered choices log in against an in-process SSH server in the
    // sftp_login_ tests, which give the login its own ~/.ssh.
    for protocol in PROTOCOLS
        .into_iter()
        .filter(|protocol| *protocol != RemoteProtocol::Sftp)
    {
        let proxies: &[Option<ProxyKind>] = if supports_proxy(protocol) {
            &[None, Some(ProxyKind::Socks5), Some(ProxyKind::HttpConnect)]
        } else {
            &[None]
        };
        for choice in security_choices(protocol) {
            for proxy_kind in proxies {
                let proxy = proxy_kind.map(|kind| {
                    ProxySettings::new(
                        protocol,
                        kind,
                        RemoteHost::new(protocol, "127.0.0.1").unwrap(),
                        closed_port,
                        None::<&str>,
                        None,
                    )
                    .unwrap()
                });
                let profile = ConnectionProfile::new(
                    ConnectionId::new("offered").unwrap(),
                    "Offered",
                    protocol,
                    RemoteHost::new(protocol, "127.0.0.1").unwrap(),
                    Some(closed_port),
                    "/",
                    None::<&str>,
                    None,
                    security_policy(*choice),
                    proxy,
                )
                .unwrap();
                let mut settings = SettingsDocument::default();
                settings
                    .set_value(
                        "remote.connections",
                        &ConnectionProfiles::new(vec![profile]).export().unwrap(),
                    )
                    .unwrap();
                let runtime = ProviderRuntime::from_settings(&settings).unwrap();
                let store = runtime.store();
                let entries = futures_lite::future::block_on(store.read_directory(
                    &network_root_path(),
                    PageRequest::new(16, None).unwrap(),
                    CancellationToken::new(),
                ))
                .unwrap();
                let described = format!("{protocol:?} {choice:?} {proxy_kind:?}");
                assert_eq!(
                    entries.items().len(),
                    1,
                    "Network lists the offered connection: {described}"
                );
                let error = futures_lite::future::block_on(store.read_directory(
                    entries.items()[0].path(),
                    PageRequest::new(16, None).unwrap(),
                    CancellationToken::new(),
                ))
                .expect_err("nothing listens on the closed port");
                assert!(
                    !refused(&error),
                    "browsing tries to reach the server of every choice the editor offers: {described}: {error:?}"
                );
            }
        }
    }
}

struct FailingTester(RemoteErrorCategory);

impl musheen_desktop::ProfileConnectionTest for FailingTester {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        let error = RemoteError::new(profile.protocol(), self.0, Some(profile.host().clone()));
        Box::pin(async move { Err(error) })
    }
}

#[gpui_kit::test]
async fn remote_parity_a_failed_test_names_its_cause(cx: &mut TestAppContext) {
    let editor = open_editor(
        cx,
        Some(MemoryKeyring::default()),
        Some(Arc::new(FailingTester(RemoteErrorCategory::Authentication))),
    );
    fill_ftp_connection(&editor, cx, 21);

    editor.test_connection(cx);

    let cause = cx
        .update_window(editor.handle, |_, window, cx| {
            window.render_frame(cx);
            window
                .try_find("settings-remote-test-cause")
                .and_then(|cause| cause.label().map(str::to_owned))
        })
        .unwrap();
    assert!(cause.is_some(), "a failed test names its cause");
    let expected = Catalog::load(Locale::EnUs)
        .unwrap()
        .message("settings-remote-cause-authentication")
        .unwrap()
        .to_owned();
    assert_eq!(cause.as_deref(), Some(expected.as_str()));
}

#[gpui_kit::test]
async fn remote_password_typed_in_the_editor_reaches_the_test_the_keyring_and_browsing(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::default();
    let server = FakeFtp::start(FtpServerOptions {
        password: Some("hunter2"),
        lists_root: true,
    });
    let editor = open_editor(cx, Some(keyring.clone()), None);
    fill_ftp_connection(&editor, cx, server.port());
    editor.type_password(cx, "hunter2");

    editor.test_connection(cx);

    assert_eq!(
        server.passwords(),
        ["hunter2"],
        "Test connection sends the password typed in the editor"
    );

    editor.save_connection(cx, "team-ftp");
    cx.run_until_parked();
    assert!(
        keyring.ids().is_empty(),
        "Save keeps the password for Apply, so Cancel leaves the keyring as it was"
    );
    let saved = editor.saved_connections(cx);
    assert!(saved.contains("secret-service:team-ftp"), "{saved}");
    assert!(
        !saved.contains("hunter2"),
        "settings keep only the reference"
    );
    let profile = ConnectionProfiles::import(&saved).unwrap().profiles()[0].clone();
    assert!(!format!("{profile:?}").contains("hunter2"));

    editor.apply(cx);
    assert_eq!(
        keyring.secret("team-ftp").as_deref(),
        Some(b"hunter2".as_slice()),
        "Apply stores the password in the secret service under the connection's ID"
    );
    assert_eq!(
        browse_saved_connection(&saved, cx).unwrap(),
        ["hello.txt"],
        "browsing the saved connection logs in with the stored password"
    );
    assert_eq!(
        server.passwords().last().map(String::as_str),
        Some("hunter2")
    );

    editor.fill(cx, &[("remote-profile-name", " renamed")]);
    editor.test_connection(cx);
    editor.save_connection(cx, "team-ftp");
    editor.apply(cx);
    assert_eq!(
        keyring.secret("team-ftp").as_deref(),
        Some(b"hunter2".as_slice()),
        "saving with the password field empty keeps the stored password"
    );
    let file = std::fs::read_to_string(editor.store.path()).unwrap();
    assert!(
        !file.contains("hunter2"),
        "the settings file never holds it"
    );

    editor.click(cx, "settings-remote-remove");
    cx.run_until_parked();
    assert!(!editor.saved_connections(cx).contains("team-ftp"));
    assert_eq!(
        keyring.secret("team-ftp").as_deref(),
        Some(b"hunter2".as_slice()),
        "a removal not yet applied keeps the password, so Cancel keeps a working connection"
    );
    editor.apply(cx);
    assert!(
        keyring.secret("team-ftp").is_none(),
        "applying the removal deletes the connection's password"
    );
}

#[gpui_kit::test]
async fn remote_password_of_a_connection_removed_before_apply_never_reaches_the_keyring(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::default();
    let server = FakeFtp::start(FtpServerOptions {
        password: Some("hunter2"),
        lists_root: true,
    });
    let editor = open_editor(cx, Some(keyring.clone()), None);
    fill_ftp_connection(&editor, cx, server.port());
    editor.type_password(cx, "hunter2");
    editor.test_connection(cx);
    editor.save_connection(cx, "team-ftp");

    editor.click(cx, "settings-remote-remove");
    editor.apply(cx);

    assert!(
        keyring.ids().is_empty(),
        "a connection removed before Apply leaves no password behind"
    );
}

#[gpui_kit::test]
async fn remote_password_a_deletion_the_keyring_refuses_is_named_and_tried_again(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::default();
    let server = FakeFtp::start(FtpServerOptions {
        password: Some("hunter2"),
        lists_root: true,
    });
    let editor = open_editor(cx, Some(keyring.clone()), None);
    fill_ftp_connection(&editor, cx, server.port());
    editor.type_password(cx, "hunter2");
    editor.test_connection(cx);
    editor.save_connection(cx, "team-ftp");
    editor.apply(cx);
    assert!(keyring.secret("team-ftp").is_some());

    keyring.set_locked(true);
    editor.click(cx, "settings-remote-remove");
    editor.apply(cx);
    assert_eq!(
        editor.failure(cx),
        Some("settings-remote-keyring-delete-failed"),
        "a password the keyring would not delete is named"
    );
    assert!(keyring.secret("team-ftp").is_some());

    keyring.set_locked(false);
    editor.apply(cx);
    assert!(
        keyring.secret("team-ftp").is_none(),
        "the next Apply deletes it"
    );
    assert_eq!(editor.failure(cx), None);
}

#[gpui_kit::test]
async fn remote_password_stored_under_another_id_is_kept_and_used(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::default();
    keyring.put("shared", b"hunter2");
    let server = FakeFtp::start(FtpServerOptions {
        password: Some("hunter2"),
        lists_root: true,
    });
    let editor = open_editor(cx, Some(keyring.clone()), None);
    // A connection an older build saved with the global credential.
    let profile = ConnectionProfile::new(
        ConnectionId::new("team-ftp").unwrap(),
        "Team FTP",
        RemoteProtocol::Ftp,
        RemoteHost::new(RemoteProtocol::Ftp, "127.0.0.1").unwrap(),
        Some(server.port()),
        "/",
        Some("alice"),
        Some(CredentialReference::persistent(
            ConnectionId::new("shared").unwrap(),
        )),
        SecurityPolicy::PlaintextConfirmed,
        None,
    )
    .unwrap();
    let encoded = ConnectionProfiles::new(vec![profile]).export().unwrap();
    editor.view.update(cx, |view, cx| {
        view.state.edit("remote.connections", &encoded).unwrap();
        view.remote_editor_open = false;
        cx.notify();
    });
    editor.click(cx, "settings-remote-edit-team-ftp");

    editor.test_connection(cx);
    assert_eq!(
        server.passwords(),
        ["hunter2"],
        "Test connection uses the stored password the connection names"
    );
    editor.save_connection(cx, "team-ftp");
    assert!(
        editor
            .saved_connections(cx)
            .contains("secret-service:shared"),
        "saving with the password field empty keeps that reference"
    );
    editor.apply(cx);
    assert!(keyring.secret("shared").is_some());
}

#[gpui_kit::test]
async fn remote_parity_a_changed_password_or_key_needs_a_new_test(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::default();
    let server = FakeFtp::start(FtpServerOptions {
        password: Some("hunter2"),
        lists_root: true,
    });
    let editor = open_editor(cx, Some(keyring.clone()), None);
    fill_ftp_connection(&editor, cx, server.port());
    editor.type_password(cx, "hunter2");
    editor.test_connection(cx);
    assert!(cx.read(|cx| editor.view.read(cx).remote_test_report.is_some()));

    editor.fill(cx, &[("remote-profile-password", "x")]);
    assert!(
        cx.read(|cx| editor.view.read(cx).remote_test_report.is_none()),
        "a password typed after the test needs a new test"
    );

    editor.click(cx, "settings-remote-protocol-sftp");
    editor.click(cx, "settings-remote-login-stored-key");
    editor.fill(cx, &[("remote-profile-host", "office")]);
    editor.view.update(cx, |view, cx| {
        view.remote_test_report = Some(TestReport::passed(&view.remote_profile(cx).unwrap()));
    });
    editor.fill(cx, &[("remote-login-key-text", "k")]);
    assert!(
        cx.read(|cx| editor.view.read(cx).remote_test_report.is_none()),
        "a key pasted after the test needs a new test"
    );
}

#[gpui_kit::test]
async fn remote_session_password_a_locked_keyring_offers_session_only_use(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::locked();
    let server = FakeFtp::start(FtpServerOptions {
        password: Some("hunter2"),
        lists_root: true,
    });
    let editor = open_editor(cx, Some(keyring.clone()), None);
    fill_ftp_connection(&editor, cx, server.port());
    editor.type_password(cx, "hunter2");
    editor.test_connection(cx);
    editor.click(cx, "settings-remote-save");

    let handle = editor.handle;
    let offered = settle(cx, |cx| {
        handle
            .update(cx, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("settings-remote-session-only").is_some()
            })
            .unwrap_or(false)
    });
    assert!(
        offered,
        "a locked keyring leaves the user a session-only choice"
    );

    editor.click(cx, "settings-remote-session-only");
    let view = editor.view.clone();
    assert!(settle(cx, |cx| {
        view.read(cx)
            .state
            .draft()
            .value("remote.connections")
            .is_some_and(|saved| saved.contains("team-ftp"))
    }));
    let saved = editor.saved_connections(cx);
    assert!(saved.contains("secret-service:team-ftp"), "{saved}");
    assert!(!saved.contains("hunter2"));

    editor.apply(cx);
    assert_eq!(editor.failure(cx), None);
    assert!(
        keyring.ids().is_empty(),
        "nothing reaches the locked keyring"
    );
    assert_eq!(
        browse_saved_connection(&saved, cx).unwrap(),
        ["hello.txt"],
        "the session-only password logs the saved connection in"
    );
    assert_eq!(
        server.passwords().last().map(String::as_str),
        Some("hunter2")
    );
    let file = std::fs::read_to_string(editor.store.path()).unwrap();
    assert!(
        !file.contains("hunter2"),
        "a session-only password is never written"
    );
}

struct PassingTester;

impl musheen_desktop::ProfileConnectionTest for PassingTester {
    fn test<'a>(
        &'a self,
        _profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        Box::pin(async { Ok(()) })
    }
}

fn fill_sftp_connection(editor: &Editor, cx: &mut TestAppContext) {
    editor.fill(
        cx,
        &[
            ("remote-profile-id", "office"),
            ("remote-profile-name", "Office"),
            ("remote-profile-host", "office"),
        ],
    );
    editor.click(cx, "settings-remote-protocol-sftp");
}

fn saved_login(editor: &Editor, cx: &mut TestAppContext) -> SshLogin {
    let saved = editor.saved_connections(cx);
    ConnectionProfiles::import(&saved).unwrap().profiles()[0]
        .login()
        .clone()
}

#[gpui_kit::test]
async fn sftp_login_the_editor_offers_each_login_method_and_saves_it(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::default();
    let editor = open_editor(cx, Some(keyring.clone()), Some(Arc::new(PassingTester)));
    fill_sftp_connection(&editor, cx);

    for method in ["password", "agent", "key-file", "stored-key"] {
        assert!(
            editor.shows(cx, &format!("settings-remote-login-{method}")),
            "an SFTP connection offers the {method} login"
        );
    }

    editor.click(cx, "settings-remote-login-agent");
    editor.test_connection(cx);
    editor.click(cx, "settings-remote-save");
    assert_eq!(saved_login(&editor, cx), SshLogin::Agent);

    editor.click(cx, "settings-remote-login-key-file");
    editor.fill(cx, &[("remote-login-key-path", "/keys/id_office")]);
    editor.test_connection(cx);
    editor.click(cx, "settings-remote-save");
    assert_eq!(
        saved_login(&editor, cx),
        SshLogin::KeyFile {
            path: Some("/keys/id_office".into())
        }
    );

    let key_text =
        "-----BEGIN OPENSSH PRIVATE KEY-----\nstored-key-body\n-----END OPENSSH PRIVATE KEY-----\n";
    editor.click(cx, "settings-remote-login-stored-key");
    editor.fill(cx, &[("remote-login-key-text", key_text)]);
    editor.type_password(cx, "key passphrase");
    editor.test_connection(cx);
    editor.save_connection(cx, "office");
    assert_eq!(saved_login(&editor, cx), SshLogin::StoredKey);
    editor.apply(cx);
    assert_eq!(
        keyring.secret("office.key").as_deref(),
        Some(key_text.as_bytes()),
        "the pasted key goes to the secret service"
    );
    assert_eq!(
        keyring.secret("office").as_deref(),
        Some(b"key passphrase".as_slice()),
        "its passphrase goes there too"
    );
    let saved = editor.saved_connections(cx);
    assert!(!saved.contains("stored-key-body") && !saved.contains("key passphrase"));

    editor.click(cx, "settings-remote-login-agent");
    editor.test_connection(cx);
    editor.save_connection(cx, "office");
    cx.run_until_parked();
    assert!(
        keyring.secret("office.key").is_some(),
        "a login change not yet applied keeps the stored key"
    );
    editor.apply(cx);
    assert!(
        keyring.secret("office.key").is_none() && keyring.secret("office").is_none(),
        "applying a switch to agent login deletes the stored key and its passphrase"
    );
}

/// The cause a failed test shows.
fn shown_cause(editor: &Editor, cx: &mut TestAppContext) -> Option<String> {
    cx.update_window(editor.handle, |_, window, cx| {
        window.render_frame(cx);
        window
            .try_find("settings-remote-test-cause")
            .and_then(|cause| cause.label().map(str::to_owned))
    })
    .unwrap()
}

fn message(key: &str) -> String {
    Catalog::load(Locale::EnUs)
        .unwrap()
        .message(key)
        .unwrap()
        .to_owned()
}

#[gpui_kit::test]
async fn remote_parity_a_failed_ftps_test_on_port_990_names_implicit_tls(cx: &mut TestAppContext) {
    let editor = open_editor(
        cx,
        Some(MemoryKeyring::default()),
        Some(Arc::new(FailingTester(RemoteErrorCategory::Timeout))),
    );
    fill_ftp_connection(&editor, cx, 990);
    editor.click(cx, "settings-remote-protocol-ftps");

    editor.test_connection(cx);

    assert_eq!(
        shown_cause(&editor, cx),
        Some(message("settings-remote-cause-implicit-tls")),
        "an FTPS server that does not answer on 990 most likely wants implicit TLS"
    );
}

#[gpui_kit::test]
async fn sftp_login_with_the_agent_and_no_agent_running_names_the_agent(cx: &mut TestAppContext) {
    cx.executor().allow_parking();
    let editor = open_editor(cx, Some(MemoryKeyring::default()), None);
    fill_sftp_connection(&editor, cx);
    editor.click(cx, "settings-remote-login-agent");

    editor.test_connection(cx);

    assert_eq!(
        shown_cause(&editor, cx),
        Some(message("settings-remote-cause-no-agent")),
        "the test that browsing runs names the missing agent, not the server"
    );
}

#[gpui_kit::test]
async fn sftp_login_a_connection_id_ending_in_key_is_refused(cx: &mut TestAppContext) {
    let editor = open_editor(
        cx,
        Some(MemoryKeyring::default()),
        Some(Arc::new(PassingTester)),
    );
    fill_sftp_connection(&editor, cx);
    editor.fill(cx, &[("remote-profile-id", ".key")]);

    editor.test_connection(cx);

    assert!(
        cx.read(|cx| editor.view.read(cx).remote_validation_failed),
        "office.key would share the secret ID of office's stored key"
    );
}

#[gpui_kit::test]
async fn remote_global_credential_is_not_a_setting(cx: &mut TestAppContext) {
    assert!(
        musheen_desktop::settings_schema()
            .iter()
            .all(|spec| spec.key != "remote.credential"),
        "settings search walks the schema, so it finds no global remote credential"
    );
    let editor = open_editor(cx, Some(MemoryKeyring::default()), None);
    assert!(
        !editor.shows(cx, "remote.credential"),
        "the Integrations page shows no global remote credential"
    );
}

#[gpui_kit::test]
async fn remote_global_credential_of_an_older_file_stays_unread_and_its_secret_still_logs_in(
    cx: &mut TestAppContext,
) {
    cx.executor().allow_parking();
    let keyring = MemoryKeyring::default();
    keyring.put("shared", b"hunter2");
    let server = FakeFtp::start(FtpServerOptions {
        password: Some("hunter2"),
        lists_root: true,
    });
    let _editor = open_editor(cx, Some(keyring.clone()), None);
    // A connection an older build saved with the global credential.
    let profile = ConnectionProfile::new(
        ConnectionId::new("team-ftp").unwrap(),
        "Team FTP",
        RemoteProtocol::Ftp,
        RemoteHost::new(RemoteProtocol::Ftp, "127.0.0.1").unwrap(),
        Some(server.port()),
        "/",
        Some("alice"),
        Some(CredentialReference::persistent(
            ConnectionId::new("shared").unwrap(),
        )),
        SecurityPolicy::PlaintextConfirmed,
        None,
    )
    .unwrap();
    let connections = ConnectionProfiles::new(vec![profile.clone()])
        .export()
        .unwrap();
    let config = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(config.path());
    std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    std::fs::write(
        store.path(),
        format!(
            "schema_version=3\nremote.credential=secret-service:shared\nremote.connections={connections}\n"
        ),
    )
    .unwrap();

    let document = store
        .load()
        .expect("a settings file with the global credential loads");
    let loaded = ConnectionProfiles::import(&document.value("remote.connections").unwrap())
        .expect("its connections load");
    assert_eq!(loaded.profiles(), [profile]);
    store.save(&document).unwrap();
    assert!(
        std::fs::read_to_string(store.path())
            .unwrap()
            .contains("remote.credential=secret-service:shared"),
        "the old value stays in the file, unread"
    );

    assert_eq!(
        browse_saved_connection(&connections, cx).unwrap(),
        ["hello.txt"],
        "a connection that refers to the old global secret still logs in with it"
    );
    assert_eq!(server.passwords(), ["hunter2"]);
}
