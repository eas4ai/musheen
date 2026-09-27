//! The Properties window's Permissions page (SEARCH-019): access choices as
//! Dolphin names them, the executable checkbox, the group, and the mode
//! bits. Every value the page shows is what Apply would leave: each selected
//! item's mode with the user's edits applied to it.

use musheen_core::{CapabilityState, ItemKind};
use musheen_desktop::{AclQualifier, AclState, AggregateValue, PropertySnapshot};
use musheen_ops::{MetadataChange, MetadataEntryKind, MetadataScope, ModeEdit};
use std::collections::BTreeMap;

/// A class of users the page sets access for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessClass {
    Owner,
    Group,
    Others,
}

impl AccessClass {
    pub const ALL: [Self; 3] = [Self::Owner, Self::Group, Self::Others];

    const fn shift(self) -> u32 {
        match self {
            Self::Owner => 6,
            Self::Group => 3,
            Self::Others => 0,
        }
    }

    const fn index(self) -> usize {
        match self {
            Self::Owner => 0,
            Self::Group => 1,
            Self::Others => 2,
        }
    }

    /// The name used in element IDs and message keys.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Group => "group",
            Self::Others => "others",
        }
    }
}

/// An access level, as Dolphin names them. For a file, viewing is reading
/// and modifying adds writing; the execute bit has its own checkbox. For a
/// folder, viewing its content includes entering it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Access {
    None,
    View,
    Modify,
}

impl Access {
    pub const ALL: [Self; 3] = [Self::None, Self::View, Self::Modify];

    /// The name used in element IDs.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::None => "no-access",
            Self::View => "can-view",
            Self::Modify => "can-modify",
        }
    }

    const fn file_bits(self) -> u32 {
        match self {
            Self::None => 0,
            Self::View => 0o4,
            Self::Modify => 0o6,
        }
    }

    const fn folder_bits(self) -> u32 {
        match self {
            Self::None => 0,
            Self::View => 0o5,
            Self::Modify => 0o7,
        }
    }

    /// The access a class's three bits describe, if any does. A file's
    /// execute bit belongs to the executable checkbox, except that No Access
    /// has none: execute without read is no choice.
    fn of(kind: MetadataEntryKind, bits: u32) -> Option<Self> {
        match kind {
            MetadataEntryKind::File => match bits & 0o7 {
                0 => Some(Self::None),
                0o4 | 0o5 => Some(Self::View),
                0o6 | 0o7 => Some(Self::Modify),
                _ => None,
            },
            MetadataEntryKind::Directory => match bits & 0o7 {
                0 => Some(Self::None),
                0o5 => Some(Self::View),
                0o7 => Some(Self::Modify),
                _ => None,
            },
            MetadataEntryKind::SymbolicLink => None,
        }
    }
}

/// A mode bit the Advanced section shows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModeBit {
    pub mask: u32,
    /// The name used in element IDs, such as `owner-read` or `setuid`.
    pub key: &'static str,
}

/// The twelve mode bits, in the order the Advanced section shows them.
pub const MODE_BITS: [ModeBit; 12] = [
    ModeBit {
        mask: 0o400,
        key: "owner-read",
    },
    ModeBit {
        mask: 0o200,
        key: "owner-write",
    },
    ModeBit {
        mask: 0o100,
        key: "owner-execute",
    },
    ModeBit {
        mask: 0o040,
        key: "group-read",
    },
    ModeBit {
        mask: 0o020,
        key: "group-write",
    },
    ModeBit {
        mask: 0o010,
        key: "group-execute",
    },
    ModeBit {
        mask: 0o004,
        key: "others-read",
    },
    ModeBit {
        mask: 0o002,
        key: "others-write",
    },
    ModeBit {
        mask: 0o001,
        key: "others-execute",
    },
    ModeBit {
        mask: 0o4000,
        key: "setuid",
    },
    ModeBit {
        mask: 0o2000,
        key: "setgid",
    },
    ModeBit {
        mask: 0o1000,
        key: "sticky",
    },
];

/// What a checkbox shows for the selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Tristate {
    On,
    Off,
    Varies,
}

/// Names the page shows for users and groups, and the current user's
/// identity and groups. Loading them may ask a directory service, so it runs
/// off the UI thread, with the snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Accounts {
    effective_user: u32,
    user_groups: Vec<(u32, String)>,
    users: BTreeMap<u32, String>,
    groups: BTreeMap<u32, String>,
}

impl Accounts {
    /// Looks up the names of every owner, group and named ACL entry in
    /// `snapshot` and the current user's groups.
    #[must_use]
    pub fn load(snapshot: &PropertySnapshot) -> Self {
        let mut users = BTreeMap::new();
        let mut groups = BTreeMap::new();
        let mut add_user = |uid: u32| {
            users
                .entry(uid)
                .or_insert_with(|| musheen_desktop::user_name(uid));
        };
        let mut add_group = |gid: u32| {
            groups
                .entry(gid)
                .or_insert_with(|| musheen_desktop::group_name(gid));
        };
        for item in snapshot.items() {
            let permissions = item.permissions();
            add_user(permissions.owner());
            add_group(permissions.group());
            let acls = std::iter::once(permissions.acl()).chain(permissions.default_acl());
            for acl in acls {
                if let AclState::Available(entries) = acl {
                    for entry in entries {
                        match entry.qualifier() {
                            AclQualifier::User(uid) => add_user(*uid),
                            AclQualifier::Group(gid) => add_group(*gid),
                            _ => {}
                        }
                    }
                }
            }
        }
        let user_groups = musheen_desktop::current_user_groups();
        Self {
            effective_user: musheen_desktop::effective_user(),
            users: users
                .into_iter()
                .filter_map(|(uid, name)| Some((uid, name?)))
                .collect(),
            groups: groups
                .into_iter()
                .filter_map(|(gid, name)| Some((gid, name?)))
                .chain(user_groups.iter().cloned())
                .collect(),
            user_groups,
        }
    }

    /// The name of user `uid`, or its number.
    #[must_use]
    pub fn user_name(&self, uid: u32) -> String {
        self.users
            .get(&uid)
            .cloned()
            .unwrap_or_else(|| uid.to_string())
    }

    /// The name of group `gid`, or its number.
    #[must_use]
    pub fn group_name(&self, gid: u32) -> String {
        self.groups
            .get(&gid)
            .cloned()
            .unwrap_or_else(|| gid.to_string())
    }

    /// The current user's groups, primary group first.
    #[must_use]
    pub fn user_groups(&self) -> &[(u32, String)] {
        &self.user_groups
    }
}

/// One selected item as the page sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ItemMode {
    kind: MetadataEntryKind,
    mode: u32,
    owner: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionsPageModel {
    owner: AggregateValue<u32>,
    group: AggregateValue<u32>,
    mode: AggregateValue<u32>,
    items: Vec<ItemMode>,
    accounts: Accounts,
    /// Why the page may not change anything, when it may not.
    read_only: Option<Box<str>>,
    access: [Option<Access>; 3],
    executable: Option<bool>,
    bits: BTreeMap<u32, bool>,
    group_edit: Option<u32>,
    scope: MetadataScope,
}

impl PermissionsPageModel {
    /// The page for `snapshot`, with `accounts` for its names. `capability`
    /// is the filesystem's permission support: anything but supported makes
    /// the page read-only, with the reason.
    pub(crate) fn from_snapshot(
        snapshot: &PropertySnapshot,
        accounts: Accounts,
        capability: &CapabilityState,
    ) -> Self {
        let items = snapshot
            .items()
            .iter()
            .filter_map(|item| {
                let kind = match item.kind() {
                    ItemKind::RegularFile => MetadataEntryKind::File,
                    ItemKind::Directory => MetadataEntryKind::Directory,
                    _ => return None,
                };
                Some(ItemMode {
                    kind,
                    mode: item.permissions().mode() & 0o7777,
                    owner: item.permissions().owner(),
                })
            })
            .collect();
        let read_only = match capability {
            CapabilityState::Supported => None,
            CapabilityState::Unsupported(reason) | CapabilityState::Unknown(reason) => {
                Some(reason.as_str().into())
            }
        };
        Self {
            owner: snapshot.aggregate().owner(),
            group: snapshot.aggregate().group(),
            mode: snapshot.aggregate().mode(),
            items,
            accounts,
            read_only,
            access: [None; 3],
            executable: None,
            bits: BTreeMap::new(),
            group_edit: None,
            scope: MetadataScope::Single,
        }
    }

    pub fn owner(&self) -> &AggregateValue<u32> {
        &self.owner
    }

    pub fn group(&self) -> &AggregateValue<u32> {
        &self.group
    }

    pub fn mode(&self) -> &AggregateValue<u32> {
        &self.mode
    }

    #[must_use]
    pub fn accounts(&self) -> &Accounts {
        &self.accounts
    }

    /// Why the page may not change anything, or `None` when it may.
    #[must_use]
    pub fn read_only_reason(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// Whether every selected item is a folder, so the access choices use
    /// the folder names.
    #[must_use]
    pub fn only_folders(&self) -> bool {
        !self.items.is_empty()
            && self
                .items
                .iter()
                .all(|item| item.kind == MetadataEntryKind::Directory)
    }

    /// Whether the selection holds a file, so the executable checkbox shows.
    #[must_use]
    pub fn has_files(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.kind == MetadataEntryKind::File)
    }

    /// The edit to each item's mode that the page's choices make.
    #[must_use]
    pub fn mode_edit(&self) -> ModeEdit {
        let mut edit = ModeEdit {
            file_execute: self.executable,
            ..ModeEdit::default()
        };
        for class in AccessClass::ALL {
            if let Some(access) = self.access[class.index()] {
                // No Access on a file also takes execute away; the other
                // choices keep it, as the executable checkbox sets it.
                let file_clear = if access == Access::None { 0o7 } else { 0o6 };
                edit.file_clear |= file_clear << class.shift();
                edit.file_set |= access.file_bits() << class.shift();
                edit.directory_clear |= 0o7 << class.shift();
                edit.directory_set |= access.folder_bits() << class.shift();
            }
        }
        for (mask, on) in &self.bits {
            if *on {
                edit.bits_set |= mask;
            } else {
                edit.bits_clear |= mask;
            }
        }
        edit
    }

    /// Each item's mode as Apply would leave it.
    fn projected(&self) -> impl Iterator<Item = (MetadataEntryKind, u32)> + '_ {
        let edit = self.mode_edit();
        self.items
            .iter()
            .map(move |item| (item.kind, edit.apply(item.kind, item.mode)))
    }

    /// The access `class` has across the selection, or `None` when the
    /// items differ or their bits match no choice (Varies).
    #[must_use]
    pub fn access(&self, class: AccessClass) -> Option<Access> {
        let mut shown = None;
        for (kind, mode) in self.projected() {
            let access = Access::of(kind, (mode >> class.shift()) & 0o7)?;
            if shown.is_some_and(|shown| shown != access) {
                return None;
            }
            shown = Some(access);
        }
        shown
    }

    pub fn set_access(&mut self, class: AccessClass, access: Access) {
        if self.read_only.is_none() {
            self.access[class.index()] = Some(access);
        }
    }

    /// Whether the selected files may be executed, across the selection.
    #[must_use]
    pub fn executable(&self) -> Tristate {
        let modes = self
            .projected()
            .filter(|(kind, _)| *kind == MetadataEntryKind::File)
            .map(|(_, mode)| mode & 0o111 != 0);
        tristate(modes)
    }

    /// Checks the executable checkbox, or clears it when it is checked.
    pub fn toggle_executable(&mut self) {
        if self.read_only.is_none() {
            self.executable = Some(self.executable() != Tristate::On);
        }
    }

    /// Whether `bit` is set across the selection.
    #[must_use]
    pub fn bit(&self, bit: u32) -> Tristate {
        tristate(self.projected().map(|(_, mode)| mode & bit != 0))
    }

    /// Sets `bit`, or clears it when it is set.
    pub fn toggle_bit(&mut self, bit: u32) {
        if self.read_only.is_none() {
            let on = self.bit(bit) != Tristate::On;
            self.bits.insert(bit, on);
        }
    }

    /// Sets every mode bit to `mode`, as one explicit edit.
    pub fn set_file_mode(&mut self, mode: u32) {
        for bit in MODE_BITS {
            self.bits.insert(bit.mask, mode & bit.mask != 0);
        }
    }

    /// The group Apply would leave, when the selection shares one.
    #[must_use]
    pub fn shown_group(&self) -> Option<u32> {
        match (self.group_edit, &self.group) {
            (Some(group), _) => Some(group),
            (None, AggregateValue::Same(group)) => Some(*group),
            _ => None,
        }
    }

    /// Whether the user may choose the group: only on items they own.
    #[must_use]
    pub fn group_editable(&self) -> bool {
        self.read_only.is_none()
            && !self.items.is_empty()
            && self
                .items
                .iter()
                .all(|item| item.owner == self.accounts.effective_user)
    }

    /// Chooses group `gid`, when it is one of the user's groups.
    pub fn set_group(&mut self, gid: u32) {
        if self.group_editable()
            && self
                .accounts
                .user_groups
                .iter()
                .any(|(group, _)| *group == gid)
        {
            self.group_edit = (self.group != AggregateValue::Same(gid)).then_some(gid);
        }
    }

    pub fn set_single(&mut self) {
        self.scope = MetadataScope::Single;
    }

    pub fn set_recursive(&mut self, include_nested_mounts: bool) {
        self.scope = MetadataScope::recursive(include_nested_mounts, false);
    }

    pub fn review_recursive_scope(&mut self) {
        self.scope = self.scope.reviewed();
    }

    pub fn is_dirty(&self) -> bool {
        self.change().is_dirty()
    }

    pub fn is_valid(&self) -> bool {
        self.read_only.is_none() && self.scope.is_reviewed()
    }

    /// The change Apply submits.
    #[must_use]
    pub fn change(&self) -> MetadataChange {
        let change = MetadataChange::new().with_mode_edit(self.mode_edit());
        match self.group_edit {
            Some(group) => change.with_group(group),
            None => change,
        }
    }

    pub fn scope(&self) -> MetadataScope {
        self.scope
    }
}

fn tristate(values: impl Iterator<Item = bool>) -> Tristate {
    let mut shown = None;
    for value in values {
        match shown {
            None => shown = Some(value),
            Some(shown) if shown != value => return Tristate::Varies,
            Some(_) => {}
        }
    }
    match shown {
        Some(true) => Tristate::On,
        Some(false) | None => Tristate::Off,
    }
}
