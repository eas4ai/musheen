mod folder_prefs;
mod home;
mod pins;
mod tags;

pub use folder_prefs::*;
pub use home::*;
pub use pins::*;
pub use tags::*;

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const CATALOG_SCHEMA_VERSION: u32 = 1;
const CATALOG_FILE_NAME: &str = "catalog.json";
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CatalogDocument {
    schema_version: u32,
    tags: TagCatalog,
    pins: PinCatalog,
    recents: RecentLocations,
    folder_preferences: FolderPreferenceCatalog,
}

impl CatalogDocument {
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub const fn tags(&self) -> &TagCatalog {
        &self.tags
    }

    pub fn tags_mut(&mut self) -> &mut TagCatalog {
        &mut self.tags
    }

    #[must_use]
    pub const fn pins(&self) -> &PinCatalog {
        &self.pins
    }

    pub fn pins_mut(&mut self) -> &mut PinCatalog {
        &mut self.pins
    }

    #[must_use]
    pub const fn recents(&self) -> &RecentLocations {
        &self.recents
    }

    pub fn recents_mut(&mut self) -> &mut RecentLocations {
        &mut self.recents
    }

    #[must_use]
    pub const fn folder_preferences(&self) -> &FolderPreferenceCatalog {
        &self.folder_preferences
    }

    pub fn folder_preferences_mut(&mut self) -> &mut FolderPreferenceCatalog {
        &mut self.folder_preferences
    }

    /// The operation layer calls this only after a move has published its
    /// destination identity. Unsupported tag destinations return an explicit
    /// outcome and leave the source record intact for review.
    pub fn note_completed_move(
        &mut self,
        source: &musheen_core::ItemId,
        destination: musheen_core::ItemId,
        destination_path_hint: musheen_core::StorePath,
        destination_supports_tags: bool,
    ) -> TagMoveOutcome {
        self.tags.note_app_move(
            source,
            destination,
            destination_path_hint,
            destination_supports_tags,
        )
    }
}

impl Default for CatalogDocument {
    fn default() -> Self {
        Self {
            schema_version: CATALOG_SCHEMA_VERSION,
            tags: TagCatalog::default(),
            pins: PinCatalog::default(),
            recents: RecentLocations::default(),
            folder_preferences: FolderPreferenceCatalog::default(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CatalogStore {
    path: PathBuf,
    backup_path: PathBuf,
}

impl CatalogStore {
    #[must_use]
    pub fn for_current_user() -> Self {
        Self::from_data_home(freedesktop::xdg_data_home())
    }

    #[must_use]
    pub fn from_data_home(data_home: impl AsRef<Path>) -> Self {
        Self::at(data_home.as_ref().join("musheen").join(CATALOG_FILE_NAME))
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

    pub fn load(&self) -> Result<CatalogDocument, CatalogError> {
        match load_document(&self.path) {
            Ok(Some(document)) => Ok(document),
            Ok(None) | Err(CatalogError::Corrupt { .. }) => self.recover_or_default(),
            Err(error) => Err(error),
        }
    }

    pub fn save(&self, document: &CatalogDocument) -> Result<(), CatalogError> {
        ensure_parent(&self.path)?;
        match load_document(&self.path) {
            Ok(Some(previous)) => atomic_replace(&self.backup_path, &serialize(&previous)?)?,
            Ok(None) | Err(CatalogError::Corrupt { .. }) => {}
            Err(error) => return Err(error),
        }
        atomic_replace(&self.path, &serialize(document)?)
    }

    fn recover_or_default(&self) -> Result<CatalogDocument, CatalogError> {
        let Some(document) = load_document(&self.backup_path)? else {
            if self.path.exists() {
                return Err(CatalogError::Corrupt {
                    path: self.path.clone(),
                    message: "no valid last-known-good backup is available".into(),
                });
            }
            return Ok(CatalogDocument::default());
        };
        atomic_replace(&self.path, &serialize(&document)?)?;
        Ok(document)
    }
}

fn load_document(path: &Path) -> Result<Option<CatalogDocument>, CatalogError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("read catalog", path, source)),
    };
    let document: CatalogDocument =
        serde_json::from_slice(&bytes).map_err(|error| CatalogError::Corrupt {
            path: path.to_path_buf(),
            message: error.to_string().into(),
        })?;
    if document.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(CatalogError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: document.schema_version,
        });
    }
    Ok(Some(document))
}

fn serialize(document: &CatalogDocument) -> Result<Vec<u8>, CatalogError> {
    serde_json::to_vec_pretty(document).map_err(CatalogError::Serialize)
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), CatalogError> {
    ensure_parent(path)?;
    let parent = parent_directory(path);
    let temp_path = unique_temp_path(path);
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp_path)
            .map_err(|source| io_error("create temporary catalog", &temp_path, source))?;
        file.write_all(bytes)
            .map_err(|source| io_error("write temporary catalog", &temp_path, source))?;
        file.sync_all()
            .map_err(|source| io_error("sync temporary catalog", &temp_path, source))?;
        fs::rename(&temp_path, path)
            .map_err(|source| io_error("replace catalog atomically", path, source))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error("sync catalog directory", parent, source))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn ensure_parent(path: &Path) -> Result<(), CatalogError> {
    let parent = parent_directory(path);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|source| io_error("create catalog directory", parent, source))
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
        .unwrap_or(CATALOG_FILE_NAME);
    parent_directory(path).join(format!(".{name}.tmp.{}.{sequence}", std::process::id()))
}

fn io_error(operation: &'static str, path: &Path, source: std::io::Error) -> CatalogError {
    CatalogError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[derive(Debug)]
pub enum CatalogError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Corrupt {
        path: PathBuf,
        message: Box<str>,
    },
    UnsupportedVersion {
        path: PathBuf,
        version: u32,
    },
    Serialize(serde_json::Error),
}

impl fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
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
                    "catalog at {} is corrupt: {message}",
                    path.display()
                )
            }
            Self::UnsupportedVersion { path, version } => write!(
                formatter,
                "unsupported catalog schema {version} at {}",
                path.display()
            ),
            Self::Serialize(error) => write!(formatter, "could not serialize catalog: {error}"),
        }
    }
}

impl Error for CatalogError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Serialize(error) => Some(error),
            Self::Corrupt { .. } | Self::UnsupportedVersion { .. } => None,
        }
    }
}
