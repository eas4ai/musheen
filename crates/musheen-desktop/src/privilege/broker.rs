use base64::Engine as _;
use rustix::fs::{Mode, OFlags, open, openat};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::session::{BrokerChannel, BrokerProcess, ChannelError, spawn_output_reader};
use super::{
    BrokerOperation, BrokerRequest, BrokerSession, ElevatedRootReference,
    MAX_SESSION_LISTING_BYTES, OwnershipContents, OwnershipItem, PrivilegeProvider, RequestSubject,
    RootGrant, RootedDirectoryEntry, RootedEntryKind, RootedStore,
};
use crate::SecretBuffer;
use musheen_core::CancellationToken;

pub const SUDO_BROKER_READY: &str = "MUSHEEN_BROKER_READY";
pub const BROKER_REQUEST_FRAME: &str = "MUSHEEN_REQUEST ";
pub const BROKER_RESPONSE_FRAME: &str = "MUSHEEN_RESPONSE ";
pub const INSTALLED_BROKER_PATH: &str = "/usr/lib/musheen/musheen-broker";
const MAX_BROKER_OUTPUT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizationError {
    Cancelled,
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationGrant {
    subject: Box<str>,
    expires_at_unix_millis: u64,
}

impl AuthorizationGrant {
    #[must_use]
    pub fn new(subject: impl Into<Box<str>>, expires_at_unix_millis: u64) -> Self {
        Self {
            subject: subject.into(),
            expires_at_unix_millis,
        }
    }

    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub const fn expires_at_unix_millis(&self) -> u64 {
        self.expires_at_unix_millis
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizationRequest {
    request_id: Box<str>,
    action_id: &'static str,
    target: PathBuf,
    provider: PrivilegeProvider,
    subject: RequestSubject,
    binding_digest: blake3::Hash,
}

impl AuthorizationRequest {
    #[must_use]
    pub fn from_broker_request(request: &BrokerRequest, provider: PrivilegeProvider) -> Self {
        Self {
            request_id: request.id().into(),
            action_id: request.operation().action_id(),
            target: request.target().to_path_buf(),
            provider,
            subject: request.subject(),
            binding_digest: request.binding_digest(),
        }
    }

    #[must_use]
    pub fn request_id(&self) -> &str {
        &self.request_id
    }

    #[must_use]
    pub const fn action_id(&self) -> &'static str {
        self.action_id
    }

    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    #[must_use]
    pub const fn provider(&self) -> PrivilegeProvider {
        self.provider
    }

    #[must_use]
    pub const fn subject(&self) -> RequestSubject {
        self.subject
    }

    #[must_use]
    pub const fn binding_digest(&self) -> blake3::Hash {
        self.binding_digest
    }
}

pub trait Authorizer: Send + Sync + 'static {
    fn authorize(
        &self,
        request: &AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationError>;

    fn authorize_cancellable(
        &self,
        request: &AuthorizationRequest,
        cancellation: &CancellationToken,
        _timeout: Duration,
    ) -> Result<AuthorizationGrant, AuthorizationError> {
        if cancellation.is_cancelled() {
            return Err(AuthorizationError::Cancelled);
        }
        let result = self.authorize(request);
        if cancellation.is_cancelled() {
            Err(AuthorizationError::Cancelled)
        } else {
            result
        }
    }
}

pub trait Clock: Clone + Send + Sync + 'static {
    fn now_unix_millis(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_millis(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

#[derive(Debug)]
pub struct ValidatedRequest {
    request_id: Box<str>,
    operation: BrokerOperation,
    target: ValidatedTarget,
    environment: BTreeMap<String, String>,
    authorization: AuthorizationGrant,
    provider: PrivilegeProvider,
}

impl ValidatedRequest {
    #[must_use]
    pub const fn operation(&self) -> &BrokerOperation {
        &self.operation
    }

    #[must_use]
    pub const fn target_file(&self) -> &File {
        &self.target.file
    }

    #[must_use]
    pub const fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    #[must_use]
    pub const fn provider(&self) -> PrivilegeProvider {
        self.provider
    }

    #[must_use]
    pub const fn authorization(&self) -> &AuthorizationGrant {
        &self.authorization
    }
}

pub trait OperationRunner: Send + Sync + 'static {
    fn execute(&self, request: ValidatedRequest) -> Result<BrokerOutput, BrokerError>;
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BrokerOutput {
    RootReferenced(ElevatedRootReference),
    DirectoryEntries(Vec<BrokerDirectoryEntry>),
    Exited(i32),
    OwnershipChanged(OwnershipReport),
}

/// What an ownership change did: how many items it changed, and the item
/// it stopped at, if one failed. Items changed before a failure stay changed.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipReport {
    changed: u64,
    failure: Option<OwnershipFailure>,
}

impl OwnershipReport {
    #[must_use]
    pub const fn new(changed: u64, failure: Option<OwnershipFailure>) -> Self {
        Self { changed, failure }
    }

    #[must_use]
    pub const fn changed(&self) -> u64 {
        self.changed
    }

    #[must_use]
    pub const fn failure(&self) -> Option<&OwnershipFailure> {
        self.failure.as_ref()
    }
}

/// The item an ownership change could not change, and why.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipFailure {
    #[serde(with = "super::request::path_bytes")]
    path: PathBuf,
    reason: Box<str>,
}

impl OwnershipFailure {
    #[must_use]
    pub fn new(path: impl AsRef<Path>, error: &BrokerError) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            reason: error.code().into(),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The broker error, when the reason is one Musheen knows.
    #[must_use]
    pub fn error(&self) -> BrokerError {
        BrokerError::from_code(&self.reason).unwrap_or(BrokerError::Io)
    }
}

/// One entry of a folder listing. It goes as a [`WireEntry`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "WireEntry", into = "WireEntry")]
pub struct BrokerDirectoryEntry {
    name: Vec<u8>,
    identity: [u8; 16],
    kind: RootedEntryKind,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
}

/// A listing entry as the broker sends it: a JSON array of the name, the
/// identity, a one-digit kind, the size and the modification time, with no
/// key names. The name and the identity go as base64 text, a third longer
/// than their bytes; as arrays of numbers they were up to four times as long.
/// An entry with an 8-byte name and a 4-digit size adds 59 bytes to a
/// listing, where an object with key names added 127.
#[derive(Deserialize, Serialize)]
struct WireEntry(
    #[serde(with = "base64_bytes")] Vec<u8>,
    #[serde(with = "base64_bytes")] [u8; 16],
    u8,
    Option<u64>,
    Option<i64>,
);

impl From<BrokerDirectoryEntry> for WireEntry {
    fn from(entry: BrokerDirectoryEntry) -> Self {
        let kind = match entry.kind {
            RootedEntryKind::Directory => 0,
            RootedEntryKind::RegularFile => 1,
            RootedEntryKind::SymbolicLink => 2,
            RootedEntryKind::Other => 3,
        };
        Self(
            entry.name,
            entry.identity,
            kind,
            entry.size,
            entry.modified_unix_seconds,
        )
    }
}

impl TryFrom<WireEntry> for BrokerDirectoryEntry {
    type Error = &'static str;

    fn try_from(
        WireEntry(name, identity, kind, size, modified_unix_seconds): WireEntry,
    ) -> Result<Self, Self::Error> {
        let kind = match kind {
            0 => RootedEntryKind::Directory,
            1 => RootedEntryKind::RegularFile,
            2 => RootedEntryKind::SymbolicLink,
            3 => RootedEntryKind::Other,
            _ => return Err("unknown entry kind"),
        };
        Ok(Self {
            name,
            identity,
            kind,
            size,
            modified_unix_seconds,
        })
    }
}

impl From<&RootedDirectoryEntry> for BrokerDirectoryEntry {
    fn from(entry: &RootedDirectoryEntry) -> Self {
        Self {
            name: entry.name().as_bytes().to_vec(),
            identity: *entry.identity(),
            kind: entry.kind(),
            size: entry.size(),
            modified_unix_seconds: entry.modified_unix_seconds(),
        }
    }
}

impl BrokerDirectoryEntry {
    /// The bytes this entry adds to an encoded listing: its JSON and the
    /// comma after it.
    fn listing_bytes(&self) -> usize {
        struct Counter(usize);
        impl std::io::Write for Counter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut counter = Counter(1);
        // Writing to the counter cannot fail, and the entry's fields always
        // serialize.
        let _ = serde_json::to_writer(&mut counter, self);
        counter.0
    }

    #[must_use]
    pub fn name(&self) -> &[u8] {
        &self.name
    }

    #[must_use]
    pub const fn identity(&self) -> &[u8; 16] {
        &self.identity
    }

    #[must_use]
    pub const fn kind(&self) -> RootedEntryKind {
        self.kind
    }

    #[must_use]
    pub const fn size(&self) -> Option<u64> {
        self.size
    }

    #[must_use]
    pub const fn modified_unix_seconds(&self) -> Option<i64> {
        self.modified_unix_seconds
    }
}

/// Lists `relative` under `store`'s grant, sorted by name. The read stops
/// with [`BrokerError::ListingTooLarge`] as soon as the encoded listing
/// passes `limit` bytes, before the rest of the folder is read.
fn list_directory<C: Clock>(
    store: &RootedStore<C>,
    relative: &Path,
    limit: usize,
) -> Result<Vec<BrokerDirectoryEntry>, BrokerError> {
    let mut listed = 0_usize;
    let entries = store.read_directory_with(relative, |entry| {
        listed = listed.saturating_add(BrokerDirectoryEntry::from(entry).listing_bytes());
        if listed > limit {
            Err(BrokerError::ListingTooLarge)
        } else {
            Ok(())
        }
    })?;
    Ok(entries.iter().map(BrokerDirectoryEntry::from).collect())
}

/// Bytes as unpadded URL-safe base64 text, as request paths are sent.
mod base64_bytes {
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};

    const ENGINE: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

    pub fn serialize<S: Serializer>(
        bytes: &impl AsRef<[u8]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&ENGINE.encode(bytes.as_ref()))
    }

    pub fn deserialize<'de, D: Deserializer<'de>, B: TryFrom<Vec<u8>>>(
        deserializer: D,
    ) -> Result<B, D::Error> {
        let bytes = ENGINE
            .decode(String::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)?;
        B::try_from(bytes).map_err(|_| serde::de::Error::custom("wrong number of bytes"))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BrokerResponse {
    ok: bool,
    result: Option<BrokerOutput>,
    error: Option<Box<str>>,
}

impl BrokerResponse {
    #[must_use]
    pub fn success(result: BrokerOutput) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    #[must_use]
    pub fn failure(error: &BrokerError) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(error.code().into()),
        }
    }

    fn into_result(self) -> Result<BrokerOutput, BrokerError> {
        if self.ok {
            self.result.ok_or(BrokerError::BrokerCrashed)
        } else {
            Err(self
                .error
                .as_deref()
                .and_then(BrokerError::from_code)
                .unwrap_or(BrokerError::BrokerCrashed))
        }
    }
}

pub fn encode_broker_request(request: &BrokerRequest) -> Result<String, BrokerError> {
    let payload = serde_json::to_vec(request).map_err(|_| BrokerError::InvalidRequest)?;
    Ok(format!(
        "{BROKER_REQUEST_FRAME}{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(payload)
    ))
}

pub fn decode_broker_request(frame: &str) -> Result<BrokerRequest, BrokerError> {
    let payload = frame
        .strip_prefix(BROKER_REQUEST_FRAME)
        .ok_or(BrokerError::InvalidRequest)?;
    let payload = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(payload)
        .map_err(|_| BrokerError::InvalidRequest)?;
    serde_json::from_slice(&payload).map_err(|_| BrokerError::InvalidRequest)
}

pub fn encode_broker_response(response: &BrokerResponse) -> Result<String, BrokerError> {
    let payload = serde_json::to_vec(response).map_err(|_| BrokerError::BrokerCrashed)?;
    Ok(format!(
        "{BROKER_RESPONSE_FRAME}{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(payload)
    ))
}

pub fn decode_broker_response(frame: &str) -> Result<BrokerOutput, BrokerError> {
    let payload = frame
        .strip_prefix(BROKER_RESPONSE_FRAME)
        .ok_or(BrokerError::BrokerCrashed)?;
    let payload = base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(payload)
        .map_err(|_| BrokerError::BrokerCrashed)?;
    serde_json::from_slice::<BrokerResponse>(&payload)
        .map_err(|_| BrokerError::BrokerCrashed)?
        .into_result()
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditOutcome {
    Denied,
    Expired,
    Failed,
    Started,
    Succeeded,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuditPhase {
    Attempt,
    Authorization,
    Dispatch,
    Completion,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuditRecord {
    request_id: Box<str>,
    operation: Box<str>,
    #[serde(with = "super::request::path_bytes")]
    target: PathBuf,
    provider: PrivilegeProvider,
    phase: AuditPhase,
    outcome: AuditOutcome,
    timestamp_unix_millis: u64,
}

impl AuditRecord {
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    #[must_use]
    pub const fn provider(&self) -> PrivilegeProvider {
        self.provider
    }

    #[must_use]
    pub const fn outcome(&self) -> AuditOutcome {
        self.outcome
    }

    #[must_use]
    pub const fn phase(&self) -> AuditPhase {
        self.phase
    }
}

pub trait AuditSink: Send + Sync + 'static {
    fn record(&self, record: &AuditRecord) -> Result<(), BrokerError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NoopAudit;

impl AuditSink for NoopAudit {
    fn record(&self, _: &AuditRecord) -> Result<(), BrokerError> {
        Ok(())
    }
}

pub struct JsonAuditLog {
    file: Mutex<File>,
}

impl JsonAuditLog {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, BrokerError> {
        let path = path.as_ref();
        let parent = path.parent().ok_or(BrokerError::AuditFailed)?;
        std::fs::create_dir_all(parent).map_err(|_| BrokerError::AuditFailed)?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| BrokerError::AuditFailed)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| BrokerError::AuditFailed)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| BrokerError::AuditFailed)?;
        Ok(Self {
            file: Mutex::new(file),
        })
    }
}

impl AuditSink for JsonAuditLog {
    fn record(&self, record: &AuditRecord) -> Result<(), BrokerError> {
        // One write per record: brokers of several windows append to the same
        // log, and a session broker can end the process between two writes.
        let mut line = serde_json::to_vec(record).map_err(|_| BrokerError::AuditFailed)?;
        line.push(b'\n');
        let mut file = self.file.lock().map_err(|_| BrokerError::AuditFailed)?;
        file.write_all(&line)
            .and_then(|()| file.sync_data())
            .map_err(|_| BrokerError::AuditFailed)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrokerLaunch {
    program: PathBuf,
    arguments: Box<[std::ffi::OsString]>,
    provider: PrivilegeProvider,
}

impl BrokerLaunch {
    #[must_use]
    pub fn new(broker: impl AsRef<Path>, provider: PrivilegeProvider) -> Self {
        let broker = broker.as_ref().as_os_str().to_owned();
        let provider_argument = format!("--provider={}", provider.as_str());
        let (program, arguments) = match provider {
            PrivilegeProvider::Polkit => (
                PathBuf::from("/usr/bin/pkexec"),
                vec![
                    "--disable-internal-agent".into(),
                    broker,
                    "--stdio".into(),
                    provider_argument.into(),
                ],
            ),
            PrivilegeProvider::Sudo => (
                PathBuf::from("/usr/bin/sudo"),
                vec![
                    "--".into(),
                    broker,
                    "--stdio".into(),
                    provider_argument.into(),
                ],
            ),
        };
        Self {
            program,
            arguments: arguments.into_boxed_slice(),
            provider,
        }
    }

    /// Constructs the same narrowly typed sudo invocation with an explicit
    /// sudo executable. This is useful for sandboxed/recording transports and
    /// does not permit changing the broker argument schema.
    #[must_use]
    pub fn sudo_with_program(sudo: impl AsRef<Path>, broker: impl AsRef<Path>) -> Self {
        Self {
            program: sudo.as_ref().to_path_buf(),
            arguments: vec![
                "--".into(),
                broker.as_ref().as_os_str().to_owned(),
                "--stdio".into(),
                "--provider=sudo".into(),
            ]
            .into_boxed_slice(),
            provider: PrivilegeProvider::Sudo,
        }
    }

    #[must_use]
    pub fn polkit_with_program(pkexec: impl AsRef<Path>, broker: impl AsRef<Path>) -> Self {
        Self {
            program: pkexec.as_ref().to_path_buf(),
            arguments: vec![
                "--disable-internal-agent".into(),
                broker.as_ref().as_os_str().to_owned(),
                "--stdio".into(),
                "--provider=polkit".into(),
            ]
            .into_boxed_slice(),
            provider: PrivilegeProvider::Polkit,
        }
    }

    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    #[must_use]
    pub fn arguments_for(&self, request: &BrokerRequest) -> Vec<std::ffi::OsString> {
        let mut arguments = self.arguments.to_vec();
        arguments.insert(
            2,
            format!("--action-id={}", request.operation().action_id()).into(),
        );
        arguments.push(format!("--request-digest={}", request.operation_digest().to_hex()).into());
        arguments.push("--target".into());
        arguments.push(request.target().as_os_str().to_owned());
        arguments
    }

    #[must_use]
    pub const fn provider(&self) -> PrivilegeProvider {
        self.provider
    }
}

pub trait BrokerTransport: Send + Sync + 'static {
    fn perform(&self, request: &BrokerRequest) -> Result<BrokerOutput, BrokerError>;

    fn perform_cancellable(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
    ) -> Result<BrokerOutput, BrokerError> {
        if cancellation.is_cancelled() {
            return Err(BrokerError::AuthorizationCancelled);
        }
        self.perform(request)
    }

    fn perform_with_authentication(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> Result<BrokerOutput, BrokerError> {
        drop(authentication);
        self.perform_cancellable(request, cancellation)
    }

    /// Authorizes an Open as Administrator request once and keeps its broker
    /// for the elevated window: the session answers the window's listings
    /// (SYS-034). A transport that cannot keep a broker refuses.
    fn open_session(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> Result<BrokerSession, BrokerError> {
        drop(authentication);
        let _ = (request, cancellation);
        Err(BrokerError::AuthorizationUnavailable)
    }
}

/// Turns the first answer to an Open as Administrator request into the
/// window's session.
fn session_from(
    request: &BrokerRequest,
    output: BrokerOutput,
    channel: BrokerChannel,
    timeout: Duration,
) -> Result<BrokerSession, BrokerError> {
    match (request.operation(), output) {
        (BrokerOperation::OpenDirectory { target }, BrokerOutput::RootReferenced(root))
            if root.root() == target =>
        {
            Ok(BrokerSession::new(root, channel, timeout))
        }
        _ => Err(BrokerError::BrokerCrashed),
    }
}

/// Maps a channel that gave no first answer to the transport's error.
const fn start_error(error: ChannelError) -> BrokerError {
    match error {
        ChannelError::Cancelled => BrokerError::AuthorizationCancelled,
        ChannelError::TimedOut => BrokerError::ExecutionTimedOut,
        ChannelError::Ended | ChannelError::Oversized => BrokerError::BrokerCrashed,
    }
}

#[derive(Clone)]
pub struct ProcessBrokerTransport {
    launch: BrokerLaunch,
    timeout: Duration,
    active: Arc<AtomicBool>,
}

impl fmt::Debug for ProcessBrokerTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessBrokerTransport")
            .field("launch", &self.launch)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl ProcessBrokerTransport {
    #[must_use]
    pub fn new(launch: BrokerLaunch) -> Self {
        Self {
            launch,
            timeout: Duration::from_secs(120),
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl ProcessBrokerTransport {
    /// Starts pkexec with the broker, sends `request`, and waits for the
    /// broker's first answer. The channel stays open for later requests.
    fn start(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
    ) -> Result<(BrokerOutput, BrokerChannel), BrokerError> {
        use std::os::unix::process::CommandExt as _;
        use std::process::Stdio;

        let _admission = TransportAdmission::enter(&self.active)?;
        if cancellation.is_cancelled() {
            return Err(BrokerError::AuthorizationCancelled);
        }
        if self.launch.provider() != PrivilegeProvider::Polkit {
            return Err(BrokerError::AuthorizationUnavailable);
        }
        let deadline = Instant::now() + self.timeout;
        let encoded = encode_broker_request(request)?;
        // A socket pair, not pipes: another process of the same user can
        // reopen a pipe through /proc/<pid>/fd and write into the session,
        // but it cannot reopen a socket (SYS-034).
        let (input, broker_side) =
            std::os::unix::net::UnixStream::pair().map_err(|_| BrokerError::BrokerCrashed)?;
        let output = input.try_clone().map_err(|_| BrokerError::BrokerCrashed)?;
        let broker_output = broker_side
            .try_clone()
            .map_err(|_| BrokerError::BrokerCrashed)?;
        // The command, and with it Musheen's copy of the broker's side, drops
        // at the end of this statement, so the broker alone holds that side.
        let mut child = std::process::Command::new(self.launch.program())
            .args(self.launch.arguments_for(request))
            .env_clear()
            .stdin(Stdio::from(std::os::fd::OwnedFd::from(broker_side)))
            .stdout(Stdio::from(std::os::fd::OwnedFd::from(broker_output)))
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map_err(|_| BrokerError::BrokerCrashed)?;
        let input = SocketInput(input);
        let chunks = match spawn_output_reader(output, 64 * 1024) {
            Ok(chunks) => chunks,
            Err(error) => {
                kill_and_reap_process_group(&mut child);
                return Err(error);
            }
        };
        let mut channel = BrokerChannel::new(
            Box::new(input),
            chunks,
            Vec::new(),
            Box::new(PolkitBroker(child)),
        );
        if channel.send(&encoded).is_err() {
            channel.stop_now();
            return Err(BrokerError::BrokerCrashed);
        }
        match channel.next_response(deadline, cancellation) {
            Ok(response) => Ok((decode_broker_response(&response)?, channel)),
            Err(error) => {
                channel.stop_now();
                Err(start_error(error))
            }
        }
    }
}

impl BrokerTransport for ProcessBrokerTransport {
    fn perform(&self, request: &BrokerRequest) -> Result<BrokerOutput, BrokerError> {
        self.perform_cancellable(request, &CancellationToken::new())
    }

    fn perform_cancellable(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
    ) -> Result<BrokerOutput, BrokerError> {
        self.start(request, cancellation)
            .map(|(output, _channel)| output)
    }

    fn open_session(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> Result<BrokerSession, BrokerError> {
        drop(authentication);
        if !matches!(request.operation(), BrokerOperation::OpenDirectory { .. }) {
            return Err(BrokerError::InvalidRequest);
        }
        let (output, channel) = self.start(request, cancellation)?;
        session_from(request, output, channel, self.timeout)
    }
}

/// Musheen's side of a pkexec-started broker's socket. Dropping it ends the
/// broker's input, as closing a pipe would; the output reader keeps its own
/// handle to the socket.
struct SocketInput(std::os::unix::net::UnixStream);

impl std::io::Write for SocketInput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl Drop for SocketInput {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Write);
    }
}

/// A pkexec-started broker. Once its input closes it exits on its own; one
/// that has not after two seconds is killed.
struct PolkitBroker(std::process::Child);

impl BrokerProcess for PolkitBroker {
    fn end(self: Box<Self>, graceful: bool) {
        let mut child = self.0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while graceful && Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        kill_and_reap_process_group(&mut child);
    }
}

fn kill_and_reap_process_group(child: &mut std::process::Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs the sudo provider with sudo and the broker attached to a dedicated
/// pseudoterminal. The request is withheld until the elevated broker emits a
/// readiness marker, so request JSON can never be consumed as authentication
/// input. Musheen never captures or logs terminal output.
#[derive(Clone, Debug)]
pub struct SudoPtyBrokerTransport {
    launch: BrokerLaunch,
    timeout: Duration,
    active: Arc<AtomicBool>,
}

impl SudoPtyBrokerTransport {
    #[must_use]
    pub fn new(launch: BrokerLaunch) -> Self {
        Self {
            launch,
            timeout: Duration::from_secs(120),
            active: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl SudoPtyBrokerTransport {
    /// Starts sudo with the broker on a dedicated pseudoterminal, answers
    /// its password prompt, sends `request` once the broker is ready, and
    /// waits for the broker's first answer. The channel stays open for later
    /// requests.
    fn start(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
        mut authentication: Option<SecretBuffer>,
    ) -> Result<(BrokerOutput, BrokerChannel), BrokerError> {
        let _admission = TransportAdmission::enter(&self.active)?;
        if cancellation.is_cancelled() {
            return Err(BrokerError::AuthorizationCancelled);
        }
        if authentication.as_ref().is_some_and(|secret| {
            secret.expose_secret(|bytes| {
                bytes.len() > 1024
                    || bytes
                        .iter()
                        .any(|byte| matches!(byte, b'\0' | b'\n' | b'\r'))
            })
        }) {
            return Err(BrokerError::InvalidRequest);
        }
        let encoded = encode_broker_request(request)?;
        let pty = portable_pty::native_pty_system()
            .openpty(portable_pty::PtySize::default())
            .map_err(|_| BrokerError::AuthorizationUnavailable)?;
        make_terminal_exclusive(pty.master.tty_name())?;
        let mut command = portable_pty::CommandBuilder::new(self.launch.program());
        command.args(self.launch.arguments_for(request));
        command.env_clear();
        command.env("LC_ALL", "C");
        command.env("SUDO_PROMPT", "MUSHEEN_SUDO_PASSWORD:");
        let child = pty
            .slave
            .spawn_command(command)
            .map_err(|_| BrokerError::AuthorizationUnavailable)?;
        drop(pty.slave);
        let process_group = pty.master.process_group_leader();
        let process = SudoBroker {
            child,
            process_group,
        };
        let (writer, reader) = match (pty.master.take_writer(), pty.master.try_clone_reader()) {
            (Ok(writer), Ok(reader)) => (writer, reader),
            _ => {
                Box::new(process).end(false);
                return Err(BrokerError::BrokerCrashed);
            }
        };
        let chunks = match spawn_output_reader(reader, 4096) {
            Ok(chunks) => chunks,
            Err(error) => {
                Box::new(process).end(false);
                return Err(error);
            }
        };
        let mut channel = BrokerChannel::new(
            writer,
            chunks,
            Vec::new(),
            Box::new(SudoMaster {
                process,
                master: pty.master,
            }),
        );
        let deadline = Instant::now() + self.timeout;
        let mut request_sent = false;
        let mut authentication_sent = false;
        let mut denied = false;
        let result = 'handshake: loop {
            if cancellation.is_cancelled() {
                break Err(BrokerError::AuthorizationCancelled);
            }
            if Instant::now() >= deadline {
                break Err(BrokerError::ExecutionTimedOut);
            }
            if channel.receive_chunk(Duration::from_millis(20)).is_err() {
                break Err(if denied {
                    BrokerError::AuthorizationDenied
                } else {
                    BrokerError::BrokerCrashed
                });
            }
            if channel.pending_output().len() > MAX_BROKER_OUTPUT {
                break Err(BrokerError::BrokerCrashed);
            }
            if !request_sent
                && channel
                    .pending_output()
                    .windows(b"MUSHEEN_SUDO_PASSWORD:".len())
                    .any(|window| window == b"MUSHEEN_SUDO_PASSWORD:")
            {
                if authentication_sent {
                    break Err(BrokerError::AuthorizationDenied);
                }
                let Some(mut secret) = authentication.take() else {
                    break Err(BrokerError::AuthorizationCancelled);
                };
                let written = secret.expose_secret(|bytes| channel.send_bytes(bytes));
                secret.clear();
                if written.is_err() {
                    break Err(BrokerError::BrokerCrashed);
                }
                authentication_sent = true;
                channel.clear_output();
            }
            for line in channel.take_lines() {
                if !request_sent && line == SUDO_BROKER_READY {
                    if channel.send(&encoded).is_err() {
                        break 'handshake Err(BrokerError::BrokerCrashed);
                    }
                    request_sent = true;
                } else if line.contains("Sorry, try again.") {
                    denied = true;
                } else if request_sent && line.starts_with(BROKER_RESPONSE_FRAME) {
                    break 'handshake decode_broker_response(&line);
                }
            }
        };
        match result {
            Ok(output) => Ok((output, channel)),
            Err(error) => {
                channel.stop_now();
                Err(error)
            }
        }
    }
}

impl BrokerTransport for SudoPtyBrokerTransport {
    fn perform(&self, request: &BrokerRequest) -> Result<BrokerOutput, BrokerError> {
        self.perform_cancellable(request, &CancellationToken::new())
    }

    fn perform_cancellable(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
    ) -> Result<BrokerOutput, BrokerError> {
        self.perform_with_authentication(request, cancellation, None)
    }

    fn perform_with_authentication(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> Result<BrokerOutput, BrokerError> {
        self.start(request, cancellation, authentication)
            .map(|(output, _channel)| output)
    }

    fn open_session(
        &self,
        request: &BrokerRequest,
        cancellation: &CancellationToken,
        authentication: Option<SecretBuffer>,
    ) -> Result<BrokerSession, BrokerError> {
        if !matches!(request.operation(), BrokerOperation::OpenDirectory { .. }) {
            return Err(BrokerError::InvalidRequest);
        }
        let (output, channel) = self.start(request, cancellation, authentication)?;
        session_from(request, output, channel, self.timeout)
    }
}

/// Makes the pseudoterminal `name` refuse any further open by an
/// unprivileged process, before sudo starts on it. Another process of the
/// same user could otherwise open it and read the password or write into
/// the session (SYS-034). sudo and the broker run as root and may still open
/// it.
fn make_terminal_exclusive(name: Option<PathBuf>) -> Result<(), BrokerError> {
    let name = name.ok_or(BrokerError::AuthorizationUnavailable)?;
    let terminal = open(
        name.as_path(),
        OFlags::RDWR | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|_| BrokerError::AuthorizationUnavailable)?;
    rustix::termios::ioctl_tiocexcl(&terminal).map_err(|_| BrokerError::AuthorizationUnavailable)
}

/// A sudo-started broker and its process group. It ends when the session's
/// end line arrives or its terminal closes; one still running after two
/// seconds is killed.
struct SudoBroker {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    process_group: Option<i32>,
}

impl SudoBroker {
    /// Waits up to two seconds for the broker to exit; true when it did.
    fn exited_within_grace(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return true,
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        false
    }
}

impl BrokerProcess for SudoBroker {
    fn end(mut self: Box<Self>, graceful: bool) {
        if graceful && self.exited_within_grace() {
            return;
        }
        if let Some(process_group) = self.process_group
            && let Some(pid) = rustix::process::Pid::from_raw(process_group)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Keeps the pseudoterminal's master open while the broker runs. A broker
/// still running after the end line and its grace is hung up by closing the
/// master, which stops even a root process, then killed and reaped.
struct SudoMaster {
    process: SudoBroker,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

impl BrokerProcess for SudoMaster {
    fn end(self: Box<Self>, graceful: bool) {
        let Self {
            mut process,
            master,
        } = *self;
        if graceful && process.exited_within_grace() {
            return;
        }
        drop(master);
        Box::new(process).end(false);
    }
}

struct TransportAdmission<'a>(&'a AtomicBool);

impl<'a> TransportAdmission<'a> {
    fn enter(active: &'a Arc<AtomicBool>) -> Result<Self, BrokerError> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self(active.as_ref()))
            .map_err(|_| BrokerError::Busy)
    }
}

impl Drop for TransportAdmission<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BrokerError {
    AuditFailed,
    AuthorizationCancelled,
    AuthorizationDenied,
    AuthorizationExpired,
    AuthorizationUnavailable,
    BrokerCrashed,
    Busy,
    ExecutionTimedOut,
    InvalidRequest,
    Io,
    /// A filesystem without POSIX ownership, or not on this machine.
    FilesystemUnsupported,
    /// A folder's listing passed [`MAX_SESSION_LISTING_BYTES`].
    ListingTooLarge,
    NotExecutable,
    ScopeEscape,
    SymlinkRefused,
    TargetReplaced,
    UnsafeExecutable,
}

impl fmt::Display for BrokerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AuditFailed => "privilege audit failed",
            Self::AuthorizationCancelled => "authorization was cancelled",
            Self::AuthorizationDenied => "authorization was denied",
            Self::AuthorizationExpired => "authorization expired",
            Self::AuthorizationUnavailable => "authorization is unavailable",
            Self::BrokerCrashed => "the privilege broker stopped unexpectedly",
            Self::Busy => "the privilege broker is busy",
            Self::ExecutionTimedOut => "the privileged command timed out",
            Self::InvalidRequest => "the privilege request is invalid",
            Self::Io => "the privilege operation failed",
            Self::FilesystemUnsupported => {
                "the filesystem has no POSIX ownership or is not on this machine"
            }
            Self::ListingTooLarge => "the folder's listing is larger than 64 MiB",
            Self::NotExecutable => "the selected target is not executable",
            Self::ScopeEscape => "the path leaves the authorized root",
            Self::SymlinkRefused => "symbolic links require new authorization",
            Self::TargetReplaced => "the target changed during authorization",
            Self::UnsafeExecutable => "the executable can be modified by the invoking user",
        })
    }
}

impl std::error::Error for BrokerError {}

impl BrokerError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::AuditFailed => "audit-failed",
            Self::AuthorizationCancelled => "authorization-cancelled",
            Self::AuthorizationDenied => "authorization-denied",
            Self::AuthorizationExpired => "authorization-expired",
            Self::AuthorizationUnavailable => "authorization-unavailable",
            Self::BrokerCrashed => "broker-crashed",
            Self::Busy => "busy",
            Self::ExecutionTimedOut => "execution-timed-out",
            Self::InvalidRequest => "invalid-request",
            Self::Io => "io-failed",
            Self::FilesystemUnsupported => "filesystem-unsupported",
            Self::ListingTooLarge => "listing-too-large",
            Self::NotExecutable => "not-executable",
            Self::ScopeEscape => "scope-escape",
            Self::SymlinkRefused => "symlink-refused",
            Self::TargetReplaced => "target-replaced",
            Self::UnsafeExecutable => "unsafe-executable",
        }
    }

    fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "audit-failed" => Self::AuditFailed,
            "authorization-cancelled" => Self::AuthorizationCancelled,
            "authorization-denied" => Self::AuthorizationDenied,
            "authorization-expired" => Self::AuthorizationExpired,
            "authorization-unavailable" => Self::AuthorizationUnavailable,
            "broker-crashed" => Self::BrokerCrashed,
            "busy" => Self::Busy,
            "execution-timed-out" => Self::ExecutionTimedOut,
            "invalid-request" => Self::InvalidRequest,
            "io-failed" => Self::Io,
            "filesystem-unsupported" => Self::FilesystemUnsupported,
            "listing-too-large" => Self::ListingTooLarge,
            "not-executable" => Self::NotExecutable,
            "scope-escape" => Self::ScopeEscape,
            "symlink-refused" => Self::SymlinkRefused,
            "target-replaced" => Self::TargetReplaced,
            "unsafe-executable" => Self::UnsafeExecutable,
            _ => return None,
        })
    }
}

pub struct Broker<A, R, S, C> {
    authorizer: A,
    runner: R,
    audit: S,
    clock: C,
    provider: PrivilegeProvider,
}

impl<A, R, S, C> Broker<A, R, S, C>
where
    A: Authorizer,
    R: OperationRunner,
    S: AuditSink,
    C: Clock,
{
    #[must_use]
    pub fn new(authorizer: A, runner: R, audit: S, clock: C) -> Self {
        Self {
            authorizer,
            runner,
            audit,
            clock,
            provider: PrivilegeProvider::Polkit,
        }
    }

    #[must_use]
    pub fn with_provider(mut self, provider: PrivilegeProvider) -> Self {
        self.provider = provider;
        self
    }

    pub fn handle(&self, request: BrokerRequest) -> Result<BrokerOutput, BrokerError> {
        self.handle_with_environment(request, std::env::vars().collect())
    }

    pub fn handle_with_environment(
        &self,
        request: BrokerRequest,
        environment: BTreeMap<String, String>,
    ) -> Result<BrokerOutput, BrokerError> {
        self.record(&request, AuditPhase::Attempt, AuditOutcome::Started)?;
        if let Err(error) = request.validate_subject(self.provider) {
            self.record(&request, AuditPhase::Completion, AuditOutcome::Failed)?;
            return Err(error);
        }
        let before = match ValidatedTarget::open(request.operation()) {
            Ok(target) => target,
            Err(error) => {
                self.record(&request, AuditPhase::Completion, AuditOutcome::Failed)?;
                return Err(error);
            }
        };
        let authorization = AuthorizationRequest::from_broker_request(&request, self.provider);
        let grant = match self.authorizer.authorize(&authorization) {
            Ok(grant) => grant,
            Err(AuthorizationError::Cancelled) => {
                self.record(&request, AuditPhase::Authorization, AuditOutcome::Denied)?;
                return Err(BrokerError::AuthorizationCancelled);
            }
            Err(AuthorizationError::Denied) => {
                self.record(&request, AuditPhase::Authorization, AuditOutcome::Denied)?;
                return Err(BrokerError::AuthorizationDenied);
            }
            Err(AuthorizationError::Unavailable) => {
                self.record(&request, AuditPhase::Authorization, AuditOutcome::Failed)?;
                return Err(BrokerError::AuthorizationUnavailable);
            }
        };
        if grant.expires_at_unix_millis() <= self.clock.now_unix_millis() {
            self.record(&request, AuditPhase::Authorization, AuditOutcome::Expired)?;
            return Err(BrokerError::AuthorizationExpired);
        }
        self.record(&request, AuditPhase::Authorization, AuditOutcome::Succeeded)?;
        let after = match ValidatedTarget::open(request.operation()) {
            Ok(target) => target,
            Err(error) => {
                self.record(&request, AuditPhase::Completion, AuditOutcome::Failed)?;
                return Err(error);
            }
        };
        if before.identity != after.identity {
            self.record(&request, AuditPhase::Completion, AuditOutcome::Failed)?;
            return Err(BrokerError::TargetReplaced);
        }
        let validated = ValidatedRequest {
            request_id: request.id().into(),
            operation: request.operation().clone(),
            target: after,
            environment: scrub_environment(environment),
            authorization: grant,
            provider: self.provider,
        };
        self.record(&request, AuditPhase::Dispatch, AuditOutcome::Started)?;
        let result = self.runner.execute(validated);
        self.record(
            &request,
            AuditPhase::Completion,
            if result.is_ok() {
                AuditOutcome::Succeeded
            } else {
                AuditOutcome::Failed
            },
        )?;
        result
    }

    fn record(
        &self,
        request: &BrokerRequest,
        phase: AuditPhase,
        outcome: AuditOutcome,
    ) -> Result<(), BrokerError> {
        let record = AuditRecord {
            request_id: request.id().into(),
            operation: request.operation().command_label().into(),
            target: request.target().to_path_buf(),
            provider: self.provider,
            phase,
            outcome,
            timestamp_unix_millis: self.clock.now_unix_millis(),
        };
        self.audit.record(&record)
    }
}

#[derive(Debug)]
struct ValidatedTarget {
    file: File,
    identity: FileIdentity,
}

impl ValidatedTarget {
    fn open(operation: &BrokerOperation) -> Result<Self, BrokerError> {
        if let BrokerOperation::ChangeOwnership { items, .. } = operation {
            return Self::open_ownership_items(items);
        }
        let directory = matches!(
            operation,
            BrokerOperation::OpenDirectory { .. } | BrokerOperation::ReadDirectory { .. }
        );
        let file = open_absolute_no_symlinks(operation.target(), directory)?;
        let metadata = file.metadata().map_err(|_| BrokerError::Io)?;
        if matches!(operation, BrokerOperation::RunExecutable { .. })
            && (!metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0)
        {
            return Err(BrokerError::NotExecutable);
        }
        if matches!(operation, BrokerOperation::RunExecutable { .. })
            && (!executable_mode_is_trusted(metadata.uid(), metadata.permissions().mode())
                || executable_has_access_acl(&file)?)
        {
            return Err(BrokerError::UnsafeExecutable);
        }
        Ok(Self {
            identity: FileIdentity::from_metadata(&metadata),
            file,
        })
    }
}

impl ValidatedTarget {
    /// Opens every item of an ownership change as it is, a symbolic link as
    /// the link, and checks that each is still the file the user reviewed on
    /// a filesystem with POSIX ownership. The first item is the target.
    fn open_ownership_items(items: &[OwnershipItem]) -> Result<Self, BrokerError> {
        let mut first = None;
        for item in items {
            let (file, metadata) = open_ownership_item(item)?;
            if first.is_none() {
                first = Some(Self {
                    identity: FileIdentity::from_metadata(&metadata),
                    file,
                });
            }
        }
        first.ok_or(BrokerError::InvalidRequest)
    }
}

/// Opens `item` without following any symbolic link, the last one
/// included, and checks it is the reviewed file on a supported filesystem.
fn open_ownership_item(item: &OwnershipItem) -> Result<(File, std::fs::Metadata), BrokerError> {
    let parent = item.path().parent().ok_or(BrokerError::InvalidRequest)?;
    let name = item.path().file_name().ok_or(BrokerError::InvalidRequest)?;
    let parent = open_absolute_no_symlinks(parent, true)?;
    let file = File::from(
        openat(
            &parent,
            name,
            OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(map_open_error)?,
    );
    let metadata = file.metadata().map_err(|_| BrokerError::Io)?;
    if metadata.dev() != item.device() || metadata.ino() != item.inode() {
        return Err(BrokerError::TargetReplaced);
    }
    ownership_supported(&file)?;
    Ok((file, metadata))
}

/// The owner and group an ownership change sets; `None` keeps one.
#[derive(Clone, Copy, Debug)]
struct OwnershipChange {
    owner: Option<u32>,
    group: Option<u32>,
}

impl OwnershipChange {
    /// Changes `file`, opened without following a link, and never its mode:
    /// the kernel alone clears setuid and setgid bits on an owner change.
    fn apply(self, file: &File) -> Result<(), BrokerError> {
        rustix::fs::chownat(
            file,
            "",
            self.owner.map(rustix::fs::Uid::from_raw),
            self.group.map(rustix::fs::Gid::from_raw),
            rustix::fs::AtFlags::EMPTY_PATH,
        )
        .map_err(map_open_error)
    }
}

/// Changes each item, then what its folders contain when `contents` says
/// so, and stops at the first item it cannot change. Items changed before
/// that stay changed.
fn change_ownership(
    items: &[OwnershipItem],
    change: OwnershipChange,
    contents: Option<OwnershipContents>,
) -> OwnershipReport {
    let mut report = OwnershipReport::new(0, None);
    for item in items {
        let changed = open_ownership_item(item)
            .map_err(|error| (item.path().to_path_buf(), error))
            .and_then(|(file, metadata)| {
                change
                    .apply(&file)
                    .map_err(|error| (item.path().to_path_buf(), error))?;
                report.changed += 1;
                match contents {
                    Some(contents) if metadata.is_dir() => change_contents(
                        &file,
                        item.path(),
                        metadata.dev(),
                        change,
                        contents,
                        &mut report,
                    ),
                    _ => Ok(()),
                }
            });
        if let Err((path, error)) = changed {
            report.failure = Some(OwnershipFailure::new(path, &error));
            break;
        }
    }
    report
}

/// Changes everything inside `folder`, depth first. It opens each entry
/// without following a link, so a link is changed itself and never leads
/// anywhere, and it leaves out a nested mount, and all it holds, unless
/// `contents` includes nested mounts.
fn change_contents(
    folder: &File,
    path: &Path,
    device: u64,
    change: OwnershipChange,
    contents: OwnershipContents,
    report: &mut OwnershipReport,
) -> Result<(), (PathBuf, BrokerError)> {
    use std::os::unix::ffi::OsStrExt as _;

    let failed = |error| (path.to_path_buf(), error);
    let listing = File::from(
        openat(
            folder,
            ".",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(map_open_error)
        .map_err(failed)?,
    );
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(&listing).map_err(|_| failed(BrokerError::Io))? {
        let entry = entry.map_err(|_| failed(BrokerError::Io))?;
        let name = entry.file_name();
        if name != c"." && name != c".." {
            names.push(name.to_owned());
        }
    }
    for name in names {
        let entry_path = path.join(std::ffi::OsStr::from_bytes(name.as_bytes()));
        let failed = |error| (entry_path.clone(), error);
        let entry = File::from(
            openat(
                &listing,
                name.as_c_str(),
                OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(map_open_error)
            .map_err(failed)?,
        );
        let metadata = entry.metadata().map_err(|_| failed(BrokerError::Io))?;
        if metadata.dev() != device {
            if !contents.nested_mounts {
                continue;
            }
            ownership_supported(&entry).map_err(failed)?;
        }
        change.apply(&entry).map_err(failed)?;
        report.changed += 1;
        if metadata.is_dir() {
            change_contents(
                &entry,
                &entry_path,
                metadata.dev(),
                change,
                contents,
                report,
            )?;
        }
    }
    Ok(())
}

/// Refuses filesystems whose ownership is not POSIX or not on this
/// machine: network filesystems, FUSE, and FAT, exFAT, NTFS, ISO 9660 and
/// UDF. The Permissions page is read-only on them; this is the broker's
/// own check.
fn ownership_supported(file: &File) -> Result<(), BrokerError> {
    const REFUSED: [u64; 14] = [
        0x6969,      // NFS
        0x517b,      // SMB
        0xff53_4d42, // CIFS
        0xfe53_4d42, // SMB2
        0x6573_5546, // FUSE
        0x00c3_6400, // Ceph
        0x5346_414f, // AFS
        0x0102_1997, // 9P
        0x4d44,      // FAT
        0x2011_bab0, // exFAT
        0x7366_746e, // NTFS (ntfs3)
        0x5346_544e, // NTFS
        0x9660,      // ISO 9660
        0x1501_3346, // UDF
    ];
    let filesystem = rustix::fs::fstatfs(file).map_err(|_| BrokerError::Io)?;
    #[allow(clippy::cast_sign_loss, clippy::unnecessary_cast)]
    let kind = filesystem.f_type as u64;
    if REFUSED.contains(&kind) {
        return Err(BrokerError::FilesystemUnsupported);
    }
    Ok(())
}

const fn executable_mode_is_trusted(owner: u32, mode: u32) -> bool {
    owner == 0 && mode & 0o022 == 0
}

fn executable_has_access_acl(file: &File) -> Result<bool, BrokerError> {
    use xattr::FileExt as _;

    file.get_xattr("system.posix_acl_access")
        .map(|attribute| attribute.is_some())
        .map_err(|_| BrokerError::Io)
}

#[cfg(test)]
mod listing_tests {
    use super::{
        BrokerDirectoryEntry, BrokerError, BrokerOutput, BrokerResponse, Clock, PrivilegeProvider,
        RootGrant, RootedEntryKind, RootedStore, decode_broker_response, encode_broker_response,
        list_directory,
    };
    use std::path::Path;

    #[derive(Clone)]
    struct At(u64);

    impl Clock for At {
        fn now_unix_millis(&self) -> u64 {
            self.0
        }
    }

    fn store(root: &Path) -> RootedStore<At> {
        let grant = RootGrant::open(root, "grant", 1_000, PrivilegeProvider::Polkit).unwrap();
        RootedStore::new(grant, At(100))
    }

    #[test]
    fn a_listing_counts_what_it_encodes_and_keeps_every_name_byte() {
        use std::os::unix::ffi::OsStrExt as _;

        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("plain.txt"), b"plain").unwrap();
        std::fs::write(
            root.path()
                .join(std::ffi::OsStr::from_bytes(b"caf\xe9-\x01\xff")),
            b"",
        )
        .unwrap();
        std::fs::create_dir(root.path().join("folder")).unwrap();
        let entries = list_directory(&store(root.path()), Path::new(""), usize::MAX).unwrap();
        assert_eq!(entries.len(), 3);

        let encoded = |entries: Vec<BrokerDirectoryEntry>| -> usize {
            serde_json::to_vec(&BrokerResponse::success(BrokerOutput::DirectoryEntries(
                entries,
            )))
            .unwrap()
            .len()
        };
        let counted: usize = entries
            .iter()
            .map(BrokerDirectoryEntry::listing_bytes)
            .sum();
        assert_eq!(encoded(entries.clone()), encoded(Vec::new()) + counted - 1);

        let frame = encode_broker_response(&BrokerResponse::success(
            BrokerOutput::DirectoryEntries(entries.clone()),
        ))
        .unwrap();
        assert_eq!(
            decode_broker_response(&frame).unwrap(),
            BrokerOutput::DirectoryEntries(entries.clone())
        );
        assert!(
            entries
                .iter()
                .any(|entry| entry.name() == b"caf\xe9-\x01\xff")
        );
    }

    #[test]
    fn an_entry_goes_as_an_array_without_key_names() {
        let entry = BrokerDirectoryEntry {
            name: b"abcdefgh".to_vec(),
            identity: [0; 16],
            kind: RootedEntryKind::RegularFile,
            size: Some(4096),
            modified_unix_seconds: Some(1_790_000_000),
        };
        assert_eq!(
            serde_json::to_string(&entry).unwrap(),
            r#"["YWJjZGVmZ2g","AAAAAAAAAAAAAAAAAAAAAA",1,4096,1790000000]"#
        );
        assert_eq!(entry.listing_bytes(), 59);
        assert!(
            serde_json::from_str::<BrokerDirectoryEntry>(
                r#"["YQ","AAAAAAAAAAAAAAAAAAAAAA",4,null,null]"#
            )
            .is_err(),
            "an unknown kind is refused"
        );
        assert!(
            serde_json::from_str::<BrokerDirectoryEntry>(r#"["YQ","AAAA",1,null,null]"#).is_err(),
            "an identity of the wrong length is refused"
        );
    }

    #[test]
    fn a_refused_entry_ends_the_read() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..100 {
            std::fs::write(root.path().join(format!("file-{index:03}")), b"").unwrap();
        }
        let mut admitted = 0;
        let result = store(root.path()).read_directory_with(Path::new(""), |_| {
            admitted += 1;
            if admitted == 10 {
                Err(BrokerError::ListingTooLarge)
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(BrokerError::ListingTooLarge));
        assert_eq!(admitted, 10, "admit sees no entry after the refusal");
    }

    #[test]
    fn a_listing_stops_when_it_passes_its_limit() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..100 {
            std::fs::write(root.path().join(format!("file-{index:03}")), b"").unwrap();
        }
        let store = store(root.path());
        let whole = list_directory(&store, Path::new(""), usize::MAX).unwrap();
        let bytes: usize = whole.iter().map(BrokerDirectoryEntry::listing_bytes).sum();

        assert_eq!(
            list_directory(&store, Path::new(""), bytes).unwrap(),
            whole,
            "a listing of exactly the limit is served"
        );
        assert_eq!(
            list_directory(&store, Path::new(""), bytes - 1),
            Err(BrokerError::ListingTooLarge)
        );
    }
}

#[cfg(test)]
mod executable_policy_tests {
    use super::executable_mode_is_trusted;

    #[test]
    fn executable_must_be_root_owned_and_not_group_or_world_writable() {
        assert!(executable_mode_is_trusted(0, 0o755));
        assert!(!executable_mode_is_trusted(1_000, 0o555));
        assert!(!executable_mode_is_trusted(0, 0o775));
        assert!(!executable_mode_is_trusted(0, 0o757));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileIdentity {
    device: u64,
    inode: u64,
    mode: u32,
}

impl FileIdentity {
    pub(crate) fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
        }
    }
}

pub(crate) fn open_absolute_no_symlinks(
    path: &Path,
    require_directory: bool,
) -> Result<File, BrokerError> {
    if !path.is_absolute() {
        return Err(BrokerError::InvalidRequest);
    }
    let mut directory = File::from(
        open(
            "/",
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| BrokerError::Io)?,
    );
    let components = path.components().collect::<Vec<_>>();
    let normal_count = components
        .iter()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    if normal_count == 0 {
        return if require_directory {
            Ok(directory)
        } else {
            Err(BrokerError::InvalidRequest)
        };
    }
    let mut seen = 0;
    for component in components {
        let Component::Normal(name) = component else {
            if matches!(component, Component::ParentDir | Component::Prefix(_)) {
                return Err(BrokerError::ScopeEscape);
            }
            continue;
        };
        seen += 1;
        let final_component = seen == normal_count;
        let mut flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        if !final_component || require_directory {
            flags |= OFlags::DIRECTORY;
        }
        directory =
            File::from(openat(&directory, name, flags, Mode::empty()).map_err(map_open_error)?);
    }
    Ok(directory)
}

fn map_open_error(error: rustix::io::Errno) -> BrokerError {
    if error == rustix::io::Errno::LOOP {
        BrokerError::SymlinkRefused
    } else {
        BrokerError::Io
    }
}

fn scrub_environment(environment: BTreeMap<String, String>) -> BTreeMap<String, String> {
    environment
        .into_iter()
        .filter(|(key, value)| {
            (key == "LANG" || key == "TERM" || key.starts_with("LC_"))
                && value.len() <= 128
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._@+-".contains(&byte))
        })
        .collect()
}

#[derive(Clone, Copy, Debug)]
pub struct SystemOperationRunner {
    timeout: Duration,
}

impl Default for SystemOperationRunner {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl SystemOperationRunner {
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl OperationRunner for SystemOperationRunner {
    fn execute(&self, request: ValidatedRequest) -> Result<BrokerOutput, BrokerError> {
        match request.operation {
            BrokerOperation::OpenDirectory { target } => {
                let reference = ElevatedRootReference::from_file(&target, &request.target.file)?;
                Ok(BrokerOutput::RootReferenced(reference))
            }
            BrokerOperation::ReadDirectory { root, relative } => {
                let grant = RootGrant::from_reference_file(
                    root,
                    request.target.file,
                    request.request_id,
                    request.authorization.expires_at_unix_millis(),
                    request.provider,
                )?;
                let store = RootedStore::new(grant, SystemClock);
                let entries = list_directory(&store, &relative, MAX_SESSION_LISTING_BYTES)?;
                Ok(BrokerOutput::DirectoryEntries(entries))
            }
            BrokerOperation::ChangeOwnership {
                items,
                owner,
                group,
                contents,
            } => {
                if owner == Some(u32::MAX) || group == Some(u32::MAX) {
                    return Err(BrokerError::InvalidRequest);
                }
                let change = OwnershipChange { owner, group };
                Ok(BrokerOutput::OwnershipChanged(change_ownership(
                    &items, change, contents,
                )))
            }
            BrokerOperation::RunExecutable { arguments, .. } => {
                use std::os::fd::AsRawFd as _;
                use std::os::unix::process::CommandExt as _;
                use std::process::Stdio;

                let executable = format!("/proc/self/fd/{}", request.target.file.as_raw_fd());
                let fd_flags =
                    rustix::io::fcntl_getfd(&request.target.file).map_err(|_| BrokerError::Io)?;
                rustix::io::fcntl_setfd(
                    &request.target.file,
                    fd_flags & !rustix::io::FdFlags::CLOEXEC,
                )
                .map_err(|_| BrokerError::Io)?;
                let child = std::process::Command::new(executable)
                    .args(arguments.iter())
                    .env_clear()
                    .envs(request.environment)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0)
                    .spawn();
                let _ = rustix::io::fcntl_setfd(&request.target.file, fd_flags);
                let mut child = child.map_err(|_| BrokerError::BrokerCrashed)?;
                let deadline = Instant::now() + self.timeout;
                loop {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            return Ok(BrokerOutput::Exited(status.code().unwrap_or(128)));
                        }
                        Ok(None) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Ok(None) => {
                            kill_and_reap_process_group(&mut child);
                            return Err(BrokerError::ExecutionTimedOut);
                        }
                        Err(_) => return Err(BrokerError::BrokerCrashed),
                    }
                }
            }
        }
    }
}
