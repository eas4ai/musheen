//! Validated custom commands. User-controlled file names only enter argv.
pub mod scripts;
use musheen_core::StorePath;
pub use scripts::{ScriptActionLoadError, ScriptActionLoader};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
const ENVIRONMENT_ALLOWLIST: &[&str] = &[
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "HOME",
];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    tag = "kind",
    content = "value",
    deny_unknown_fields
)]
pub enum ActionArgument {
    Literal(String),
    File,
    Files,
    Directory,
    Uris,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", deny_unknown_fields)]
pub enum ActionExecution {
    Direct {
        #[serde(with = "local_path")]
        executable: PathBuf,
    },
    /// The script is constant. Selection values are positional arguments ($1, "$@").
    Shell { script: String, opted_in: bool },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    tag = "policy",
    content = "path",
    deny_unknown_fields
)]
pub enum WorkingDirectory {
    CurrentLocation,
    Fixed(#[serde(with = "local_path")] PathBuf),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionConfirmation {
    Never,
    Always,
    Destructive,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomAction {
    pub id: String,
    pub label: String,
    pub execution: ActionExecution,
    pub arguments: Vec<ActionArgument>,
    pub working_directory: WorkingDirectory,
    pub mime_patterns: Vec<String>,
    #[serde(with = "optional_local_path")]
    pub location_prefix: Option<PathBuf>,
    pub supports_provider_uris: bool,
    pub confirmation: ActionConfirmation,
    pub environment: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomActionDocument {
    version: u32,
    actions: Vec<CustomAction>,
}

impl Default for CustomActionDocument {
    fn default() -> Self {
        Self {
            version: 1,
            actions: Vec::new(),
        }
    }
}

impl CustomActionDocument {
    pub fn new(actions: Vec<CustomAction>) -> Result<Self, CustomActionError> {
        if actions.len() > 64 {
            return Err(CustomActionError::InvalidDocument);
        }
        let mut ids = BTreeSet::new();
        for action in &actions {
            action.validate()?;
            if !ids.insert(&action.id) {
                return Err(CustomActionError::InvalidDocument);
            }
        }
        let document = Self {
            version: 1,
            actions,
        };
        if document.export().len() > MAX_DOCUMENT_BYTES {
            return Err(CustomActionError::InvalidDocument);
        }
        Ok(document)
    }
    pub fn import(text: &str) -> Result<Self, CustomActionError> {
        if text.len() > MAX_DOCUMENT_BYTES {
            return Err(CustomActionError::InvalidDocument);
        }
        let parsed: Self =
            serde_json::from_str(text).map_err(|_| CustomActionError::InvalidDocument)?;
        if parsed.version != 1 {
            return Err(CustomActionError::InvalidDocument);
        }
        Self::new(parsed.actions)
    }
    pub fn export(&self) -> String {
        serde_json::to_string(self).expect("serializable action document")
    }
    pub fn actions(&self) -> &[CustomAction] {
        &self.actions
    }
    pub fn get(&self, id: &str) -> Option<&CustomAction> {
        self.actions.iter().find(|action| action.id == id)
    }
    pub fn upsert(&mut self, action: CustomAction) -> Result<(), CustomActionError> {
        action.validate()?;
        let mut actions = self.actions.clone();
        if let Some(existing) = actions.iter_mut().find(|item| item.id == action.id) {
            *existing = action;
        } else {
            actions.push(action);
        }
        *self = Self::new(actions)?;
        Ok(())
    }
    pub fn remove(&mut self, id: &str) {
        self.actions.retain(|action| action.id != id);
    }
}

pub struct ActionSelection {
    pub paths: Vec<StorePath>,
    pub mime_types: Vec<String>,
    pub location: StorePath,
}

impl ActionSelection {
    pub fn inspect(paths: Vec<StorePath>, location: StorePath) -> Result<Self, CustomActionError> {
        if paths.is_empty() || paths.len() > 4096 {
            return Err(CustomActionError::SelectionMismatch);
        }
        let detector = crate::MimeDetector::default();
        let mime_types = paths
            .iter()
            .map(|path| {
                let Some(path) = path.as_unix_path() else {
                    return Ok("application/octet-stream".to_owned());
                };
                let metadata = path.symlink_metadata().map_err(CustomActionError::Io)?;
                if !metadata.is_file() && !metadata.is_dir() && !metadata.is_symlink() {
                    return Err(CustomActionError::SelectionMismatch);
                }
                detector
                    .detect(path)
                    .map(|mime| mime.mime_type().to_owned())
                    .map_err(|_| CustomActionError::SelectionMismatch)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            paths,
            mime_types,
            location,
        })
    }
}

impl CustomAction {
    pub fn validate(&self) -> Result<(), CustomActionError> {
        let valid_id = !self.id.is_empty()
            && self.id.len() <= 64
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        let valid_label = !self.label.trim().is_empty()
            && self.label.len() <= 160
            && !self.label.chars().any(char::is_control);
        if !valid_id
            || !valid_label
            || self.arguments.len() > 128
            || self.mime_patterns.is_empty()
            || self.mime_patterns.len() > 32
            || !(1..=300_000).contains(&self.timeout_ms)
            || self
                .environment
                .iter()
                .any(|key| !ENVIRONMENT_ALLOWLIST.contains(&key.as_str()))
            || self
                .mime_patterns
                .iter()
                .any(|pattern| !valid_mime_pattern(pattern))
        {
            return Err(CustomActionError::InvalidDocument);
        }
        match &self.execution {
            ActionExecution::Direct { executable } if valid_absolute_path(executable) => {}
            ActionExecution::Shell {
                script,
                opted_in: true,
            } if !script.is_empty()
                && script.len() <= 16_384
                && !script.contains('\0')
                && !["{file}", "{files}", "{directory}", "{uris}"]
                    .iter()
                    .any(|p| script.contains(p))
                && self.confirmation != ActionConfirmation::Never => {}
            _ => return Err(CustomActionError::InvalidDocument),
        }
        if let WorkingDirectory::Fixed(path) = &self.working_directory
            && !valid_absolute_path(path)
        {
            return Err(CustomActionError::InvalidDocument);
        }
        if self
            .location_prefix
            .as_ref()
            .is_some_and(|path| !valid_absolute_path(path))
        {
            return Err(CustomActionError::InvalidDocument);
        }
        if self.arguments.iter().any(|arg| matches!(arg, ActionArgument::Literal(value) if value.len() > 16_384 || value.contains('\0'))) { return Err(CustomActionError::InvalidDocument); }
        Ok(())
    }
    pub fn fingerprint(&self) -> String {
        serde_json::to_string(self).expect("serializable custom action")
    }
    pub fn requires_confirmation(&self) -> bool {
        self.confirmation != ActionConfirmation::Never
            || matches!(self.execution, ActionExecution::Shell { .. })
    }

    pub fn prepare(
        &self,
        selection: &ActionSelection,
        environment: &BTreeMap<String, OsString>,
    ) -> Result<PreparedCustomAction, CustomActionError> {
        self.validate()?;
        self.matches_selection(selection)?;
        let working_directory = match &self.working_directory {
            WorkingDirectory::CurrentLocation => selection
                .location
                .as_unix_path()
                .ok_or(CustomActionError::WorkingDirectory)?
                .to_path_buf(),
            WorkingDirectory::Fixed(path) => path.clone(),
        };
        if !valid_absolute_path(&working_directory) || !working_directory.is_dir() {
            return Err(CustomActionError::WorkingDirectory);
        }
        let mut arguments = Vec::new();
        let executable = match &self.execution {
            ActionExecution::Direct { executable } => executable.clone(),
            ActionExecution::Shell { script, .. } => {
                arguments.extend([
                    OsString::from("-c"),
                    script.into(),
                    OsString::from("musheen-action"),
                ]);
                PathBuf::from("/bin/sh")
            }
        };
        for argument in &self.arguments {
            expand_argument(argument, selection, &mut arguments)?;
        }
        if arguments.len() > 4096
            || arguments.iter().map(|value| value.len()).sum::<usize>() > MAX_DOCUMENT_BYTES
        {
            return Err(CustomActionError::SelectionMismatch);
        }
        Ok(PreparedCustomAction {
            executable,
            arguments,
            working_directory,
            environment: self
                .environment
                .iter()
                .filter_map(|key| {
                    environment
                        .get(key)
                        .map(|value| (key.clone(), value.clone()))
                })
                .collect(),
            timeout: Duration::from_millis(self.timeout_ms),
            confirmation: self.requires_confirmation(),
        })
    }

    pub fn matches_selection(&self, selection: &ActionSelection) -> Result<(), CustomActionError> {
        if selection.paths.is_empty()
            || selection.paths.len() > 4096
            || selection.paths.len() != selection.mime_types.len()
        {
            return Err(CustomActionError::SelectionMismatch);
        }
        if selection
            .paths
            .iter()
            .any(|path| path.as_unix_path().is_none())
            && !self.supports_provider_uris
        {
            return Err(CustomActionError::ProviderUrisUnsupported);
        }
        if self.location_prefix.as_ref().is_some_and(|prefix| {
            selection
                .location
                .as_unix_path()
                .is_none_or(|path| !path.starts_with(prefix))
        }) || selection.mime_types.iter().any(|mime| {
            !self
                .mime_patterns
                .iter()
                .any(|pattern| mime_matches(pattern, mime))
        }) {
            return Err(CustomActionError::SelectionMismatch);
        }
        Ok(())
    }
}

fn valid_absolute_path(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.is_absolute()
        && !path.as_os_str().as_bytes().contains(&0)
        && !path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
}
fn valid_mime_pattern(pattern: &str) -> bool {
    pattern.len() <= 128
        && pattern.split_once('/').is_some_and(|(kind, subtype)| {
            !kind.is_empty()
                && !subtype.is_empty()
                && (kind != "*" || subtype == "*")
                && [kind, subtype].iter().all(|part| {
                    *part == "*"
                        || part
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b".+-_".contains(&b))
                })
        })
}
fn mime_matches(pattern: &str, mime: &str) -> bool {
    pattern == "*/*"
        || pattern == mime
        || pattern.strip_suffix("/*").is_some_and(|kind| {
            mime.strip_prefix(kind)
                .is_some_and(|rest| rest.starts_with('/'))
        })
}
fn expand_argument(
    argument: &ActionArgument,
    selection: &ActionSelection,
    arguments: &mut Vec<OsString>,
) -> Result<(), CustomActionError> {
    match argument {
        ActionArgument::Literal(value) => arguments.push(value.into()),
        ActionArgument::File if selection.paths.len() != 1 => {
            return Err(CustomActionError::SelectionMismatch);
        }
        ActionArgument::File | ActionArgument::Files => {
            for path in &selection.paths {
                let path = path
                    .as_unix_path()
                    .filter(|path| valid_absolute_path(path))
                    .ok_or(CustomActionError::ProviderUrisUnsupported)?;
                arguments.push(path.into());
            }
        }
        ActionArgument::Directory => arguments.push(
            selection
                .location
                .as_unix_path()
                .filter(|path| valid_absolute_path(path))
                .ok_or(CustomActionError::WorkingDirectory)?
                .into(),
        ),
        ActionArgument::Uris => {
            for path in &selection.paths {
                arguments.push(provider_uri(path)?);
            }
        }
    }
    Ok(())
}
fn provider_uri(path: &StorePath) -> Result<OsString, CustomActionError> {
    use std::fmt::Write;
    if let Some(bytes) = path.unix_bytes() {
        if !path.as_unix_path().is_some_and(valid_absolute_path) {
            return Err(CustomActionError::SelectionMismatch);
        }
        let mut uri = String::from("file://");
        for byte in bytes {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(byte) {
                uri.push(char::from(*byte));
            } else {
                write!(uri, "%{byte:02X}").expect("write to string");
            }
        }
        return Ok(uri.into());
    }
    let (provider, bytes) = path
        .provider_key()
        .ok_or(CustomActionError::ProviderUrisUnsupported)?;
    let uri = std::str::from_utf8(bytes).map_err(|_| CustomActionError::ProviderUrisUnsupported)?;
    let prefix = format!("{}://", provider.as_str());
    if !uri.starts_with(&prefix)
        || uri.len() <= prefix.len()
        || uri.chars().any(|c| c.is_control() || c.is_whitespace())
        || uri[prefix.len()..]
            .split('/')
            .next()
            .is_some_and(|host| host.contains('@'))
    {
        return Err(CustomActionError::ProviderUrisUnsupported);
    }
    Ok(uri.into())
}

pub struct PreparedCustomAction {
    executable: PathBuf,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
    environment: BTreeMap<String, OsString>,
    timeout: Duration,
    confirmation: bool,
}
impl PreparedCustomAction {
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }
    pub fn environment(&self) -> &BTreeMap<String, OsString> {
        &self.environment
    }
}

pub struct CustomActionRunner;
impl CustomActionRunner {
    /// Run on a background worker. The isolated process group bounds descendants too.
    pub fn run(action: PreparedCustomAction, confirmed: bool) -> Result<(), CustomActionError> {
        if action.confirmation && !confirmed {
            return Err(CustomActionError::ConfirmationRequired);
        }
        let mut child = spawn_action(&action)?;
        wait_for_action(&mut child, action.timeout)
    }
}

fn spawn_action(action: &PreparedCustomAction) -> Result<std::process::Child, CustomActionError> {
    Command::new(&action.executable)
        .args(&action.arguments)
        .current_dir(&action.working_directory)
        .env_clear()
        .envs(&action.environment)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                CustomActionError::MissingExecutable
            } else {
                CustomActionError::Io(error)
            }
        })
}

fn wait_for_action(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<(), CustomActionError> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(CustomActionError::ExitStatus(status.code())),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(5));
            }
            result => {
                terminate_action_group(child)?;
                return Err(match result {
                    Err(error) => CustomActionError::Io(error),
                    _ => CustomActionError::Timeout,
                });
            }
        }
    }
}

fn terminate_action_group(child: &mut std::process::Child) -> Result<(), CustomActionError> {
    let group = rustix::process::Pid::from_raw(child.id() as i32)
        .expect("a spawned child has a positive process ID");
    if let Err(error) = rustix::process::kill_process_group(group, rustix::process::Signal::KILL)
        && error != rustix::io::Errno::SRCH
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(CustomActionError::Io(error.into()));
    }
    child.wait().map_err(CustomActionError::Io)?;
    Ok(())
}

#[derive(Debug)]
pub enum CustomActionError {
    InvalidDocument,
    SelectionMismatch,
    ProviderUrisUnsupported,
    WorkingDirectory,
    ConfirmationRequired,
    MissingExecutable,
    Timeout,
    ExitStatus(Option<i32>),
    Io(std::io::Error),
    ScriptSource(Box<ScriptActionLoadError>),
}
impl CustomActionError {
    pub fn message_key(&self) -> &'static str {
        match self {
            Self::InvalidDocument => "custom-action-invalid",
            Self::SelectionMismatch => "custom-action-selection",
            Self::ProviderUrisUnsupported => "custom-action-provider",
            Self::WorkingDirectory => "custom-action-directory",
            Self::ConfirmationRequired => "custom-action-confirmation",
            Self::MissingExecutable => "custom-action-missing",
            Self::Timeout => "custom-action-timeout",
            Self::ExitStatus(_) => "custom-action-exit",
            Self::Io(_) => "custom-action-io",
            Self::ScriptSource(error) => error.message_key(),
        }
    }
}
impl std::fmt::Display for CustomActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "custom action process failed: {error}"),
            Self::ExitStatus(code) => write!(f, "custom action exited unsuccessfully: {code:?}"),
            _ => f.write_str(self.message_key()),
        }
    }
}
impl std::error::Error for CustomActionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::ScriptSource(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

// Reuse the provider path wire format so configuration paths, like selection
// paths, never pass through lossy UTF-8 conversion during persistence.
mod local_path {
    use super::*;
    pub fn serialize<S: serde::Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
        StorePath::from_unix_path(path).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<PathBuf, D::Error> {
        StorePath::deserialize(deserializer)?
            .as_unix_path()
            .map(Path::to_path_buf)
            .ok_or_else(|| serde::de::Error::custom("a local path is required"))
    }
}
mod optional_local_path {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        path: &Option<PathBuf>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        path.as_ref()
            .map(StorePath::from_unix_path)
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<PathBuf>, D::Error> {
        Option::<StorePath>::deserialize(deserializer)?
            .map(|path| {
                path.as_unix_path()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| serde::de::Error::custom("a local path is required"))
            })
            .transpose()
    }
}
