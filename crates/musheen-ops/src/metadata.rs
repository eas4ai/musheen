use crate::MutationError;
use musheen_core::{CancellationToken, StorePath};

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

/// One step of a mode edit (SEARCH-019).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeStep {
    /// Sets one class's access; `shift` places the class (6 owner, 3 group,
    /// 0 others). A file gets `file_bits` (0, 4 or 6) as its read and write
    /// bits. Its execute bit is cleared with No Access; it follows read when
    /// the file's execute bits followed its read bits before the step (the
    /// executable checkbox was on); otherwise it stays. A folder gets
    /// `folder_bits` (0, 5 or 7).
    Access {
        shift: u32,
        file_bits: u32,
        folder_bits: u32,
    },
    /// The executable checkbox: `true` adds execute for each class that may
    /// read a file, `false` clears execute for all three. Folders keep theirs.
    Executable(bool),
    /// An Advanced bit, set or cleared only on the kinds of entry it was
    /// chosen on, so a bit chosen on a folder does not reach its files.
    Bit {
        mask: u32,
        on: bool,
        files: bool,
        folders: bool,
    },
}

impl ModeStep {
    fn apply(self, kind: MetadataEntryKind, mode: u32) -> u32 {
        match (self, kind) {
            (
                Self::Access {
                    shift, file_bits, ..
                },
                MetadataEntryKind::File,
            ) => {
                let execute = if file_bits == 0 {
                    0
                } else if executes_where_readable(mode) {
                    1
                } else {
                    (mode >> shift) & 1
                };
                (mode & !(0o7 << shift)) | ((file_bits | execute) << shift)
            }
            (
                Self::Access {
                    shift, folder_bits, ..
                },
                MetadataEntryKind::Directory,
            ) => (mode & !(0o7 << shift)) | (folder_bits << shift),
            (Self::Executable(true), MetadataEntryKind::File) => mode | ((mode & 0o444) >> 2),
            (Self::Executable(false), MetadataEntryKind::File) => mode & !0o111,
            (
                Self::Bit {
                    mask,
                    on,
                    files,
                    folders,
                },
                kind,
            ) if (files && kind == MetadataEntryKind::File)
                || (folders && kind == MetadataEntryKind::Directory) =>
            {
                if on {
                    mode | mask
                } else {
                    mode & !mask
                }
            }
            _ => mode,
        }
    }
}

/// Whether a file's execute bits are exactly its read bits, and not none:
/// the state the executable checkbox shows as checked.
#[must_use]
pub const fn executes_where_readable(mode: u32) -> bool {
    let execute = mode & 0o111;
    execute != 0 && execute == (mode & 0o444) >> 2
}

/// The mode a non-folder keeps after a change of owner or group: the kernel
/// clears set-user-ID, and set-group-ID when the group may execute.
#[must_use]
pub const fn mode_after_ownership_change(kind: MetadataEntryKind, mode: u32) -> u32 {
    match kind {
        MetadataEntryKind::File => {
            let setgid = if mode & 0o010 != 0 { 0o2000 } else { 0 };
            mode & !(0o4000 | setgid)
        }
        MetadataEntryKind::Directory | MetadataEntryKind::SymbolicLink => mode,
    }
}

/// A change that each entry applies to its own mode (SEARCH-019), step by
/// step in the order the user made them, so a later step wins and a
/// selection whose items differ keeps what the user did not change.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ModeEdit {
    steps: Vec<ModeStep>,
}

impl ModeEdit {
    #[must_use]
    pub const fn new(steps: Vec<ModeStep>) -> Self {
        Self { steps }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }

    #[must_use]
    pub fn steps(&self) -> &[ModeStep] {
        &self.steps
    }

    /// The mode an entry of `kind` whose mode is `mode` gets. A link keeps
    /// its mode.
    #[must_use]
    pub fn apply(&self, kind: MetadataEntryKind, mode: u32) -> u32 {
        self.steps
            .iter()
            .fold(mode & 0o7777, |mode, step| step.apply(kind, mode))
            & 0o7777
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MetadataChange {
    file_mode: Option<u32>,
    directory_mode: Option<u32>,
    mode_edit: Option<ModeEdit>,
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

    /// Edits each entry's own mode instead of setting one mode for all.
    #[must_use]
    pub fn with_mode_edit(mut self, edit: ModeEdit) -> Self {
        self.mode_edit = (!edit.is_empty()).then_some(edit);
        self
    }

    #[must_use]
    pub const fn mode_edit(&self) -> Option<&ModeEdit> {
        self.mode_edit.as_ref()
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
            || self.mode_edit.is_some()
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
            || self.mode_edit.is_some()
            || self.access_acl.is_some()
            || self.default_acl.is_some()
    }

    #[must_use]
    pub const fn requires_ownership(&self) -> bool {
        self.owner.is_some() || self.group.is_some()
    }

    fn resolve(&self, entry: &MetadataEntry) -> ResolvedMetadataChange {
        let kind = entry.kind;
        let mode = match kind {
            MetadataEntryKind::File => self.file_mode,
            MetadataEntryKind::Directory => self.directory_mode,
            MetadataEntryKind::SymbolicLink => None,
        };
        // An entry already in the chosen group is left as it is: a chown
        // would still clear its set-user-ID bit, and fails for a non-owner.
        let group = self
            .group
            .filter(|group| entry.current_group != Some(*group));
        // A mode edit reaches an entry whose mode it would change. The
        // provider applies it to the mode it finds when it applies it, and
        // before any owner or group change (SEARCH-019).
        let mode_edit = self
            .mode_edit
            .as_ref()
            .filter(|edit| {
                mode.is_none()
                    && kind != MetadataEntryKind::SymbolicLink
                    && entry
                        .current_mode
                        .is_none_or(|current| edit.apply(kind, current) != current & 0o7777)
            })
            .cloned();
        ResolvedMetadataChange {
            kind,
            mode,
            mode_edit,
            owner: self.owner,
            group,
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
    /// The entry's mode when previewed.
    current_mode: Option<u32>,
    /// The entry's group when previewed.
    current_group: Option<u32>,
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
            current_mode: None,
            current_group: None,
        }
    }

    #[must_use]
    pub const fn with_current_mode(mut self, mode: u32) -> Self {
        self.current_mode = Some(mode);
        self
    }

    #[must_use]
    pub const fn with_current_group(mut self, group: u32) -> Self {
        self.current_group = Some(group);
        self
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
    kind: MetadataEntryKind,
    mode: Option<u32>,
    mode_edit: Option<ModeEdit>,
    owner: Option<u32>,
    group: Option<u32>,
    access_acl: Option<AclChange>,
    default_acl: Option<AclChange>,
}

impl ResolvedMetadataChange {
    fn is_dirty(&self) -> bool {
        self.mode.is_some()
            || self.mode_edit.is_some()
            || self.owner.is_some()
            || self.group.is_some()
            || self.access_acl.is_some()
            || self.default_acl.is_some()
    }

    /// The one mode the change sets, when it sets one.
    #[must_use]
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    #[must_use]
    pub const fn mode_edit(&self) -> Option<&ModeEdit> {
        self.mode_edit.as_ref()
    }

    /// The mode to set on the entry, whose mode is now `current`, or `None`
    /// to leave it: a mode edit is applied to the mode found when applying,
    /// so a change made after Apply is kept.
    #[must_use]
    pub fn mode_for(&self, current: u32) -> Option<u32> {
        if self.mode.is_some() {
            return self.mode;
        }
        let mode = self.mode_edit.as_ref()?.apply(self.kind, current);
        (mode != current & 0o7777).then_some(mode)
    }

    #[must_use]
    pub const fn owner(&self) -> Option<u32> {
        self.owner
    }

    #[must_use]
    pub const fn group(&self) -> Option<u32> {
        self.group
    }

    /// The owner to set on the entry, whose owner is now `current`.
    #[must_use]
    pub fn owner_for(&self, current: u32) -> Option<u32> {
        self.owner.filter(|owner| *owner != current)
    }

    /// The group to set on the entry, whose group is now `current`.
    #[must_use]
    pub fn group_for(&self, current: u32) -> Option<u32> {
        self.group.filter(|group| *group != current)
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
                let resolved = change.resolve(&entry);
                (entry, resolved)
            })
            .filter(|(_, change)| change.is_dirty())
            .collect();
        // A mode edit or a group may leave every entry of a root as it is,
        // which is not a failure: the selection's other roots still change.
        if entries.is_empty()
            && change.mode_edit.is_none()
            && change.owner.is_none()
            && change.group.is_none()
        {
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
        self.execute_controlled(provider, &CancellationToken::new())
    }

    pub fn execute_controlled(
        self,
        provider: &mut impl MetadataProvider,
        cancellation: &CancellationToken,
    ) -> Result<(), MutationError> {
        for (entry, change) in self.entries {
            cancellation
                .wait_if_paused()
                .map_err(|_| MutationError::Cancelled)?;
            provider.apply_metadata(&entry, &change)?;
        }
        Ok(())
    }
}
