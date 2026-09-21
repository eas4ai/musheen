use super::{
    DesktopApplication, DesktopEntryCatalog, DesktopEntryError, DesktopEntryIndex, DesktopPaths,
    valid_mime_type, validate_desktop_id,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_MIMEAPPS_BYTES: u64 = 1024 * 1024;
const MAX_ASSOCIATIONS: usize = 16_384;
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const DEFAULT_APPLICATIONS: &str = "Default Applications";
const ADDED_ASSOCIATIONS: &str = "Added Associations";
const REMOVED_ASSOCIATIONS: &str = "Removed Associations";

/// Custom freedesktop `mimeapps.list` resolver required by DEP-004.
#[derive(Clone, Debug)]
pub struct MimeAppsResolver {
    paths: DesktopPaths,
}

impl MimeAppsResolver {
    #[must_use]
    pub const fn new(paths: DesktopPaths) -> Self {
        Self { paths }
    }

    pub fn associations_for(
        &self,
        mime_type: &str,
        catalog: &DesktopEntryCatalog,
    ) -> Result<Vec<DesktopApplication>, MimeAppsError> {
        validate_mime_type(mime_type)?;
        let index = catalog.index()?;
        let mut result = Vec::<DesktopApplication>::new();
        let mut included = BTreeSet::<Box<str>>::new();
        let mut blacklist = BTreeSet::<Box<str>>::new();

        for location in self.locations() {
            let document = MimeAppsDocument::read_optional(&location.path)?;
            if !location.desktop_specific {
                for id in document.values(ADDED_ASSOCIATIONS, mime_type) {
                    if result.len() >= MAX_ASSOCIATIONS {
                        return Err(MimeAppsError::AssociationLimitExceeded);
                    }
                    if blacklist.contains(id)
                        || included.contains(id)
                        || !association_applies(&location, id, &index)
                    {
                        continue;
                    }
                    if let Some(application) = usable_application(&index, id) {
                        included.insert(id.clone());
                        result.push(application);
                    }
                }
                for id in document.values(REMOVED_ASSOCIATIONS, mime_type) {
                    if association_applies(&location, id, &index) {
                        blacklist.insert(id.clone());
                    }
                }
            }

            let Some(rank) = location.scan_application_rank else {
                continue;
            };
            for id in index.ids_at_rank(rank) {
                if !included.contains(id.as_ref())
                    && !blacklist.contains(id.as_ref())
                    && let Some(application) = usable_application(&index, id)
                    && application.source_rank() == rank
                    && application
                        .mime_types()
                        .iter()
                        .any(|candidate| candidate.as_ref() == mime_type)
                {
                    included.insert(id.clone());
                    result.push(application);
                }
                blacklist.insert(id.clone());
            }
        }
        Ok(result)
    }

    pub fn visible_applications_for(
        &self,
        mime_type: &str,
        catalog: &DesktopEntryCatalog,
    ) -> Result<Vec<DesktopApplication>, MimeAppsError> {
        Ok(self
            .associations_for(mime_type, catalog)?
            .into_iter()
            .filter(DesktopApplication::visible)
            .collect())
    }

    pub fn default_for(
        &self,
        mime_type: &str,
        catalog: &DesktopEntryCatalog,
    ) -> Result<Option<DesktopApplication>, MimeAppsError> {
        validate_mime_type(mime_type)?;
        let associations = self.associations_for(mime_type, catalog)?;
        let associated = associations
            .iter()
            .map(|application| Box::<str>::from(application.desktop_id()))
            .collect::<BTreeSet<_>>();
        for location in self.locations() {
            let document = MimeAppsDocument::read_optional(&location.path)?;
            for id in document.values(DEFAULT_APPLICATIONS, mime_type) {
                if associated.contains(id.as_ref())
                    && let Some(application) = associations
                        .iter()
                        .find(|application| application.desktop_id() == id.as_ref())
                {
                    return Ok(Some(application.clone()));
                }
            }
        }
        Ok(associations.into_iter().next())
    }

    /// Atomically record a user default and the association required by the spec.
    pub fn set_default(
        &self,
        mime_type: &str,
        desktop_id: &str,
        catalog: &DesktopEntryCatalog,
    ) -> Result<(), MimeAppsError> {
        validate_mime_type(mime_type)?;
        validate_desktop_id(desktop_id)?;
        let Some(application) = catalog.load(desktop_id)? else {
            return Err(MimeAppsError::ApplicationUnavailable);
        };
        if !catalog.is_available(&application) {
            return Err(MimeAppsError::ApplicationUnavailable);
        }
        if !self.paths.config_home().is_absolute() {
            return Err(MimeAppsError::InvalidPersistencePath);
        }
        let target = self.paths.config_home().join("mimeapps.list");
        fs::create_dir_all(self.paths.config_home())
            .map_err(|source| MimeAppsError::io(self.paths.config_home(), source))?;

        let _lock = lock_exclusive(&target)?;
        persist_default_locked(&target, mime_type, desktop_id, |_| Ok(()))
    }

    fn locations(&self) -> Vec<MimeAppsLocation> {
        let mut locations = Vec::new();
        push_locations(
            &mut locations,
            self.paths.config_home(),
            self.paths.current_desktops(),
            None,
        );
        for directory in self.paths.config_dirs() {
            push_locations(
                &mut locations,
                directory,
                self.paths.current_desktops(),
                None,
            );
        }
        push_locations(
            &mut locations,
            &self.paths.data_home().join("applications"),
            self.paths.current_desktops(),
            Some(0),
        );
        for (index, directory) in self.paths.data_dirs().iter().enumerate() {
            push_locations(
                &mut locations,
                &directory.join("applications"),
                self.paths.current_desktops(),
                Some(index + 1),
            );
        }
        locations
    }
}

fn push_locations(
    result: &mut Vec<MimeAppsLocation>,
    directory: &Path,
    desktops: &[Box<str>],
    application_rank: Option<usize>,
) {
    for desktop in desktops {
        result.push(MimeAppsLocation {
            path: directory.join(format!("{}-mimeapps.list", desktop.to_ascii_lowercase())),
            association_rank: application_rank,
            scan_application_rank: None,
            desktop_specific: true,
        });
    }
    result.push(MimeAppsLocation {
        path: directory.join("mimeapps.list"),
        association_rank: application_rank,
        scan_application_rank: application_rank,
        desktop_specific: false,
    });
}

#[derive(Debug)]
struct MimeAppsLocation {
    path: PathBuf,
    association_rank: Option<usize>,
    scan_application_rank: Option<usize>,
    desktop_specific: bool,
}

fn association_applies(
    location: &MimeAppsLocation,
    desktop_id: &str,
    index: &DesktopEntryIndex,
) -> bool {
    if validate_desktop_id(desktop_id).is_err() {
        return false;
    }
    let Some(source_rank) = index.source_rank(desktop_id) else {
        return false;
    };
    location
        .association_rank
        .is_none_or(|location_rank| source_rank >= location_rank)
}

fn usable_application(index: &DesktopEntryIndex, desktop_id: &str) -> Option<DesktopApplication> {
    if validate_desktop_id(desktop_id).is_err() {
        return None;
    }
    index
        .load(desktop_id)
        .filter(|application| index.is_available(application))
}

fn validate_mime_type(mime_type: &str) -> Result<(), MimeAppsError> {
    if valid_mime_type(mime_type) {
        Ok(())
    } else {
        Err(MimeAppsError::InvalidMimeType)
    }
}

#[derive(Default)]
struct MimeAppsDocument {
    groups: MimeAppsGroups,
}

type DesktopIds = Vec<Box<str>>;
type MimeAssociations = BTreeMap<Box<str>, DesktopIds>;
type MimeAppsGroups = BTreeMap<Box<str>, MimeAssociations>;

impl MimeAppsDocument {
    fn read_optional(path: &Path) -> Result<Self, MimeAppsError> {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => return Err(MimeAppsError::io(path, source)),
        };
        let length = file
            .metadata()
            .map_err(|source| MimeAppsError::io(path, source))?
            .len();
        if length > MAX_MIMEAPPS_BYTES {
            return Err(MimeAppsError::FileTooLarge(path.to_path_buf()));
        }
        let mut contents = String::new();
        file.take(MAX_MIMEAPPS_BYTES + 1)
            .read_to_string(&mut contents)
            .map_err(|source| MimeAppsError::io(path, source))?;
        if u64::try_from(contents.len()).unwrap_or(u64::MAX) > MAX_MIMEAPPS_BYTES {
            return Err(MimeAppsError::FileTooLarge(path.to_path_buf()));
        }
        Ok(Self::parse(&contents))
    }

    fn parse(contents: &str) -> Self {
        let mut document = Self::default();
        let mut current_group: Option<Box<str>> = None;
        for raw_line in contents.lines() {
            let line = raw_line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if let Some(group) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            {
                let group = group.trim();
                current_group = (!group.is_empty()).then(|| group.into());
                continue;
            }
            let Some(group) = current_group.as_ref() else {
                continue;
            };
            let Some((key, values)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() || key.chars().any(char::is_control) {
                continue;
            }
            let values = values
                .split(';')
                .map(str::trim)
                .filter(|value| validate_desktop_id(value).is_ok())
                .map(Into::into)
                .take(MAX_ASSOCIATIONS)
                .collect::<Vec<_>>();
            document
                .groups
                .entry(group.clone())
                .or_default()
                .insert(key.into(), values);
        }
        document
    }

    fn values(&self, group: &str, mime_type: &str) -> &[Box<str>] {
        self.groups
            .get(group)
            .and_then(|values| values.get(mime_type))
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    fn prepend(&mut self, group: &str, mime_type: &str, desktop_id: &str) {
        let values = self
            .groups
            .entry(group.into())
            .or_default()
            .entry(mime_type.into())
            .or_default();
        values.retain(|existing| existing.as_ref() != desktop_id);
        values.insert(0, desktop_id.into());
    }

    fn remove(&mut self, group: &str, mime_type: &str, desktop_id: &str) {
        if let Some(values) = self
            .groups
            .get_mut(group)
            .and_then(|associations| associations.get_mut(mime_type))
        {
            values.retain(|existing| existing.as_ref() != desktop_id);
        }
    }

    fn render(&self) -> String {
        let mut result = String::new();
        for group in [
            DEFAULT_APPLICATIONS,
            ADDED_ASSOCIATIONS,
            REMOVED_ASSOCIATIONS,
        ]
        .into_iter()
        .chain(self.groups.keys().map(Box::as_ref).filter(|group| {
            ![
                DEFAULT_APPLICATIONS,
                ADDED_ASSOCIATIONS,
                REMOVED_ASSOCIATIONS,
            ]
            .contains(group)
        })) {
            let Some(values) = self.groups.get(group) else {
                continue;
            };
            result.push('[');
            result.push_str(group);
            result.push_str("]\n");
            for (mime_type, ids) in values {
                result.push_str(mime_type);
                result.push('=');
                for id in ids {
                    result.push_str(id);
                    result.push(';');
                }
                result.push('\n');
            }
            result.push('\n');
        }
        result
    }
}

fn lock_exclusive(target: &Path) -> Result<File, MimeAppsError> {
    let mut lock_path = target.as_os_str().to_os_string();
    lock_path.push(".lock");
    let lock_path = PathBuf::from(lock_path);
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|source| MimeAppsError::io(&lock_path, source))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|source| MimeAppsError::io(&lock_path, io::Error::from(source)))?;
    Ok(lock)
}

struct TemporaryFile {
    path: PathBuf,
}

impl TemporaryFile {
    fn replace(mut self, target: &Path) -> io::Result<()> {
        fs::rename(&self.path, target)?;
        self.path.clear();
        Ok(())
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn persist_default_locked(
    target: &Path,
    mime_type: &str,
    desktop_id: &str,
    post_write_check: impl FnOnce(&Path) -> io::Result<()>,
) -> Result<(), MimeAppsError> {
    let mut document = MimeAppsDocument::read_optional(target)?;
    document.prepend(DEFAULT_APPLICATIONS, mime_type, desktop_id);
    document.prepend(ADDED_ASSOCIATIONS, mime_type, desktop_id);
    document.remove(REMOVED_ASSOCIATIONS, mime_type, desktop_id);
    let bytes = document.render();
    if bytes.len() > usize::try_from(MAX_MIMEAPPS_BYTES).expect("limit fits usize") {
        return Err(MimeAppsError::FileTooLarge(target.to_path_buf()));
    }
    let temporary = write_temporary(target, bytes.as_bytes())?;
    post_write_check(target).map_err(|source| MimeAppsError::io(target, source))?;
    temporary
        .replace(target)
        .map_err(|source| MimeAppsError::io(target, source))?;
    sync_parent(target)
}

fn write_temporary(target: &Path, bytes: &[u8]) -> Result<TemporaryFile, MimeAppsError> {
    let directory = target
        .parent()
        .ok_or(MimeAppsError::InvalidPersistencePath)?;
    for _ in 0..16 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = directory.join(format!(
            ".mimeapps.list.musheen-{}-{sequence}",
            std::process::id()
        ));
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
        {
            Ok(file) => file,
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(MimeAppsError::io(&temporary, source)),
        };
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            let _ = fs::remove_file(&temporary);
            return Err(MimeAppsError::io(&temporary, source));
        }
        return Ok(TemporaryFile { path: temporary });
    }
    Err(MimeAppsError::TemporaryFileUnavailable)
}

fn sync_parent(target: &Path) -> Result<(), MimeAppsError> {
    let parent = target
        .parent()
        .ok_or(MimeAppsError::InvalidPersistencePath)?;
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| MimeAppsError::io(parent, source))
}

#[derive(Debug)]
pub enum MimeAppsError {
    InvalidMimeType,
    InvalidDesktopEntry(DesktopEntryError),
    ApplicationUnavailable,
    Io { path: PathBuf, source: io::Error },
    FileTooLarge(PathBuf),
    AssociationLimitExceeded,
    InvalidPersistencePath,
    TemporaryFileUnavailable,
}

impl MimeAppsError {
    fn io(path: &Path, source: io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl From<DesktopEntryError> for MimeAppsError {
    fn from(value: DesktopEntryError) -> Self {
        Self::InvalidDesktopEntry(value)
    }
}

impl std::fmt::Display for MimeAppsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidMimeType => formatter.write_str("invalid MIME type"),
            Self::InvalidDesktopEntry(error) => write!(formatter, "{error}"),
            Self::ApplicationUnavailable => formatter.write_str("desktop application unavailable"),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "mimeapps I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::FileTooLarge(path) => {
                write!(formatter, "mimeapps file is too large: {}", path.display())
            }
            Self::AssociationLimitExceeded => {
                formatter.write_str("MIME association limit exceeded")
            }
            Self::InvalidPersistencePath => {
                formatter.write_str("invalid mimeapps persistence path")
            }
            Self::TemporaryFileUnavailable => {
                formatter.write_str("cannot allocate mimeapps temporary file")
            }
        }
    }
}

impl std::error::Error for MimeAppsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidDesktopEntry(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{TemporaryFile, persist_default_locked};
    use std::fs;

    #[test]
    fn failed_replace_removes_the_private_temporary_file() {
        let temporary = tempfile::tempdir().unwrap();
        let pending = temporary.path().join(".mimeapps.list.pending");
        let pending_path = pending.clone();
        let target = temporary.path().join("mimeapps.list");
        fs::write(&pending, "pending").unwrap();
        fs::create_dir(&target).unwrap();

        let pending = TemporaryFile { path: pending };
        assert!(pending.replace(&target).is_err());
        assert!(!pending_path.exists());
    }

    #[test]
    fn post_write_failure_removes_the_private_temporary_file() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("mimeapps.list");
        let error = persist_default_locked(&target, "text/plain", "writer.desktop", |_| {
            Err(std::io::Error::other("injected post-write stat failure"))
        })
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected post-write stat failure")
        );
        assert_eq!(fs::read_dir(temporary.path()).unwrap().count(), 0);
    }
}
