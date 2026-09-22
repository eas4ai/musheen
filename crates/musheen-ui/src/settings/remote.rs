use gpui_kit::component::input::InputState;
use gpui_kit::{AppContext, Context, Entity, Window};
use musheen_core::{BoxFuture, CancellationToken};
use musheen_desktop::{
    ConnectionId, ConnectionProfile, CredentialReference, HostKeyPolicy, RemoteError,
    RemoteErrorCategory, RemoteHost, RemoteProtocol, SecurityPolicy, SettingSpec, SettingsPage,
    TlsPolicy,
};
use std::collections::BTreeMap;

pub const PROFILE_ID: &str = "remote-profile-id";
pub const PROFILE_NAME: &str = "remote-profile-name";
pub const PROFILE_HOST: &str = "remote-profile-host";
pub const PROFILE_PORT: &str = "remote-profile-port";
pub const PROFILE_PATH: &str = "remote-profile-path";
pub const PROFILE_USERNAME: &str = "remote-profile-username";

pub trait ConnectionTestService: Send + Sync + 'static {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableConnectionTestService;

impl ConnectionTestService for UnavailableConnectionTestService {
    fn test<'a>(
        &'a self,
        profile: &'a ConnectionProfile,
        _cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<(), RemoteError>> {
        Box::pin(async move {
            Err(RemoteError::new(
                profile.protocol(),
                RemoteErrorCategory::Unavailable,
                Some(profile.host().clone()),
            ))
        })
    }
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
    credential: Option<CredentialReference>,
    cx: &gpui_kit::App,
) -> Result<ConnectionProfile, RemoteError> {
    let value = |key| inputs[key].read(cx).value().trim().to_owned();
    let host = RemoteHost::new(value(PROFILE_HOST))?;
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
        None,
    )
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
