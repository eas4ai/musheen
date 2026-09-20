//! Opt-in script-directory discovery for custom actions.
//!
//! Every action needs an explicit `*.musheen-action.json` manifest. The
//! loader never infers policy from a filename and never invokes a shell.

use super::{
    ActionArgument, ActionConfirmation, ActionExecution, CustomAction, CustomActionDocument,
    CustomActionError, WorkingDirectory,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const MANIFEST_SUFFIX: &[u8] = b".musheen-action.json";

/// Loads explicitly enabled script-directory actions under fixed resource
/// limits. Callers opt in by invoking this loader with a configured directory.
pub struct ScriptActionLoader;

impl ScriptActionLoader {
    /// Called only by the explicit Settings control. Existing paths are not chmodded.
    pub fn create_directory(directory: &Path) -> Result<(), ScriptActionLoadError> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(|source| ScriptActionLoadError::io(directory, source))
    }

    pub fn load_optional(directory: &Path) -> Result<CustomActionDocument, ScriptActionLoadError> {
        match Self::load(directory) {
            Err(ScriptActionLoadError::Io { path, source })
                if path == directory && source.kind() == io::ErrorKind::NotFound =>
            {
                Ok(CustomActionDocument::default())
            }
            result => result,
        }
    }
    pub const MAX_DIRECTORY_ENTRIES: usize = 256;
    pub const MAX_MANIFEST_BYTES: usize = 32 * 1024;
    pub const MAX_TOTAL_MANIFEST_BYTES: usize = 256 * 1024;
    pub const MAX_SCRIPT_BYTES: u64 = 4 * 1024 * 1024;
    pub const MAX_TOTAL_SCRIPT_BYTES: u64 = 16 * 1024 * 1024;

    pub fn load(directory: &Path) -> Result<CustomActionDocument, ScriptActionLoadError> {
        let root = canonical_directory(directory)?;
        let mut actions = Vec::new();
        let mut budget = LoadBudget::default();
        for manifest_path in manifest_paths(&root)? {
            let manifest = budget.read_manifest(&manifest_path)?;
            actions.push(manifest.into_action(&root, &mut budget)?);
        }
        actions.sort_by(|left, right| left.id.cmp(&right.id));
        CustomActionDocument::new(actions).map_err(ScriptActionLoadError::InvalidAction)
    }
}

fn manifest_paths(root: &Path) -> Result<Vec<PathBuf>, ScriptActionLoadError> {
    let entries = fs::read_dir(root)
        .map_err(|source| ScriptActionLoadError::io(root, source))?
        .take(ScriptActionLoader::MAX_DIRECTORY_ENTRIES + 1)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| ScriptActionLoadError::io(root, source))?;
    if entries.len() > ScriptActionLoader::MAX_DIRECTORY_ENTRIES {
        return Err(ScriptActionLoadError::TooManyEntries);
    }
    let mut paths = entries
        .into_iter()
        .filter(|entry| entry.file_name().as_bytes().ends_with(MANIFEST_SUFFIX))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

#[derive(Default)]
struct LoadBudget {
    manifest_bytes: usize,
    script_bytes: u64,
    counted_scripts: BTreeSet<PathBuf>,
}

impl LoadBudget {
    fn read_manifest(
        &mut self,
        path: &Path,
    ) -> Result<ScriptActionManifest, ScriptActionLoadError> {
        let before =
            fs::symlink_metadata(path).map_err(|source| ScriptActionLoadError::io(path, source))?;
        if before.file_type().is_symlink() || !before.file_type().is_file() {
            return Err(ScriptActionLoadError::InvalidManifest { path: path.into() });
        }
        let length = usize::try_from(before.len()).unwrap_or(usize::MAX);
        if length > ScriptActionLoader::MAX_MANIFEST_BYTES {
            return Err(ScriptActionLoadError::ManifestTooLarge { path: path.into() });
        }
        self.manifest_bytes = self
            .manifest_bytes
            .checked_add(length)
            .filter(|total| *total <= ScriptActionLoader::MAX_TOTAL_MANIFEST_BYTES)
            .ok_or(ScriptActionLoadError::TotalManifestBytesExceeded)?;
        let bytes = read_stable_manifest(path, &before)?;
        let manifest: ScriptActionManifest = serde_json::from_slice(&bytes)
            .map_err(|_| ScriptActionLoadError::InvalidManifest { path: path.into() })?;
        if manifest.version != 1 {
            return Err(ScriptActionLoadError::InvalidManifest { path: path.into() });
        }
        Ok(manifest)
    }

    fn validate_script(&mut self, path: &Path) -> Result<(), ScriptActionLoadError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|source| ScriptActionLoadError::io(path, source))?;
        validate_script(path, &metadata)?;
        if self.counted_scripts.insert(path.into()) {
            self.script_bytes = self
                .script_bytes
                .checked_add(metadata.len())
                .filter(|total| *total <= ScriptActionLoader::MAX_TOTAL_SCRIPT_BYTES)
                .ok_or(ScriptActionLoadError::TotalScriptBytesExceeded)?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptActionManifest {
    version: u32,
    id: String,
    label: String,
    script: ScriptReference,
    arguments: Vec<ActionArgument>,
    working_directory: WorkingDirectory,
    mime_patterns: Vec<String>,
    location_prefix: Option<PathBuf>,
    supports_provider_uris: bool,
    confirmation: ActionConfirmation,
    environment: Vec<String>,
    timeout_ms: u64,
}

impl ScriptActionManifest {
    fn into_action(
        self,
        root: &Path,
        budget: &mut LoadBudget,
    ) -> Result<CustomAction, ScriptActionLoadError> {
        let script_name = self
            .script
            .into_os_string()
            .ok_or_else(|| ScriptActionLoadError::UnsafeScript { path: root.into() })?;
        let script_path = root.join(script_name);
        budget.validate_script(&script_path)?;
        let action = CustomAction {
            id: self.id,
            label: self.label,
            execution: ActionExecution::Direct {
                executable: script_path,
            },
            arguments: self.arguments,
            working_directory: self.working_directory,
            mime_patterns: self.mime_patterns,
            location_prefix: self.location_prefix,
            supports_provider_uris: self.supports_provider_uris,
            confirmation: self.confirmation,
            environment: self.environment,
            timeout_ms: self.timeout_ms,
        };
        action
            .validate()
            .map_err(ScriptActionLoadError::InvalidAction)?;
        Ok(action)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ScriptReference {
    Utf8(String),
    UnixBytes { unix_bytes: Vec<u8> },
}

impl ScriptReference {
    fn into_os_string(self) -> Option<OsString> {
        let bytes = match self {
            Self::Utf8(value) => value.into_bytes(),
            Self::UnixBytes { unix_bytes } => unix_bytes,
        };
        if bytes.is_empty()
            || bytes.len() > 255
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&0)
            || bytes.contains(&b'/')
        {
            return None;
        }
        Some(OsString::from_vec(bytes))
    }
}

fn canonical_directory(directory: &Path) -> Result<PathBuf, ScriptActionLoadError> {
    let root = fs::canonicalize(directory)
        .map_err(|source| ScriptActionLoadError::io(directory, source))?;
    let metadata =
        fs::metadata(&root).map_err(|source| ScriptActionLoadError::io(&root, source))?;
    if !metadata.is_dir() {
        return Err(ScriptActionLoadError::NotDirectory(root));
    }
    Ok(root)
}

fn read_stable_manifest(path: &Path, before: &Metadata) -> Result<Vec<u8>, ScriptActionLoadError> {
    let file = File::open(path).map_err(|source| ScriptActionLoadError::io(path, source))?;
    let opened = file
        .metadata()
        .map_err(|source| ScriptActionLoadError::io(path, source))?;
    if !opened.is_file()
        || before.dev() != opened.dev()
        || before.ino() != opened.ino()
        || before.len() != opened.len()
    {
        return Err(ScriptActionLoadError::InvalidManifest {
            path: path.to_path_buf(),
        });
    }
    let limit =
        u64::try_from(ScriptActionLoader::MAX_MANIFEST_BYTES + 1).expect("manifest limit fits u64");
    let mut bytes = Vec::with_capacity(usize::try_from(opened.len()).unwrap_or(0));
    file.take(limit)
        .read_to_end(&mut bytes)
        .map_err(|source| ScriptActionLoadError::io(path, source))?;
    if bytes.len() > ScriptActionLoader::MAX_MANIFEST_BYTES {
        return Err(ScriptActionLoadError::ManifestTooLarge {
            path: path.to_path_buf(),
        });
    }
    Ok(bytes)
}

fn validate_script(path: &Path, metadata: &Metadata) -> Result<(), ScriptActionLoadError> {
    let file_type = metadata.file_type();
    if file_type.is_symlink()
        || !file_type.is_file()
        || file_type.is_block_device()
        || file_type.is_char_device()
        || file_type.is_fifo()
        || file_type.is_socket()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(ScriptActionLoadError::UnsafeScript {
            path: path.to_path_buf(),
        });
    }
    if metadata.len() > ScriptActionLoader::MAX_SCRIPT_BYTES {
        return Err(ScriptActionLoadError::ScriptTooLarge {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[derive(Debug)]
pub enum ScriptActionLoadError {
    Io { path: PathBuf, source: io::Error },
    NotDirectory(PathBuf),
    TooManyEntries,
    ManifestTooLarge { path: PathBuf },
    TotalManifestBytesExceeded,
    ScriptTooLarge { path: PathBuf },
    TotalScriptBytesExceeded,
    InvalidManifest { path: PathBuf },
    UnsafeScript { path: PathBuf },
    InvalidAction(CustomActionError),
}

impl ScriptActionLoadError {
    /// UI copy is selected by category, never by an untrusted path or OS error string.
    pub fn message_key(&self) -> &'static str {
        match self {
            Self::Io { source, .. } if source.kind() == io::ErrorKind::PermissionDenied => {
                "custom-action-script-permission"
            }
            Self::Io { source, .. } if source.kind() == io::ErrorKind::NotFound => {
                "custom-action-script-missing"
            }
            Self::Io { .. } => "custom-action-script-io",
            Self::NotDirectory(_) => "custom-action-script-directory-invalid",
            Self::TooManyEntries
            | Self::ManifestTooLarge { .. }
            | Self::TotalManifestBytesExceeded
            | Self::ScriptTooLarge { .. }
            | Self::TotalScriptBytesExceeded => "custom-action-script-limit",
            Self::InvalidManifest { .. } => "custom-action-script-manifest",
            Self::UnsafeScript { .. } => "custom-action-script-unsafe",
            Self::InvalidAction(error) => error.message_key(),
        }
    }

    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl std::fmt::Display for ScriptActionLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "script action I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::NotDirectory(path) => {
                write!(
                    formatter,
                    "script action path is not a directory: {}",
                    path.display()
                )
            }
            Self::TooManyEntries => {
                formatter.write_str("script action directory has too many entries")
            }
            Self::ManifestTooLarge { path } => {
                write!(
                    formatter,
                    "script action manifest is too large: {}",
                    path.display()
                )
            }
            Self::TotalManifestBytesExceeded => {
                formatter.write_str("script action manifests exceed the byte limit")
            }
            Self::ScriptTooLarge { path } => {
                write!(
                    formatter,
                    "script action executable is too large: {}",
                    path.display()
                )
            }
            Self::TotalScriptBytesExceeded => {
                formatter.write_str("script action executables exceed the byte limit")
            }
            Self::InvalidManifest { path } => {
                write!(
                    formatter,
                    "invalid script action manifest: {}",
                    path.display()
                )
            }
            Self::UnsafeScript { path } => {
                write!(
                    formatter,
                    "unsafe script action executable: {}",
                    path.display()
                )
            }
            Self::InvalidAction(error) => write!(formatter, "invalid script action: {error}"),
        }
    }
}

impl std::error::Error for ScriptActionLoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidAction(error) => Some(error),
            _ => None,
        }
    }
}
