use super::{RemoteError, RemoteErrorCategory};
use crate::{ConnectionId, CredentialReference, SecretPersistence};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

const PROFILE_VERSION: u32 = 1;
const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_PROFILES: usize = 256;
pub const TLS_PIN_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteProtocol {
    Ftp,
    Ftps,
    Sftp,
    WebDav,
    Http,
    Smb,
    Nfs,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RemoteHost(Box<str>);

impl RemoteHost {
    pub fn new(protocol: RemoteProtocol, value: impl Into<Box<str>>) -> Result<Self, RemoteError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 253
            && !value.contains("://")
            && !value
                .bytes()
                .any(|byte| byte.is_ascii_control() || b"/@\\?#".contains(&byte))
            && !value.chars().any(char::is_whitespace)
            && value.trim() == value.as_ref();
        valid
            .then_some(Self(value))
            .ok_or_else(|| RemoteError::new(protocol, RemoteErrorCategory::InvalidProfile, None))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for RemoteHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("RemoteHost").field(&self.0).finish()
    }
}

impl fmt::Display for RemoteHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "policy", content = "sha256")]
pub enum TlsPolicy {
    SystemRoots,
    PinnedSha256([u8; TLS_PIN_BYTES]),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "policy", content = "sha256")]
pub enum HostKeyPolicy {
    KnownHosts,
    PinnedSha256([u8; TLS_PIN_BYTES]),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "policy")]
pub enum SecurityPolicy {
    PlaintextConfirmed,
    Tls(TlsPolicy),
    Ssh(HostKeyPolicy),
    SystemManaged,
}

impl SecurityPolicy {
    fn is_compatible(&self, protocol: RemoteProtocol) -> bool {
        match protocol {
            RemoteProtocol::Ftp => matches!(self, Self::PlaintextConfirmed),
            RemoteProtocol::Ftps => matches!(self, Self::Tls(_)),
            RemoteProtocol::Sftp => matches!(self, Self::Ssh(_)),
            RemoteProtocol::WebDav | RemoteProtocol::Http => {
                matches!(self, Self::PlaintextConfirmed | Self::Tls(_))
            }
            RemoteProtocol::Smb | RemoteProtocol::Nfs => matches!(self, Self::SystemManaged),
        }
    }

    fn requires_confirmation(&self, previous: &Self) -> bool {
        if self == previous {
            return false;
        }
        match previous {
            Self::PlaintextConfirmed => false,
            Self::Tls(TlsPolicy::SystemRoots) => matches!(self, Self::PlaintextConfirmed),
            Self::Ssh(HostKeyPolicy::KnownHosts) => matches!(self, Self::PlaintextConfirmed),
            Self::Tls(TlsPolicy::PinnedSha256(_))
            | Self::Ssh(HostKeyPolicy::PinnedSha256(_))
            | Self::SystemManaged => true,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProxyKind {
    Socks5,
    HttpConnect,
}

#[derive(Clone, Eq, PartialEq)]
pub struct ProxySettings {
    kind: ProxyKind,
    host: RemoteHost,
    port: u16,
    username: Option<Box<str>>,
    credential: Option<CredentialReference>,
}

impl ProxySettings {
    pub fn new(
        protocol: RemoteProtocol,
        kind: ProxyKind,
        host: RemoteHost,
        port: u16,
        username: Option<impl Into<Box<str>>>,
        credential: Option<CredentialReference>,
    ) -> Result<Self, RemoteError> {
        let username = username.map(Into::into);
        if port == 0
            || credential
                .as_ref()
                .is_some_and(|reference| reference.persistence() != SecretPersistence::Persistent)
            || username.as_deref().is_some_and(|value| {
                value.is_empty()
                    || value.len() > 256
                    || value.bytes().any(|byte| byte.is_ascii_control())
            })
        {
            return Err(RemoteError::new(
                protocol,
                RemoteErrorCategory::InvalidProfile,
                Some(host),
            ));
        }
        Ok(Self {
            kind,
            host,
            port,
            username,
            credential,
        })
    }

    #[must_use]
    pub const fn kind(&self) -> ProxyKind {
        self.kind
    }

    #[must_use]
    pub fn host(&self) -> &RemoteHost {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }

    #[must_use]
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    #[must_use]
    pub fn credential(&self) -> Option<&CredentialReference> {
        self.credential.as_ref()
    }
}

impl fmt::Debug for ProxySettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProxySettings")
            .field("kind", &self.kind)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username.as_ref().map(|_| "<redacted>"))
            .field("credential", &self.credential.as_ref().map(|_| "<stored>"))
            .finish()
    }
}

#[derive(Clone, Eq, PartialEq)]
pub struct ConnectionProfile {
    id: ConnectionId,
    name: Box<str>,
    protocol: RemoteProtocol,
    host: RemoteHost,
    port: Option<u16>,
    path: Box<str>,
    username: Option<Box<str>>,
    credential: Option<CredentialReference>,
    security: SecurityPolicy,
    proxy: Option<ProxySettings>,
}

impl ConnectionProfile {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: ConnectionId,
        name: impl Into<Box<str>>,
        protocol: RemoteProtocol,
        host: RemoteHost,
        port: Option<u16>,
        path: impl Into<Box<str>>,
        username: Option<impl Into<Box<str>>>,
        credential: Option<CredentialReference>,
        security: SecurityPolicy,
        proxy: Option<ProxySettings>,
    ) -> Result<Self, RemoteError> {
        let name = name.into();
        let path = path.into();
        let username = username.map(Into::into);
        let valid_text = |value: &str, maximum: usize| {
            !value.is_empty()
                && value.len() <= maximum
                && !value.bytes().any(|byte| byte.is_ascii_control())
        };
        let credentials_are_persistent = credential
            .as_ref()
            .is_none_or(|reference| reference.persistence() == SecretPersistence::Persistent)
            && proxy
                .as_ref()
                .and_then(ProxySettings::credential)
                .is_none_or(|reference| reference.persistence() == SecretPersistence::Persistent);
        let valid = valid_text(&name, 128)
            && path.starts_with('/')
            && valid_text(&path, 4096)
            && port != Some(0)
            && username.as_deref().is_none_or(|value| {
                value.len() <= 256 && !value.bytes().any(|b| b.is_ascii_control())
            })
            && security.is_compatible(protocol)
            && credentials_are_persistent;
        if !valid {
            return Err(RemoteError::new(
                protocol,
                RemoteErrorCategory::InvalidProfile,
                Some(host),
            ));
        }
        Ok(Self {
            id,
            name,
            protocol,
            host,
            port,
            path,
            username,
            credential,
            security,
            proxy,
        })
    }

    #[must_use]
    pub fn id(&self) -> &ConnectionId {
        &self.id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn protocol(&self) -> RemoteProtocol {
        self.protocol
    }

    #[must_use]
    pub fn host(&self) -> &RemoteHost {
        &self.host
    }

    #[must_use]
    pub const fn port(&self) -> Option<u16> {
        self.port
    }

    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    #[must_use]
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    #[must_use]
    pub fn credential(&self) -> Option<&CredentialReference> {
        self.credential.as_ref()
    }

    #[must_use]
    pub fn security(&self) -> &SecurityPolicy {
        &self.security
    }

    #[must_use]
    pub fn proxy(&self) -> Option<&ProxySettings> {
        self.proxy.as_ref()
    }

    #[must_use]
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hash_bytes(&mut hasher, b"musheen-connection-profile-v1");
        hash_bytes(&mut hasher, self.id.as_str().as_bytes());
        hash_bytes(&mut hasher, self.name.as_bytes());
        hash_byte(&mut hasher, protocol_tag(self.protocol));
        hash_bytes(&mut hasher, self.host.as_str().as_bytes());
        hash_optional_port(&mut hasher, self.port);
        hash_bytes(&mut hasher, self.path.as_bytes());
        hash_optional_bytes(&mut hasher, self.username.as_deref().map(str::as_bytes));
        hash_credential(&mut hasher, self.credential.as_ref());
        hash_security(&mut hasher, &self.security);
        match &self.proxy {
            None => hash_byte(&mut hasher, 0),
            Some(proxy) => {
                hash_byte(&mut hasher, 1);
                hash_byte(
                    &mut hasher,
                    match proxy.kind {
                        ProxyKind::Socks5 => 0,
                        ProxyKind::HttpConnect => 1,
                    },
                );
                hash_bytes(&mut hasher, proxy.host.as_str().as_bytes());
                hash_optional_port(&mut hasher, Some(proxy.port));
                hash_optional_bytes(&mut hasher, proxy.username.as_deref().map(str::as_bytes));
                hash_credential(&mut hasher, proxy.credential.as_ref());
            }
        }
        *hasher.finalize().as_bytes()
    }

    #[must_use]
    pub fn save_requirement(
        &self,
        previous: Option<&Self>,
        report: Option<&TestReport>,
        confirmation: SaveConfirmation,
    ) -> SaveRequirement {
        let Some(report) = report.filter(|report| report.fingerprint == self.fingerprint()) else {
            return SaveRequirement::TestRequired;
        };
        let failed = report.error.is_some() && !confirmation.failed_test;
        let security = previous.is_some_and(|profile| {
            self.protocol != profile.protocol
                || self.security.requires_confirmation(&profile.security)
        }) && !confirmation.security_change;
        match (failed, security) {
            (false, false) => SaveRequirement::Ready,
            (true, false) => SaveRequirement::ConfirmFailedTest,
            (false, true) => SaveRequirement::ConfirmSecurityChange,
            (true, true) => SaveRequirement::ConfirmFailedTestAndSecurityChange,
        }
    }
}

fn hash_byte(hasher: &mut blake3::Hasher, value: u8) {
    hasher.update(&[value]);
}

fn hash_bytes(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&value.len().to_le_bytes());
    hasher.update(value);
}

fn hash_optional_bytes(hasher: &mut blake3::Hasher, value: Option<&[u8]>) {
    match value {
        None => hash_byte(hasher, 0),
        Some(value) => {
            hash_byte(hasher, 1);
            hash_bytes(hasher, value);
        }
    }
}

fn hash_optional_port(hasher: &mut blake3::Hasher, value: Option<u16>) {
    match value {
        None => hash_byte(hasher, 0),
        Some(value) => {
            hash_byte(hasher, 1);
            hasher.update(&value.to_le_bytes());
        }
    }
}

fn hash_credential(hasher: &mut blake3::Hasher, value: Option<&CredentialReference>) {
    hash_optional_bytes(
        hasher,
        value.map(|reference| reference.connection_id().as_str().as_bytes()),
    );
}

const fn protocol_tag(protocol: RemoteProtocol) -> u8 {
    match protocol {
        RemoteProtocol::Ftp => 0,
        RemoteProtocol::Ftps => 1,
        RemoteProtocol::Sftp => 2,
        RemoteProtocol::WebDav => 3,
        RemoteProtocol::Http => 4,
        RemoteProtocol::Smb => 5,
        RemoteProtocol::Nfs => 6,
    }
}

fn hash_security(hasher: &mut blake3::Hasher, security: &SecurityPolicy) {
    match security {
        SecurityPolicy::PlaintextConfirmed => hash_byte(hasher, 0),
        SecurityPolicy::Tls(TlsPolicy::SystemRoots) => hash_byte(hasher, 1),
        SecurityPolicy::Tls(TlsPolicy::PinnedSha256(pin)) => {
            hash_byte(hasher, 2);
            hash_bytes(hasher, pin);
        }
        SecurityPolicy::Ssh(HostKeyPolicy::KnownHosts) => hash_byte(hasher, 3),
        SecurityPolicy::Ssh(HostKeyPolicy::PinnedSha256(pin)) => {
            hash_byte(hasher, 4);
            hash_bytes(hasher, pin);
        }
        SecurityPolicy::SystemManaged => hash_byte(hasher, 5),
    }
}

impl fmt::Debug for ConnectionProfile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ConnectionProfile")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("protocol", &self.protocol)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("path", &self.path)
            .field("username", &self.username.as_ref().map(|_| "<redacted>"))
            .field("credential", &self.credential.as_ref().map(|_| "<stored>"))
            .field("security", &self.security)
            .field("proxy", &self.proxy)
            .finish()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ConnectionProfiles(Vec<ConnectionProfile>);

impl ConnectionProfiles {
    pub fn new(profiles: Vec<ConnectionProfile>) -> Self {
        Self(profiles)
    }

    #[must_use]
    pub fn profiles(&self) -> &[ConnectionProfile] {
        &self.0
    }

    pub fn import(value: &str) -> Result<Self, RemoteError> {
        if value.len() > MAX_DOCUMENT_BYTES {
            return Err(invalid_document());
        }
        let document: ProfilesWire = serde_json::from_str(value).map_err(|_| invalid_document())?;
        if document.version != PROFILE_VERSION || document.profiles.len() > MAX_PROFILES {
            return Err(invalid_document());
        }
        let mut ids = BTreeSet::new();
        let mut profiles = Vec::with_capacity(document.profiles.len());
        for profile in document.profiles {
            let profile = ConnectionProfile::try_from(profile)?;
            if !ids.insert(profile.id.clone()) {
                return Err(invalid_document());
            }
            profiles.push(profile);
        }
        Ok(Self(profiles))
    }

    pub fn export(&self) -> Result<String, RemoteError> {
        if self.0.len() > MAX_PROFILES {
            return Err(invalid_document());
        }
        let mut ids = BTreeSet::new();
        let profiles = self
            .0
            .iter()
            .map(|profile| {
                if !ids.insert(profile.id.clone()) {
                    return Err(invalid_document());
                }
                ProfileWire::try_from(profile)
            })
            .collect::<Result<Vec<_>, _>>()?;
        serde_json::to_string(&ProfilesWire {
            version: PROFILE_VERSION,
            profiles,
        })
        .map_err(|_| invalid_document())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SaveConfirmation {
    pub failed_test: bool,
    pub security_change: bool,
}

impl SaveConfirmation {
    #[must_use]
    pub const fn all() -> Self {
        Self {
            failed_test: true,
            security_change: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveRequirement {
    Ready,
    TestRequired,
    ConfirmFailedTest,
    ConfirmSecurityChange,
    ConfirmFailedTestAndSecurityChange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestReport {
    fingerprint: [u8; 32],
    error: Option<RemoteError>,
}

impl TestReport {
    #[must_use]
    pub fn passed(profile: &ConnectionProfile) -> Self {
        Self {
            fingerprint: profile.fingerprint(),
            error: None,
        }
    }

    #[must_use]
    pub fn failed(profile: &ConnectionProfile, error: RemoteError) -> Self {
        Self {
            fingerprint: profile.fingerprint(),
            error: Some(error),
        }
    }

    #[must_use]
    pub fn error(&self) -> Option<&RemoteError> {
        self.error.as_ref()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfilesWire {
    version: u32,
    profiles: Vec<ProfileWire>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileWire {
    id: String,
    name: String,
    protocol: RemoteProtocol,
    host: String,
    port: Option<u16>,
    path: String,
    username: Option<String>,
    credential: Option<String>,
    security: SecurityPolicy,
    proxy: Option<ProxyWire>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProxyWire {
    kind: ProxyKind,
    host: String,
    port: u16,
    username: Option<String>,
    credential: Option<String>,
}

impl TryFrom<&ConnectionProfile> for ProfileWire {
    type Error = RemoteError;

    fn try_from(profile: &ConnectionProfile) -> Result<Self, Self::Error> {
        Ok(Self {
            id: profile.id.as_str().to_owned(),
            name: profile.name.to_string(),
            protocol: profile.protocol,
            host: profile.host.to_string(),
            port: profile.port,
            path: profile.path.to_string(),
            username: profile.username.as_deref().map(str::to_owned),
            credential: stored_reference(profile.protocol, profile.credential.as_ref())?,
            security: profile.security.clone(),
            proxy: profile
                .proxy
                .as_ref()
                .map(|proxy| ProxyWire::from_settings(profile.protocol, proxy))
                .transpose()?,
        })
    }
}

impl TryFrom<ProfileWire> for ConnectionProfile {
    type Error = RemoteError;

    fn try_from(profile: ProfileWire) -> Result<Self, Self::Error> {
        let host = RemoteHost::new(profile.protocol, profile.host)?;
        let credential = profile
            .credential
            .as_deref()
            .map(CredentialReference::from_setting_value)
            .transpose()
            .map_err(|_| invalid_profile(profile.protocol, Some(host.clone())))?;
        let proxy = profile
            .proxy
            .map(|proxy| proxy.into_settings(profile.protocol))
            .transpose()?;
        let id = ConnectionId::new(profile.id)
            .map_err(|_| invalid_profile(profile.protocol, Some(host.clone())))?;
        Self::new(
            id,
            profile.name,
            profile.protocol,
            host,
            profile.port,
            profile.path,
            profile.username,
            credential,
            profile.security,
            proxy,
        )
    }
}

impl ProxyWire {
    fn from_settings(protocol: RemoteProtocol, proxy: &ProxySettings) -> Result<Self, RemoteError> {
        Ok(Self {
            kind: proxy.kind,
            host: proxy.host.to_string(),
            port: proxy.port,
            username: proxy.username.as_deref().map(str::to_owned),
            credential: stored_reference(protocol, proxy.credential.as_ref())?,
        })
    }

    fn into_settings(self, protocol: RemoteProtocol) -> Result<ProxySettings, RemoteError> {
        let host = RemoteHost::new(protocol, self.host)?;
        let credential = self
            .credential
            .as_deref()
            .map(CredentialReference::from_setting_value)
            .transpose()
            .map_err(|_| invalid_profile(protocol, Some(host.clone())))?;
        ProxySettings::new(
            protocol,
            self.kind,
            host,
            self.port,
            self.username,
            credential,
        )
    }
}

fn stored_reference(
    protocol: RemoteProtocol,
    reference: Option<&CredentialReference>,
) -> Result<Option<String>, RemoteError> {
    reference
        .map(|reference| {
            reference
                .to_setting_value()
                .ok_or_else(|| invalid_profile(protocol, None))
        })
        .transpose()
}

fn invalid_document() -> RemoteError {
    invalid_profile(RemoteProtocol::Http, None)
}

fn invalid_profile(protocol: RemoteProtocol, host: Option<RemoteHost>) -> RemoteError {
    RemoteError::new(protocol, RemoteErrorCategory::InvalidProfile, host)
}
