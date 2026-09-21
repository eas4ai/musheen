//! Freedesktop application discovery, MIME associations, and safe launching.

mod desktop_entry;
mod launch;
mod mimeapps;

pub use desktop_entry::*;
pub use launch::*;
pub use mimeapps::*;

use std::path::{Path, PathBuf};

/// XDG paths captured once at the desktop-service boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DesktopPaths {
    config_home: PathBuf,
    config_dirs: Vec<PathBuf>,
    data_home: PathBuf,
    data_dirs: Vec<PathBuf>,
    current_desktops: Vec<Box<str>>,
    executable_dirs: Vec<PathBuf>,
}

impl DesktopPaths {
    #[must_use]
    pub fn new(config_home: impl Into<PathBuf>, data_home: impl Into<PathBuf>) -> Self {
        Self {
            config_home: config_home.into(),
            config_dirs: Vec::new(),
            data_home: data_home.into(),
            data_dirs: Vec::new(),
            current_desktops: Vec::new(),
            executable_dirs: Vec::new(),
        }
    }

    /// Capture the current process environment without converting paths to UTF-8.
    pub fn from_environment() -> Result<Self, DesktopPathsError> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let config_home = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".config")))
            .ok_or(DesktopPathsError::MissingHome)?;
        let data_home = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".local/share")))
            .ok_or(DesktopPathsError::MissingHome)?;
        if !config_home.is_absolute() {
            return Err(DesktopPathsError::RelativeConfigHome);
        }
        if !data_home.is_absolute() {
            return Err(DesktopPathsError::RelativeDataHome);
        }
        let config_dirs = std::env::var_os("XDG_CONFIG_DIRS")
            .map(|value| absolute_paths(std::env::split_paths(&value)))
            .filter(|paths| !paths.is_empty())
            .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")]);
        let data_dirs = std::env::var_os("XDG_DATA_DIRS")
            .map(|value| absolute_paths(std::env::split_paths(&value)))
            .filter(|paths| !paths.is_empty())
            .unwrap_or_else(|| {
                vec![
                    PathBuf::from("/usr/local/share"),
                    PathBuf::from("/usr/share"),
                ]
            });
        let executable_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        let current_desktops = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
        Ok(Self::new(config_home, data_home)
            .with_config_dirs(config_dirs)
            .with_data_dirs(data_dirs)
            .with_current_desktops(current_desktops)
            .with_executable_dirs(executable_dirs))
    }

    #[must_use]
    pub fn with_config_dirs(mut self, directories: impl IntoIterator<Item = PathBuf>) -> Self {
        self.config_dirs = clean_paths(directories);
        self
    }

    #[must_use]
    pub fn with_data_dirs(mut self, directories: impl IntoIterator<Item = PathBuf>) -> Self {
        self.data_dirs = clean_paths(directories);
        self
    }

    #[must_use]
    pub fn with_current_desktops(mut self, desktops: impl AsRef<str>) -> Self {
        self.current_desktops = desktops
            .as_ref()
            .split(':')
            .filter_map(|desktop| {
                let desktop = desktop.trim();
                (!desktop.is_empty()).then(|| Box::<str>::from(desktop))
            })
            .collect();
        self.current_desktops.dedup();
        self
    }

    #[must_use]
    pub fn with_executable_dirs(mut self, directories: impl IntoIterator<Item = PathBuf>) -> Self {
        self.executable_dirs = clean_paths(directories);
        self
    }

    #[must_use]
    pub fn config_home(&self) -> &Path {
        &self.config_home
    }

    #[must_use]
    pub fn data_home(&self) -> &Path {
        &self.data_home
    }

    #[must_use]
    pub fn config_dirs(&self) -> &[PathBuf] {
        &self.config_dirs
    }

    #[must_use]
    pub fn data_dirs(&self) -> &[PathBuf] {
        &self.data_dirs
    }

    #[must_use]
    pub fn current_desktops(&self) -> &[Box<str>] {
        &self.current_desktops
    }

    #[must_use]
    pub fn executable_dirs(&self) -> &[PathBuf] {
        &self.executable_dirs
    }

    pub(crate) fn application_directories(&self) -> Vec<PathBuf> {
        std::iter::once(self.data_home.join("applications"))
            .chain(
                self.data_dirs
                    .iter()
                    .map(|directory| directory.join("applications")),
            )
            .collect()
    }
}

fn clean_paths(directories: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for directory in directories {
        if !directory.as_os_str().is_empty() && !result.contains(&directory) {
            result.push(directory);
        }
    }
    result
}

fn absolute_paths(directories: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    clean_paths(
        directories
            .into_iter()
            .filter(|directory| directory.is_absolute()),
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopPathsError {
    MissingHome,
    RelativeConfigHome,
    RelativeDataHome,
}

impl std::fmt::Display for DesktopPathsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingHome => {
                formatter.write_str("HOME is required when XDG user directories are unset")
            }
            Self::RelativeConfigHome => formatter.write_str("XDG_CONFIG_HOME must be absolute"),
            Self::RelativeDataHome => formatter.write_str("XDG_DATA_HOME must be absolute"),
        }
    }
}

impl std::error::Error for DesktopPathsError {}
