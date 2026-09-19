use crate::{SessionStore, SessionStoreError};
use std::path::{Path, PathBuf};

const STATUS_FILE_NAME: &str = "operations.json";

/// Private, atomically replaced storage for operation status history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatusStore {
    document: SessionStore,
}

impl StatusStore {
    #[must_use]
    pub fn for_current_user() -> Self {
        Self::from_config_home(freedesktop::xdg_config_home())
    }

    #[must_use]
    pub fn from_config_home(config_home: impl AsRef<Path>) -> Self {
        Self::at(config_home.as_ref().join("musheen").join(STATUS_FILE_NAME))
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            document: SessionStore::at(path),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        self.document.path()
    }

    pub fn load(&self) -> Result<Option<Vec<u8>>, StatusStoreError> {
        self.document.load()
    }

    pub fn load_backup(&self) -> Result<Option<Vec<u8>>, StatusStoreError> {
        self.document.load_backup()
    }

    pub fn save(&self, document: &[u8]) -> Result<(), StatusStoreError> {
        self.document.save(document)
    }
}

pub type StatusStoreError = SessionStoreError;
