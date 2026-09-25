use std::ffi::{OsStr, OsString};
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use musheen_core::StorePath;

use crate::{
    DesktopEntryCatalog, DesktopEntryLauncher, DesktopPaths, PreparedLaunches, ProcessRunner,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalSize {
    columns: u16,
    rows: u16,
    cell_width: u16,
    cell_height: u16,
}

impl TerminalSize {
    pub fn new(
        columns: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
    ) -> Result<Self, TerminalError> {
        if columns == 0 || rows == 0 {
            return Err(TerminalError::InvalidSize);
        }
        Ok(Self {
            columns,
            rows,
            cell_width,
            cell_height,
        })
    }

    #[must_use]
    pub const fn columns(self) -> u16 {
        self.columns
    }

    #[must_use]
    pub const fn rows(self) -> u16 {
        self.rows
    }

    #[must_use]
    pub const fn cell_width(self) -> u16 {
        self.cell_width
    }

    #[must_use]
    pub const fn cell_height(self) -> u16 {
        self.cell_height
    }
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self::new(80, 24, 8, 16).expect("the default terminal dimensions are valid")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalProfile {
    id: Box<str>,
    program: OsString,
    arguments: Vec<OsString>,
}

impl TerminalProfile {
    pub fn new(
        id: impl Into<Box<str>>,
        program: impl Into<OsString>,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Result<Self, TerminalError> {
        let id = id.into();
        let program = program.into();
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        if id.trim().is_empty()
            || !valid_argument(&program)
            || arguments.iter().any(|argument| !valid_argument(argument))
        {
            return Err(TerminalError::InvalidProfile);
        }
        Ok(Self {
            id,
            program,
            arguments,
        })
    }

    #[must_use]
    pub fn system() -> Self {
        let shell = std::env::var_os("SHELL")
            .filter(|value| valid_argument(value))
            .unwrap_or_else(|| OsString::from("/bin/sh"));
        Self {
            id: "system".into(),
            program: shell,
            arguments: Vec::new(),
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn prepare(&self, cwd: &StorePath) -> Result<TerminalLaunch, TerminalError> {
        let cwd = local_cwd(cwd)?;
        Ok(TerminalLaunch {
            program: self.program.clone(),
            arguments: self.arguments.clone(),
            working_directory: cwd,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalTerminalCommand {
    program: OsString,
    arguments: Vec<OsString>,
}

impl ExternalTerminalCommand {
    pub fn new(
        program: impl Into<OsString>,
        arguments: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Result<Self, TerminalError> {
        let program = program.into();
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        if !valid_argument(&program) || arguments.iter().any(|argument| !valid_argument(argument)) {
            return Err(TerminalError::InvalidExternalCommand);
        }
        Ok(Self { program, arguments })
    }

    pub fn prepare(&self, cwd: &StorePath) -> Result<TerminalLaunch, TerminalError> {
        Ok(TerminalLaunch {
            program: self.program.clone(),
            arguments: self.arguments.clone(),
            working_directory: local_cwd(cwd)?,
        })
    }

    pub fn launch(&self, launch: &TerminalLaunch) -> Result<(), TerminalError> {
        use std::os::unix::process::CommandExt;

        Command::new(launch.program())
            .args(launch.arguments())
            .current_dir(
                launch
                    .working_directory()
                    .expect("external terminal launches always have a local cwd"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .map(|_| ())
            .map_err(|error| TerminalError::Spawn(error.to_string().into()))
    }
}

#[derive(Clone, Debug)]
pub struct DesktopEntryTerminalLauncher {
    desktop_id: Box<str>,
    paths: DesktopPaths,
}

impl DesktopEntryTerminalLauncher {
    pub fn new(
        desktop_id: impl Into<Box<str>>,
        paths: DesktopPaths,
    ) -> Result<Self, TerminalError> {
        let desktop_id = desktop_id.into();
        if desktop_id.is_empty() || !desktop_id.ends_with(".desktop") {
            return Err(TerminalError::InvalidExternalCommand);
        }
        Ok(Self { desktop_id, paths })
    }

    pub fn prepare(&self, cwd: &StorePath) -> Result<PreparedLaunches, TerminalError> {
        let cwd = local_cwd(cwd)?;
        let catalog = DesktopEntryCatalog::new(self.paths.clone());
        let application = catalog
            .load(&self.desktop_id)
            .map_err(|error| TerminalError::Spawn(error.to_string().into()))?
            .ok_or(TerminalError::DesktopEntryUnavailable)?;
        DesktopEntryLauncher::new(self.paths.executable_dirs().to_vec())
            .prepare_in_working_directory(&application, &[], None, &cwd)
            .map_err(|error| TerminalError::Spawn(error.to_string().into()))
    }

    pub fn launch(
        &self,
        prepared: &PreparedLaunches,
        runner: &(impl ProcessRunner + ?Sized),
    ) -> Result<(), TerminalError> {
        DesktopEntryLauncher::new(self.paths.executable_dirs().to_vec())
            .launch(prepared, runner)
            .map_err(|error| TerminalError::Spawn(error.to_string().into()))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalLaunch {
    program: OsString,
    arguments: Vec<OsString>,
    working_directory: PathBuf,
}

impl TerminalLaunch {
    #[must_use]
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    #[must_use]
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }

    #[must_use]
    pub fn working_directory(&self) -> Option<&Path> {
        Some(&self.working_directory)
    }
}

fn local_cwd(cwd: &StorePath) -> Result<PathBuf, TerminalError> {
    cwd.as_unix_path()
        .map(Path::to_path_buf)
        .ok_or(TerminalError::UnrepresentableWorkingDirectory)
}

fn valid_argument(value: &OsStr) -> bool {
    !value.is_empty() && !value.as_encoded_bytes().contains(&0)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TerminalError {
    InvalidSize,
    InvalidProfile,
    InvalidExternalCommand,
    UnrepresentableWorkingDirectory,
    DesktopEntryUnavailable,
    Spawn(Box<str>),
    Io(Box<str>),
    NotRunning,
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSize => "terminal dimensions must be nonzero",
            Self::InvalidProfile => "terminal profile is invalid",
            Self::InvalidExternalCommand => "external terminal command is invalid",
            Self::UnrepresentableWorkingDirectory => {
                "this location cannot be represented as a local terminal working directory"
            }
            Self::DesktopEntryUnavailable => "the configured terminal desktop entry is unavailable",
            Self::Spawn(message) | Self::Io(message) => message,
            Self::NotRunning => "terminal child is not running",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for TerminalError {}
