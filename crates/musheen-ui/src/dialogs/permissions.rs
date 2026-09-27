//! The Properties window's Permissions page (SEARCH-019): access choices as
//! Dolphin names them, the executable checkbox, the group, and the mode
//! bits. Every value the page shows is what Apply would leave: each selected
//! item's mode with the user's edits applied to it.

use musheen_core::{CapabilityState, ItemKind};
use musheen_desktop::{AclQualifier, AclState, AggregateValue, PropertySnapshot};
use musheen_ops::{
    MetadataChange, MetadataEntryKind, MetadataScope, ModeEdit, ModeStep, executes_where_readable,
    mode_after_ownership_change,
};
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
    /// Every user account and every group the system lists, sorted by
    /// name, for the owner and group choosers (SEARCH-019).
    all_users: Vec<(u32, String)>,
    all_groups: Vec<(u32, String)>,
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
        let all_users = musheen_desktop::all_users();
        let all_groups = musheen_desktop::all_groups();
        Self {
            effective_user: musheen_desktop::effective_user(),
            users: users
                .into_iter()
                .filter_map(|(uid, name)| Some((uid, name?)))
                .chain(all_users.iter().cloned())
                .collect(),
            groups: groups
                .into_iter()
                .filter_map(|(gid, name)| Some((gid, name?)))
                .chain(user_groups.iter().cloned())
                .chain(all_groups.iter().cloned())
                .collect(),
            user_groups,
            all_users,
            all_groups,
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

    /// Loads the names for `snapshot` again, keeping the lists of every
    /// account and group and the user's groups: a refresh that sees a
    /// metadata change does not ask the account database for them again.
    #[must_use]
    pub fn reload_for(&self, snapshot: &PropertySnapshot) -> Self {
        let mut accounts = self.clone();
        for item in snapshot.items() {
            let permissions = item.permissions();
            let owner = permissions.owner();
            if let std::collections::btree_map::Entry::Vacant(entry) = accounts.users.entry(owner)
                && let Some(name) = musheen_desktop::user_name(owner)
            {
                entry.insert(name);
            }
            let group = permissions.group();
            if let std::collections::btree_map::Entry::Vacant(entry) = accounts.groups.entry(group)
                && let Some(name) = musheen_desktop::group_name(group)
            {
                entry.insert(name);
            }
        }
        accounts
    }

    /// Accounts for a test: `effective_user`, their groups, and every user
    /// and group listed.
    #[cfg(test)]
    pub(crate) fn fixed(
        effective_user: u32,
        user_groups: Vec<(u32, String)>,
        all_users: Vec<(u32, String)>,
        all_groups: Vec<(u32, String)>,
    ) -> Self {
        Self {
            effective_user,
            users: all_users.iter().cloned().collect(),
            groups: all_groups.iter().cloned().collect(),
            user_groups,
            all_users,
            all_groups,
        }
    }

    /// Every user account the system lists, by name.
    #[must_use]
    pub fn all_users(&self) -> &[(u32, String)] {
        &self.all_users
    }

    /// Every group the system lists, by name.
    #[must_use]
    pub fn all_groups(&self) -> &[(u32, String)] {
        &self.all_groups
    }

    fn is_user_group(&self, gid: u32) -> bool {
        self.user_groups.iter().any(|(group, _)| *group == gid)
    }
}

/// One selected file or folder as the page sees it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ItemMode {
    kind: MetadataEntryKind,
    mode: u32,
    owner: u32,
    group: u32,
}

/// One change the user made, kept in the order made so a later change wins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Access(AccessClass, Access),
    Executable(bool),
    Bit(u32, bool),
}

impl Step {
    /// Whether `self` and `other` set the same control, so the later one
    /// replaces the earlier.
    fn same_control(self, other: Self) -> bool {
        match (self, other) {
            (Self::Access(first, _), Self::Access(second, _)) => first == second,
            (Self::Executable(_), Self::Executable(_)) => true,
            (Self::Bit(first, _), Self::Bit(second, _)) => first == second,
            _ => false,
        }
    }
}

/// Why the page may not change the selection's modes, though the
/// filesystem supports them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeLock {
    /// A selected file or folder belongs to another user.
    NotOwner,
    /// The selection holds no file or folder; a link has no mode of its own.
    NoModes,
}

impl ModeLock {
    /// The message key of the reason the page shows.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::NotOwner => "permissions-not-owner",
            Self::NoModes => "permissions-no-modes",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PermissionsPageModel {
    owner: AggregateValue<u32>,
    group: AggregateValue<u32>,
    mode: AggregateValue<u32>,
    items: Vec<ItemMode>,
    /// The owner of every selected item, links and special files included.
    owners: Vec<u32>,
    /// Whether the selection holds a socket, pipe or device, which Apply
    /// leaves as it is.
    special: bool,
    /// Whether a selected item has ACL entries for named users or groups.
    named_acl: bool,
    accounts: Accounts,
    /// Why the filesystem lets the page change nothing, when it does not.
    read_only: Option<Box<str>>,
    mode_lock: Option<ModeLock>,
    steps: Vec<Step>,
    owner_edit: Option<u32>,
    group_edit: Option<u32>,
    scope: MetadataScope,
    /// Whether every selected item is on a local filesystem with POSIX
    /// ownership, where the broker makes owner and group changes (SYS-037).
    admin_ownership: bool,
}

/// The owner and group change Apply makes as administrator (SYS-037);
/// `None` keeps one as it is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OwnershipEdit {
    pub owner: Option<u32>,
    pub group: Option<u32>,
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
        let items: Vec<ItemMode> = snapshot
            .items()
            .iter()
            .filter_map(|item| {
                let kind = match item.kind() {
                    ItemKind::RegularFile => MetadataEntryKind::File,
                    ItemKind::Directory => MetadataEntryKind::Directory,
                    ItemKind::SymbolicLink | ItemKind::Other => return None,
                };
                Some(ItemMode {
                    kind,
                    mode: item.permissions().mode() & 0o7777,
                    owner: item.permissions().owner(),
                    group: item.permissions().group(),
                })
            })
            .collect();
        let owners = snapshot
            .items()
            .iter()
            .map(|item| item.permissions().owner())
            .collect();
        let special = snapshot
            .items()
            .iter()
            .any(|item| item.kind() == ItemKind::Other);
        let named_acl = snapshot.items().iter().any(|item| {
            let permissions = item.permissions();
            std::iter::once(permissions.acl())
                .chain(permissions.default_acl())
                .any(|acl| match acl {
                    AclState::Available(entries) => entries.iter().any(|entry| {
                        matches!(
                            entry.qualifier(),
                            AclQualifier::User(_) | AclQualifier::Group(_)
                        )
                    }),
                    AclState::Unsupported(_) | AclState::Unavailable(_) => false,
                })
        });
        let read_only = match capability {
            CapabilityState::Supported => None,
            CapabilityState::Unsupported(reason) | CapabilityState::Unknown(reason) => {
                Some(reason.as_str().into())
            }
        };
        // Only an item's owner may change its mode; the superuser may
        // change any.
        let mode_lock = if items.is_empty() {
            Some(ModeLock::NoModes)
        } else if accounts.effective_user != 0
            && items
                .iter()
                .any(|item| item.owner != accounts.effective_user)
        {
            Some(ModeLock::NotOwner)
        } else {
            None
        };
        Self {
            owner: snapshot.aggregate().owner(),
            group: snapshot.aggregate().group(),
            mode: snapshot.aggregate().mode(),
            items,
            owners,
            special,
            named_acl,
            accounts,
            read_only,
            mode_lock,
            steps: Vec::new(),
            owner_edit: None,
            group_edit: None,
            scope: MetadataScope::Single,
            admin_ownership: true,
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

    /// Why the filesystem lets the page change nothing, or `None` when it
    /// may.
    #[must_use]
    pub fn read_only_reason(&self) -> Option<&str> {
        self.read_only.as_deref()
    }

    /// Why the access choices, the checkbox and the bits are disabled
    /// though the filesystem supports them.
    #[must_use]
    pub fn mode_lock(&self) -> Option<ModeLock> {
        self.mode_lock
    }

    /// Whether the access choices, the checkbox and the bits may change.
    #[must_use]
    pub fn modes_editable(&self) -> bool {
        self.read_only.is_none() && self.mode_lock.is_none()
    }

    /// Whether the selection holds a socket, pipe or device.
    #[must_use]
    pub fn has_special_items(&self) -> bool {
        self.special
    }

    /// Whether a selected item has ACL entries for named users or groups,
    /// so the Group row sets their mask.
    #[must_use]
    pub fn has_named_acl(&self) -> bool {
        self.named_acl
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

    fn has_folders(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.kind == MetadataEntryKind::Directory)
    }

    /// The edit `steps` make. An Advanced bit reaches only the kinds of
    /// item the selection holds, so a bit chosen on a folder does not reach
    /// the files Apply to contents covers.
    fn edit_of(&self, steps: &[Step]) -> ModeEdit {
        let (files, folders) = (self.has_files(), self.has_folders());
        ModeEdit::new(
            steps
                .iter()
                .map(|step| match *step {
                    Step::Access(class, access) => ModeStep::Access {
                        shift: class.shift(),
                        file_bits: access.file_bits(),
                        folder_bits: access.folder_bits(),
                    },
                    Step::Executable(on) => ModeStep::Executable(on),
                    Step::Bit(mask, on) => ModeStep::Bit {
                        mask,
                        on,
                        files,
                        folders,
                    },
                })
                .collect(),
        )
    }

    /// The edit to each item's mode that the page's choices make.
    #[must_use]
    pub fn mode_edit(&self) -> ModeEdit {
        self.edit_of(&self.steps)
    }

    /// Each item's mode as Apply would leave it with `steps`, including the
    /// bits the kernel clears when the group changes.
    fn projected_with(&self, steps: &[Step]) -> Vec<(MetadataEntryKind, u32)> {
        let edit = self.edit_of(steps);
        self.items
            .iter()
            .map(|item| {
                let ownership_changes = self.group_edit.is_some_and(|group| group != item.group)
                    || self.owner_edit.is_some_and(|owner| owner != item.owner);
                // Apply changes the mode first and the owner and group after
                // (SEARCH-019); the kernel then clears setuid and setgid bits
                // where the owner or group changes.
                let mode = edit.apply(item.kind, item.mode);
                let mode = if ownership_changes {
                    mode_after_ownership_change(item.kind, mode)
                } else {
                    mode
                };
                (item.kind, mode)
            })
            .collect()
    }

    /// The steps without the one that sets the same control as `step`.
    fn steps_without(&self, step: Step) -> Vec<Step> {
        self.steps
            .iter()
            .copied()
            .filter(|kept| !kept.same_control(step))
            .collect()
    }

    /// Records `step` as the latest change, replacing an earlier one to the
    /// same control.
    fn record(&mut self, step: Step) {
        self.steps.retain(|kept| !kept.same_control(step));
        self.steps.push(step);
    }

    fn access_with(&self, steps: &[Step], class: AccessClass) -> Option<Access> {
        let mut shown = None;
        for (kind, mode) in self.projected_with(steps) {
            let access = Access::of(kind, (mode >> class.shift()) & 0o7)?;
            if shown.is_some_and(|shown| shown != access) {
                return None;
            }
            shown = Some(access);
        }
        shown
    }

    /// The access `class` has across the selection, or `None` when the
    /// items differ or their bits match no choice (Varies).
    #[must_use]
    pub fn access(&self, class: AccessClass) -> Option<Access> {
        self.access_with(&self.steps, class)
    }

    /// Whether `class` would show Varies without the user's choice for it,
    /// so the page offers Varies to go back to leaving it as it is.
    #[must_use]
    pub fn access_varies_unchanged(&self, class: AccessClass) -> bool {
        self.access_with(
            &self.steps_without(Step::Access(class, Access::None)),
            class,
        )
        .is_none()
    }

    /// Whether the user chose an access for `class`.
    #[must_use]
    pub fn access_chosen(&self, class: AccessClass) -> bool {
        self.steps
            .iter()
            .any(|step| matches!(step, Step::Access(chosen, _) if *chosen == class))
    }

    pub fn set_access(&mut self, class: AccessClass, access: Access) {
        if self.modes_editable() {
            self.record(Step::Access(class, access));
        }
    }

    /// Drops the user's choice for `class`, leaving each item's bits as
    /// they are.
    pub fn clear_access(&mut self, class: AccessClass) {
        self.steps
            .retain(|step| !matches!(step, Step::Access(chosen, _) if *chosen == class));
    }

    fn executable_with(&self, steps: &[Step]) -> Tristate {
        let mut shown = None;
        for (kind, mode) in self.projected_with(steps) {
            if kind != MetadataEntryKind::File {
                continue;
            }
            let state = if mode & 0o111 == 0 {
                Tristate::Off
            } else if executes_where_readable(mode) {
                Tristate::On
            } else {
                return Tristate::Varies;
            };
            if shown.is_some_and(|shown| shown != state) {
                return Tristate::Varies;
            }
            shown = Some(state);
        }
        shown.unwrap_or(Tristate::Off)
    }

    /// Whether the selected files may be executed by each class that may
    /// read them (On), by none (Off), or neither across the selection
    /// (Varies).
    #[must_use]
    pub fn executable(&self) -> Tristate {
        self.executable_with(&self.steps)
    }

    /// Checks the executable checkbox, or clears it when it is checked.
    /// From Varies it goes to checked, cleared, and back to Varies.
    pub fn toggle_executable(&mut self) {
        if self.modes_editable() {
            let control = Step::Executable(true);
            let unchanged = self.executable_with(&self.steps_without(control));
            let next = next_toggle(self.executable(), unchanged, self.chosen(control));
            self.apply_toggle(control, next.map(Step::Executable));
        }
    }

    fn bit_with(&self, steps: &[Step], bit: u32) -> Tristate {
        tristate(
            self.projected_with(steps)
                .into_iter()
                .map(|(_, mode)| mode & bit != 0),
        )
    }

    /// Whether `bit` is set across the selection.
    #[must_use]
    pub fn bit(&self, bit: u32) -> Tristate {
        self.bit_with(&self.steps, bit)
    }

    /// Sets `bit`, or clears it when it is set. From Varies it goes to set,
    /// cleared, and back to Varies.
    pub fn toggle_bit(&mut self, bit: u32) {
        if self.modes_editable() {
            let control = Step::Bit(bit, true);
            let unchanged = self.bit_with(&self.steps_without(control), bit);
            let next = next_toggle(self.bit(bit), unchanged, self.chosen(control));
            self.apply_toggle(control, next.map(|on| Step::Bit(bit, on)));
        }
    }

    /// The value the user chose for the control `step` sets, if any.
    fn chosen(&self, step: Step) -> Option<bool> {
        self.steps.iter().find_map(|kept| match *kept {
            Step::Executable(on) | Step::Bit(_, on) if kept.same_control(step) => Some(on),
            _ => None,
        })
    }

    fn apply_toggle(&mut self, control: Step, next: Option<Step>) {
        match next {
            Some(step) => self.record(step),
            None => self.steps.retain(|kept| !kept.same_control(control)),
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

    /// The owner the page shows: the chosen one, or the one every item has.
    #[must_use]
    pub fn shown_owner(&self) -> Option<u32> {
        match (self.owner_edit, &self.owner) {
            (Some(owner), _) => Some(owner),
            (None, AggregateValue::Same(owner)) => Some(*owner),
            _ => None,
        }
    }

    /// Marks whether the selected items are on filesystems where the broker
    /// makes owner and group changes as administrator (SYS-037).
    #[must_use]
    pub fn with_admin_ownership(mut self, supported: bool) -> Self {
        self.admin_ownership = supported;
        self
    }

    /// Whether changes the user may not make alone can be made here: as the
    /// superuser, or through the broker on a local filesystem with POSIX
    /// ownership.
    #[must_use]
    pub fn admin_changes_available(&self) -> bool {
        self.accounts.effective_user == 0 || self.admin_ownership
    }

    fn owns_every_item(&self) -> bool {
        self.owners
            .iter()
            .all(|owner| *owner == self.accounts.effective_user)
    }

    /// Whether the page offers the group chooser: on every filesystem with
    /// POSIX permissions, links and special files included. A change the
    /// user may not make alone applies as administrator; where that is not
    /// available, only the owner of every item may choose among their own
    /// groups.
    #[must_use]
    pub fn group_editable(&self) -> bool {
        self.read_only.is_none()
            && !self.owners.is_empty()
            && (self.admin_changes_available() || self.owns_every_item())
    }

    /// Whether the owner may be chosen: where changes the user may not make
    /// alone are available.
    #[must_use]
    pub fn owner_editable(&self) -> bool {
        self.read_only.is_none() && !self.owners.is_empty() && self.admin_changes_available()
    }

    /// Chooses owner `uid`, one of the system's user accounts.
    pub fn set_owner(&mut self, uid: u32) {
        if self.owner_editable() && self.accounts.all_users.iter().any(|(user, _)| *user == uid) {
            self.owner_edit = (self.owner != AggregateValue::Same(uid)).then_some(uid);
        }
    }

    /// Chooses group `gid`: any of the system's groups where changes the
    /// user may not make alone are available, else one of the user's own.
    pub fn set_group(&mut self, gid: u32) {
        let offered = self.accounts.is_user_group(gid)
            || (self.admin_changes_available()
                && self
                    .accounts
                    .all_groups
                    .iter()
                    .any(|(group, _)| *group == gid));
        if self.group_editable() && offered {
            self.group_edit = (self.group != AggregateValue::Same(gid)).then_some(gid);
        }
    }

    /// The groups the chooser offers: every group, or the user's own where
    /// changes the user may not make alone are not available.
    #[must_use]
    pub fn group_choices(&self) -> &[(u32, String)] {
        if self.admin_changes_available() {
            &self.accounts.all_groups
        } else {
            &self.accounts.user_groups
        }
    }

    /// Whether Apply needs administrator rights (SEARCH-019, SYS-037): a
    /// new owner, or a group that is not one of the user's own or is for an
    /// item the user does not own. The superuser needs none.
    #[must_use]
    pub fn needs_administrator(&self) -> bool {
        if self.accounts.effective_user == 0 {
            return false;
        }
        self.owner_edit.is_some()
            || self.group_edit.is_some_and(|gid| {
                !self.accounts.is_user_group(gid)
                    || self
                        .owners
                        .iter()
                        .any(|owner| *owner != self.accounts.effective_user)
            })
    }

    /// The owner and group change Apply makes as administrator, after the
    /// user's own mode change.
    #[must_use]
    pub fn ownership_edit(&self) -> Option<OwnershipEdit> {
        self.needs_administrator().then_some(OwnershipEdit {
            owner: self.owner_edit,
            group: self.group_edit,
        })
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

    /// Whether Apply would change anything: for the selected items alone,
    /// whether a mode or the group would differ; with Apply to contents,
    /// whether the user made any change, as the contents may differ.
    pub fn is_dirty(&self) -> bool {
        if self.group_edit.is_some() || self.owner_edit.is_some() {
            return true;
        }
        if self.scope.is_recursive() {
            return !self.steps.is_empty();
        }
        self.projected_with(&self.steps)
            .iter()
            .zip(&self.items)
            .any(|((_, mode), item)| *mode != item.mode)
    }

    pub fn is_valid(&self) -> bool {
        self.read_only.is_none() && self.scope.is_reviewed()
    }

    /// The change Apply submits as the user: the modes, and the group when
    /// the user may set it alone. The rest goes in [`Self::ownership_edit`].
    #[must_use]
    pub fn change(&self) -> MetadataChange {
        let change = MetadataChange::new().with_mode_edit(self.mode_edit());
        if self.needs_administrator() {
            return change;
        }
        // Without administrator rights the group is one the user may set,
        // and an owner is chosen only by the superuser.
        let change = match self.owner_edit {
            Some(owner) => change.with_owner(owner),
            None => change,
        };
        match self.group_edit {
            Some(group) => change.with_group(group),
            None => change,
        }
    }

    pub fn scope(&self) -> MetadataScope {
        self.scope
    }
}

/// The value a toggle showing `shown` takes next, or `None` to drop the
/// user's change and show `unchanged` again. `chosen` is the value the user
/// chose, if any. From Varies the toggle goes to on, off and back to Varies;
/// otherwise a second click gives back what the items have.
fn next_toggle(shown: Tristate, unchanged: Tristate, chosen: Option<bool>) -> Option<bool> {
    let next = shown != Tristate::On;
    match (unchanged, chosen) {
        (Tristate::Varies, Some(false)) => None,
        (Tristate::Varies, _) | (Tristate::On | Tristate::Off, None) => Some(next),
        (unchanged, Some(_)) if (unchanged == Tristate::On) == next => None,
        (_, Some(_)) => Some(next),
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
