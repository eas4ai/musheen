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
