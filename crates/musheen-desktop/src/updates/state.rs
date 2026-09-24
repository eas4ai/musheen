use super::UpdateError;
use crate::settings::atomic_replace;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const STATE_SCHEMA_VERSION: u32 = 1;
const MAX_STATE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct UpdateSequenceStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl UpdateSequenceStore {
    #[must_use]
    pub fn for_current_user() -> Self {
        Self::at(
            freedesktop::xdg_config_home()
                .join("musheen")
                .join("update-sequences.json"),
        )
    }

    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut lock_path = path.as_os_str().to_os_string();
        lock_path.push(".lock");
        Self {
            path,
            lock_path: PathBuf::from(lock_path),
        }
    }

    pub(crate) fn accept(&self, channel: &str, sequence: u64) -> Result<(), UpdateError> {
        self.ensure_parent()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(&self.lock_path)
            .map_err(|error| state_error("open lock", &self.lock_path, error))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|error| state_error("lock", &self.lock_path, error.into()))?;

        let mut state = self.read_state()?;
        let previous = state.channels.get(channel).copied().unwrap_or(0);
        if sequence < previous {
            return Err(UpdateError::Replay);
        }
        if sequence == previous {
            return Ok(());
        }
        state.channels.insert(channel.into(), sequence);
        let bytes = serde_json::to_vec(&state).map_err(|_| UpdateError::InvalidState)?;
        atomic_replace(&self.path, &bytes)
            .map_err(|error| UpdateError::State(error.to_string().into()))
    }

    fn ensure_parent(&self) -> Result<(), UpdateError> {
        let parent = self.path.parent().ok_or(UpdateError::InvalidState)?;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .map_err(|error| state_error("create directory", parent, error))
    }

    fn read_state(&self) -> Result<SequenceState, UpdateError> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(SequenceState::default());
            }
            Err(error) => return Err(state_error("read", &self.path, error)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| state_error("read", &self.path, error))?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(UpdateError::InvalidState);
        }
        let state: SequenceState =
            serde_json::from_slice(&bytes).map_err(|_| UpdateError::InvalidState)?;
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err(UpdateError::InvalidState);
        }
        Ok(state)
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SequenceState {
    schema_version: u32,
    channels: BTreeMap<Box<str>, u64>,
}

impl Default for SequenceState {
    fn default() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            channels: BTreeMap::new(),
        }
    }
}

fn state_error(operation: &str, path: &Path, error: std::io::Error) -> UpdateError {
    UpdateError::State(format!("{operation} {}: {error}", path.display()).into())
}
