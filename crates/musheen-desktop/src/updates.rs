use base64::Engine as _;
use musheen_core::{BoxFuture, CancellationToken};
use ring::signature::{self, UnparsedPublicKey};
use serde::Deserialize;
use std::error::Error;
use std::fmt;
use std::time::Duration;

mod state;
pub use state::UpdateSequenceStore;

const PROJECT_KEY: [u8; 32] = [
    0x9e, 0x6c, 0x2f, 0x23, 0x57, 0x48, 0x11, 0x47, 0xe3, 0x8a, 0x3c, 0xc9, 0xb5, 0xe5, 0x24, 0x05,
    0xe3, 0x20, 0x8f, 0xdb, 0x1b, 0xe2, 0xc9, 0xf6, 0x3b, 0xf0, 0x54, 0x7d, 0xcf, 0xaa, 0xea, 0xe8,
];
const MAX_METADATA_BYTES: usize = 1024 * 1024;

pub trait MetadataFetcher: Send + Sync + 'static {
    fn fetch(
        &self,
        url: &str,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<Box<[u8]>, UpdateError>>;
}

#[derive(Clone)]
pub struct HttpsMetadataFetcher {
    agent: ureq::Agent,
}

impl Default for HttpsMetadataFetcher {
    fn default() -> Self {
        let config = ureq::Agent::config_builder()
            .https_only(true)
            .timeout_global(Some(Duration::from_secs(5)))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
        }
    }
}

impl MetadataFetcher for HttpsMetadataFetcher {
    fn fetch(
        &self,
        url: &str,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<Box<[u8]>, UpdateError>> {
        let agent = self.agent.clone();
        let url = url.to_owned();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(UpdateError::Cancelled);
            }
            let mut response = agent
                .get(&url)
                .call()
                .map_err(|error| UpdateError::Transport(error.to_string().into()))?;
            let bytes = response
                .body_mut()
                .with_config()
                .limit(MAX_METADATA_BYTES as u64)
                .read_to_vec()
                .map_err(|error| UpdateError::Transport(error.to_string().into()))?;
            if cancellation.is_cancelled() {
                return Err(UpdateError::Cancelled);
            }
            Ok(bytes.into_boxed_slice())
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdatePolicy {
    enabled: bool,
    metadata_url: Option<Box<str>>,
    channel: &'static str,
    not_before_unix: u64,
}

impl UpdatePolicy {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            enabled: false,
            metadata_url: None,
            channel: "stable",
            not_before_unix: 0,
        }
    }

    #[must_use]
    pub fn enabled(url: impl Into<Box<str>>, not_before_unix: u64) -> Self {
        Self {
            enabled: true,
            metadata_url: Some(url.into()),
            channel: "stable",
            not_before_unix,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateOffer {
    Disabled,
    NotDue,
    Current,
    Information(UpdateInformation),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UpdateInformation {
    channel: Box<str>,
    sequence: u64,
    version: Box<str>,
    information_url: Box<str>,
}

impl UpdateInformation {
    #[must_use]
    pub fn channel(&self) -> &str {
        &self.channel
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    #[must_use]
    pub fn information_url(&self) -> &str {
        &self.information_url
    }

    /// Updates are informational. Musheen never installs a package itself.
    #[must_use]
    pub const fn can_install(&self) -> bool {
        false
    }
}

pub struct UpdateCheck<F> {
    fetcher: F,
    verifier: UpdateMetadataVerifier,
    running_version: semver::Version,
    sequences: UpdateSequenceStore,
}

impl<F: MetadataFetcher> UpdateCheck<F> {
    #[must_use]
    pub fn new(fetcher: F, verifier: UpdateMetadataVerifier, running_version: &str) -> Self {
        Self::with_sequence_store(
            fetcher,
            verifier,
            running_version,
            UpdateSequenceStore::for_current_user(),
        )
    }

    #[must_use]
    pub fn with_sequence_store(
        fetcher: F,
        verifier: UpdateMetadataVerifier,
        running_version: &str,
        sequences: UpdateSequenceStore,
    ) -> Self {
        Self {
            fetcher,
            verifier,
            running_version: semver::Version::parse(running_version)
                .expect("the application package version is valid semantic versioning"),
            sequences,
        }
    }

    pub async fn check(
        &self,
        policy: UpdatePolicy,
        now_unix: u64,
        cancellation: CancellationToken,
    ) -> Result<UpdateOffer, UpdateError> {
        if !policy.enabled {
            return Ok(UpdateOffer::Disabled);
        }
        if now_unix < policy.not_before_unix {
            return Ok(UpdateOffer::NotDue);
        }
        let url = policy
            .metadata_url
            .as_deref()
            .ok_or(UpdateError::InvalidMetadata)?;
        if !url.starts_with("https://") {
            return Err(UpdateError::InsecureTransport);
        }
        if cancellation.is_cancelled() {
            return Err(UpdateError::Cancelled);
        }
        let body = self.fetcher.fetch(url, cancellation).await?;
        let information = self.verifier.verify(&body, now_unix)?;
        if information.channel() != policy.channel {
            return Err(UpdateError::WrongChannel);
        }
        let candidate = semver::Version::parse(information.version())
            .map_err(|_| UpdateError::InvalidMetadata)?;
        self.sequences
            .accept(information.channel(), information.sequence())?;
        if candidate <= self.running_version {
            Ok(UpdateOffer::Current)
        } else {
            Ok(UpdateOffer::Information(information))
        }
    }
}

#[derive(Clone, Debug)]
pub struct UpdateMetadataVerifier {
    key: [u8; 32],
}

impl UpdateMetadataVerifier {
    #[must_use]
    pub const fn project_key() -> Self {
        Self { key: PROJECT_KEY }
    }

    #[must_use]
    pub const fn from_key(key: [u8; 32]) -> Self {
        Self { key }
    }

    pub fn verify(&self, document: &[u8], now_unix: u64) -> Result<UpdateInformation, UpdateError> {
        if document.len() > MAX_METADATA_BYTES {
            return Err(UpdateError::InvalidMetadata);
        }
        let document: SignedUpdateDocument =
            serde_json::from_slice(document).map_err(|_| UpdateError::InvalidMetadata)?;
        if document.schema_version != 2
            || document.sequence == 0
            || document.channel.is_empty()
            || document.channel.len() > 32
            || !document
                .channel
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(UpdateError::InvalidMetadata);
        }
        if !document.information_url.starts_with("https://") {
            return Err(UpdateError::InsecureTransport);
        }
        let message = serde_json::to_vec(&(
            document.schema_version,
            &document.channel,
            document.sequence,
            &document.version,
            &document.information_url,
            document.expires_unix,
        ))
        .map_err(|_| UpdateError::InvalidMetadata)?;
        let signature = base64::engine::general_purpose::STANDARD
            .decode(document.signature.as_bytes())
            .map_err(|_| UpdateError::InvalidSignature)?;
        UnparsedPublicKey::new(&signature::ED25519, self.key)
            .verify(&message, &signature)
            .map_err(|_| UpdateError::InvalidSignature)?;
        if document.expires_unix <= now_unix {
            return Err(UpdateError::Expired);
        }
        if document.version.is_empty() || document.version.len() > 64 {
            return Err(UpdateError::InvalidMetadata);
        }
        Ok(UpdateInformation {
            channel: document.channel,
            sequence: document.sequence,
            version: document.version,
            information_url: document.information_url,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedUpdateDocument {
    schema_version: u32,
    channel: Box<str>,
    sequence: u64,
    version: Box<str>,
    information_url: Box<str>,
    expires_unix: u64,
    signature: Box<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateError {
    Cancelled,
    InsecureTransport,
    InvalidMetadata,
    InvalidSignature,
    WrongChannel,
    Replay,
    InvalidState,
    State(Box<str>),
    Expired,
    Transport(Box<str>),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("the update check was cancelled"),
            Self::InsecureTransport => formatter.write_str("update metadata requires HTTPS"),
            Self::InvalidMetadata => formatter.write_str("update metadata is invalid"),
            Self::InvalidSignature => formatter.write_str("update metadata signature is invalid"),
            Self::WrongChannel => formatter.write_str("update metadata is for another channel"),
            Self::Replay => formatter.write_str("update metadata sequence was already seen"),
            Self::InvalidState => formatter.write_str("update sequence state is invalid"),
            Self::State(reason) => write!(formatter, "update sequence state failed: {reason}"),
            Self::Expired => formatter.write_str("update metadata has expired"),
            Self::Transport(reason) => write!(formatter, "update metadata fetch failed: {reason}"),
        }
    }
}

impl Error for UpdateError {}

pub trait UpdateOfferSink: Send + Sync + 'static {
    fn offer(&self, offer: UpdateInformation) -> Result<(), Box<str>>;
}

impl UpdateOfferSink for async_channel::Sender<UpdateInformation> {
    fn offer(&self, offer: UpdateInformation) -> Result<(), Box<str>> {
        self.try_send(offer)
            .map_err(|error| error.to_string().into())
    }
}

pub struct UpdateMaintenanceTask<F, S> {
    check: UpdateCheck<F>,
    policy: UpdatePolicy,
    sink: S,
}

impl<F, S> UpdateMaintenanceTask<F, S> {
    #[must_use]
    pub const fn new(check: UpdateCheck<F>, policy: UpdatePolicy, sink: S) -> Self {
        Self {
            check,
            policy,
            sink,
        }
    }
}

impl<F: MetadataFetcher + Clone, S: UpdateOfferSink + Clone> crate::MaintenanceTask
    for UpdateMaintenanceTask<F, S>
{
    fn run(
        &self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<(), crate::MaintenanceError>> {
        let check = UpdateCheck {
            fetcher: self.check.fetcher.clone(),
            verifier: self.check.verifier.clone(),
            running_version: self.check.running_version.clone(),
            sequences: self.check.sequences.clone(),
        };
        let policy = self.policy.clone();
        let sink = self.sink.clone();
        Box::pin(async move {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| crate::MaintenanceError::Task(error.to_string().into()))?
                .as_secs();
            match check.check(policy, now, cancellation).await {
                Ok(UpdateOffer::Information(offer)) => {
                    sink.offer(offer).map_err(crate::MaintenanceError::Task)
                }
                Ok(UpdateOffer::Disabled | UpdateOffer::NotDue | UpdateOffer::Current) => Ok(()),
                Err(UpdateError::Cancelled) => Err(crate::MaintenanceError::Cancelled),
                Err(error) => Err(crate::MaintenanceError::Task(error.to_string().into())),
            }
        })
    }
}
