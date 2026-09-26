use gpui_kit::component::Disableable;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, AppContext, Context, Entity, Role, TestSupportExt, Window, div};
use musheen_desktop::{
    ConnectionId, ConnectionProfile, ConnectionProfiles, CredentialReference, HostKeyPolicy,
    ProxyKind, ProxySettings, RemoteError, RemoteErrorCategory, RemoteHost, RemoteProtocol,
    SaveConfirmation, SaveRequirement, SecurityPolicy, SettingSpec, SettingsPage, TLS_PIN_BYTES,
    TestReport, TlsPolicy, settings_schema,
};
use std::collections::BTreeMap;

pub const PROFILE_ID: &str = "remote-profile-id";
pub const PROFILE_NAME: &str = "remote-profile-name";
pub const PROFILE_HOST: &str = "remote-profile-host";
pub const PROFILE_PORT: &str = "remote-profile-port";
pub const PROFILE_PATH: &str = "remote-profile-path";
pub const PROFILE_USERNAME: &str = "remote-profile-username";
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

pub use musheen_desktop::ProfileConnectionTest as ConnectionTestService;

/// The connection test a Settings window runs unless a test supplies its own.
pub(crate) fn default_connection_tester() -> std::sync::Arc<dyn ConnectionTestService> {
    std::sync::Arc::new(musheen_desktop::ProfileConnectionTester::default())
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
        (SECURITY_PIN, ""),
        (PROXY_HOST, ""),
        (PROXY_PORT, ""),
        (PROXY_USERNAME, ""),
        (PROXY_CREDENTIAL, ""),
    ]
    .into_iter()
    .map(|(id, value)| {
        (
            id,
            cx.new(|cx| InputState::new(window, cx).default_value(value)),
        )
    })
    .collect()
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
        RemoteProtocol::Ftps => &[TlsSystemRoots, TlsPinned],
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

#[must_use]
pub(super) const fn supports_proxy(protocol: RemoteProtocol) -> bool {
    !matches!(protocol, RemoteProtocol::Smb | RemoteProtocol::Nfs)
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

pub(super) const PROTOCOLS: [RemoteProtocol; 7] = [
    RemoteProtocol::Ftp,
    RemoteProtocol::Ftps,
    RemoteProtocol::Sftp,
    RemoteProtocol::WebDav,
    RemoteProtocol::Http,
    RemoteProtocol::Smb,
    RemoteProtocol::Nfs,
];

impl super::SettingsWindow {
    pub(super) fn remote_profile(&self, cx: &App) -> Result<ConnectionProfile, ()> {
        build_profile(
            &self.remote_inputs,
            self.remote_protocol,
            self.remote_security.clone(),
            self.remote_proxy,
            self.remote_credential.clone(),
            cx,
        )
        .map_err(|_| ())
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
            let result = tester.test(&profile, cancellation).await;
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

    fn stage_remote_connection(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        let Ok(profile) = self.remote_profile(cx) else {
            self.remote_validation_failed = true;
            self.remote_save_requirement = None;
            cx.notify();
            return;
        };
        let Some(document) = self.state.draft().value("remote.connections") else {
            self.remote_validation_failed = true;
            cx.notify();
            return;
        };
        let Ok(saved) = ConnectionProfiles::import(&document) else {
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

        let mut profiles = saved.profiles().to_vec();
        if let Some(index) = profiles.iter().position(|saved| saved.id() == profile.id()) {
            profiles[index] = profile;
        } else {
            profiles.push(profile);
        }
        let encoded = ConnectionProfiles::new(profiles)
            .export()
            .expect("validated connection profiles serialize");
        if self.state.edit("remote.connections", &encoded).is_ok() {
            self.remote_save_requirement = None;
            self.remote_validation_failed = false;
        } else {
            self.remote_validation_failed = true;
        }
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
        self.remote_credential = self
            .state
            .draft()
            .value("remote.credential")
            .filter(|value| !value.is_empty())
            .and_then(|value| CredentialReference::from_setting_value(&value).ok());
        for (key, value) in [
            (PROFILE_ID, ""),
            (PROFILE_NAME, ""),
            (PROFILE_HOST, ""),
            (PROFILE_PORT, ""),
            (PROFILE_PATH, "/"),
            (PROFILE_USERNAME, ""),
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
            summary = summary.child(super::window::native_button(id, label, cx).on_click(
                cx.listener(move |settings, _, window, cx| {
                    settings.load_remote_profile(profile.clone(), window, cx);
                }),
            ));
        }
        summary.into_any_element()
    }

    fn render_remote_profile_fields(&self) -> AnyElement {
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
        fields.into_any_element()
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

    fn render_remote_feedback(&self) -> AnyElement {
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
    let credential = settings
        .state
        .draft()
        .value("remote.credential")
        .filter(|value| !value.is_empty())
        .map_or(
            "settings-value-none",
            |_| "settings-value-credential-stored",
        );
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
                        .on_click(cx.listener(|settings, _, _, cx| {
                            settings.stage_remote_connection(true, cx);
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
                        .disabled(settings.blocked() || settings.remote_testing)
                        .on_click(cx.listener(|settings, _, _, cx| {
                            settings.stage_remote_connection(false, cx);
                        })),
                ),
        )
        .child(settings.render_remote_feedback())
        .child(settings.render_remote_profile_fields())
        .child(super::window::observed_label(
            "settings-remote-protocol-label",
            settings.label("settings-remote-protocol"),
        ))
        .child(settings.render_remote_protocols(cx))
        .child(settings.render_remote_security(cx))
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
