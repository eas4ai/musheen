use super::{SETTINGS_SCHEMA_VERSION, SettingsError};
use std::path::Path;

/// v1 contains only resource limits. v2 adds domain keys with schema defaults;
/// its resource keys retain their exact spelling and unknown values survive.
pub(super) fn migrate_version(path: &Path, version: u32) -> Result<u32, SettingsError> {
    match version {
        1 | 2 | SETTINGS_SCHEMA_VERSION => Ok(SETTINGS_SCHEMA_VERSION),
        _ => Err(SettingsError::UnsupportedVersion {
            path: path.to_path_buf(),
            version,
        }),
    }
}
