use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, PosixACL, Qualifier};
use std::fs::Metadata;
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclQualifier {
    Owner,
    OwningGroup,
    Other,
    User(u32),
    Group(u32),
    Mask,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclEntry {
    qualifier: AclQualifier,
    read: bool,
    write: bool,
    execute: bool,
}

impl AclEntry {
    pub fn qualifier(&self) -> &AclQualifier {
        &self.qualifier
    }

    pub fn read(&self) -> bool {
        self.read
    }

    pub fn write(&self) -> bool {
        self.write
    }

    pub fn execute(&self) -> bool {
        self.execute
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclState {
    Available(Vec<AclEntry>),
    Unsupported(Box<str>),
    Unavailable(Box<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionSnapshot {
    owner: u32,
    group: u32,
    mode: u32,
    acl: AclState,
    default_acl: Option<AclState>,
}

impl PermissionSnapshot {
    pub(crate) fn read(path: &Path, metadata: &Metadata, is_symlink: bool) -> Self {
        let acl = if is_symlink {
            AclState::Unsupported("POSIX ACLs are not read through symbolic links".into())
        } else {
            read_acl(path, false)
        };
        let default_acl = metadata.is_dir().then(|| read_acl(path, true));
        Self {
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            acl,
            default_acl,
        }
    }

    pub fn owner(&self) -> u32 {
        self.owner
    }

    pub fn group(&self) -> u32 {
        self.group
    }

    pub fn mode(&self) -> u32 {
        self.mode
    }

    pub fn executable(&self) -> bool {
        self.mode & 0o111 != 0
    }

    pub fn acl(&self) -> &AclState {
        &self.acl
    }

    pub fn default_acl(&self) -> Option<&AclState> {
        self.default_acl.as_ref()
    }
}

fn read_acl(path: &Path, default: bool) -> AclState {
    let result = if default {
        PosixACL::read_default_acl(path)
    } else {
        PosixACL::read_acl(path)
    };
    match result {
        Ok(acl) => AclState::Available(
            acl.entries()
                .into_iter()
                .map(|entry| AclEntry {
                    qualifier: map_qualifier(entry.qual),
                    read: entry.perm & ACL_READ != 0,
                    write: entry.perm & ACL_WRITE != 0,
                    execute: entry.perm & ACL_EXECUTE != 0,
                })
                .collect(),
        ),
        Err(error) if error.kind() == ErrorKind::Unsupported => {
            AclState::Unsupported(error.to_string().into())
        }
        Err(error) => AclState::Unavailable(error.to_string().into()),
    }
}

fn map_qualifier(qualifier: Qualifier) -> AclQualifier {
    match qualifier {
        Qualifier::UserObj => AclQualifier::Owner,
        Qualifier::GroupObj => AclQualifier::OwningGroup,
        Qualifier::Other => AclQualifier::Other,
        Qualifier::User(id) => AclQualifier::User(id),
        Qualifier::Group(id) => AclQualifier::Group(id),
        Qualifier::Mask => AclQualifier::Mask,
        Qualifier::Undefined => AclQualifier::Unknown,
    }
}
