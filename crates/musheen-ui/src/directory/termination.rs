//! Removes the live directory indexes when the process is asked to end.
//!
//! `Drop` never runs when a process ends by signal, so an index directory
//! would survive SIGTERM or SIGINT. A watcher thread waits for either signal,
//! removes every live index, then restores the default disposition and
//! raises the signal again, so the process still ends the way the sender
//! expects.

use super::index::remove_live_indexes;
use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use signal_hook::low_level::emulate_default_handler;
use std::io;
use std::sync::OnceLock;

static INSTALLED: OnceLock<io::Result<()>> = OnceLock::new();

/// Installs the SIGTERM and SIGINT watcher once for the process. Later calls
/// return the first call's outcome.
pub(crate) fn install_index_cleanup_on_termination() -> io::Result<()> {
    match INSTALLED.get_or_init(install) {
        Ok(()) => Ok(()),
        Err(error) => Err(io::Error::new(error.kind(), error.to_string())),
    }
}

fn install() -> io::Result<()> {
    let mut signals = Signals::new([SIGTERM, SIGINT])?;
    std::thread::Builder::new()
        .name("musheen-termination".into())
        .spawn(move || {
            if let Some(signal) = signals.forever().next() {
                remove_live_indexes();
                let _ = emulate_default_handler(signal);
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::index::DiskDirectoryIndex;
    use super::install_index_cleanup_on_termination;
    use rustix::process::{Pid, Signal, kill_process};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{Duration, Instant};

    const HELPER_ROOT: &str = "MUSHEEN_INDEX_SIGTERM_ROOT";

    #[test]
    fn indexed_folder_indexes_are_removed_on_sigterm() {
        if let Some(root) = std::env::var_os(HELPER_ROOT) {
            // The child: hold a live index, tell the parent where it is, and
            // wait for the signal.
            let root = PathBuf::from(root);
            install_index_cleanup_on_termination().unwrap();
            let index = DiskDirectoryIndex::new_in(&root).unwrap();
            std::fs::write(root.join("ready"), index.path().as_os_str().as_bytes()).unwrap();
            std::thread::sleep(Duration::from_secs(30));
            drop(index);
            return;
        }

        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "directory::termination::tests::indexed_folder_indexes_are_removed_on_sigterm",
                "--nocapture",
            ])
            .env(HELPER_ROOT, root.path())
            .spawn()
            .unwrap();
        let ready = root.path().join("ready");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !ready.exists() {
            assert!(
                Instant::now() < deadline,
                "the child never created its index"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let index_path =
            PathBuf::from(std::ffi::OsStr::from_bytes(&std::fs::read(&ready).unwrap()));
        assert!(
            index_path.is_dir(),
            "the child's index exists before the signal"
        );

        kill_process(Pid::from_child(&child), Signal::TERM).unwrap();
        let status = child.wait().unwrap();

        assert_eq!(
            status.signal(),
            Some(Signal::TERM.as_raw()),
            "the child still ends by SIGTERM: {status:?}"
        );
        assert!(
            !index_path.exists(),
            "the live index is removed before the process ends"
        );
    }
}
