use super::*;
use crate::providers::{ProviderRuntime, network_root_path};
use crate::settings::{RemoteCredentialStore, SettingsBackends, SettingsWindow};
use crate::test_fixtures::{FakeFtp, FtpServerOptions, MemoryKeyring};
use crate::{Catalog, Locale};
use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AnyWindowHandle, TestAppContext, px, size};
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

impl Editor {
    fn fill(&self, cx: &mut TestAppContext, fields: &[(&str, &str)]) {
        cx.update_window(self.handle, |_, window, cx| {
            window.render_frame(cx);
            for (id, value) in fields {
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
            window.render_frame(cx);
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

    for refused in ["settings-remote-protocol-smb", "settings-remote-protocol-nfs"] {
        assert!(
            !editor.shows(cx, refused),
            "the editor offers no protocol browsing refuses: {refused}"
        );
    }
    for protocol in ["ftp", "ftps", "sftp", "webdav", "http"] {
        editor.click(cx, &format!("settings-remote-protocol-{protocol}"));
        for proxy in ["settings-remote-proxy-socks5", "settings-remote-proxy-http-connect"] {
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
                .err()
                .expect("nothing listens on the closed port");
                assert!(
                    !refused(&error),
                    "browsing tries to reach the server of every choice the editor offers: {described}: {error:?}"
                );
            }
        }
    }
}

struct FailingTester(RemoteErrorCategory);

impl ConnectionTestService for FailingTester {
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

    editor.click(cx, "settings-remote-save");
    assert!(
        settle(cx, |_| keyring.secret("team-ftp").is_some()),
        "saving stores the password in the secret service"
    );
    assert_eq!(keyring.secret("team-ftp").as_deref(), Some(b"hunter2".as_slice()));
    let saved = editor.saved_connections(cx);
    assert!(saved.contains("secret-service:team-ftp"), "{saved}");
    assert!(!saved.contains("hunter2"), "settings keep only the reference");
    let profile = ConnectionProfiles::import(&saved).unwrap().profiles()[0].clone();
    assert!(!format!("{profile:?}").contains("hunter2"));

    assert_eq!(
        browse_saved_connection(&saved, cx).unwrap(),
        ["hello.txt"],
        "browsing the saved connection logs in with the stored password"
    );
    assert_eq!(server.passwords().last().map(String::as_str), Some("hunter2"));

    editor.fill(cx, &[("remote-profile-name", " renamed")]);
    editor.test_connection(cx);
    editor.click(cx, "settings-remote-save");
    assert!(settle(cx, |cx| {
        editor.view.read(cx).remote_save_requirement.is_none()
            && !editor.view.read(cx).remote_testing
    }));
    assert_eq!(
        keyring.secret("team-ftp").as_deref(),
        Some(b"hunter2".as_slice()),
        "saving with the password field empty keeps the stored password"
    );

    editor.click(cx, "settings-apply");
    let view = editor.view.clone();
    assert!(settle(cx, |cx| !view.read(cx).saving));
    let file = std::fs::read_to_string(editor.store.path()).unwrap();
    assert!(!file.contains("hunter2"), "the settings file never holds it");

    editor.click(cx, "settings-remote-remove");
    assert!(
        settle(cx, |_| keyring.secret("team-ftp").is_none()),
        "removing the connection deletes its password"
    );
    assert!(!editor.saved_connections(cx).contains("team-ftp"));
}

#[gpui_kit::test]
async fn remote_session_password_a_locked_keyring_offers_session_only_use(
    cx: &mut TestAppContext,
) {
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
    assert!(offered, "a locked keyring leaves the user a session-only choice");

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
    assert!(keyring.ids().is_empty(), "nothing reaches the locked keyring");
    assert_eq!(
        browse_saved_connection(&saved, cx).unwrap(),
        ["hello.txt"],
        "the session-only password logs the saved connection in"
    );
    assert_eq!(server.passwords().last().map(String::as_str), Some("hunter2"));

    editor.click(cx, "settings-apply");
    let view = editor.view.clone();
    assert!(settle(cx, |cx| !view.read(cx).saving));
    let file = std::fs::read_to_string(editor.store.path()).unwrap();
    assert!(!file.contains("hunter2"), "a session-only password is never written");
}

struct PassingTester;

impl ConnectionTestService for PassingTester {
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

    let key_text = "-----BEGIN OPENSSH PRIVATE KEY-----\nstored-key-body\n-----END OPENSSH PRIVATE KEY-----\n";
    editor.click(cx, "settings-remote-login-stored-key");
    editor.fill(cx, &[("remote-login-key-text", key_text)]);
    editor.type_password(cx, "key passphrase");
    editor.test_connection(cx);
    editor.click(cx, "settings-remote-save");
    assert!(settle(cx, |_| keyring.secret("office.key").is_some()));
    assert_eq!(saved_login(&editor, cx), SshLogin::StoredKey);
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
}
