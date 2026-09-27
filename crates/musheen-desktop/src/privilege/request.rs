use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::{BrokerError, ElevatedRootReference};

const MAX_ARGUMENTS: usize = 256;
const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub const OPEN_DIRECTORY_ACTION_ID: &str = "org.musheen.open-directory-as-administrator";
pub const RUN_EXECUTABLE_ACTION_ID: &str = "org.musheen.run-executable-as-administrator";
pub const CHANGE_OWNERSHIP_ACTION_ID: &str = "org.musheen.change-ownership-as-administrator";
pub const ADMIN_ACTION_IDS: [&str; 3] = [
    OPEN_DIRECTORY_ACTION_ID,
    RUN_EXECUTABLE_ACTION_ID,
    CHANGE_OWNERSHIP_ACTION_ID,
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrivilegeProvider {
    Polkit,
    Sudo,
}

impl PrivilegeProvider {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Polkit => "polkit",
            Self::Sudo => "sudo",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrokerOperation {
    OpenDirectory {
        #[serde(with = "path_bytes")]
        target: PathBuf,
    },
    RunExecutable {
        #[serde(with = "path_bytes")]
        target: PathBuf,
        arguments: Box<[String]>,
    },
    ReadDirectory {
        root: ElevatedRootReference,
        #[serde(with = "path_bytes")]
        relative: PathBuf,
    },
    /// Sets the owner, the group or both of `items`, each still the file the
    /// user reviewed, and with `contents` of what their folders contain
    /// (SYS-037).
    ChangeOwnership {
        items: Box<[OwnershipItem]>,
        owner: Option<u32>,
        group: Option<u32>,
        contents: Option<OwnershipContents>,
    },
}

/// A file or folder an ownership change names: its path and the identity the
/// user reviewed. A symbolic link is the link itself.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipItem {
    #[serde(with = "path_bytes")]
    path: PathBuf,
    device: u64,
    inode: u64,
}

impl OwnershipItem {
    /// `path` as it is now, without following a symbolic link at its end.
    pub fn reviewed(path: impl AsRef<Path>) -> Result<Self, BrokerError> {
        use std::os::unix::fs::MetadataExt as _;

        let path = path.as_ref();
        validate_target(path)?;
        let metadata = std::fs::symlink_metadata(path).map_err(|_| BrokerError::Io)?;
        Ok(Self::new(path, metadata.dev(), metadata.ino()))
    }

    #[must_use]
    pub fn new(path: impl AsRef<Path>, device: u64, inode: u64) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            device,
            inode,
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub const fn device(&self) -> u64 {
        self.device
    }

    #[must_use]
    pub const fn inode(&self) -> u64 {
        self.inode
    }
}

/// Apply to contents for an ownership change: what a folder contains, and
/// the folders of nested mounts only when the reviewed scope includes them.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnershipContents {
    pub nested_mounts: bool,
}

impl BrokerOperation {
    #[must_use]
    pub fn target(&self) -> &Path {
        match self {
            Self::OpenDirectory { target } | Self::RunExecutable { target, .. } => target,
            Self::ReadDirectory { root, .. } => root.root(),
            Self::ChangeOwnership { items, .. } => {
                items.first().map_or(Path::new("/"), |item| &item.path)
            }
        }
    }

    /// The Polkit action that authorizes the operation. A folder listing runs
    /// in the session its Open as Administrator request authorized (SYS-034).
    #[must_use]
    pub const fn action_id(&self) -> &'static str {
        match self {
            Self::OpenDirectory { .. } | Self::ReadDirectory { .. } => OPEN_DIRECTORY_ACTION_ID,
            Self::RunExecutable { .. } => RUN_EXECUTABLE_ACTION_ID,
            Self::ChangeOwnership { .. } => CHANGE_OWNERSHIP_ACTION_ID,
        }
    }

    #[must_use]
    pub const fn command_label(&self) -> &'static str {
        match self {
            Self::OpenDirectory { .. } => "Open as Administrator",
            Self::RunExecutable { .. } => "Run as Administrator",
            Self::ReadDirectory { .. } => "Browse as Administrator",
            Self::ChangeOwnership { .. } => "Change Ownership as Administrator",
        }
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        match self {
            Self::OpenDirectory { .. } => &[],
            Self::RunExecutable { arguments, .. } => arguments,
            Self::ReadDirectory { .. } | Self::ChangeOwnership { .. } => &[],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerRequest {
    id: Box<str>,
    subject: RequestSubject,
    operation: BrokerOperation,
    #[serde(skip, default)]
    subject_is_trusted: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSubject {
    pid: u32,
    uid: u32,
    start_time: u64,
}

impl RequestSubject {
    pub fn current() -> Result<Self, BrokerError> {
        Ok(Self {
            pid: std::process::id(),
            uid: rustix::process::geteuid().as_raw(),
            start_time: process_start_time(std::process::id())
                .ok_or(BrokerError::InvalidRequest)?,
        })
    }

    #[must_use]
    pub const fn pid(self) -> u32 {
        self.pid
    }

    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    #[must_use]
    pub const fn start_time(self) -> u64 {
        self.start_time
    }

    pub fn validate_live(self) -> Result<(), BrokerError> {
        use std::os::unix::fs::MetadataExt as _;

        let process = PathBuf::from(format!("/proc/{}", self.pid));
        let metadata = std::fs::metadata(&process).map_err(|_| BrokerError::AuthorizationDenied)?;
        if metadata.uid() != self.uid || process_start_time(self.pid) != Some(self.start_time) {
            return Err(BrokerError::AuthorizationDenied);
        }
        Ok(())
    }

    fn from_process(pid: u32, uid: u32) -> Result<Self, BrokerError> {
        let subject = Self {
            pid,
            uid,
            start_time: process_start_time(pid).ok_or(BrokerError::AuthorizationDenied)?,
        };
        subject.validate_live()?;
        Ok(subject)
    }

    const fn from_authenticated_uid(uid: u32) -> Self {
        Self {
            pid: 0,
            uid,
            start_time: 0,
        }
    }
}

impl BrokerRequest {
    pub fn open_directory(target: impl AsRef<Path>) -> Result<Self, BrokerError> {
        let target = target.as_ref();
        validate_target(target)?;
        Ok(Self {
            id: next_request_id(),
            subject: RequestSubject::current()?,
            operation: BrokerOperation::OpenDirectory {
                target: target.to_path_buf(),
            },
            subject_is_trusted: true,
        })
    }

    pub fn run_executable<I, S>(target: impl AsRef<Path>, arguments: I) -> Result<Self, BrokerError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let target = target.as_ref();
        validate_target(target)?;
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        if arguments.len() > MAX_ARGUMENTS
            || arguments.iter().map(String::len).sum::<usize>() > MAX_ARGUMENT_BYTES
            || arguments.iter().any(|argument| argument.contains('\0'))
        {
            return Err(BrokerError::InvalidRequest);
        }
        Ok(Self {
            id: next_request_id(),
            subject: RequestSubject::current()?,
            operation: BrokerOperation::RunExecutable {
                target: target.to_path_buf(),
                arguments: arguments.into_boxed_slice(),
            },
            subject_is_trusted: true,
        })
    }

    pub fn read_directory(
        root: ElevatedRootReference,
        relative: impl AsRef<Path>,
    ) -> Result<Self, BrokerError> {
        let relative = relative.as_ref();
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err(BrokerError::ScopeEscape);
        }
        Ok(Self {
            id: next_request_id(),
            subject: RequestSubject::current()?,
            operation: BrokerOperation::ReadDirectory {
                root,
                relative: relative.to_path_buf(),
            },
            subject_is_trusted: true,
        })
    }

    /// One ownership change for every item of an Apply, so it asks for
    /// authorization once (SYS-037). It must change an owner or a group, and
    /// it must fit one broker request line.
    pub fn change_ownership(
        items: Vec<OwnershipItem>,
        owner: Option<u32>,
        group: Option<u32>,
        contents: Option<OwnershipContents>,
    ) -> Result<Self, BrokerError> {
        if items.is_empty()
            || (owner.is_none() && group.is_none())
            || owner == Some(u32::MAX)
            || group == Some(u32::MAX)
        {
            return Err(BrokerError::InvalidRequest);
        }
        for item in &items {
            validate_target(&item.path)?;
        }
        let request = Self {
            id: next_request_id(),
            subject: RequestSubject::current()?,
            operation: BrokerOperation::ChangeOwnership {
                items: items.into_boxed_slice(),
                owner,
                group,
                contents,
            },
            subject_is_trusted: true,
        };
        if super::encode_broker_request(&request)?.len() >= super::MAX_REQUEST_LINE_BYTES {
            return Err(BrokerError::InvalidRequest);
        }
        Ok(request)
    }

    /// Replaces the untrusted JSON subject with identity established by the
    /// elevation mechanism. `parent_pid` must come from `getppid(2)`, never
    /// from request data.
    pub fn bind_to_invoker(
        &mut self,
        provider: PrivilegeProvider,
        environment: &BTreeMap<String, String>,
        parent_pid: u32,
    ) -> Result<(), BrokerError> {
        if environment.contains_key(match provider {
            PrivilegeProvider::Polkit => "SUDO_UID",
            PrivilegeProvider::Sudo => "PKEXEC_UID",
        }) {
            return Err(BrokerError::AuthorizationDenied);
        }
        self.subject = match provider {
            PrivilegeProvider::Polkit => {
                let uid = parse_invoking_uid(environment.get("PKEXEC_UID"))?;
                RequestSubject::from_process(parent_pid, uid)?
            }
            PrivilegeProvider::Sudo => RequestSubject::from_authenticated_uid(parse_invoking_uid(
                environment.get("SUDO_UID"),
            )?),
        };
        self.subject_is_trusted = true;
        Ok(())
    }

    pub(crate) fn validate_subject(&self, provider: PrivilegeProvider) -> Result<(), BrokerError> {
        if !self.subject_is_trusted {
            return Err(BrokerError::AuthorizationDenied);
        }
        if provider == PrivilegeProvider::Sudo && self.subject.pid == 0 {
            return Ok(());
        }
        self.subject.validate_live()
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    #[must_use]
    pub const fn operation(&self) -> &BrokerOperation {
        &self.operation
    }

    #[must_use]
    pub const fn subject(&self) -> RequestSubject {
        self.subject
    }

    #[must_use]
    pub fn binding_digest(&self) -> blake3::Hash {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.operation_digest().as_bytes());
        hasher.update(&self.subject.pid.to_le_bytes());
        hasher.update(&self.subject.uid.to_le_bytes());
        hasher.update(&self.subject.start_time.to_le_bytes());
        hasher.finalize()
    }

    #[must_use]
    pub fn operation_digest(&self) -> blake3::Hash {
        let operation = serde_json::to_vec(&self.operation)
            .expect("the closed broker operation schema always serializes");
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.id.as_bytes());
        hasher.update(&operation);
        hasher.finalize()
    }

    #[must_use]
    pub fn target(&self) -> &Path {
        self.operation.target()
    }

    #[must_use]
    pub fn confirmation(&self, provider: PrivilegeProvider) -> ConfirmationSummary {
        ConfirmationSummary {
            provider,
            command: self.operation.command_label(),
            target: self.target().to_path_buf(),
            arguments: self.operation.arguments().to_vec().into_boxed_slice(),
        }
    }
}

fn parse_invoking_uid(value: Option<&String>) -> Result<u32, BrokerError> {
    let value = value.ok_or(BrokerError::AuthorizationDenied)?;
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(BrokerError::AuthorizationDenied);
    }
    value
        .parse::<u32>()
        .map_err(|_| BrokerError::AuthorizationDenied)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConfirmationSummary {
    provider: PrivilegeProvider,
    command: &'static str,
    target: PathBuf,
    arguments: Box<[String]>,
}

impl ConfirmationSummary {
    #[must_use]
    pub const fn provider(&self) -> &'static str {
        self.provider.as_str()
    }

    #[must_use]
    pub const fn command(&self) -> &'static str {
        self.command
    }

    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    #[must_use]
    pub fn arguments(&self) -> &[String] {
        &self.arguments
    }
}

fn validate_target(target: &Path) -> Result<(), BrokerError> {
    if !target.is_absolute() || target.as_os_str().is_empty() {
        return Err(BrokerError::InvalidRequest);
    }
    Ok(())
}

fn next_request_id() -> Box<str> {
    format!(
        "{}-{}",
        std::process::id(),
        NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
    )
    .into_boxed_str()
}

fn process_start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = stat.rsplit_once(')')?.1;
    tail.split_whitespace().nth(19)?.parse().ok()
}

pub(crate) mod path_bytes {
    use base64::Engine as _;
    use serde::{Deserialize as _, Deserializer, Serializer};
    use std::ffi::OsString;
    use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
    use std::path::{Path, PathBuf};

    pub fn serialize<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(path.as_os_str().as_bytes()),
        )
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PathBuf, D::Error> {
        let encoded = <String>::deserialize(deserializer)?;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }
}
