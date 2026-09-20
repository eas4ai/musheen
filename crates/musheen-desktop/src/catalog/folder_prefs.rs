use musheen_core::{CoreError, ItemId, ProviderId, StorePath};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct FolderIdentity(ItemId);

impl FolderIdentity {
    pub fn new(provider: ProviderId, key: impl Into<Box<[u8]>>) -> Result<Self, CoreError> {
        ItemId::new(provider, key).map(Self)
    }

    #[must_use]
    pub const fn from_item(item: ItemId) -> Self {
        Self(item)
    }

    #[must_use]
    pub const fn as_item(&self) -> &ItemId {
        &self.0
    }

    #[must_use]
    pub fn provider(&self) -> &ProviderId {
        self.0.provider()
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderView {
    Details,
    List,
    Cards,
    #[default]
    Grid,
    Columns,
    Adaptive,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderSortKey {
    #[default]
    Name,
    Size,
    Kind,
    Modified,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FolderSortDirection {
    #[default]
    Ascending,
    Descending,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderPreference {
    view: FolderView,
    sort_key: FolderSortKey,
    sort_direction: FolderSortDirection,
}

impl FolderPreference {
    #[must_use]
    pub const fn new(
        view: FolderView,
        sort_key: FolderSortKey,
        sort_direction: FolderSortDirection,
    ) -> Self {
        Self {
            view,
            sort_key,
            sort_direction,
        }
    }

    #[must_use]
    pub const fn view(&self) -> FolderView {
        self.view
    }

    #[must_use]
    pub const fn sort_key(&self) -> FolderSortKey {
        self.sort_key
    }

    #[must_use]
    pub const fn sort_direction(&self) -> FolderSortDirection {
        self.sort_direction
    }
}

impl Default for FolderPreference {
    fn default() -> Self {
        Self::new(
            FolderView::Grid,
            FolderSortKey::Name,
            FolderSortDirection::Ascending,
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct FolderPreferenceRecord {
    identity: FolderIdentity,
    path_hint: StorePath,
    parent: Option<FolderIdentity>,
    preference: Option<FolderPreference>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FolderPreferenceCatalog {
    defaults: FolderPreference,
    records: Vec<FolderPreferenceRecord>,
}

impl FolderPreferenceCatalog {
    #[must_use]
    pub fn new(defaults: FolderPreference) -> Self {
        Self {
            defaults,
            records: Vec::new(),
        }
    }

    pub fn remember_location(
        &mut self,
        identity: FolderIdentity,
        path_hint: StorePath,
        parent: Option<FolderIdentity>,
    ) {
        let record = self.record_mut_or_insert(identity, path_hint);
        record.parent = parent;
    }

    pub fn set(
        &mut self,
        identity: FolderIdentity,
        path_hint: StorePath,
        parent: Option<FolderIdentity>,
        preference: FolderPreference,
    ) {
        let record = self.record_mut_or_insert(identity, path_hint);
        record.parent = parent;
        record.preference = Some(preference);
    }

    #[must_use]
    pub fn resolve(&self, identity: &FolderIdentity) -> &FolderPreference {
        let mut current = Some(identity);
        for _ in 0..=self.records.len() {
            let Some(identity) = current else {
                break;
            };
            let Some(record) = self
                .records
                .iter()
                .find(|record| &record.identity == identity)
            else {
                break;
            };
            if let Some(preference) = &record.preference {
                return preference;
            }
            current = record.parent.as_ref();
        }
        &self.defaults
    }

    #[must_use]
    pub fn path_hint(&self, identity: &FolderIdentity) -> Option<&StorePath> {
        self.records
            .iter()
            .find(|record| &record.identity == identity)
            .map(|record| &record.path_hint)
    }

    #[must_use]
    pub const fn defaults(&self) -> &FolderPreference {
        &self.defaults
    }

    fn record_mut_or_insert(
        &mut self,
        identity: FolderIdentity,
        path_hint: StorePath,
    ) -> &mut FolderPreferenceRecord {
        if let Some(index) = self
            .records
            .iter()
            .position(|record| record.identity == identity)
        {
            let record = &mut self.records[index];
            record.path_hint = path_hint;
            return record;
        }
        self.records.push(FolderPreferenceRecord {
            identity,
            path_hint,
            parent: None,
            preference: None,
        });
        self.records
            .last_mut()
            .expect("a folder preference record was inserted")
    }
}

impl Default for FolderPreferenceCatalog {
    fn default() -> Self {
        Self::new(FolderPreference::default())
    }
}
