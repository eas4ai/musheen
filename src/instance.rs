use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

pub(crate) enum InstanceStatus {
    Primary(InstanceGuard),
    AlreadyRunning,
}

pub(crate) struct InstanceGuard {
    _locks: Vec<File>,
}

pub(crate) fn acquire_for_current_user() -> io::Result<InstanceStatus> {
    let status = musheen_desktop::StatusStore::for_current_user();
    let legacy_path = status.path().with_file_name("instance.lock");
    let runtime_directory = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let path = instance_lock_path(runtime_directory.as_deref(), status.path());
    if path == legacy_path {
        return acquire_at(&path);
    }
    match acquire_at(&path) {
        Ok(InstanceStatus::Primary(mut runtime)) => match acquire_at(&legacy_path) {
            Ok(InstanceStatus::Primary(mut legacy)) => {
                runtime._locks.append(&mut legacy._locks);
                Ok(InstanceStatus::Primary(runtime))
            }
            Ok(InstanceStatus::AlreadyRunning) => Ok(InstanceStatus::AlreadyRunning),
            Err(_) => Ok(InstanceStatus::Primary(runtime)),
        },
        Ok(InstanceStatus::AlreadyRunning) => Ok(InstanceStatus::AlreadyRunning),
        Err(_) => acquire_at(&legacy_path),
    }
}

fn instance_lock_path(runtime_directory: Option<&Path>, status_path: &Path) -> PathBuf {
    runtime_directory
        .filter(|path| path.is_absolute())
        .map_or_else(
            || status_path.with_file_name("instance.lock"),
            |path| path.join("musheen/instance.lock"),
        )
}

pub(crate) fn acquire_at(path: &Path) -> io::Result<InstanceStatus> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the instance lock has no parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let lock = File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(io::Error::from)?,
    );
    match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok(InstanceStatus::Primary(InstanceGuard {
            _locks: vec![lock],
        })),
        Err(error) if error == rustix::io::Errno::WOULDBLOCK => Ok(InstanceStatus::AlreadyRunning),
        Err(error) => Err(io::Error::from(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_allows_only_one_live_instance_and_releases_on_drop() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("state/instance.lock");
        let first = acquire_at(&path).unwrap();
        assert!(matches!(first, InstanceStatus::Primary(_)));
        assert!(matches!(
            acquire_at(&path).unwrap(),
            InstanceStatus::AlreadyRunning
        ));

        drop(first);
        assert!(matches!(
            acquire_at(&path).unwrap(),
            InstanceStatus::Primary(_)
        ));
    }

    #[test]
    fn runtime_directory_hosts_the_ephemeral_instance_lock() {
        assert_eq!(
            instance_lock_path(
                Some(Path::new("/run/user/1000")),
                Path::new("/home/user/.config/musheen/operations.json"),
            ),
            Path::new("/run/user/1000/musheen/instance.lock")
        );
        assert_eq!(
            instance_lock_path(
                Some(Path::new("relative-runtime")),
                Path::new("/home/user/.config/musheen/operations.json"),
            ),
            Path::new("/home/user/.config/musheen/instance.lock")
        );
    }
}
