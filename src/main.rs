use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use musheen_core::StorePath;

mod instance;

fn main() -> ExitCode {
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
