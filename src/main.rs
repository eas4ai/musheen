use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use musheen_core::StorePath;

mod instance;

/// The flag the portal backend's D-Bus activation file starts Musheen with.
const PORTAL_BACKEND_FLAG: &str = "--portal-backend";

/// How this start was asked for.
#[derive(Debug, Eq, PartialEq)]
enum Launch {
    /// A file-manager window, at a folder.
    Folder,
    /// Only the FileChooser portal backend, started by xdg-desktop-portal
    /// through D-Bus activation (SYS-027).
    PortalBackend,
}

fn launch(arguments: impl IntoIterator<Item = std::ffi::OsString>) -> Launch {
    if arguments.into_iter().nth(1).as_deref() == Some(PORTAL_BACKEND_FLAG.as_ref()) {
        Launch::PortalBackend
    } else {
        Launch::Folder
    }
}

fn main() -> ExitCode {
    // A portal start takes no instance lock and forwards nothing: it opens
    // no folder window, and when another Musheen already serves the backend
    // it cannot claim the backend's name and ends.
    if launch(std::env::args_os()) == Launch::PortalBackend {
        musheen_ui::run_portal_backend();
        return ExitCode::SUCCESS;
    }
    let initial_path = initial_path();
    let _instance = match instance::acquire_for_current_user() {
        Ok(instance::InstanceStatus::Primary(instance)) => instance,
        Ok(instance::InstanceStatus::AlreadyRunning) => {
            return match forward_to_primary(&initial_path) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("Musheen could not forward this launch to the running app: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        Err(error) => {
            eprintln!("Musheen could not acquire its instance lock: {error}");
            return ExitCode::FAILURE;
        }
    };
    musheen_ui::run(initial_path);
    ExitCode::SUCCESS
}

fn initial_path() -> PathBuf {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/"));
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(&path))
            .unwrap_or(path)
    }
}

fn forward_to_primary(path: &std::path::Path) -> Result<(), musheen_desktop::FileManagerError> {
    let location = StorePath::from_unix_path(path.as_os_str());
    let startup_id = std::env::var("DESKTOP_STARTUP_ID")
        .or_else(|_| std::env::var("XDG_ACTIVATION_TOKEN"))
        .unwrap_or_default();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match futures_lite::future::block_on(musheen_desktop::forward_show_folders_to_musheen(
            None,
            std::slice::from_ref(&location),
            &startup_id,
        )) {
            Ok(()) => return Ok(()),
            Err(musheen_desktop::FileManagerError::Service(_)) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_backend_start_is_not_a_folder_launch() {
        let arguments = |list: &[&str]| {
            list.iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            launch(arguments(&["musheen", "--portal-backend"])),
            Launch::PortalBackend
        );
        assert_eq!(launch(arguments(&["musheen"])), Launch::Folder);
        assert_eq!(
            launch(arguments(&["musheen", "/home/user/--portal-backend"])),
            Launch::Folder
        );
    }
}
