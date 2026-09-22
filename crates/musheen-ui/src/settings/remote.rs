use gpui_kit::component::input::InputState;
use gpui_kit::{AppContext, Context, Entity, Window};
use musheen_desktop::{
    ConnectionId, ConnectionProfile, CredentialReference, HostKeyPolicy, ProxyKind, ProxySettings,
    RemoteError, RemoteErrorCategory, RemoteHost, RemoteProtocol, SecurityPolicy, SettingSpec,
    SettingsPage, TLS_PIN_BYTES, TlsPolicy,
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
    let value = |key| inputs[key].read(cx).value().trim().to_owned();
    let host = RemoteHost::new(protocol, value(PROFILE_HOST))?;
    let port = match value(PROFILE_PORT) {
        value if value.is_empty() => None,
        value => Some(value.parse::<u16>().map_err(|_| {
            RemoteError::new(
                protocol,
                RemoteErrorCategory::InvalidProfile,
                Some(host.clone()),
            )
        })?),
    };
    let username = value(PROFILE_USERNAME);
    let security = configured_security(protocol, security, &value(SECURITY_PIN), &host)?;
    let proxy = proxy_kind
        .map(|kind| {
            let proxy_host = RemoteHost::new(protocol, value(PROXY_HOST))?;
            let proxy_port = value(PROXY_PORT).parse::<u16>().map_err(|_| {
                RemoteError::new(
                    protocol,
                    RemoteErrorCategory::InvalidProfile,
                    Some(proxy_host.clone()),
                )
            })?;
            let proxy_username = value(PROXY_USERNAME);
            let proxy_credential = match value(PROXY_CREDENTIAL) {
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
        ConnectionId::new(value(PROFILE_ID)).map_err(|_| {
            RemoteError::new(
                protocol,
                RemoteErrorCategory::InvalidProfile,
                Some(host.clone()),
            )
        })?,
        value(PROFILE_NAME),
        protocol,
        host,
        port,
        value(PROFILE_PATH),
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
