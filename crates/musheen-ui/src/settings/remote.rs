use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, AppContext, Context, Entity, Role, TestSupportExt, Window, div};
use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    BrowseRefusal, ConnectionId, ConnectionProfile, ConnectionProfiles, CredentialReference,
    HostKeyPolicy, ProxyKind, ProxySettings, RemoteError, RemoteErrorCategory, RemoteHost,
    RemoteProtocol, SaveConfirmation, SaveRequirement, SecurityPolicy, SettingSpec,
    SettingsDocument, SettingsPage, TLS_PIN_BYTES, TestReport, TlsPolicy, settings_schema,
};
use musheen_desktop::{CredentialResolver, SecretBuffer, SshLogin};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub const PROFILE_ID: &str = "remote-profile-id";
pub const PROFILE_NAME: &str = "remote-profile-name";
pub const PROFILE_HOST: &str = "remote-profile-host";
pub const PROFILE_PORT: &str = "remote-profile-port";
pub const PROFILE_PATH: &str = "remote-profile-path";
pub const PROFILE_USERNAME: &str = "remote-profile-username";
pub const PROFILE_PASSWORD: &str = "remote-profile-password";
pub const LOGIN_KEY_PATH: &str = "remote-login-key-path";
pub const LOGIN_KEY_TEXT: &str = "remote-login-key-text";
pub const SECURITY_PIN: &str = "remote-security-pin";
pub const PROXY_HOST: &str = "remote-proxy-host";
pub const PROXY_PORT: &str = "remote-proxy-port";
pub const PROXY_USERNAME: &str = "remote-proxy-username";
pub const PROXY_CREDENTIAL: &str = "remote-proxy-credential";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SecurityChoice {
    Plaintext,
    TlsSystemRoots,
    TlsPinned,
    SshKnownHosts,
    SshPinned,
    SystemManaged,
}

/// Tests a connection before it is saved. `credentials` answers with the
/// secrets typed in the editor ahead of the stored ones.
pub trait ConnectionTestService: Send + Sync + 'static {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        credentials: Arc<dyn CredentialResolver>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>>;
}

/// A tester that brings its own credentials, such as a protocol probe.
impl<T: musheen_desktop::ProfileConnectionTest> ConnectionTestService for T {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        _credentials: Arc<dyn CredentialResolver>,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        musheen_desktop::ProfileConnectionTest::test(self, profile, cancellation)
    }
}

/// The connection test a Settings window runs unless a test supplies its
/// own: it opens the connection the way browsing does and reads its root.
pub(crate) fn default_connection_tester() -> Arc<dyn ConnectionTestService> {
    Arc::new(crate::providers::BrowseConnectionTester::new(
        crate::providers::ssh_environment(),
    ))
}

pub(super) fn controls() -> Vec<&'static SettingSpec> {
    super::controls_for(SettingsPage::Integrations)
        .into_iter()
        .filter(|spec| spec.group != "settings-group-terminal")
        .collect()
}

pub(super) fn inputs(
    window: &mut Window,
    cx: &mut Context<super::SettingsWindow>,
) -> BTreeMap<&'static str, Entity<InputState>> {
    [
        (PROFILE_ID, ""),
        (PROFILE_NAME, ""),
        (PROFILE_HOST, ""),
        (PROFILE_PORT, ""),
        (PROFILE_PATH, "/"),
        (PROFILE_USERNAME, ""),
        (PROFILE_PASSWORD, ""),
        (LOGIN_KEY_PATH, ""),
        (SECURITY_PIN, ""),
        (PROXY_HOST, ""),
        (PROXY_PORT, ""),
        (PROXY_USERNAME, ""),
        (PROXY_CREDENTIAL, ""),
    ]
    .into_iter()
    .map(|(id, value)| {
        let masked = id == PROFILE_PASSWORD;
        (
            id,
            cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(value)
                    .masked(masked)
            }),
        )
    })
    .collect()
}

/// The box a private key is pasted into, for a stored-key SFTP login.
pub(super) fn key_text_input(
    window: &mut Window,
    cx: &mut Context<super::SettingsWindow>,
) -> Entity<TextareaState> {
    cx.new(|cx| TextareaState::new(window, cx))
}

pub(super) fn build_profile(
    inputs: &BTreeMap<&'static str, Entity<InputState>>,
    protocol: RemoteProtocol,
    security: SecurityPolicy,
    proxy_kind: Option<ProxyKind>,
    credential: Option<CredentialReference>,
    cx: &gpui_kit::App,
) -> Result<ConnectionProfile, RemoteError> {
    let raw_value = |key| inputs[key].read(cx).value().to_string();
    let syntax_value = |key| inputs[key].read(cx).value().trim().to_owned();
    let host = RemoteHost::new(protocol, syntax_value(PROFILE_HOST))?;
    let port = match syntax_value(PROFILE_PORT) {
        value if value.is_empty() => None,
        value => Some(value.parse::<u16>().map_err(|_| {
            RemoteError::new(
                protocol,
                RemoteErrorCategory::InvalidProfile,
                Some(host.clone()),
            )
        })?),
    };
    let username = raw_value(PROFILE_USERNAME);
    let security = configured_security(protocol, security, &syntax_value(SECURITY_PIN), &host)?;
    let proxy = proxy_kind
        .map(|kind| {
            let proxy_host = RemoteHost::new(protocol, syntax_value(PROXY_HOST))?;
            let proxy_port = syntax_value(PROXY_PORT).parse::<u16>().map_err(|_| {
                RemoteError::new(
                    protocol,
                    RemoteErrorCategory::InvalidProfile,
                    Some(proxy_host.clone()),
                )
            })?;
            let proxy_username = raw_value(PROXY_USERNAME);
            let proxy_credential = match syntax_value(PROXY_CREDENTIAL) {
                value if value.is_empty() => None,
                value => Some(
                    CredentialReference::from_setting_value(&value).map_err(|_| {
                        RemoteError::new(
                            protocol,
                            RemoteErrorCategory::InvalidProfile,
                            Some(proxy_host.clone()),
                        )
                    })?,
                ),
            };
            ProxySettings::new(
                protocol,
                kind,
                proxy_host,
                proxy_port,
                (!proxy_username.is_empty()).then_some(proxy_username),
                proxy_credential,
            )
        })
        .transpose()?;
    ConnectionProfile::new(
        ConnectionId::new(syntax_value(PROFILE_ID)).map_err(|_| {
            RemoteError::new(
                protocol,
                RemoteErrorCategory::InvalidProfile,
                Some(host.clone()),
            )
        })?,
        syntax_value(PROFILE_NAME),
        protocol,
        host,
        port,
        raw_value(PROFILE_PATH),
        (!username.is_empty()).then_some(username),
        credential,
        security,
        proxy,
    )
}

fn configured_security(
    protocol: RemoteProtocol,
    security: SecurityPolicy,
    pin: &str,
    host: &RemoteHost,
) -> Result<SecurityPolicy, RemoteError> {
    let invalid = || {
        RemoteError::new(
            protocol,
            RemoteErrorCategory::InvalidProfile,
            Some(host.clone()),
        )
    };
    let decode = || {
        if pin.len() != TLS_PIN_BYTES * 2 || !pin.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        let mut decoded = [0; TLS_PIN_BYTES];
        for (index, byte) in decoded.iter_mut().enumerate() {
            *byte =
                u8::from_str_radix(&pin[index * 2..index * 2 + 2], 16).map_err(|_| invalid())?;
        }
        Ok(decoded)
    };
    match security {
        SecurityPolicy::Tls(TlsPolicy::PinnedSha256(_)) => {
            Ok(SecurityPolicy::Tls(TlsPolicy::PinnedSha256(decode()?)))
        }
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(_)) => {
            Ok(SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(decode()?)))
        }
        policy => Ok(policy),
    }
}

#[must_use]
pub(super) fn default_security(protocol: RemoteProtocol) -> SecurityPolicy {
    match protocol {
        RemoteProtocol::Ftp => SecurityPolicy::PlaintextConfirmed,
        RemoteProtocol::Ftps | RemoteProtocol::WebDav | RemoteProtocol::Http => {
            SecurityPolicy::Tls(TlsPolicy::SystemRoots)
        }
        RemoteProtocol::Sftp => SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        RemoteProtocol::Smb | RemoteProtocol::Nfs => SecurityPolicy::SystemManaged,
    }
}

#[must_use]
pub(super) const fn security_choices(protocol: RemoteProtocol) -> &'static [SecurityChoice] {
    use SecurityChoice::{
        Plaintext, SshKnownHosts, SshPinned, SystemManaged, TlsPinned, TlsSystemRoots,
    };
    match protocol {
        RemoteProtocol::Ftp => &[Plaintext],
        // OpenDAL's FTP service has no certificate hook, so browsing cannot
        // check a pinned FTPS certificate (SYS-031).
        RemoteProtocol::Ftps => &[TlsSystemRoots],
        RemoteProtocol::Sftp => &[SshKnownHosts, SshPinned],
        RemoteProtocol::WebDav | RemoteProtocol::Http => &[TlsSystemRoots, TlsPinned, Plaintext],
        RemoteProtocol::Smb | RemoteProtocol::Nfs => &[SystemManaged],
    }
}

#[must_use]
pub(super) const fn security_id(choice: SecurityChoice) -> &'static str {
    match choice {
        SecurityChoice::Plaintext => "plaintext",
        SecurityChoice::TlsSystemRoots => "tls-system-roots",
        SecurityChoice::TlsPinned => "tls-pinned",
        SecurityChoice::SshKnownHosts => "ssh-known-hosts",
        SecurityChoice::SshPinned => "ssh-pinned",
        SecurityChoice::SystemManaged => "system-managed",
    }
}

#[must_use]
pub(super) const fn security_label(choice: SecurityChoice) -> &'static str {
    match choice {
        SecurityChoice::Plaintext => "settings-remote-security-plaintext",
        SecurityChoice::TlsSystemRoots => "settings-remote-security-tls",
        SecurityChoice::TlsPinned => "settings-remote-security-tls-pin",
        SecurityChoice::SshKnownHosts => "settings-remote-security-host-key",
        SecurityChoice::SshPinned => "settings-remote-security-host-key-pin",
        SecurityChoice::SystemManaged => "settings-remote-security-system",
    }
}

#[must_use]
pub(super) fn security_policy(choice: SecurityChoice) -> SecurityPolicy {
    match choice {
        SecurityChoice::Plaintext => SecurityPolicy::PlaintextConfirmed,
        SecurityChoice::TlsSystemRoots => SecurityPolicy::Tls(TlsPolicy::SystemRoots),
        SecurityChoice::TlsPinned => {
            SecurityPolicy::Tls(TlsPolicy::PinnedSha256([0; TLS_PIN_BYTES]))
        }
        SecurityChoice::SshKnownHosts => SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
        SecurityChoice::SshPinned => {
            SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256([0; TLS_PIN_BYTES]))
        }
        SecurityChoice::SystemManaged => SecurityPolicy::SystemManaged,
    }
}

#[must_use]
pub(super) const fn security_selected(policy: &SecurityPolicy, choice: SecurityChoice) -> bool {
    matches!(
        (policy, choice),
        (
            SecurityPolicy::PlaintextConfirmed,
            SecurityChoice::Plaintext
        ) | (
            SecurityPolicy::Tls(TlsPolicy::SystemRoots),
            SecurityChoice::TlsSystemRoots
        ) | (
            SecurityPolicy::Tls(TlsPolicy::PinnedSha256(_)),
            SecurityChoice::TlsPinned
        ) | (
            SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts),
            SecurityChoice::SshKnownHosts
        ) | (
            SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(_)),
            SecurityChoice::SshPinned
        ) | (SecurityPolicy::SystemManaged, SecurityChoice::SystemManaged)
    )
}

/// No browse store connects through a proxy yet, so the editor offers none
/// (SYS-031); the proxy fields return when one does.
#[must_use]
pub(super) const fn supports_proxy(_protocol: RemoteProtocol) -> bool {
    false
}

#[must_use]
pub(super) const fn protocol_id(protocol: RemoteProtocol) -> &'static str {
    match protocol {
        RemoteProtocol::Ftp => "ftp",
        RemoteProtocol::Ftps => "ftps",
        RemoteProtocol::Sftp => "sftp",
        RemoteProtocol::WebDav => "webdav",
        RemoteProtocol::Http => "http",
        RemoteProtocol::Smb => "smb",
        RemoteProtocol::Nfs => "nfs",
    }
}

/// The protocols browsing can open (SYS-031). SMB and NFS return when they
/// have a browse store; HTTP when its store can list a folder.
pub(super) const PROTOCOLS: [RemoteProtocol; 4] = [
    RemoteProtocol::Ftp,
    RemoteProtocol::Ftps,
    RemoteProtocol::Sftp,
    RemoteProtocol::WebDav,
];

/// A connection whose secrets wait for the user to accept session-only use,
/// because the secret service is locked or missing.
pub(super) struct PendingRemoteSave {
    profile: ConnectionProfile,
    secrets: Vec<(ConnectionId, SecretBuffer)>,
    state: musheen_desktop::SecretServiceState,
}

/// A secret saved with its connection in the editor, which Apply stores where
/// the user chose before it writes the settings file.
pub(super) struct PendingSecret {
    secret: SecretBuffer,
    label: String,
    storage: musheen_desktop::SecretStorage,
}

/// The secret service work of one Apply: the saved secrets the new settings
/// use, and the stored secrets the old settings used and the new ones do not.
pub(super) struct SecretWork {
    writes: Vec<(ConnectionId, PendingSecret)>,
    retired: Vec<ConnectionId>,
}

/// Stores each saved secret, in order; the first failure stops Apply before
/// it writes the settings file.
pub(super) async fn store_secret_writes(
    credentials: &musheen_desktop::RemoteCredentials,
    work: &SecretWork,
) -> Result<(), musheen_desktop::SecretError> {
    for (id, pending) in &work.writes {
        credentials
            .store(
                id,
                &pending.label,
                &pending.secret,
                pending.storage,
                CancellationToken::new(),
            )
            .await?;
    }
    Ok(())
}

/// Deletes the secrets the settings no longer use, and returns those the
/// secret service could not delete, for the next Apply to try again.
pub(super) async fn delete_retired_secrets(
    credentials: &musheen_desktop::RemoteCredentials,
    work: &SecretWork,
) -> Vec<ConnectionId> {
    let mut kept = Vec::new();
    for id in &work.retired {
        if credentials
            .remove(id, CancellationToken::new())
            .await
            .is_err()
        {
            kept.push(id.clone());
        }
    }
    kept
}

/// The Settings failure message for a secret Apply could not store.
pub(super) const fn secret_failure_key(error: musheen_desktop::SecretError) -> &'static str {
    match error {
        musheen_desktop::SecretError::SessionOnlyAvailable(
            musheen_desktop::SecretServiceState::Locked,
        ) => "settings-remote-keyring-locked",
        musheen_desktop::SecretError::SessionOnlyAvailable(_) => {
            "settings-remote-keyring-unavailable"
        }
        _ => "settings-remote-keyring-failed",
    }
}

/// Answers with the secrets typed in the editor ahead of the stored ones, so
/// Test connection uses what the user just typed.
struct EditorCredentials {
    typed: BTreeMap<ConnectionId, SecretBuffer>,
    stored: Arc<musheen_desktop::RemoteCredentials>,
}

impl CredentialResolver for EditorCredentials {
    fn resolve<'a>(
        &'a self,
        reference: &'a CredentialReference,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<SecretBuffer, musheen_desktop::RemoteErrorCategory>> {
        match self.typed.get(reference.connection_id()) {
            Some(secret) => {
                let secret = SecretBuffer::new(secret.expose_secret(<[u8]>::to_vec));
                Box::pin(async move { Ok(secret) })
            }
            None => self.stored.resolve(reference, cancellation),
        }
    }
}

/// The secret IDs the connections saved in `document` use, or `None` when
/// its connections cannot be read, so that nothing is deleted on a guess.
fn referenced_secrets(document: &SettingsDocument) -> Option<BTreeSet<ConnectionId>> {
    let Some(encoded) = document.value("remote.connections") else {
        return Some(BTreeSet::new());
    };
    let profiles = ConnectionProfiles::import(&encoded).ok()?;
    Some(
        profiles
            .profiles()
            .iter()
            .flat_map(|profile| {
                profile
                    .credential()
                    .cloned()
                    .into_iter()
                    .chain(profile.stored_key_reference())
                    .chain(profile.proxy().and_then(ProxySettings::credential).cloned())
            })
            .map(|reference| reference.connection_id().clone())
            .collect(),
    )
}

impl super::SettingsWindow {
    /// The login the editor shows: an SFTP connection's chosen method, with
    /// its key file path; every other protocol logs in with a password.
    fn remote_login(&self, cx: &App) -> SshLogin {
        if self.remote_protocol != RemoteProtocol::Sftp {
            return SshLogin::Password;
        }
        match &self.remote_login {
            SshLogin::KeyFile { .. } => {
                let path = self.remote_inputs[LOGIN_KEY_PATH]
                    .read(cx)
                    .value()
                    .trim()
                    .to_owned();
                SshLogin::KeyFile {
                    path: (!path.is_empty()).then(|| path.into()),
                }
            }
            login => login.clone(),
        }
    }

    fn typed_password(&self, cx: &App) -> Option<SecretBuffer> {
        let value = self.remote_inputs[PROFILE_PASSWORD].read(cx).value();
        (!value.is_empty() && self.remote_login(cx) != SshLogin::Agent)
            .then(|| SecretBuffer::new(value.as_bytes().to_vec()))
    }

    fn typed_key(&self, cx: &App) -> Option<SecretBuffer> {
        let value = self.remote_key_text.read(cx).value();
        (!value.trim().is_empty() && self.remote_login(cx) == SshLogin::StoredKey)
            .then(|| SecretBuffer::new(value.as_bytes().to_vec()))
    }

    pub(super) fn remote_profile(&self, cx: &App) -> Result<ConnectionProfile, ()> {
        let id = ConnectionId::new(self.remote_inputs[PROFILE_ID].read(cx).value().trim())
            .map_err(|_| ())?;
        // `<id>.key` names a connection's stored private key in the secret
        // service, so no connection may take such an ID.
        if id.as_str().ends_with(".key") {
            return Err(());
        }
        let login = self.remote_login(cx);
        // A typed password is stored under the connection's ID; an empty field
        // keeps what the connection already stored; agent login keeps none.
        let credential = if login == SshLogin::Agent {
            None
        } else if self.typed_password(cx).is_some() {
            Some(CredentialReference::persistent(id.clone()))
        } else {
            self.remote_credential.clone()
        };
        build_profile(
            &self.remote_inputs,
            self.remote_protocol,
            self.remote_security.clone(),
            self.remote_proxy,
            credential,
            cx,
        )
        .and_then(|profile| profile.with_login(login))
        .map_err(|_| ())
    }

    /// The secrets typed for `profile`, each under the ID it is stored as.
    fn typed_secrets(
        &self,
        profile: &ConnectionProfile,
        cx: &App,
    ) -> Vec<(ConnectionId, SecretBuffer)> {
        let mut secrets = Vec::new();
        if let Some(password) = self.typed_password(cx) {
            secrets.push((profile.id().clone(), password));
        }
        if let (Some(key), Some(reference)) = (self.typed_key(cx), profile.stored_key_reference()) {
            secrets.push((reference.connection_id().clone(), key));
        }
        secrets
    }

    pub(super) fn invalidate_remote_test(&mut self) {
        if let Some(cancellation) = self.remote_test_cancellation.take() {
            cancellation.cancel();
        }
        self.remote_test_generation = self.remote_test_generation.wrapping_add(1);
        self.remote_testing = false;
        self.remote_test_report = None;
        self.remote_save_requirement = None;
        self.remote_validation_failed = false;
        self.remote_session_offer = None;
        self.remote_secret_error = None;
    }

    fn test_remote_connection(&mut self, cx: &mut Context<Self>) {
        if self.remote_testing {
            return;
        }
        let Ok(profile) = self.remote_profile(cx) else {
            self.remote_validation_failed = true;
            self.remote_save_requirement = None;
            cx.notify();
            return;
        };
        if let Some(cancellation) = self.remote_test_cancellation.take() {
            cancellation.cancel();
        }
        let mut typed = self
            .remote_pending_secrets
            .iter()
            .map(|(id, pending)| {
                let secret = SecretBuffer::new(pending.secret.expose_secret(<[u8]>::to_vec));
                (id.clone(), secret)
            })
            .collect::<BTreeMap<_, _>>();
        typed.extend(self.typed_secrets(&profile, cx));
        let credentials: Arc<dyn CredentialResolver> = Arc::new(EditorCredentials {
            typed,
            stored: Arc::clone(&self.remote_credentials),
        });
        let cancellation = musheen_core::CancellationToken::new();
        self.remote_test_cancellation = Some(cancellation.clone());
        self.remote_testing = true;
        self.remote_validation_failed = false;
        self.remote_save_requirement = None;
        self.remote_test_report = None;
        self.remote_test_generation = self.remote_test_generation.wrapping_add(1);
        let generation = self.remote_test_generation;
        let tester = self.remote_tester.clone();
        let work = cx.background_spawn(async move {
            let result = tester.test(&profile, credentials, cancellation).await;
            (profile, result)
        });
        cx.spawn(async move |this, cx| {
            let (profile, result) = work.await;
            cx.update(|cx| {
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        if this.remote_test_generation != generation {
                            return;
                        }
                        this.remote_testing = false;
                        this.remote_test_cancellation = None;
                        this.remote_test_report = Some(match result {
                            Ok(()) => TestReport::passed(&profile),
                            Err(error) => TestReport::failed(&profile, error),
                        });
                        cx.notify();
                    });
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn stage_remote_connection(
        &mut self,
        confirmed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.remote_saving {
            return;
        }
        let Ok(profile) = self.remote_profile(cx) else {
            self.remote_validation_failed = true;
            self.remote_save_requirement = None;
            cx.notify();
            return;
        };
        let Some(saved) = self.saved_remote_profiles() else {
            self.remote_validation_failed = true;
            cx.notify();
            return;
        };
        let previous = saved
            .profiles()
            .iter()
            .find(|saved| saved.id() == profile.id());
        let requirement = profile.save_requirement(
            previous,
            self.remote_test_report.as_ref(),
            if confirmed {
                SaveConfirmation::all()
            } else {
                SaveConfirmation::default()
            },
        );
        if requirement != SaveRequirement::Ready {
            self.remote_save_requirement = Some(requirement);
            self.remote_validation_failed = false;
            cx.notify();
            return;
        }
        let secrets = self.typed_secrets(&profile, cx);
        if secrets.is_empty() {
            self.commit_remote_profile(
                profile,
                secrets,
                musheen_desktop::SecretStorage::Persistent,
                window,
                cx,
            );
            return;
        }
        self.save_remote_with_secrets(profile, secrets, window, cx);
    }

    /// Saves the profile with the secrets typed for it, which Apply stores.
    /// A locked or missing secret service offers session-only use first;
    /// nothing reaches the secret service before Apply.
    fn save_remote_with_secrets(
        &mut self,
        profile: ConnectionProfile,
        secrets: Vec<(ConnectionId, SecretBuffer)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remote_saving = true;
        self.remote_session_offer = None;
        self.remote_secret_error = None;
        let credentials = Arc::clone(&self.remote_credentials);
        let work =
            cx.background_spawn(
                async move { credentials.availability(CancellationToken::new()).await },
            );
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.remote_saving = false;
                match result {
                    Ok(()) => this.commit_remote_profile(
                        profile,
                        secrets,
                        musheen_desktop::SecretStorage::Persistent,
                        window,
                        cx,
                    ),
                    Err(musheen_desktop::SecretError::SessionOnlyAvailable(state)) => {
                        this.remote_session_offer = Some(PendingRemoteSave {
                            profile,
                            secrets,
                            state,
                        });
                        cx.notify();
                    }
                    Err(error) => {
                        this.remote_secret_error = Some(error);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// Saves the waiting profile with its secrets marked for this session
    /// only: Apply keeps them in memory until Musheen exits.
    fn accept_session_only(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.remote_session_offer.take() else {
            return;
        };
        self.commit_remote_profile(
            pending.profile,
            pending.secrets,
            musheen_desktop::SecretStorage::SessionOnlyConfirmed,
            window,
            cx,
        );
    }

    fn saved_remote_profiles(&self) -> Option<ConnectionProfiles> {
        self.state
            .draft()
            .value("remote.connections")
            .and_then(|document| ConnectionProfiles::import(&document).ok())
    }

    /// Writes `profile` to the draft settings, replacing a saved connection
    /// with its ID, and keeps its typed secrets for Apply to store as
    /// `storage` says.
    fn commit_remote_profile(
        &mut self,
        profile: ConnectionProfile,
        secrets: Vec<(ConnectionId, SecretBuffer)>,
        storage: musheen_desktop::SecretStorage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(saved) = self.saved_remote_profiles() else {
            self.remote_validation_failed = true;
            cx.notify();
            return;
        };
        let mut profiles = saved.profiles().to_vec();
        if let Some(index) = profiles.iter().position(|saved| saved.id() == profile.id()) {
            profiles[index] = profile.clone();
        } else {
            profiles.push(profile.clone());
        }
        let encoded = ConnectionProfiles::new(profiles)
            .export()
            .expect("validated connection profiles serialize");
        if self.state.edit("remote.connections", &encoded).is_ok() {
            let label = format!("Musheen: {}", profile.name());
            for (id, secret) in secrets {
                self.remote_pending_secrets.insert(
                    id,
                    PendingSecret {
                        secret,
                        label: label.clone(),
                        storage,
                    },
                );
            }
            self.remote_save_requirement = None;
            self.remote_validation_failed = false;
            self.remote_credential = profile.credential().cloned();
            self.remote_inputs[PROFILE_PASSWORD]
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.remote_key_text
                .update(cx, |input, cx| input.set_value("", window, cx));
        } else {
            self.remote_validation_failed = true;
        }
        cx.notify();
    }

    /// Whether Apply has secret service work even when the settings are
    /// unchanged: a saved secret to store, or a deletion to try again.
    pub(super) fn has_remote_secret_work(&self) -> bool {
        !self.remote_pending_secrets.is_empty() || !self.remote_retired_secrets.is_empty()
    }

    /// The secret service work for applying `after` over the committed
    /// settings: the saved secrets `after` uses, and the secrets the committed
    /// connections used that `after` does not, with the deletions an earlier
    /// Apply could not finish. Nothing is deleted when either side's
    /// connections cannot be read.
    pub(super) fn remote_secret_work(&self, after: &SettingsDocument) -> SecretWork {
        let used = referenced_secrets(after);
        let writes = self
            .remote_pending_secrets
            .iter()
            .filter(|(id, _)| used.as_ref().is_some_and(|used| used.contains(*id)))
            .map(|(id, pending)| {
                let secret = SecretBuffer::new(pending.secret.expose_secret(<[u8]>::to_vec));
                (
                    id.clone(),
                    PendingSecret {
                        secret,
                        label: pending.label.clone(),
                        storage: pending.storage,
                    },
                )
            })
            .collect();
        let retired = match (referenced_secrets(&self.state.committed), used) {
            (Some(before), Some(after)) => before
                .union(&self.remote_retired_secrets)
                .filter(|id| !after.contains(*id))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        SecretWork { writes, retired }
    }

    /// Removes the connection being edited from the draft settings, with any
    /// secret saved for it and not yet applied. Its stored password and key
    /// leave the secret service when the removal is applied.
    fn remove_remote_connection(&mut self, cx: &mut Context<Self>) {
        let Ok(id) = ConnectionId::new(self.remote_inputs[PROFILE_ID].read(cx).value().trim())
        else {
            return;
        };
        let Some(saved) = self.saved_remote_profiles() else {
            return;
        };
        let profiles = saved
            .profiles()
            .iter()
            .filter(|profile| profile.id() != &id)
            .cloned()
            .collect::<Vec<_>>();
        let encoded = ConnectionProfiles::new(profiles)
            .export()
            .expect("validated connection profiles serialize");
        if self.state.edit("remote.connections", &encoded).is_err() {
            self.remote_validation_failed = true;
            cx.notify();
            return;
        }
        self.remote_pending_secrets.remove(&id);
        if let Ok(key) = ConnectionId::new(format!("{}.key", id.as_str())) {
            self.remote_pending_secrets.remove(&key);
        }
        self.remote_credential = None;
        self.remote_editor_open = false;
        self.invalidate_remote_test();
        cx.notify();
    }

    fn editing_saved_connection(&self, cx: &App) -> bool {
        let id = self.remote_inputs[PROFILE_ID].read(cx).value();
        self.saved_remote_profiles().is_some_and(|saved| {
            saved
                .profiles()
                .iter()
                .any(|profile| profile.id().as_str() == id.trim())
        })
    }

    fn select_remote_login(&mut self, login: SshLogin, cx: &mut Context<Self>) {
        self.remote_login = login;
        self.invalidate_remote_test();
        cx.notify();
    }

    fn select_remote_protocol(&mut self, protocol: RemoteProtocol, cx: &mut Context<Self>) {
        if self.remote_protocol == protocol {
            return;
        }
        self.remote_protocol = protocol;
        self.remote_security = default_security(protocol);
        if !supports_proxy(protocol) {
            self.remote_proxy = None;
        }
        self.invalidate_remote_test();
        cx.notify();
    }

    fn select_remote_security(&mut self, choice: SecurityChoice, cx: &mut Context<Self>) {
        self.remote_security = security_policy(choice);
        self.invalidate_remote_test();
        cx.notify();
    }

    fn select_remote_proxy(&mut self, proxy: Option<ProxyKind>, cx: &mut Context<Self>) {
        self.remote_proxy = proxy;
        self.invalidate_remote_test();
        cx.notify();
    }

    fn open_new_remote_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remote_protocol = RemoteProtocol::Sftp;
        self.remote_security = default_security(RemoteProtocol::Sftp);
        self.remote_proxy = None;
        self.remote_credential = None;
        self.remote_login = SshLogin::Password;
        self.remote_key_text
            .update(cx, |input, cx| input.set_value("", window, cx));
        for (key, value) in [
            (PROFILE_ID, ""),
            (PROFILE_NAME, ""),
            (PROFILE_HOST, ""),
            (PROFILE_PORT, ""),
            (PROFILE_PATH, "/"),
            (PROFILE_USERNAME, ""),
            (PROFILE_PASSWORD, ""),
            (LOGIN_KEY_PATH, ""),
            (SECURITY_PIN, ""),
            (PROXY_HOST, ""),
            (PROXY_PORT, ""),
            (PROXY_USERNAME, ""),
            (PROXY_CREDENTIAL, ""),
        ] {
            self.remote_inputs[key].update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.invalidate_remote_test();
        self.remote_editor_open = true;
        cx.notify();
    }

    fn load_remote_profile(
        &mut self,
        profile: ConnectionProfile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.remote_protocol = profile.protocol();
        self.remote_security = profile.security().clone();
        self.remote_proxy = profile.proxy().map(ProxySettings::kind);
        self.remote_credential = profile.credential().cloned();
        self.remote_login = profile.login().clone();
        self.remote_key_text
            .update(cx, |input, cx| input.set_value("", window, cx));
        let key_path = match profile.login() {
            SshLogin::KeyFile { path } => path.as_deref().unwrap_or_default().to_owned(),
            _ => String::new(),
        };
        let security_pin = match profile.security() {
            SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin))
            | SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(pin)) => hex_pin(pin),
            _ => String::new(),
        };
        let proxy = profile.proxy();
        let values = [
            (PROFILE_ID, profile.id().as_str().to_owned()),
            (PROFILE_NAME, profile.name().to_owned()),
            (PROFILE_HOST, profile.host().to_string()),
            (
                PROFILE_PORT,
                profile
                    .port()
                    .map_or_else(String::new, |port| port.to_string()),
            ),
            (PROFILE_PATH, profile.path().to_owned()),
            (
                PROFILE_USERNAME,
                profile.username().unwrap_or_default().to_owned(),
            ),
            (PROFILE_PASSWORD, String::new()),
            (LOGIN_KEY_PATH, key_path),
            (SECURITY_PIN, security_pin),
            (
                PROXY_HOST,
                proxy.map_or_else(String::new, |proxy| proxy.host().to_string()),
            ),
            (
                PROXY_PORT,
                proxy.map_or_else(String::new, |proxy| proxy.port().to_string()),
            ),
            (
                PROXY_USERNAME,
                proxy
                    .and_then(ProxySettings::username)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            (
                PROXY_CREDENTIAL,
                proxy
                    .and_then(ProxySettings::credential)
                    .and_then(CredentialReference::to_setting_value)
                    .unwrap_or_default(),
            ),
        ];
        for (key, value) in values {
            self.remote_inputs[key].update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.invalidate_remote_test();
        self.remote_editor_open = true;
        cx.notify();
    }

    fn render_remote_summary(&self, cx: &Context<Self>) -> AnyElement {
        let profiles = self
            .state
            .draft()
            .value("remote.connections")
            .and_then(|value| ConnectionProfiles::import(&value).ok())
            .unwrap_or_default();
        let value = self
            .state
            .draft()
            .value("remote.connections")
            .expect("connection schema value");
        let count = super::presentation::display_value(
            settings_schema()
                .iter()
                .find(|spec| spec.key == "remote.connections")
                .expect("connection schema key"),
            &value,
            &self.catalog,
        );
        let mut summary = div()
            .id("remote.connections")
            .test_support()
            .role(Role::Group)
            .aria_label(self.label("setting-remote-connections"))
            .flex()
            .flex_col()
            .gap_2()
            .child(super::window::status_label(
                "settings-remote-profile-count",
                count,
                Role::Status,
            ))
            .child(
                Button::new("settings-remote-add")
                    .label(self.label("settings-remote-add"))
                    .on_click(cx.listener(|settings, _, window, cx| {
                        settings.open_new_remote_profile(window, cx);
                    })),
            );
        for profile in profiles.profiles() {
            let profile = profile.clone();
            let id = format!("settings-remote-edit-{}", profile.id().as_str());
            let label = format!("{}: {}", self.label("settings-remote-edit"), profile.name());
            let refusal = profile.browse_refusal();
            let refused_id = format!("settings-remote-refused-{}", profile.id().as_str());
            summary = summary
                .child(
                    super::window::native_button(id, label, cx).on_click(cx.listener(
                        move |settings, _, window, cx| {
                            settings.load_remote_profile(profile.clone(), window, cx);
                        },
                    )),
                )
                .when_some(refusal, |summary, refusal| {
                    summary.child(super::window::status_label(
                        refused_id,
                        self.label(refusal_key(refusal)),
                        Role::Status,
                    ))
                });
        }
        summary.into_any_element()
    }

    fn render_remote_profile_fields(&self, cx: &App) -> AnyElement {
        let mut fields = div()
            .id("settings-remote-profile-list")
            .test_support()
            .role(Role::Group)
            .aria_label(self.label("settings-remote-editor"))
            .flex()
            .flex_col()
            .gap_2();
        for (id, label) in [
            (PROFILE_ID, "settings-remote-id"),
            (PROFILE_NAME, "settings-remote-name"),
            (PROFILE_HOST, "settings-remote-host"),
            (PROFILE_PORT, "settings-remote-port"),
            (PROFILE_PATH, "settings-remote-path"),
            (PROFILE_USERNAME, "settings-remote-username"),
        ] {
            fields = fields
                .child(super::window::observed_label(
                    format!("label-{id}"),
                    self.label(label),
                ))
                .child(
                    Input::new(&self.remote_inputs[id])
                        .id(id)
                        .disabled(self.blocked() || self.remote_testing)
                        .accessibility_id(id)
                        .aria_label(self.label(label)),
                );
        }
        let password_label = match self.remote_login(cx) {
            SshLogin::Agent => None,
            SshLogin::Password => Some("settings-remote-password"),
            SshLogin::KeyFile { .. } | SshLogin::StoredKey => Some("settings-remote-passphrase"),
        };
        if let Some(label) = password_label {
            fields = fields
                .child(super::window::observed_label(
                    format!("label-{PROFILE_PASSWORD}"),
                    self.label(label),
                ))
                .child(
                    Input::new(&self.remote_inputs[PROFILE_PASSWORD])
                        .id(PROFILE_PASSWORD)
                        .mask_toggle()
                        .disabled(self.blocked() || self.remote_testing || self.remote_saving)
                        .accessibility_id(PROFILE_PASSWORD)
                        .aria_label(self.label(label)),
                );
        }
        fields.into_any_element()
    }

    /// The sign-in methods of an SFTP connection, with the key file path or
    /// the pasted key the chosen method needs.
    fn render_remote_login(&self, cx: &Context<Self>) -> AnyElement {
        let mut methods = div()
            .id("settings-remote-login-options")
            .test_support()
            .flex()
            .flex_wrap()
            .gap_2()
            .role(Role::Group)
            .aria_label(self.label("settings-remote-login"));
        for (id, label, login) in [
            (
                "settings-remote-login-password",
                "settings-remote-login-password",
                SshLogin::Password,
            ),
            (
                "settings-remote-login-agent",
                "settings-remote-login-agent",
                SshLogin::Agent,
            ),
            (
                "settings-remote-login-key-file",
                "settings-remote-login-key-file",
                SshLogin::KeyFile { path: None },
            ),
            (
                "settings-remote-login-stored-key",
                "settings-remote-login-stored-key",
                SshLogin::StoredKey,
            ),
        ] {
            let selected =
                std::mem::discriminant(&self.remote_login) == std::mem::discriminant(&login);
            methods = methods.child(
                super::window::native_button(id, self.label(label), cx)
                    .selected(selected)
                    .disabled(self.blocked() || self.remote_testing || self.remote_saving)
                    .on_click(cx.listener(move |settings, _, _, cx| {
                        settings.select_remote_login(login.clone(), cx);
                    })),
            );
        }
        let mut login = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(super::window::observed_label(
                "settings-remote-login-label",
                self.label("settings-remote-login"),
            ))
            .child(methods);
        match self.remote_login {
            SshLogin::KeyFile { .. } => {
                login = login
                    .child(super::window::observed_label(
                        format!("label-{LOGIN_KEY_PATH}"),
                        self.label("settings-remote-key-path"),
                    ))
                    .child(
                        Input::new(&self.remote_inputs[LOGIN_KEY_PATH])
                            .id(LOGIN_KEY_PATH)
                            .disabled(self.blocked() || self.remote_testing)
                            .accessibility_id(LOGIN_KEY_PATH)
                            .aria_label(self.label("settings-remote-key-path")),
                    );
            }
            SshLogin::StoredKey => {
                login = login
                    .child(super::window::observed_label(
                        format!("label-{LOGIN_KEY_TEXT}"),
                        self.label("settings-remote-key-text"),
                    ))
                    .child(
                        div().id(LOGIN_KEY_TEXT).test_support().child(
                            Textarea::new(&self.remote_key_text)
                                .disabled(
                                    self.blocked() || self.remote_testing || self.remote_saving,
                                )
                                .accessibility_id(LOGIN_KEY_TEXT)
                                .aria_label(self.label("settings-remote-key-text")),
                        ),
                    );
            }
            SshLogin::Password | SshLogin::Agent => {}
        }
        login.into_any_element()
    }

    fn render_remote_protocols(&self, cx: &Context<Self>) -> AnyElement {
        let mut protocols = div()
            .id("settings-remote-protocol-options")
            .test_support()
            .flex()
            .flex_wrap()
            .gap_2()
            .role(Role::Group)
            .aria_label(self.label("settings-remote-protocol"));
        for protocol in PROTOCOLS {
            let id = protocol_id(protocol);
            protocols = protocols.child(
                super::window::native_button(
                    format!("settings-remote-protocol-{id}"),
                    self.label(&format!("settings-value-{id}")),
                    cx,
                )
                .selected(self.remote_protocol == protocol)
                .disabled(self.blocked() || self.remote_testing)
                .on_click(cx.listener(move |settings, _, _, cx| {
                    settings.select_remote_protocol(protocol, cx);
                })),
            );
        }
        protocols.into_any_element()
    }

    fn render_remote_security(&self, cx: &Context<Self>) -> AnyElement {
        let mut options = div()
            .id("settings-remote-security-options")
            .test_support()
            .flex()
            .flex_wrap()
            .gap_2()
            .role(Role::Group)
            .aria_label(self.label("settings-remote-security"));
        for choice in security_choices(self.remote_protocol) {
            let choice = *choice;
            options = options.child(
                super::window::native_button(
                    format!("settings-remote-security-{}", security_id(choice)),
                    self.label(security_label(choice)),
                    cx,
                )
                .selected(security_selected(&self.remote_security, choice))
                .disabled(self.blocked() || self.remote_testing)
                .on_click(cx.listener(move |settings, _, _, cx| {
                    settings.select_remote_security(choice, cx);
                })),
            );
        }
        let uses_pin = matches!(
            self.remote_security,
            SecurityPolicy::Tls(TlsPolicy::PinnedSha256(_))
                | SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(_))
        );
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(super::window::observed_label(
                "settings-remote-security-label",
                self.label("settings-remote-security"),
            ))
            .child(options)
            .when(uses_pin, |security| {
                security
                    .child(super::window::observed_label(
                        "label-remote-security-pin",
                        self.label("settings-remote-security-pin"),
                    ))
                    .child(
                        Input::new(&self.remote_inputs[SECURITY_PIN])
                            .id(SECURITY_PIN)
                            .disabled(self.blocked() || self.remote_testing)
                            .accessibility_id(SECURITY_PIN)
                            .aria_label(self.label("settings-remote-security-pin")),
                    )
            })
            .into_any_element()
    }

    fn render_remote_proxy(&self, cx: &Context<Self>) -> AnyElement {
        let mut options = div()
            .id("settings-remote-proxy-options")
            .test_support()
            .flex()
            .flex_wrap()
            .gap_2()
            .role(Role::Group)
            .aria_label(self.label("settings-remote-proxy"))
            .child(
                super::window::native_button(
                    "settings-remote-proxy-none",
                    self.label("settings-remote-proxy-none"),
                    cx,
                )
                .selected(self.remote_proxy.is_none())
                .disabled(self.blocked() || self.remote_testing)
                .on_click(cx.listener(|settings, _, _, cx| {
                    settings.select_remote_proxy(None, cx);
                })),
            );
        for (kind, id, label) in [
            (ProxyKind::Socks5, "socks5", "settings-remote-proxy-socks5"),
            (
                ProxyKind::HttpConnect,
                "http-connect",
                "settings-remote-proxy-http",
            ),
        ] {
            options = options.child(
                super::window::native_button(
                    format!("settings-remote-proxy-{id}"),
                    self.label(label),
                    cx,
                )
                .selected(self.remote_proxy == Some(kind))
                .disabled(self.blocked() || self.remote_testing)
                .on_click(cx.listener(move |settings, _, _, cx| {
                    settings.select_remote_proxy(Some(kind), cx);
                })),
            );
        }
        let mut fields = div().flex().flex_col().gap_2();
        for (id, label) in [
            (PROXY_HOST, "settings-remote-proxy-host"),
            (PROXY_PORT, "settings-remote-proxy-port"),
            (PROXY_USERNAME, "settings-remote-proxy-username"),
            (PROXY_CREDENTIAL, "settings-remote-proxy-credential"),
        ] {
            fields = fields
                .child(super::window::observed_label(
                    format!("label-{id}"),
                    self.label(label),
                ))
                .child(
                    Input::new(&self.remote_inputs[id])
                        .id(id)
                        .disabled(self.blocked() || self.remote_testing)
                        .accessibility_id(id)
                        .aria_label(self.label(label)),
                );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(super::window::observed_label(
                "settings-remote-proxy-label",
                self.label("settings-remote-proxy"),
            ))
            .child(options)
            .when(self.remote_proxy.is_some(), |proxy| proxy.child(fields))
            .into_any_element()
    }

    fn render_remote_feedback(&self, cx: &Context<Self>) -> AnyElement {
        let mut feedback = div().flex().flex_col().gap_1();
        if self.remote_testing {
            feedback = feedback.child(super::window::status_label(
                "settings-remote-testing",
                self.label("settings-remote-testing"),
                Role::Status,
            ));
        } else if let Some(report) = &self.remote_test_report {
            let (id, label, role) = if report.error().is_some() {
                (
                    "settings-remote-test-failed",
                    "settings-remote-test-failed",
                    Role::Alert,
                )
            } else {
                (
                    "settings-remote-test-passed",
                    "settings-remote-test-passed",
                    Role::Status,
                )
            };
            feedback = feedback.child(super::window::status_label(id, self.label(label), role));
            if let Some(error) = report.error() {
                let profile = self.remote_profile(cx).ok();
                feedback = feedback.child(super::window::status_label(
                    "settings-remote-test-cause",
                    self.label(test_cause_key(profile.as_ref(), error.category())),
                    Role::Alert,
                ));
            }
        }
        if self.remote_saving {
            feedback = feedback.child(super::window::status_label(
                "settings-remote-saving",
                self.label("settings-remote-saving"),
                Role::Status,
            ));
        }
        if let Some(offer) = &self.remote_session_offer {
            let reason = if offer.state == musheen_desktop::SecretServiceState::Locked {
                "settings-remote-keyring-locked"
            } else {
                "settings-remote-keyring-unavailable"
            };
            feedback = feedback
                .child(super::window::status_label(
                    "settings-remote-keyring-refused",
                    self.label(reason),
                    Role::Alert,
                ))
                .child(
                    Button::new("settings-remote-session-only")
                        .label(self.label("settings-remote-session-only"))
                        .on_click(cx.listener(|settings, _, window, cx| {
                            settings.accept_session_only(window, cx);
                        })),
                );
        }
        if self.remote_secret_error.is_some() {
            feedback = feedback.child(super::window::status_label(
                "settings-remote-keyring-failed",
                self.label("settings-remote-keyring-failed"),
                Role::Alert,
            ));
        }
        if let Some(refusal) = self
            .remote_profile(cx)
            .ok()
            .and_then(|profile| profile.browse_refusal())
        {
            feedback = feedback.child(super::window::status_label(
                "settings-remote-refused",
                self.label(refusal_key(refusal)),
                Role::Alert,
            ));
        }
        if self.remote_validation_failed
            || self.remote_save_requirement == Some(SaveRequirement::TestRequired)
        {
            feedback = feedback.child(super::window::status_label(
                "settings-remote-validation-error",
                self.label(if self.remote_validation_failed {
                    "settings-remote-invalid"
                } else {
                    "settings-remote-test-required"
                }),
                Role::Alert,
            ));
        }
        feedback.into_any_element()
    }
}

/// The message that names the cause of a failed connection test.
const fn cause_key(category: RemoteErrorCategory) -> &'static str {
    match category {
        RemoteErrorCategory::InvalidProfile => "settings-remote-cause-invalid-profile",
        RemoteErrorCategory::Authentication => "settings-remote-cause-authentication",
        RemoteErrorCategory::HostKey => "settings-remote-cause-host-key",
        RemoteErrorCategory::Tls => "settings-remote-cause-tls",
        RemoteErrorCategory::Network => "settings-remote-cause-network",
        RemoteErrorCategory::Protocol => "settings-remote-cause-protocol",
        RemoteErrorCategory::Redirect => "settings-remote-cause-redirect",
        RemoteErrorCategory::Timeout => "settings-remote-cause-timeout",
        RemoteErrorCategory::Cancelled => "settings-remote-cause-cancelled",
        RemoteErrorCategory::Saturated => "settings-remote-cause-saturated",
        RemoteErrorCategory::Unavailable => "settings-remote-cause-unavailable",
        RemoteErrorCategory::Retryable => "settings-remote-cause-retryable",
        RemoteErrorCategory::Conflict => "settings-remote-cause-conflict",
        RemoteErrorCategory::Quota => "settings-remote-cause-quota",
        RemoteErrorCategory::Permission => "settings-remote-cause-permission",
        RemoteErrorCategory::Unsupported => "settings-remote-cause-unsupported",
        RemoteErrorCategory::Permanent => "settings-remote-cause-permanent",
        RemoteErrorCategory::KeyNeedsAgent => "settings-remote-cause-key-needs-agent",
        RemoteErrorCategory::CredentialUnavailable => {
            "settings-remote-cause-credential-unavailable"
        }
        RemoteErrorCategory::KeyUnreadable => "settings-remote-cause-key-unreadable",
        RemoteErrorCategory::KeyUndecodable => "settings-remote-cause-key-undecodable",
        RemoteErrorCategory::NoAgent => "settings-remote-cause-no-agent",
        RemoteErrorCategory::SshConfigUnreadable => "settings-remote-cause-ssh-config-unreadable",
        RemoteErrorCategory::SshConfigMatch => "settings-remote-cause-ssh-config-match",
        RemoteErrorCategory::UnknownHost => "settings-remote-cause-unknown-host",
    }
}

/// The message for a failed test of `profile`. An FTPS connection on port
/// 990 that gets no answer most likely wants implicit TLS, which Musheen
/// does not speak; the message says so and names the explicit TLS port.
fn test_cause_key(
    profile: Option<&ConnectionProfile>,
    category: RemoteErrorCategory,
) -> &'static str {
    let implicit_tls_port = profile.is_some_and(|profile| {
        profile.protocol() == RemoteProtocol::Ftps && profile.port() == Some(990)
    });
    if implicit_tls_port
        && matches!(
            category,
            RemoteErrorCategory::Timeout
                | RemoteErrorCategory::Network
                | RemoteErrorCategory::Protocol
                | RemoteErrorCategory::Tls
                | RemoteErrorCategory::Retryable
        )
    {
        "settings-remote-cause-implicit-tls"
    } else {
        cause_key(category)
    }
}

/// The message that says why browsing cannot open a saved connection.
const fn refusal_key(refusal: BrowseRefusal) -> &'static str {
    match refusal {
        BrowseRefusal::Protocol => "settings-remote-refused-protocol",
        BrowseRefusal::Proxy => "settings-remote-refused-proxy",
        BrowseRefusal::CertificatePin => "settings-remote-refused-pin",
    }
}

fn hex_pin(pin: &[u8; TLS_PIN_BYTES]) -> String {
    pin.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) fn render_editor(
    settings: &super::SettingsWindow,
    cx: &Context<super::SettingsWindow>,
) -> AnyElement {
    if !settings.remote_editor_open {
        return settings.render_remote_summary(cx);
    }
    let requires_confirmation = settings.remote_save_requirement.is_some_and(|requirement| {
        !matches!(
            requirement,
            musheen_desktop::SaveRequirement::Ready
                | musheen_desktop::SaveRequirement::TestRequired
        )
    });
    let credential = if settings.remote_credential.is_some() {
        "settings-value-credential-stored"
    } else {
        "settings-value-none"
    };
    div()
        .id("remote.connections")
        .test_support()
        .role(Role::Group)
        .aria_label(settings.label("setting-remote-connections"))
        .flex()
        .flex_col()
        .gap_2()
        .child(
            super::window::native_button(
                "settings-remote-close-editor",
                settings.label("settings-remote-close-editor"),
                cx,
            )
            .on_click(cx.listener(|settings, _, _, cx| {
                settings.remote_editor_open = false;
                settings.invalidate_remote_test();
                cx.notify();
            })),
        )
        .when(requires_confirmation, |editor| {
            editor
                .child(super::window::status_label(
                    "settings-remote-confirm-warning",
                    settings.label("settings-remote-confirm-warning"),
                    Role::Alert,
                ))
                .child(
                    Button::new("settings-remote-confirm-save")
                        .label(settings.label("settings-remote-confirm-save"))
                        .on_click(cx.listener(|settings, _, window, cx| {
                            settings.stage_remote_connection(true, window, cx);
                        })),
                )
        })
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .child(
                    super::window::native_button(
                        "settings-remote-test",
                        settings.label("settings-remote-test"),
                        cx,
                    )
                    .disabled(settings.blocked() || settings.remote_testing)
                    .on_click(cx.listener(|settings, _, _, cx| {
                        settings.test_remote_connection(cx);
                    })),
                )
                .child(
                    Button::new("settings-remote-save")
                        .label(settings.label("settings-remote-save"))
                        .primary()
                        .disabled(
                            settings.blocked() || settings.remote_testing || settings.remote_saving,
                        )
                        .on_click(cx.listener(|settings, _, window, cx| {
                            settings.stage_remote_connection(false, window, cx);
                        })),
                )
                .when(settings.editing_saved_connection(cx), |buttons| {
                    buttons.child(
                        super::window::native_button(
                            "settings-remote-remove",
                            settings.label("settings-remote-remove"),
                            cx,
                        )
                        .disabled(settings.blocked() || settings.remote_saving)
                        .on_click(cx.listener(|settings, _, _, cx| {
                            settings.remove_remote_connection(cx);
                        })),
                    )
                }),
        )
        .child(settings.render_remote_feedback(cx))
        .child(settings.render_remote_profile_fields(cx))
        .child(super::window::observed_label(
            "settings-remote-protocol-label",
            settings.label("settings-remote-protocol"),
        ))
        .child(settings.render_remote_protocols(cx))
        .child(settings.render_remote_security(cx))
        .when(settings.remote_protocol == RemoteProtocol::Sftp, |editor| {
            editor.child(settings.render_remote_login(cx))
        })
        .when(supports_proxy(settings.remote_protocol), |editor| {
            editor.child(settings.render_remote_proxy(cx))
        })
        .child(super::window::status_label(
            "settings-remote-credential",
            settings.label(credential),
            Role::Status,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests;
