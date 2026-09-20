use musheen_core::{CoreError, ResourceLimitConfig, ResourceLimits};
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

mod document;
mod migrate;
pub mod theme;
mod validate;
pub use document::*;

pub const SETTINGS_SCHEMA_VERSION: u32 = 3;
const SETTINGS_FILE_NAME: &str = "settings.conf";
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsDocument {
    schema_version: u32,
    resource_limits: ResourceLimitConfig,
    unknown: BTreeMap<Box<str>, Box<str>>,
    values: BTreeMap<Box<str>, Box<str>>,
}

impl SettingsDocument {
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub fn resource_limits(&self) -> &ResourceLimitConfig {
        &self.resource_limits
    }

    pub fn resource_limits_mut(&mut self) -> &mut ResourceLimitConfig {
        &mut self.resource_limits
    }

    pub fn resource_limits_snapshot(&self) -> Result<ResourceLimits, CoreError> {
        ResourceLimits::try_from(self.resource_limits.clone())
    }

    fn validate(&self) -> Result<(), SettingsError> {
        for spec in settings_schema() {
            validate::validate_value(
                spec,
                &self
                    .value(spec.key)
                    .ok_or_else(|| SettingsError::InvalidValue {
                        key: spec.key.into(),
                    })?,
            )?;
        }
        self.resource_limits_snapshot()
            .map(|_| ())
            .map_err(SettingsError::InvalidLimits)
    }
}

impl Default for SettingsDocument {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            resource_limits: ResourceLimitConfig::default(),
            unknown: BTreeMap::new(),
            values: settings_schema()
                .iter()
                .filter(|spec| {
                    !spec.key.starts_with("directory_") && !spec.key.starts_with("operation_")
                })
                .map(|spec| (spec.key.into(), spec.default.into()))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingsStore {
    path: PathBuf,
    backup_path: PathBuf,
}

impl SettingsStore {
    #[must_use]
    pub fn for_current_user() -> Self {
        Self::from_config_home(freedesktop::xdg_config_home())
    }

    #[must_use]
    pub fn from_config_home(config_home: impl AsRef<Path>) -> Self {
        Self::at(
            config_home
                .as_ref()
                .join("musheen")
                .join(SETTINGS_FILE_NAME),
        )
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut backup = path.as_os_str().to_os_string();
        backup.push(".bak");
        Self {
            path,
            backup_path: PathBuf::from(backup),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn backup_path(&self) -> &Path {
        &self.backup_path
    }

    pub fn load(&self) -> Result<SettingsDocument, SettingsError> {
        match load_document(&self.path) {
            Ok(Some(document)) => Ok(document),
            Ok(None) | Err(SettingsError::Corrupt { .. }) => self.recover_or_default(),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, document: &SettingsDocument) -> Result<(), SettingsError> {
        document.validate()?;
        ensure_parent(&self.path)?;

        match load_document(&self.path) {
            Ok(Some(previous)) => {
                atomic_replace(&self.backup_path, serialize(&previous).as_bytes())?
            }
            Ok(None) | Err(SettingsError::Corrupt { .. }) => {}
            Err(error) => return Err(error),
        }
        atomic_replace(&self.path, serialize(document).as_bytes())
    }

    fn recover_or_default(&self) -> Result<SettingsDocument, SettingsError> {
        let Some(document) = load_document(&self.backup_path)? else {
            if self.path.exists() {
                return Err(SettingsError::Corrupt {
                    path: self.path.clone(),
                    message: "no valid last-known-good backup is available".into(),
                });
            }
            return Ok(SettingsDocument::default());
        };
        ensure_parent(&self.path)?;
        atomic_replace(&self.path, serialize(&document).as_bytes())?;
        Ok(document)
    }
}

#[derive(Debug)]
pub enum SettingsError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Corrupt {
        path: PathBuf,
        message: Box<str>,
    },
    InvalidLimits(CoreError),
    InvalidValue {
        key: Box<str>,
    },
    UnsupportedVersion {
        path: PathBuf,
        version: u32,
    },
}

impl fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidValue { key } => write!(formatter, "invalid setting: {key}"),
            Self::UnsupportedVersion { path, version } => write!(
                formatter,
                "unsupported settings schema {version} at {}",
                path.display()
            ),
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "{operation} failed for {}: {source}",
                path.display()
            ),
            Self::Corrupt { path, message } => {
                write!(
                    formatter,
                    "settings at {} are corrupt: {message}",
                    path.display()
                )
            }
            Self::InvalidLimits(error) => write!(formatter, "settings limits are invalid: {error}"),
        }
    }
}

impl Error for SettingsError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::InvalidLimits(source) => Some(source),
            Self::Corrupt { .. } | Self::InvalidValue { .. } | Self::UnsupportedVersion { .. } => {
                None
            }
        }
    }
}

fn load_document(path: &Path) -> Result<Option<SettingsDocument>, SettingsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(settings_error(
                path,
                SettingsFailure::Io {
                    operation: "read settings",
                    source,
                },
            ));
        }
    };
    let text = std::str::from_utf8(&bytes).map_err(|error| SettingsError::Corrupt {
        path: path.to_path_buf(),
        message: error.to_string().into(),
    })?;
    parse_document(path, text).map(Some)
}

fn parse_document(path: &Path, text: &str) -> Result<SettingsDocument, SettingsError> {
    let mut entries = BTreeMap::<Box<str>, Box<str>>::new();
    let mut seen = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(settings_error(
                path,
                SettingsFailure::Corrupt(format!("line {} has no '='", index + 1).into()),
            ));
        };
        let key = key.trim();
        if key.is_empty() || !seen.insert(key.to_owned()) {
            return Err(settings_error(
                path,
                SettingsFailure::Corrupt(
                    format!("line {} has an empty or duplicate key", index + 1).into(),
                ),
            ));
        }
        entries.insert(key.into(), value.trim().into());
    }

    let version = entries
        .remove("schema_version")
        .ok_or_else(|| {
            settings_error(
                path,
                SettingsFailure::Corrupt("schema_version is missing".into()),
            )
        })?
        .parse::<u32>()
        .map_err(|error| {
            settings_error(
                path,
                SettingsFailure::Corrupt(format!("schema_version is invalid: {error}").into()),
            )
        })?;
    let version = migrate::migrate_version(path, version)?;

    if entries
        .get("advanced.custom_actions")
        .is_some_and(|value| matches!(value.as_ref(), "true" | "false"))
    {
        entries.insert(
            "advanced.custom_actions".into(),
            crate::CustomActionDocument::default().export().into(),
        );
    }

    let resource_limits = parse_resource_limits(&mut entries);
    let mut values = SettingsDocument::default().values;
    for spec in settings_schema()
        .iter()
        .filter(|spec| values.contains_key(spec.key))
        .collect::<Vec<_>>()
    {
        if let Some(value) = entries.remove(spec.key)
            && validate::validate_value(spec, &value).is_ok()
        {
            values.insert(spec.key.into(), value);
        }
    }

    Ok(SettingsDocument {
        schema_version: version,
        resource_limits,
        unknown: entries,
        values,
    })
}

fn parse_resource_limits(entries: &mut BTreeMap<Box<str>, Box<str>>) -> ResourceLimitConfig {
    let defaults = ResourceLimitConfig::default();
    ResourceLimitConfig {
        directory_page_items: take_limit(
            entries,
            "directory_page_items",
            defaults.directory_page_items,
            ResourceLimitConfig::MAX_DIRECTORY_PAGE_ITEMS,
        ),
        directory_prefetch_pages: take_limit(
            entries,
            "directory_prefetch_pages",
            defaults.directory_prefetch_pages,
            ResourceLimitConfig::MAX_DIRECTORY_PREFETCH_PAGES,
        ),
        directory_retained_items: take_limit(
            entries,
            "directory_retained_items",
            defaults.directory_retained_items,
            ResourceLimitConfig::MAX_DIRECTORY_RETAINED_ITEMS,
        ),
        directory_rendered_viewports: take_limit(
            entries,
            "directory_rendered_viewports",
            defaults.directory_rendered_viewports,
            ResourceLimitConfig::MAX_DIRECTORY_RENDERED_VIEWPORTS,
        ),
        operation_data_mutations: take_limit(
            entries,
            "operation_data_mutations",
            defaults.operation_data_mutations,
            ResourceLimitConfig::MAX_OPERATION_DATA_MUTATIONS,
        ),
        operation_metadata_jobs: take_limit(
            entries,
            "operation_metadata_jobs",
            defaults.operation_metadata_jobs,
            ResourceLimitConfig::MAX_OPERATION_METADATA_JOBS,
        ),
        operation_hash_preview_jobs: take_limit(
            entries,
            "operation_hash_preview_jobs",
            defaults.operation_hash_preview_jobs,
            ResourceLimitConfig::MAX_OPERATION_HASH_PREVIEW_JOBS,
        ),
    }
}

fn take_limit(
    entries: &mut BTreeMap<Box<str>, Box<str>>,
    key: &str,
    default: usize,
    maximum: usize,
) -> usize {
    entries
        .remove(key)
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| (1..=maximum).contains(value))
        .unwrap_or(default)
}

fn serialize(document: &SettingsDocument) -> String {
    let limits = &document.resource_limits;
    let mut output = format!(
        "schema_version={}\ndirectory_page_items={}\ndirectory_prefetch_pages={}\ndirectory_retained_items={}\ndirectory_rendered_viewports={}\noperation_data_mutations={}\noperation_metadata_jobs={}\noperation_hash_preview_jobs={}\n",
        document.schema_version,
        limits.directory_page_items,
        limits.directory_prefetch_pages,
        limits.directory_retained_items,
        limits.directory_rendered_viewports,
        limits.operation_data_mutations,
        limits.operation_metadata_jobs,
        limits.operation_hash_preview_jobs,
    );
    for (key, value) in document.values.iter().chain(&document.unknown) {
        output.push_str(key);
        output.push('=');
        output.push_str(value);
        output.push('\n');
    }
    output
}

pub(crate) fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), SettingsError> {
    ensure_parent(path)?;
    let parent = parent_directory(path);
    let temp_path = unique_temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp_path)
            .map_err(|source| {
                settings_error(
                    &temp_path,
                    SettingsFailure::Io {
                        operation: "create temporary settings",
                        source,
                    },
                )
            })?;
        file.write_all(bytes).map_err(|source| {
            settings_error(
                &temp_path,
                SettingsFailure::Io {
                    operation: "write temporary settings",
                    source,
                },
            )
        })?;
        file.sync_all().map_err(|source| {
            settings_error(
                &temp_path,
                SettingsFailure::Io {
                    operation: "sync temporary settings",
                    source,
                },
            )
        })?;
        fs::rename(&temp_path, path).map_err(|source| {
            settings_error(
                path,
                SettingsFailure::Io {
                    operation: "replace settings atomically",
                    source,
                },
            )
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| {
                settings_error(
                    parent,
                    SettingsFailure::Io {
                        operation: "sync settings directory",
                        source,
                    },
                )
            })
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn ensure_parent(path: &Path) -> Result<(), SettingsError> {
    let parent = parent_directory(path);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|source| {
            settings_error(
                parent,
                SettingsFailure::Io {
                    operation: "create settings directory",
                    source,
                },
            )
        })
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn unique_temp_path(path: &Path) -> PathBuf {
    let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(SETTINGS_FILE_NAME);
    parent_directory(path).join(format!(".{name}.tmp.{}.{sequence}", std::process::id()))
}

enum SettingsFailure {
    Corrupt(Box<str>),
    Io {
        operation: &'static str,
        source: std::io::Error,
    },
}

fn settings_error(path: &Path, failure: SettingsFailure) -> SettingsError {
    match failure {
        SettingsFailure::Corrupt(message) => SettingsError::Corrupt {
            path: path.to_path_buf(),
            message,
        },
        SettingsFailure::Io { operation, source } => SettingsError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        },
    }
}
