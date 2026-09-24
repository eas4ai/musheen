use crate::settings::{SettingsError, atomic_replace};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const SESSION_FILE_NAME: &str = "session.json";
pub const SESSION_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    #[must_use]
    pub fn for_current_user() -> Self {
        Self::from_config_home(freedesktop::xdg_config_home())
    }

    #[must_use]
    pub fn from_config_home(config_home: impl AsRef<Path>) -> Self {
        Self::at(config_home.as_ref().join("musheen").join(SESSION_FILE_NAME))
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<Vec<u8>>, SessionStoreError> {
        read_optional(&self.path, "read session")
    }

    pub fn load_backup(&self) -> Result<Option<Vec<u8>>, SessionStoreError> {
        read_optional(&self.backup_path(), "read session backup")
    }

    pub fn save(&self, document: &[u8]) -> Result<(), SessionStoreError> {
        let current = self.load()?;
        let backup_path = self.backup_path();
        let backup = self.load_backup()?;
        for (path, stored) in [(&self.path, &current), (&backup_path, &backup)] {
            if let Some(error) = stored
                .as_deref()
                .and_then(|document| unsupported_schema(document, path))
            {
                return Err(error);
            }
        }
        if let Some(current) =
            current.filter(|bytes| backup.is_none() || has_supported_session_shape(bytes))
        {
            atomic_replace(&self.backup_path(), &current).map_err(SessionStoreError::from)?;
        }
        atomic_replace(&self.path, document).map_err(SessionStoreError::from)
    }

    fn backup_path(&self) -> PathBuf {
        let mut path = self.path.as_os_str().to_owned();
        path.push(".bak");
        PathBuf::from(path)
    }
}

fn unsupported_schema(document: &[u8], path: &Path) -> Option<SessionStoreError> {
    let value = serde_json::from_slice::<serde_json::Value>(document).ok()?;
    let schema = value.get("schema_version")?;
    match schema.as_u64() {
        Some(version) if version > u64::from(SESSION_SCHEMA_VERSION) => {
            Some(SessionStoreError::FutureSchema {
                path: path.to_path_buf(),
                version,
            })
        }
        Some(_) => None,
        None => Some(SessionStoreError::UnrecognizedSchema {
            path: path.to_path_buf(),
        }),
    }
}

fn has_supported_session_shape(document: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(document) else {
        return false;
    };
    value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        == Some(u64::from(SESSION_SCHEMA_VERSION))
        && (value
            .get("window")
            .is_some_and(serde_json::Value::is_object)
            || value.get("windows").is_some_and(|windows| {
                windows
                    .as_array()
                    .is_some_and(|windows| !windows.is_empty())
            }))
}

fn read_optional(
    path: &Path,
    operation: &'static str,
) -> Result<Option<Vec<u8>>, SessionStoreError> {
    match fs::read(path) {
        Ok(document) => Ok(Some(document)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SessionStoreError::Io {
            operation,
            path: path.to_path_buf(),
            source,
        }),
    }
}

#[derive(Debug)]
pub enum SessionStoreError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    Storage(SettingsError),
    FutureSchema {
        path: PathBuf,
        version: u64,
    },
    UnrecognizedSchema {
        path: PathBuf,
    },
}

impl fmt::Display for SessionStoreError {
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
            Self::Storage(error) => write!(formatter, "atomic session storage failed: {error}"),
            Self::FutureSchema { path, version } => write!(
                formatter,
                "session schema {version} in {} is newer than supported schema {SESSION_SCHEMA_VERSION}; refusing to overwrite it",
                path.display()
            ),
            Self::UnrecognizedSchema { path } => write!(
                formatter,
                "session schema in {} is unrecognized; refusing to overwrite it",
                path.display()
            ),
        }
    }
}

impl Error for SessionStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Storage(source) => Some(source),
            Self::FutureSchema { .. } | Self::UnrecognizedSchema { .. } => None,
        }
    }
}

impl From<SettingsError> for SessionStoreError {
    fn from(error: SettingsError) -> Self {
        Self::Storage(error)
    }
}
