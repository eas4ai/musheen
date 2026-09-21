use super::DesktopPaths;
use freedesktop::ApplicationEntry;
use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const MAX_DESKTOP_ENTRIES: usize = 32_768;
const MAX_DIRECTORY_DEPTH: usize = 16;

/// Musheen-owned projection of a freedesktop application entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopApplication {
    desktop_id: Box<str>,
    name: Box<str>,
    icon: Option<Box<str>>,
    exec: Box<str>,
    try_exec: Option<OsString>,
    terminal: bool,
    working_directory: Option<PathBuf>,
    mime_types: Vec<Box<str>>,
    desktop_file: PathBuf,
    visible: bool,
    source_rank: usize,
}

impl DesktopApplication {
    #[must_use]
    pub fn desktop_id(&self) -> &str {
        &self.desktop_id
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn icon(&self) -> Option<&str> {
        self.icon.as_deref()
    }

    #[must_use]
    pub fn exec(&self) -> &str {
        &self.exec
    }

    #[must_use]
    pub fn try_exec(&self) -> Option<&OsStr> {
        self.try_exec.as_deref()
    }

    #[must_use]
    pub const fn terminal(&self) -> bool {
        self.terminal
    }

    #[must_use]
    pub fn working_directory(&self) -> Option<&Path> {
        self.working_directory.as_deref()
    }

    #[must_use]
    pub fn mime_types(&self) -> &[Box<str>] {
        &self.mime_types
    }

    #[must_use]
    pub fn desktop_file(&self) -> &Path {
        &self.desktop_file
    }

    #[must_use]
    pub const fn visible(&self) -> bool {
        self.visible
    }

    pub(crate) const fn source_rank(&self) -> usize {
        self.source_rank
    }
}

/// Replaceable desktop-entry adapter. No freedesktop crate type crosses it.
#[derive(Clone, Debug)]
pub struct DesktopEntryCatalog {
    paths: DesktopPaths,
}

impl DesktopEntryCatalog {
    #[must_use]
    pub const fn new(paths: DesktopPaths) -> Self {
        Self { paths }
    }

    pub fn load(&self, desktop_id: &str) -> Result<Option<DesktopApplication>, DesktopEntryError> {
        validate_desktop_id(desktop_id)?;
        Ok(self.index()?.load(desktop_id))
    }

    pub fn all(&self) -> Result<Vec<DesktopApplication>, DesktopEntryError> {
        Ok(self
            .index()?
            .records
            .into_values()
            .filter_map(|record| (!record.hidden).then_some(record.application).flatten())
            .collect())
    }

    pub(crate) fn is_available(&self, application: &DesktopApplication) -> bool {
        application
            .try_exec()
            .is_none_or(|try_exec| executable_available(try_exec, self.paths.executable_dirs()))
    }

    pub(crate) fn index(&self) -> Result<DesktopEntryIndex, DesktopEntryError> {
        let mut records = BTreeMap::new();
        let mut ids_by_rank = Vec::new();
        for (rank, directory) in self.paths.application_directories().iter().enumerate() {
            let scanned = scan_directory(directory, rank, self.paths.current_desktops())?;
            ids_by_rank.push(
                scanned
                    .iter()
                    .map(|record| record.desktop_id.clone())
                    .collect(),
            );
            for record in scanned {
                records.entry(record.desktop_id.clone()).or_insert(record);
                if records.len() > MAX_DESKTOP_ENTRIES {
                    return Err(DesktopEntryError::EntryLimitExceeded);
                }
            }
        }
        Ok(DesktopEntryIndex {
            records,
            ids_by_rank,
            executable_directories: self.paths.executable_dirs().to_vec(),
        })
    }
}

pub(crate) struct DesktopEntryIndex {
    records: BTreeMap<Box<str>, EntryRecord>,
    ids_by_rank: Vec<Vec<Box<str>>>,
    executable_directories: Vec<PathBuf>,
}

impl DesktopEntryIndex {
    pub(crate) fn load(&self, desktop_id: &str) -> Option<DesktopApplication> {
        self.records
            .get(desktop_id)
            .filter(|record| !record.hidden)
            .and_then(|record| record.application.clone())
    }

    pub(crate) fn source_rank(&self, desktop_id: &str) -> Option<usize> {
        self.records
            .get(desktop_id)
            .map(|record| record.source_rank)
    }

    pub(crate) fn ids_at_rank(&self, rank: usize) -> &[Box<str>] {
        self.ids_by_rank.get(rank).map_or(&[], Vec::as_slice)
    }

    pub(crate) fn is_available(&self, application: &DesktopApplication) -> bool {
        application
            .try_exec()
            .is_none_or(|try_exec| executable_available(try_exec, &self.executable_directories))
    }
}

#[derive(Debug)]
struct EntryRecord {
    desktop_id: Box<str>,
    application: Option<DesktopApplication>,
    hidden: bool,
    source_rank: usize,
}

fn scan_directory(
    directory: &Path,
    rank: usize,
    current_desktops: &[Box<str>],
) -> Result<Vec<EntryRecord>, DesktopEntryError> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_desktop_files(directory, directory, 0, &mut files)?;
    files.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    Ok(files
        .into_iter()
        .filter_map(|path| desktop_id(directory, &path).map(|desktop_id| (desktop_id, path)))
        .map(|(desktop_id, path)| parse_record(desktop_id, path, rank, current_desktops))
        .collect())
}

fn collect_desktop_files(
    root: &Path,
    directory: &Path,
    depth: usize,
    files: &mut Vec<PathBuf>,
) -> Result<(), DesktopEntryError> {
    if depth > MAX_DIRECTORY_DEPTH {
        return Err(DesktopEntryError::DirectoryDepthExceeded(
            root.to_path_buf(),
        ));
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|source| DesktopEntryError::io(directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| DesktopEntryError::io(directory, source))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let file_type = entry
            .file_type()
            .map_err(|source| DesktopEntryError::io(&entry.path(), source))?;
        if file_type.is_dir() {
            collect_desktop_files(root, &entry.path(), depth + 1, files)?;
        } else if file_type.is_file() && entry.path().extension() == Some(OsStr::new("desktop")) {
            files.push(entry.path());
            if files.len() > MAX_DESKTOP_ENTRIES {
                return Err(DesktopEntryError::EntryLimitExceeded);
            }
        }
    }
    Ok(())
}

fn desktop_id(root: &Path, path: &Path) -> Option<Box<str>> {
    let relative = path.strip_prefix(root).ok()?;
    let components = relative
        .components()
        .map(|component| component.as_os_str().to_str())
        .collect::<Option<Vec<_>>>()?;
    let id = components.join("-");
    validate_desktop_id(&id).ok()?;
    Some(id.into_boxed_str())
}

fn parse_record(
    desktop_id: Box<str>,
    path: PathBuf,
    source_rank: usize,
    current_desktops: &[Box<str>],
) -> EntryRecord {
    let Ok(entry) = ApplicationEntry::try_from_path(&path) else {
        return EntryRecord {
            desktop_id,
            application: None,
            hidden: false,
            source_rank,
        };
    };
    let hidden = entry.is_hidden();
    if hidden {
        return EntryRecord {
            desktop_id,
            application: None,
            hidden: true,
            source_rank,
        };
    }
    let application = if entry.entry_type().as_deref() != Some("Application") {
        None
    } else {
        let name = entry.name().filter(|name| !name.trim().is_empty());
        let exec = entry.exec().filter(|exec| !exec.trim().is_empty());
        name.zip(exec).map(|(name, exec)| DesktopApplication {
            desktop_id: desktop_id.clone(),
            name: name.into_boxed_str(),
            icon: entry.icon().map(String::into_boxed_str),
            exec: exec.into_boxed_str(),
            try_exec: entry.get_string("TryExec").map(OsString::from),
            terminal: entry.terminal(),
            working_directory: entry.path_dir().map(PathBuf::from),
            mime_types: entry
                .mime_types()
                .unwrap_or_default()
                .into_iter()
                .filter(|mime| valid_mime_type(mime))
                .map(String::into_boxed_str)
                .collect(),
            desktop_file: path.clone(),
            visible: desktop_visible(&entry, current_desktops),
            source_rank,
        })
    };
    EntryRecord {
        desktop_id,
        application,
        hidden: false,
        source_rank,
    }
}

fn desktop_visible(entry: &ApplicationEntry, desktops: &[Box<str>]) -> bool {
    if entry.no_display() {
        return false;
    }
    let only = entry.get_vec("OnlyShowIn").unwrap_or_default();
    let excluded = entry.get_vec("NotShowIn").unwrap_or_default();
    if only.iter().any(|name| excluded.contains(name)) {
        return false;
    }
    for desktop in desktops {
        if only.iter().any(|name| name == desktop.as_ref()) {
            return true;
        }
        if excluded.iter().any(|name| name == desktop.as_ref()) {
            return false;
        }
    }
    only.is_empty()
}

pub(crate) fn valid_mime_type(value: &str) -> bool {
    value.len() <= 255
        && value.split_once('/').is_some_and(|(kind, subtype)| {
            !kind.is_empty()
                && !subtype.is_empty()
                && [kind, subtype].iter().all(|part| {
                    part.bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
                })
        })
}

pub(crate) fn validate_desktop_id(value: &str) -> Result<(), DesktopEntryError> {
    if value.is_empty()
        || value.len() > 255
        || !value.ends_with(".desktop")
        || value.starts_with('.')
        || value.contains('/')
        || value.contains('\\')
        || value.chars().any(char::is_control)
    {
        return Err(DesktopEntryError::InvalidDesktopId);
    }
    Ok(())
}

pub(crate) fn executable_available(executable: &OsStr, search: &[PathBuf]) -> bool {
    let path = Path::new(executable);
    if path.is_absolute() {
        return is_executable(path);
    }
    if path.components().count() != 1 {
        return false;
    }
    search
        .iter()
        .any(|directory| is_executable(&directory.join(path)))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[derive(Debug)]
pub enum DesktopEntryError {
    InvalidDesktopId,
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        detail: Box<str>,
    },
    EntryLimitExceeded,
    DirectoryDepthExceeded(PathBuf),
}

impl DesktopEntryError {
    fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl std::fmt::Display for DesktopEntryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidDesktopId => formatter.write_str("invalid desktop entry ID"),
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "desktop entry I/O failed at {}: {source}",
                    path.display()
                )
            }
            Self::Parse { path, detail } => {
                write!(
                    formatter,
                    "desktop entry parse failed at {}: {detail}",
                    path.display()
                )
            }
            Self::EntryLimitExceeded => formatter.write_str("desktop entry limit exceeded"),
            Self::DirectoryDepthExceeded(path) => write!(
                formatter,
                "desktop entry directory depth exceeded at {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for DesktopEntryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
