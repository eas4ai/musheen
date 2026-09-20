use musheen_core::{ItemId, ProviderId, StorePath};
use musheen_desktop::{PinCatalog, PinState};
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
    identity: Option<ItemId>,
    tag_name: Option<Box<str>>,
    unavailable_reason: Option<Box<str>>,
}

impl SidebarEntry {
    #[must_use]
    pub fn new(label: impl Into<Box<str>>, location: StorePath) -> Self {
        Self {
            label: label.into(),
            location,
            identity: None,
            tag_name: None,
            unavailable_reason: None,
        }
    }

    #[must_use]
    pub fn unavailable(
        label: impl Into<Box<str>>,
        location: StorePath,
        reason: impl Into<Box<str>>,
    ) -> Self {
        Self {
            label: label.into(),
            location,
            identity: None,
            tag_name: None,
            unavailable_reason: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn pinned(
        item: ItemId,
        label: impl Into<Box<str>>,
        location: StorePath,
        unavailable_reason: Option<Box<str>>,
    ) -> Self {
        Self {
            label: label.into(),
            location,
            identity: Some(item),
            tag_name: None,
            unavailable_reason,
        }
    }

    #[must_use]
    pub fn tag(item: ItemId, label: impl Into<Box<str>>, location: StorePath) -> Self {
        let label = label.into();
        Self {
            tag_name: Some(label.clone()),
            label,
            location,
            identity: Some(item),
            unavailable_reason: None,
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
