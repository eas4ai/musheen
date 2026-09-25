use crate::{SessionStore, SessionStoreError};
use std::path::{Path, PathBuf};

const STATUS_FILE_NAME: &str = "operations.json";
pub const STATUS_SCHEMA_VERSION: u32 = 1;

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
        self.document
            .save_with_schema(document, STATUS_SCHEMA_VERSION, has_supported_status_shape)
    }
}

pub type StatusStoreError = SessionStoreError;

fn has_supported_status_shape(document: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(document) else {
        return false;
    };
    value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        == Some(u64::from(STATUS_SCHEMA_VERSION))
        && value
            .get("entries")
            .is_some_and(serde_json::Value::is_array)
}
