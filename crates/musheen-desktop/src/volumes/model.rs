use super::{Capacity, MountRecord};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct VolumeId(Box<str>);

impl VolumeId {
    pub fn new(value: impl Into<Box<str>>) -> Result<Self, VolumeIdError> {
        let value = value.into();
        if value.is_empty() || value.contains('\0') {
            return Err(VolumeIdError);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn from_mount(record: &MountRecord) -> Self {
        use std::os::unix::ffi::OsStrExt;

        let mut identity = record.source().as_os_str().as_bytes().to_vec();
        // Device-backed bind mounts share one device identity. Anonymous
        // sources such as tmpfs and overlay are not identities at all, so the
        // first observation also includes the destination to prevent merging.
        // VolumeService reconciles those generated IDs one-to-one on remount.
        if !record.source().starts_with("/dev/") {
            identity.push(0);
            identity.extend_from_slice(record.filesystem_type().as_bytes());
            identity.push(0);
            identity.extend_from_slice(record.destination().as_os_str().as_bytes());
        }
        let bytes = identity;
        let mut encoded = String::with_capacity(bytes.len() * 2 + 6);
        encoded.push_str("mount-");
        for byte in bytes {
            use std::fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
        }
        Self(encoded.into())
    }

    pub(crate) fn from_mount_occurrence(record: &MountRecord, occurrence: usize) -> Self {
        let base = Self::from_mount(record);
        Self(format!("{}-{occurrence:x}", base.as_str()).into())
    }
}

impl fmt::Display for VolumeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VolumeIdError;

impl fmt::Display for VolumeIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a volume ID must be non-empty and contain no NUL")
    }
}

impl std::error::Error for VolumeIdError {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VolumeCapabilities {
    pub can_mount: bool,
    pub can_unmount: bool,
    pub can_eject: bool,
    pub can_unlock: bool,
    pub can_power_off: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceDescriptor {
    id: VolumeId,
    object_path: Box<str>,
    drive_path: Option<Box<str>>,
    label: Box<str>,
    device: PathBuf,
    mount_points: Vec<PathBuf>,
    capabilities: VolumeCapabilities,
    read_only: bool,
    locked: bool,
    size_bytes: Option<u64>,
}

impl DeviceDescriptor {
    #[must_use]
    pub fn new(id: VolumeId, object_path: impl Into<Box<str>>) -> Self {
        let label: Box<str> = id.as_str().into();
        Self {
            id,
            object_path: object_path.into(),
            drive_path: None,
            label,
            device: PathBuf::new(),
            mount_points: Vec::new(),
            capabilities: VolumeCapabilities::default(),
            read_only: false,
            locked: false,
            size_bytes: None,
        }
    }

    #[must_use]
    pub fn with_label(mut self, label: impl Into<Box<str>>) -> Self {
        self.label = label.into();
        self
    }

    #[must_use]
    pub fn with_device(mut self, device: impl Into<PathBuf>) -> Self {
        self.device = device.into();
        self
    }

    #[must_use]
    pub fn with_drive_path(mut self, drive_path: impl Into<Box<str>>) -> Self {
        self.drive_path = Some(drive_path.into());
        self
    }

    #[must_use]
    pub fn with_mount_points(mut self, points: impl IntoIterator<Item = PathBuf>) -> Self {
        self.mount_points = points.into_iter().collect();
        self.mount_points.sort();
        self.mount_points.dedup();
        self
    }

    #[must_use]
    pub const fn with_capabilities(
        mut self,
        can_mount: bool,
        can_unmount: bool,
        can_eject: bool,
        can_unlock: bool,
        can_power_off: bool,
    ) -> Self {
        self.capabilities = VolumeCapabilities {
            can_mount,
            can_unmount,
            can_eject,
            can_unlock,
            can_power_off,
        };
        self
    }

    #[must_use]
    pub const fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    #[must_use]
    pub const fn with_locked(mut self, locked: bool) -> Self {
        self.locked = locked;
        self
    }

    #[must_use]
    pub const fn with_size_bytes(mut self, size_bytes: u64) -> Self {
        self.size_bytes = Some(size_bytes);
        self
    }

    #[must_use]
    pub const fn id(&self) -> &VolumeId {
        &self.id
    }

    #[must_use]
    pub fn object_path(&self) -> &str {
        &self.object_path
    }

    #[must_use]
    pub fn drive_path(&self) -> Option<&str> {
        self.drive_path.as_deref()
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn device(&self) -> &Path {
        &self.device
    }

    #[must_use]
    pub fn mount_points(&self) -> &[PathBuf] {
        &self.mount_points
    }

    #[must_use]
    pub const fn capabilities(&self) -> VolumeCapabilities {
        self.capabilities
    }

    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    #[must_use]
    pub const fn is_locked(&self) -> bool {
        self.locked
    }

    #[must_use]
    pub const fn size_bytes(&self) -> Option<u64> {
        self.size_bytes
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceState {
    Available,
    Unavailable,
    Slow,
    Disconnected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Volume {
    id: VolumeId,
    descriptor: Option<DeviceDescriptor>,
    label: Box<str>,
    device: PathBuf,
    mount_points: Vec<PathBuf>,
    filesystem_type: Option<Box<str>>,
    capacity: Option<Capacity>,
    read_only: bool,
    capabilities: VolumeCapabilities,
    service_state: ServiceState,
}

impl Volume {
    pub(crate) fn from_device(descriptor: DeviceDescriptor, state: ServiceState) -> Self {
        Self {
            id: descriptor.id().clone(),
            label: descriptor.label().into(),
            device: descriptor.device().to_path_buf(),
            // The D-Bus mount list is only a matching hint. `/proc/mounts` is
            // authoritative for the live mount projection.
            mount_points: Vec::new(),
            filesystem_type: None,
            capacity: descriptor.size_bytes().map(|size| Capacity::new(size, 0)),
            read_only: descriptor.is_read_only(),
            capabilities: descriptor.capabilities(),
            descriptor: Some(descriptor),
            service_state: state,
        }
    }

    pub(crate) fn from_mount(
        record: &MountRecord,
        capacity: Option<Capacity>,
        state: ServiceState,
    ) -> Self {
        Self::from_mount_with_id(VolumeId::from_mount(record), record, capacity, state)
    }

    pub(crate) fn from_mount_with_id(
        id: VolumeId,
        record: &MountRecord,
        capacity: Option<Capacity>,
        state: ServiceState,
    ) -> Self {
        let label = record
            .destination()
            .file_name()
            .filter(|name| !name.is_empty())
            .map_or_else(
                || {
                    record
                        .destination()
                        .as_os_str()
                        .to_string_lossy()
                        .into_owned()
                },
                |name| name.to_string_lossy().into_owned(),
            );
        Self {
            id,
            descriptor: None,
            label: label.into(),
            device: record.source().to_path_buf(),
            mount_points: vec![record.destination().to_path_buf()],
            filesystem_type: Some(record.filesystem_type().into()),
            capacity,
            read_only: record.is_read_only(),
            capabilities: VolumeCapabilities::default(),
            service_state: state,
        }
    }

    pub(crate) fn attach_mount(&mut self, record: &MountRecord, capacity: Option<Capacity>) {
        if !self
            .mount_points
            .contains(&record.destination().to_path_buf())
        {
            self.mount_points.push(record.destination().to_path_buf());
            self.mount_points.sort();
        }
        self.filesystem_type = Some(record.filesystem_type().into());
        self.read_only |= record.is_read_only();
        if capacity.is_some() {
            self.capacity = capacity;
        }
    }

    pub(crate) fn mark_service_state(&mut self, state: ServiceState) {
        self.service_state = state;
        if state != ServiceState::Available {
            self.capabilities = VolumeCapabilities::default();
        }
    }

    pub(crate) fn reconcile_mount_capabilities(&mut self) {
        if self.descriptor.is_none()
            || !(self.capabilities.can_mount || self.capabilities.can_unmount)
        {
            return;
        }
        let mounted = self.is_mounted();
        self.capabilities.can_mount = !mounted;
        self.capabilities.can_unmount = mounted;
    }

    #[must_use]
    pub const fn id(&self) -> &VolumeId {
        &self.id
    }

    #[must_use]
    pub fn descriptor(&self) -> Option<&DeviceDescriptor> {
        self.descriptor.as_ref()
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn device(&self) -> &Path {
        &self.device
    }

    #[must_use]
    pub fn mount_points(&self) -> &[PathBuf] {
        &self.mount_points
    }

    #[must_use]
    pub fn filesystem_type(&self) -> Option<&str> {
        self.filesystem_type.as_deref()
    }

    #[must_use]
    pub const fn capacity(&self) -> Option<Capacity> {
        self.capacity
    }

    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    #[must_use]
    pub fn is_mounted(&self) -> bool {
        !self.mount_points.is_empty()
    }

    #[must_use]
    pub const fn capabilities(&self) -> VolumeCapabilities {
        self.capabilities
    }

    #[must_use]
    pub const fn service_state(&self) -> ServiceState {
        self.service_state
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum VolumeChange {
    Mounts,
    Capacity,
    ReadOnly,
    Capabilities,
    Availability,
    Identity,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VolumeChanges(BTreeSet<VolumeChange>);

impl VolumeChanges {
    #[must_use]
    pub fn contains(&self, change: VolumeChange) -> bool {
        self.0.contains(&change)
    }

    fn between(old: &Volume, new: &Volume) -> Self {
        let mut changes = BTreeSet::new();
        if old.mount_points != new.mount_points {
            changes.insert(VolumeChange::Mounts);
        }
        if old.capacity != new.capacity {
            changes.insert(VolumeChange::Capacity);
        }
        if old.read_only != new.read_only {
            changes.insert(VolumeChange::ReadOnly);
        }
        if old.capabilities != new.capabilities {
            changes.insert(VolumeChange::Capabilities);
        }
        if old.service_state != new.service_state {
            changes.insert(VolumeChange::Availability);
        }
        if old.label != new.label
            || old.device != new.device
            || old.filesystem_type != new.filesystem_type
        {
            changes.insert(VolumeChange::Identity);
        }
        Self(changes)
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VolumeEvent {
    Added(VolumeId),
    Removed(VolumeId),
    Changed {
        id: VolumeId,
        changes: VolumeChanges,
    },
}

#[derive(Clone, Debug, Default)]
pub struct VolumeModel {
    volumes: BTreeMap<VolumeId, Volume>,
    service_owner: Option<Box<str>>,
    revision: u64,
}

impl VolumeModel {
    pub(crate) fn replace(
        &mut self,
        owner: Option<Box<str>>,
        volumes: impl IntoIterator<Item = Volume>,
    ) -> Result<Vec<VolumeEvent>, DuplicateVolumeId> {
        let mut replacement = BTreeMap::new();
        for volume in volumes {
            let id = volume.id().clone();
            if replacement.insert(id.clone(), volume).is_some() {
                return Err(DuplicateVolumeId(id));
            }
        }
        let mut events = Vec::new();
        for id in self.volumes.keys() {
            if !replacement.contains_key(id) {
                events.push(VolumeEvent::Removed(id.clone()));
            }
        }
        for (id, volume) in &replacement {
            match self.volumes.get(id) {
                None => events.push(VolumeEvent::Added(id.clone())),
                Some(previous) => {
                    let changes = VolumeChanges::between(previous, volume);
                    if !changes.is_empty() {
                        events.push(VolumeEvent::Changed {
                            id: id.clone(),
                            changes,
                        });
                    }
                }
            }
        }
        if !events.is_empty() || self.service_owner != owner {
            self.revision = self.revision.wrapping_add(1);
        }
        self.volumes = replacement;
        self.service_owner = owner;
        Ok(events)
    }

    #[must_use]
    pub fn volumes(&self) -> Vec<&Volume> {
        self.volumes.values().collect()
    }

    #[must_use]
    pub fn get(&self, id: &VolumeId) -> Option<&Volume> {
        self.volumes.get(id)
    }

    #[must_use]
    pub fn service_owner(&self) -> Option<&str> {
        self.service_owner.as_deref()
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DuplicateVolumeId(VolumeId);

impl fmt::Display for DuplicateVolumeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "duplicate volume identity {}", self.0)
    }
}
