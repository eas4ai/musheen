use std::fs::File;
use std::io;
use std::path::Path;

pub(crate) enum InstanceStatus {
    Primary(InstanceGuard),
    AlreadyRunning,
}

pub(crate) struct InstanceGuard {
    _lock: File,
}

pub(crate) fn acquire_for_current_user() -> io::Result<InstanceStatus> {
    let status = musheen_desktop::StatusStore::for_current_user();
    let path = status.path().with_file_name("instance.lock");
    acquire_at(&path)
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
        Ok(()) => Ok(InstanceStatus::Primary(InstanceGuard { _lock: lock })),
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
}
