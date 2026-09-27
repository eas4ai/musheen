//! Running a local file (SYS-035, SYS-036): what a file could run as, and
//! the check that the file about to run is the one the user reviewed.

use super::desktop_entry::application_from_open_file;
use super::{DesktopApplication, LaunchError, PreparedLaunch};
use musheen_core::{ItemId, RunKind};
use rustix::fs::{Access, Mode, OFlags};
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::path::{Path, PathBuf};

/// What the file at `path` could run as, read from its first bytes: a
/// compiled program (ELF), a script (`#!`), or a desktop entry of type
/// Application. `None` for any other file and for one Musheen cannot open
/// and read. Whether the user may execute it is a separate question.
#[must_use]
pub fn run_kind(path: &Path) -> Option<RunKind> {
    let file = open_regular(path).ok()?;
    kind_of(&file, path)
}

/// Why a file may not run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunCheckError {
    /// The path no longer names the reviewed item.
    Changed,
    /// The current user may not execute the file, or Musheen cannot read it.
    NotExecutable,
    /// The file is not of a kind that may run this way.
    NotRunnable,
}

/// A local file opened for running and checked: the open file is the item
/// the user reviewed, a regular file the current user may execute, and of
/// a kind that may run this way.
#[derive(Debug)]
pub struct CheckedFile {
    file: File,
    path: PathBuf,
    kind: RunKind,
}

/// Opens `path` and checks the open file against `reviewed`, the item the
/// user reviewed, and `allowed`, the kinds that may run this way. Every
/// answer is about the open file, so a file put at `path` meanwhile fails
/// the check instead of running.
pub fn check_file_to_run(
    path: &Path,
    reviewed: &ItemId,
    allowed: &[RunKind],
) -> Result<CheckedFile, RunCheckError> {
    let file = open_regular(path).map_err(|error| match error.raw_os_error() {
        Some(code) if code == rustix::io::Errno::ACCESS.raw_os_error() => {
            RunCheckError::NotExecutable
        }
        _ => RunCheckError::Changed,
    })?;
    let opened = musheen_local::item_id_of_open_file(reviewed.provider(), path, &file)
        .map_err(|_| RunCheckError::Changed)?;
    if &opened != reviewed {
        return Err(RunCheckError::Changed);
    }
    // The kernel's execute check for the current user, asked of the open
    // file itself: its permission bits, ACL entries and mount.
    if rustix::fs::access(open_file_path(&file), Access::EXEC_OK).is_err() {
        return Err(RunCheckError::NotExecutable);
    }
    let kind = kind_of(&file, path)
        .filter(|kind| allowed.contains(kind))
        .ok_or(RunCheckError::NotRunnable)?;
    Ok(CheckedFile {
        file,
        path: path.to_path_buf(),
        kind,
    })
}

impl CheckedFile {
    #[must_use]
    pub const fn kind(&self) -> RunKind {
        self.kind
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The launch of a checked compiled program: it runs from the open
    /// file, named by its path, in its folder, with no arguments.
    pub fn program_launch(self) -> Result<PreparedLaunch, LaunchError> {
        if self.kind != RunKind::Program {
            return Err(LaunchError::InvalidExecutable(self.path));
        }
        PreparedLaunch::for_checked_program(&self.path, OwnedFd::from(self.file))
    }

    /// The desktop entry read from the open file, for a checked desktop
    /// entry.
    #[must_use]
    pub fn desktop_application(&self) -> Option<DesktopApplication> {
        (self.kind == RunKind::DesktopEntry)
            .then(|| application_from_open_file(&self.file, &self.path))
            .flatten()
    }
}

/// The launch that runs `program` in the system terminal (SYS-036), in
/// the folder `working_directory`: `xdg-terminal-exec program` when it is
/// installed, otherwise `x-terminal-emulator -e program`. Each is looked
/// for in `executable_directories`, in order.
pub fn system_terminal_launch(
    executable_directories: &[PathBuf],
    program: &Path,
    working_directory: &Path,
) -> Result<PreparedLaunch, LaunchError> {
    let find = |name: &str| {
        executable_directories
            .iter()
            .map(|directory| directory.join(name))
            .find(|candidate| {
                candidate.is_absolute()
                    && rustix::fs::access(candidate, Access::EXEC_OK).is_ok()
                    && candidate.is_file()
            })
    };
    let program = OsString::from(program.as_os_str());
    if let Some(terminal) = find("xdg-terminal-exec") {
        return PreparedLaunch::command(&terminal, vec![program], working_directory);
    }
    if let Some(terminal) = find("x-terminal-emulator") {
        return PreparedLaunch::command(
            &terminal,
            vec![OsString::from("-e"), program],
            working_directory,
        );
    }
    Err(LaunchError::TerminalUnavailable)
}

/// Opens a regular file for reading without following a final symbolic
/// link and without waiting on a FIFO put at the path.
fn open_regular(path: &Path) -> io::Result<File> {
    let file = File::from(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    Ok(file)
}

fn open_file_path(file: &File) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

fn kind_of(file: &File, path: &Path) -> Option<RunKind> {
    let mut start = [0_u8; 4];
    let read = rustix::io::pread(file, &mut start, 0).ok()?;
    let start = &start[..read];
    if start.starts_with(b"\x7fELF") {
        return Some(RunKind::Program);
    }
    if start.starts_with(b"#!") {
        return Some(RunKind::Script);
    }
    let desktop = path
        .extension()
        .is_some_and(|extension| extension == "desktop");
    (desktop && application_from_open_file(file, path).is_some()).then_some(RunKind::DesktopEntry)
}
