//! User and group names for the Properties window's Permissions page
//! (SEARCH-019), looked up through the system's account database.

/// The name of user `uid`, when the system knows one.
#[must_use]
pub fn user_name(uid: u32) -> Option<String> {
    nix::unistd::User::from_uid(nix::unistd::Uid::from_raw(uid))
        .ok()
        .flatten()
        .map(|user| user.name)
}

/// The name of group `gid`, when the system knows one.
#[must_use]
pub fn group_name(gid: u32) -> Option<String> {
    nix::unistd::Group::from_gid(nix::unistd::Gid::from_raw(gid))
        .ok()
        .flatten()
        .map(|group| group.name)
}

/// The groups the current user belongs to, primary group first, each once,
/// with its name, or its number when the system knows no name.
#[must_use]
pub fn current_user_groups() -> Vec<(u32, String)> {
    let mut groups = vec![rustix::process::getegid().as_raw()];
    for group in rustix::process::getgroups().unwrap_or_default() {
        if !groups.contains(&group.as_raw()) {
            groups.push(group.as_raw());
        }
    }
    groups
        .into_iter()
        .map(|gid| (gid, group_name(gid).unwrap_or_else(|| gid.to_string())))
        .collect()
}

/// The current user's effective user ID, the owner the page compares with.
#[must_use]
pub fn effective_user() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Every user account the system lists, sorted by name, each with its ID,
/// for the Permissions page's owner chooser (SEARCH-019).
#[must_use]
pub fn all_users() -> Vec<(u32, String)> {
    all_accounts("passwd")
}

/// Every group the system lists, sorted by name, each with its ID.
#[must_use]
pub fn all_groups() -> Vec<(u32, String)> {
    all_accounts("group")
}

/// How long listing an account database may take before the local file
/// is read instead: a directory service that does not answer must not hold
/// the Properties window.
const LISTING_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// The entries of an account database. `getent` sees every source the
/// system uses (files, systemd, a directory service that allows listing);
/// without it, or when it takes too long, the local file is read.
fn all_accounts(database: &str) -> Vec<(u32, String)> {
    let listing = getent(database)
        .or_else(|| std::fs::read(format!("/etc/{database}")).ok())
        .unwrap_or_default();
    let mut accounts = parse_accounts(&listing);
    accounts.sort_by(|left, right| left.1.cmp(&right.1).then(left.0.cmp(&right.0)));
    accounts.dedup();
    accounts
}

/// The output of `getent <database>`, or `None` when getent is missing,
/// fails, or runs longer than [`LISTING_TIMEOUT`]. Its output is read on its
/// own thread, so a long listing cannot fill the pipe and stall it.
fn getent(database: &str) -> Option<Vec<u8>> {
    use std::io::Read as _;

    let mut child = std::process::Command::new("getent")
        .arg(database)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = std::time::Instant::now() + LISTING_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().ok()?.ok()?;
    status
        .filter(std::process::ExitStatus::success)
        .map(|_| bytes)
}

/// `name:password:id:...` lines, as passwd and group files hold them.
fn parse_accounts(listing: &[u8]) -> Vec<(u32, String)> {
    String::from_utf8_lossy(listing)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let id = fields.nth(1)?.parse::<u32>().ok()?;
            (!name.is_empty() && id != u32::MAX).then(|| (id, name.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_accounts;

    #[test]
    fn account_lines_give_names_and_ids() {
        let listing = b"root:x:0:0:root:/root:/bin/bash\nbroken\nalice:x:1000:1000::/home/alice:/bin/sh\n:x:5:\nwheel:x:998:alice\n";
        assert_eq!(
            parse_accounts(listing),
            [
                (0, "root".to_owned()),
                (1000, "alice".to_owned()),
                (998, "wheel".to_owned())
            ]
        );
    }
}
