use super::{FolderIdentity, PinCatalog, PinState, TagCatalog};
use musheen_core::{ItemId, StorePath};
use serde::{Deserialize, Serialize};

const MAX_RECENT_LOCATIONS: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecentLocation {
    identity: FolderIdentity,
    path_hint: StorePath,
    label: Box<str>,
}

impl RecentLocation {
    #[must_use]
    pub fn identity(&self) -> &FolderIdentity {
        &self.identity
    }

    #[must_use]
    pub fn path_hint(&self) -> &StorePath {
        &self.path_hint
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }
}

fn default_recording_enabled() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RecentLocations {
    #[serde(default = "default_recording_enabled")]
    recording_enabled: bool,
    entries: Vec<RecentLocation>,
}

impl RecentLocations {
    pub fn set_recording_enabled(&mut self, enabled: bool) {
        self.recording_enabled = enabled;
    }

    #[must_use]
    pub const fn recording_enabled(&self) -> bool {
        self.recording_enabled
    }

    pub fn record(
        &mut self,
        identity: FolderIdentity,
        path_hint: StorePath,
        label: impl Into<Box<str>>,
    ) {
        if !self.recording_enabled {
            return;
        }
        self.entries.retain(|entry| entry.identity != identity);
        self.entries.insert(
            0,
            RecentLocation {
                identity,
                path_hint,
                label: label.into(),
            },
        );
        self.entries.truncate(MAX_RECENT_LOCATIONS);
    }

    /// Records a visit made before the visits to `newer`, which stay above
    /// it. A folder in `newer` already has its newer place and is left as
    /// it is.
    pub fn record_before(
        &mut self,
        newer: &[FolderIdentity],
        identity: FolderIdentity,
        path_hint: StorePath,
        label: impl Into<Box<str>>,
    ) {
        if !self.recording_enabled || newer.contains(&identity) {
            return;
        }
        self.entries.retain(|entry| entry.identity != identity);
        let below = self
            .entries
            .iter()
            .take_while(|entry| newer.contains(&entry.identity))
            .count();
        self.entries.insert(
            below,
            RecentLocation {
                identity,
                path_hint,
                label: label.into(),
            },
        );
        self.entries.truncate(MAX_RECENT_LOCATIONS);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    #[must_use]
    pub fn entries(&self) -> &[RecentLocation] {
        &self.entries
    }
}

impl Default for RecentLocations {
    fn default() -> Self {
        Self {
            recording_enabled: true,
            entries: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountShortcut {
    identity: FolderIdentity,
    path_hint: StorePath,
    label: Box<str>,
}

impl MountShortcut {
    #[must_use]
    pub fn new(identity: FolderIdentity, path_hint: StorePath, label: impl Into<Box<str>>) -> Self {
        Self {
            identity,
            path_hint,
            label: label.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HomeItemKind {
    Recent,
    Pin,
    Mount,
    Tag,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HomeItem {
    kind: HomeItemKind,
    label: Box<str>,
    path_hint: Option<StorePath>,
    identity: Option<ItemId>,
    unavailable_reason: Option<Box<str>>,
}

impl HomeItem {
    #[must_use]
    pub const fn kind(&self) -> HomeItemKind {
        self.kind
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn path_hint(&self) -> Option<&StorePath> {
        self.path_hint.as_ref()
    }

    #[must_use]
    pub fn identity(&self) -> Option<&ItemId> {
        self.identity.as_ref()
    }

    #[must_use]
    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HomeSection {
    kind: HomeItemKind,
    items: Vec<HomeItem>,
}

impl HomeSection {
    #[must_use]
    pub const fn kind(&self) -> HomeItemKind {
        self.kind
    }

    #[must_use]
    pub fn items(&self) -> &[HomeItem] {
        &self.items
    }
}

/// A non-owning Home projection. Mutations delegate to the owning catalog
/// models, so Home never creates another persistence source.
pub struct HomeModel<'a> {
    recents: &'a mut RecentLocations,
    pins: &'a mut PinCatalog,
    tags: &'a mut TagCatalog,
    mounts: &'a [MountShortcut],
}

impl<'a> HomeModel<'a> {
    #[must_use]
    pub fn new(
        recents: &'a mut RecentLocations,
        pins: &'a mut PinCatalog,
        tags: &'a mut TagCatalog,
        mounts: &'a [MountShortcut],
    ) -> Self {
        Self {
            recents,
            pins,
            tags,
            mounts,
        }
    }

    #[must_use]
    pub fn sections(&self) -> Vec<HomeSection> {
        vec![
            HomeSection {
                kind: HomeItemKind::Recent,
                items: self
                    .recents
                    .entries()
                    .iter()
                    .map(|entry| HomeItem {
                        kind: HomeItemKind::Recent,
                        label: entry.label.clone(),
                        path_hint: Some(entry.path_hint.clone()),
                        identity: Some(entry.identity.as_item().clone()),
                        unavailable_reason: None,
                    })
                    .collect(),
            },
            HomeSection {
                kind: HomeItemKind::Pin,
                items: self
                    .pins
                    .entries()
                    .iter()
                    .map(|entry| HomeItem {
                        kind: HomeItemKind::Pin,
                        label: entry.label().into(),
                        path_hint: Some(entry.path_hint().clone()),
                        identity: Some(entry.item().clone()),
                        unavailable_reason: match entry.state() {
                            PinState::Available => None,
                            PinState::Unavailable(reason) => Some(reason.clone()),
                        },
                    })
                    .collect(),
            },
            HomeSection {
                kind: HomeItemKind::Mount,
                items: self
                    .mounts
                    .iter()
                    .map(|entry| HomeItem {
                        kind: HomeItemKind::Mount,
                        label: entry.label.clone(),
                        path_hint: Some(entry.path_hint.clone()),
                        identity: Some(entry.identity.as_item().clone()),
                        unavailable_reason: None,
                    })
                    .collect(),
            },
            HomeSection {
                kind: HomeItemKind::Tag,
                items: self
                    .tags
                    .tag_names()
                    .into_iter()
                    .map(|label| HomeItem {
                        kind: HomeItemKind::Tag,
                        label,
                        path_hint: None,
                        identity: None,
                        unavailable_reason: None,
                    })
                    .collect(),
            },
        ]
    }

    pub fn unpin(&mut self, item: &ItemId) -> bool {
        self.pins.unpin(item)
    }

    pub fn rename_tag(&mut self, old: &str, new: &str) -> Result<usize, super::TagError> {
        self.tags.rename(old, new)
    }

    pub fn clear_recent_locations(&mut self) {
        self.recents.clear();
    }
}
