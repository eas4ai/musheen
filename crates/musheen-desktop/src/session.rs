use crate::settings::{SettingsError, atomic_replace};
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

const SESSION_FILE_NAME: &str = "session.json";

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
        if let Some(current) = self.load()? {
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
        }
    }
}

impl Error for SessionStoreError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Storage(source) => Some(source),
        }
    }
}

impl From<SettingsError> for SessionStoreError {
    fn from(error: SettingsError) -> Self {
        Self::Storage(error)
    }
}
