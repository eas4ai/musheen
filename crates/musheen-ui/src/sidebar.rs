use musheen_core::{ItemId, ProviderId, StorePath};
use musheen_desktop::{
    Capacity, PinCatalog, PinState, Volume, VolumeCapabilities, VolumeId, VolumeModel,
};
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, RwLock};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SidebarSectionKind {
    Home,
    Places,
    Pinned,
    Mounts,
    Remote,
    Network,
    Tags,
}

impl SidebarSectionKind {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Home => "Home",
            Self::Places => "Places",
            Self::Pinned => "Pinned",
            Self::Mounts => "Storage",
            Self::Remote => "Remote",
            Self::Network => "Network",
            Self::Tags => "Tags",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidebarEntry {
    label: Box<str>,
    location: StorePath,
    navigation_location: StorePath,
    identity: Option<ItemId>,
    tag_name: Option<Box<str>>,
    unavailable_reason: Option<Box<str>>,
    volume_id: Option<VolumeId>,
    volume_capacity: Option<Capacity>,
    volume_read_only: bool,
    volume_capabilities: Option<VolumeCapabilities>,
}

impl SidebarEntry {
    #[must_use]
    pub fn new(label: impl Into<Box<str>>, location: StorePath) -> Self {
        let navigation_location = location.clone();
        Self {
            label: label.into(),
            location,
            navigation_location,
            identity: None,
            tag_name: None,
            unavailable_reason: None,
            volume_id: None,
            volume_capacity: None,
            volume_read_only: false,
            volume_capabilities: None,
        }
    }

    #[must_use]
    pub fn unavailable(
        label: impl Into<Box<str>>,
        location: StorePath,
        reason: impl Into<Box<str>>,
    ) -> Self {
        let navigation_location = location.clone();
        Self {
            label: label.into(),
            location,
            navigation_location,
            identity: None,
            tag_name: None,
            unavailable_reason: Some(reason.into()),
            volume_id: None,
            volume_capacity: None,
            volume_read_only: false,
            volume_capabilities: None,
        }
    }

    #[must_use]
    pub fn pinned(
        item: ItemId,
        label: impl Into<Box<str>>,
        location: StorePath,
        unavailable_reason: Option<Box<str>>,
    ) -> Self {
        let navigation_location = location.clone();
        Self {
            label: label.into(),
            location,
            navigation_location,
            identity: Some(item),
            tag_name: None,
            unavailable_reason,
            volume_id: None,
            volume_capacity: None,
            volume_read_only: false,
            volume_capabilities: None,
        }
    }

    #[must_use]
    pub fn tag(item: ItemId, label: impl Into<Box<str>>, location: StorePath) -> Self {
        let label = label.into();
        Self {
            tag_name: Some(label.clone()),
            label,
            navigation_location: location.clone(),
            location,
            identity: Some(item),
            unavailable_reason: None,
            volume_id: None,
            volume_capacity: None,
            volume_read_only: false,
            volume_capabilities: None,
        }
    }

    #[must_use]
    pub fn volume(volume: &Volume) -> Self {
        let provider =
            ProviderId::new("musheen.volume").expect("the built-in volume provider ID is valid");
        let key = volume.id().as_str().as_bytes().to_vec();
        let location = StorePath::from_provider_key(provider.clone(), key.clone())
            .expect("the volume ID is a valid provider key");
        let identity = ItemId::new(provider, key).expect("the volume ID is a valid item ID");
        let navigation_location = volume
            .mount_points()
            .first()
            .map(|path| StorePath::from_unix_path(path.as_os_str()))
            .unwrap_or_else(|| location.clone());
        Self {
            label: volume.label().into(),
            location,
            navigation_location,
            identity: Some(identity),
            tag_name: None,
            unavailable_reason: (!volume.is_mounted())
                .then(|| Box::<str>::from("mount the volume before opening it")),
            volume_id: Some(volume.id().clone()),
            volume_capacity: volume.capacity(),
            volume_read_only: volume.is_read_only(),
            volume_capabilities: Some(volume.capabilities()),
        }
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn location(&self) -> &StorePath {
        &self.location
    }

    #[must_use]
    pub fn navigation_location(&self) -> &StorePath {
        &self.navigation_location
    }

    #[must_use]
    pub fn identity(&self) -> Option<&ItemId> {
        self.identity.as_ref()
    }

    #[must_use]
    pub fn tag_name(&self) -> Option<&str> {
        self.tag_name.as_deref()
    }

    #[must_use]
    pub fn is_available(&self) -> bool {
        self.unavailable_reason.is_none()
    }

    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }

    #[must_use]
    pub const fn volume_id(&self) -> Option<&VolumeId> {
        self.volume_id.as_ref()
    }

    #[must_use]
    pub const fn volume_capacity(&self) -> Option<Capacity> {
        self.volume_capacity
    }

    #[must_use]
    pub const fn volume_read_only(&self) -> bool {
        self.volume_read_only
    }

    #[must_use]
    pub const fn volume_capabilities(&self) -> Option<VolumeCapabilities> {
        self.volume_capabilities
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SidebarSection {
    kind: SidebarSectionKind,
    items: Vec<SidebarEntry>,
}

impl SidebarSection {
    #[must_use]
    pub const fn kind(&self) -> SidebarSectionKind {
        self.kind
    }

    #[must_use]
    pub const fn label(&self) -> &'static str {
        self.kind.label()
    }

    #[must_use]
    pub fn items(&self) -> &[SidebarEntry] {
        &self.items
    }
}

#[derive(Clone, Debug, Default)]
pub struct PinStore(Arc<RwLock<Vec<SidebarEntry>>>);

impl PinStore {
    pub fn replace(&self, entries: impl IntoIterator<Item = SidebarEntry>) {
        *self.0.write().expect("pin store lock is not poisoned") = entries.into_iter().collect();
    }

    #[must_use]
    pub fn entries(&self) -> Vec<SidebarEntry> {
        self.0
            .read()
            .expect("pin store lock is not poisoned")
            .clone()
    }

    pub fn replace_catalog(&self, catalog: &PinCatalog) {
        self.replace(catalog.entries().iter().map(|pin| {
            let unavailable_reason = match pin.state() {
                PinState::Available => None,
                PinState::Unavailable(reason) => Some(reason.clone()),
            };
            SidebarEntry::pinned(
                pin.item().clone(),
                pin.label(),
                pin.path_hint().clone(),
                unavailable_reason,
            )
        }));
    }
}

#[derive(Clone, Debug)]
pub struct SidebarModel {
    pins: PinStore,
    sections: BTreeMap<SidebarSectionKind, Vec<SidebarEntry>>,
    expanded: HashSet<StorePath>,
    collapsed_sections: HashSet<SidebarSectionKind>,
}

impl SidebarModel {
    #[must_use]
    pub fn new(pins: PinStore) -> Self {
        Self {
            pins,
            sections: BTreeMap::new(),
            expanded: HashSet::new(),
            collapsed_sections: HashSet::new(),
        }
    }

    pub fn set_section_items(
        &mut self,
        kind: SidebarSectionKind,
        items: impl IntoIterator<Item = SidebarEntry>,
    ) {
        let items = items.into_iter().collect::<Vec<_>>();
        if kind == SidebarSectionKind::Pinned {
            self.pins.replace(items);
        } else if items.is_empty() {
            self.sections.remove(&kind);
        } else {
            self.sections.insert(kind, items);
        }
    }

    pub fn set_tag_names<'a>(&mut self, tags: impl IntoIterator<Item = &'a str>) {
        let provider =
            ProviderId::new("musheen-tag").expect("the built-in tag shortcut provider ID is valid");
        self.set_section_items(
            SidebarSectionKind::Tags,
            tags.into_iter().filter_map(|tag| {
                let key = tag.as_bytes().to_vec();
                let location = StorePath::from_provider_key(provider.clone(), key.clone()).ok()?;
                let item = ItemId::new(provider.clone(), key).ok()?;
                Some(SidebarEntry::tag(item, tag, location))
            }),
        );
    }

    pub fn sync_volumes(&mut self, volumes: &VolumeModel) {
        self.set_section_items(
            SidebarSectionKind::Mounts,
            volumes.volumes().into_iter().map(SidebarEntry::volume),
        );
    }

    #[must_use]
    pub fn sections(&self) -> Vec<SidebarSection> {
        let mut sections = self.sections.clone();
        let pins = self.pins.entries();
        if !pins.is_empty() {
            sections.insert(SidebarSectionKind::Pinned, pins);
        }
        sections
            .into_iter()
            .filter(|(_, items)| !items.is_empty())
            .map(|(kind, items)| SidebarSection { kind, items })
            .collect()
    }

    pub fn set_expanded(&mut self, location: StorePath, expanded: bool) {
        if expanded {
            self.expanded.insert(location);
        } else {
            self.expanded.remove(&location);
        }
    }

    #[must_use]
    pub fn is_expanded(&self, location: &StorePath) -> bool {
        self.expanded.contains(location)
    }

    pub fn set_section_collapsed(&mut self, kind: SidebarSectionKind, collapsed: bool) {
        if collapsed {
            self.collapsed_sections.insert(kind);
        } else {
            self.collapsed_sections.remove(&kind);
        }
    }

    #[must_use]
    pub fn is_section_collapsed(&self, kind: SidebarSectionKind) -> bool {
        self.collapsed_sections.contains(&kind)
    }
}

impl Default for SidebarModel {
    fn default() -> Self {
        Self::new(PinStore::default())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SidebarPlace {
    pub label: &'static str,
    pub icon_key: &'static str,
}

pub const PLACES: [SidebarPlace; 5] = [
    SidebarPlace {
        label: "Home",
        icon_key: "home",
    },
    SidebarPlace {
        label: "Desktop",
        icon_key: "folder",
    },
    SidebarPlace {
        label: "Documents",
        icon_key: "folder",
    },
    SidebarPlace {
        label: "Downloads",
        icon_key: "folder",
    },
    SidebarPlace {
        label: "Trash",
        icon_key: "trash",
    },
];
