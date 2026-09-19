use crate::MutationError;
use musheen_core::StorePath;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclQualifier {
    Owner,
    OwningGroup,
    Other,
    User(u32),
    Group(u32),
    Mask,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclEntry {
    qualifier: AclQualifier,
    read: bool,
    write: bool,
    execute: bool,
}

impl AclEntry {
    #[must_use]
    pub const fn new(qualifier: AclQualifier, read: bool, write: bool, execute: bool) -> Self {
        Self {
            qualifier,
            read,
            write,
            execute,
        }
    }

    #[must_use]
    pub const fn qualifier(&self) -> &AclQualifier {
        &self.qualifier
    }

    #[must_use]
    pub const fn read(&self) -> bool {
        self.read
    }

    #[must_use]
    pub const fn write(&self) -> bool {
        self.write
    }

    #[must_use]
    pub const fn execute(&self) -> bool {
        self.execute
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclChange {
    Replace(Vec<AclEntry>),
    Remove,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataChange {
    file_mode: Option<u32>,
    directory_mode: Option<u32>,
    owner: Option<u32>,
    group: Option<u32>,
    access_acl: Option<AclChange>,
    default_acl: Option<AclChange>,
}

impl MetadataChange {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub const fn with_file_mode(mut self, mode: u32) -> Self {
        self.file_mode = Some(mode);
        self
    }

    #[must_use]
    pub const fn with_directory_mode(mut self, mode: u32) -> Self {
        self.directory_mode = Some(mode);
        self
    }

    #[must_use]
    pub const fn with_owner(mut self, owner: u32) -> Self {
        self.owner = Some(owner);
        self
    }

    #[must_use]
    pub const fn with_group(mut self, group: u32) -> Self {
        self.group = Some(group);
        self
    }

    #[must_use]
    pub fn with_access_acl(mut self, acl: AclChange) -> Self {
        self.access_acl = Some(acl);
        self
    }

    #[must_use]
    pub fn with_default_acl(mut self, acl: AclChange) -> Self {
        self.default_acl = Some(acl);
        self
    }

    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.file_mode.is_some()
            || self.directory_mode.is_some()
            || self.owner.is_some()
            || self.group.is_some()
            || self.access_acl.is_some()
            || self.default_acl.is_some()
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        [self.file_mode, self.directory_mode]
            .into_iter()
            .flatten()
            .all(|mode| mode & !0o7777 == 0)
    }

    #[must_use]
    pub fn requires_permissions(&self) -> bool {
        self.file_mode.is_some()
            || self.directory_mode.is_some()
            || self.access_acl.is_some()
            || self.default_acl.is_some()
    }

    #[must_use]
    pub const fn requires_ownership(&self) -> bool {
        self.owner.is_some() || self.group.is_some()
    }

    fn resolve(&self, kind: MetadataEntryKind) -> ResolvedMetadataChange {
        let mode = match kind {
            MetadataEntryKind::File => self.file_mode,
            MetadataEntryKind::Directory => self.directory_mode,
            MetadataEntryKind::SymbolicLink => None,
        };
        ResolvedMetadataChange {
            mode,
            owner: self.owner,
            group: self.group,
            access_acl: (kind != MetadataEntryKind::SymbolicLink)
                .then(|| self.access_acl.clone())
                .flatten(),
            default_acl: (kind == MetadataEntryKind::Directory)
                .then(|| self.default_acl.clone())
                .flatten(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataScope {
    Single,
    Recursive {
        include_nested_mounts: bool,
        reviewed: bool,
    },
}

impl MetadataScope {
    #[must_use]
    pub const fn recursive(include_nested_mounts: bool, reviewed: bool) -> Self {
        Self::Recursive {
            include_nested_mounts,
            reviewed,
        }
    }

    #[must_use]
    pub const fn includes_nested_mounts(self) -> bool {
        matches!(
            self,
            Self::Recursive {
                include_nested_mounts: true,
                ..
            }
        )
    }

    #[must_use]
    pub const fn is_recursive(self) -> bool {
        matches!(self, Self::Recursive { .. })
    }

    #[must_use]
    pub const fn is_reviewed(self) -> bool {
        matches!(self, Self::Single | Self::Recursive { reviewed: true, .. })
    }

    #[must_use]
    pub const fn reviewed(self) -> Self {
        match self {
            Self::Single => Self::Single,
            Self::Recursive {
                include_nested_mounts,
                ..
            } => Self::Recursive {
                include_nested_mounts,
                reviewed: true,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataEntryKind {
    File,
    Directory,
    SymbolicLink,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataEntry {
    path: StorePath,
    expected_identity: Box<[u8]>,
    kind: MetadataEntryKind,
    requires_privilege: bool,
}

impl MetadataEntry {
    #[must_use]
    pub fn new(
        path: StorePath,
        expected_identity: Vec<u8>,
        kind: MetadataEntryKind,
        requires_privilege: bool,
    ) -> Self {
        Self {
            path,
            expected_identity: expected_identity.into_boxed_slice(),
            kind,
            requires_privilege,
        }
    }

    #[must_use]
    pub const fn path(&self) -> &StorePath {
        &self.path
    }

    #[must_use]
    pub const fn expected_identity(&self) -> &[u8] {
        &self.expected_identity
    }

    #[must_use]
    pub const fn kind(&self) -> MetadataEntryKind {
        self.kind
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedMetadataChange {
    mode: Option<u32>,
    owner: Option<u32>,
    group: Option<u32>,
    access_acl: Option<AclChange>,
    default_acl: Option<AclChange>,
}

impl ResolvedMetadataChange {
    fn is_dirty(&self) -> bool {
        self.mode.is_some()
            || self.owner.is_some()
            || self.group.is_some()
            || self.access_acl.is_some()
            || self.default_acl.is_some()
    }

    #[must_use]
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    #[must_use]
    pub const fn owner(&self) -> Option<u32> {
        self.owner
    }

    #[must_use]
    pub const fn group(&self) -> Option<u32> {
        self.group
    }

    #[must_use]
    pub const fn access_acl(&self) -> Option<&AclChange> {
        self.access_acl.as_ref()
    }

    #[must_use]
    pub const fn default_acl(&self) -> Option<&AclChange> {
        self.default_acl.as_ref()
    }
}

pub trait MetadataProvider {
    fn preview(
        &mut self,
        root: &StorePath,
        expected_identity: &[u8],
        scope: MetadataScope,
        change: &MetadataChange,
    ) -> Result<Vec<MetadataEntry>, MutationError>;

    fn apply_metadata(
        &mut self,
        entry: &MetadataEntry,
        change: &ResolvedMetadataChange,
    ) -> Result<(), MutationError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataPlan {
    root: StorePath,
    entries: Vec<(MetadataEntry, ResolvedMetadataChange)>,
    requires_privilege: bool,
    requires_permissions: bool,
    requires_ownership: bool,
}

impl MetadataPlan {
    pub fn preflight(
        provider: &mut impl MetadataProvider,
        root: StorePath,
        expected_identity: Vec<u8>,
        scope: MetadataScope,
        change: MetadataChange,
    ) -> Result<Self, MutationError> {
        if !change.is_dirty() {
            return Err(MutationError::NoChanges);
        }
        if !change.is_valid() {
            return Err(MutationError::InvalidMetadata);
        }
        if matches!(
            scope,
            MetadataScope::Recursive {
                reviewed: false,
                ..
            }
        ) {
            return Err(MutationError::ScopeNotReviewed);
        }
        let entries = provider.preview(&root, &expected_identity, scope, &change)?;
        let requires_privilege = change.owner.is_some()
            || change.group.is_some()
            || entries.iter().any(|entry| entry.requires_privilege);
        let entries: Vec<_> = entries
            .into_iter()
            .map(|entry| {
                let resolved = change.resolve(entry.kind);
                (entry, resolved)
            })
            .filter(|(_, change)| change.is_dirty())
            .collect();
        if entries.is_empty() {
            return Err(MutationError::InvalidMetadata);
        }
        let requires_permissions = change.requires_permissions();
        let requires_ownership = change.requires_ownership();
        Ok(Self {
            root,
            entries,
            requires_privilege,
            requires_permissions,
            requires_ownership,
        })
    }

    #[must_use]
    pub const fn root(&self) -> &StorePath {
        &self.root
    }

    #[must_use]
    pub const fn requires_privilege(&self) -> bool {
        self.requires_privilege
    }

    #[must_use]
    pub const fn requires_permissions(&self) -> bool {
        self.requires_permissions
    }

    #[must_use]
    pub const fn requires_ownership(&self) -> bool {
        self.requires_ownership
    }

    pub fn execute(self, provider: &mut impl MetadataProvider) -> Result<(), MutationError> {
        for (entry, change) in self.entries {
            provider.apply_metadata(&entry, &change)?;
        }
        Ok(())
    }
}
