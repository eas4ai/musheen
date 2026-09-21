mod advanced;
mod appearance;
pub(crate) mod custom_actions;
mod files;
pub(crate) mod general;
mod operations;
mod presentation;
mod remote;
mod search;
pub mod shortcuts;
mod terminal;
pub mod toolbar;
mod window;

use crate::{Catalog, ThemeProfile};
pub use appearance::appearance_profile;
use musheen_desktop::{
    CatalogError, CatalogStore, SettingSpec, SettingsDocument, SettingsError, SettingsFeature,
    SettingsPage, SettingsStore, settings_schema,
};
use std::collections::BTreeSet;
pub(crate) use window::{
    RecentHistoryClearer, accept_native_theme_change, apply_appearance,
    open_settings_window_with_recent_clearer,
};
pub use window::{SettingsWindow, open_settings_window};

/// The General page owns the explicit clear-history action. The catalog keeps
/// pins, tags, and session restore in independent models, so this operation
/// cannot erase them as a side effect.
pub fn clear_recent_locations(store: &CatalogStore) -> Result<(), CatalogError> {
    store.update(|catalog| {
        catalog.recents_mut().clear();
        Ok(())
    })
}

/// Availability is supplied by backend owners, never inferred from a saved preference.
#[derive(Clone, Debug, Default)]
pub struct SettingsBackends {
    available: Vec<SettingsFeature>,
}

impl SettingsBackends {
    pub fn with(mut self, feature: SettingsFeature) -> Self {
        if !self.available.contains(&feature) {
            self.available.push(feature);
        }
        self
    }
    pub fn all() -> Self {
        settings_schema()
            .iter()
            .fold(Self::default(), |backends, spec| {
                backends.with(spec.feature)
            })
    }
    pub fn supports(&self, feature: SettingsFeature) -> bool {
        feature == SettingsFeature::None || self.available.contains(&feature)
    }
}

#[derive(Clone, Debug)]
pub struct SettingsSearchHit {
    pub key: &'static str,
    pub page: SettingsPage,
    pub label: String,
}

/// A single transaction spans every page; invalid edits never leak into the
/// document and remain errors until corrected, reset, or cancelled.
#[derive(Clone, Debug)]
pub struct SettingsState {
    committed: SettingsDocument,
    draft: SettingsDocument,
    errors: BTreeSet<&'static str>,
    backends: SettingsBackends,
    page: SettingsPage,
    query: String,
    focused: Option<&'static str>,
    reset_confirmation: bool,
}

impl SettingsState {
    pub fn toolbar(&self) -> musheen_core::ToolbarLayout {
        musheen_core::ToolbarLayout::import(
            &self.draft.value("layout.toolbar").expect("schema key"),
        )
        .expect("validated toolbar")
    }
    pub fn set_toolbar(
        &mut self,
        layout: musheen_core::ToolbarLayout,
    ) -> Result<(), SettingsError> {
        self.edit("layout.toolbar", &layout.export())
    }
    pub fn shortcuts(&self) -> musheen_core::ShortcutMap {
        musheen_core::ShortcutMap::import(
            &self.draft.value("shortcuts.bindings").expect("schema key"),
        )
        .expect("validated shortcuts")
    }
    pub fn set_shortcuts(
        &mut self,
        bindings: musheen_core::ShortcutMap,
    ) -> Result<(), SettingsError> {
        self.edit("shortcuts.bindings", &bindings.export())
    }
    pub fn new(document: SettingsDocument, backends: SettingsBackends) -> Self {
        Self {
            committed: document.clone(),
            draft: document,
            errors: BTreeSet::new(),
            backends,
            page: SettingsPage::General,
            query: String::new(),
            focused: None,
            reset_confirmation: false,
        }
    }
    pub fn draft(&self) -> &SettingsDocument {
        &self.draft
    }
    pub fn is_dirty(&self) -> bool {
        self.draft != self.committed || !self.errors.is_empty()
    }
    pub fn errors(&self) -> &BTreeSet<&'static str> {
        &self.errors
    }
    pub fn page(&self) -> SettingsPage {
        self.page
    }
    pub fn query(&self) -> &str {
        &self.query
    }
    pub fn set_query(&mut self, query: String) {
        self.query = query;
    }
    pub fn select_page(&mut self, page: SettingsPage) {
        self.page = page;
        self.focused = None;
    }
    pub fn focused_key(&self) -> Option<&'static str> {
        self.focused
    }
    pub fn available(&self, spec: &SettingSpec) -> bool {
        self.backends.supports(spec.feature)
    }
    pub fn edit(&mut self, key: &str, value: &str) -> Result<(), SettingsError> {
        let spec = settings_schema()
            .iter()
            .find(|spec| spec.key == key)
            .ok_or_else(|| SettingsError::InvalidValue { key: key.into() })?;
        if !self.available(spec) {
            return Err(SettingsError::InvalidValue { key: key.into() });
        }
        match self.draft.set_value(key, value) {
            Ok(()) => {
                self.errors.remove(spec.key);
                Ok(())
            }
            Err(error) => {
                self.errors.insert(spec.key);
                Err(error)
            }
        }
    }
    pub fn apply(&mut self, store: &SettingsStore) -> Result<(), SettingsError> {
        if let Some(key) = self.errors.first() {
            return Err(SettingsError::InvalidValue { key: (*key).into() });
        }
        store.save(&self.draft)?;
        self.committed = self.draft.clone();
        Ok(())
    }
    pub fn cancel(&mut self) {
        self.draft = self.committed.clone();
        self.errors.clear();
        self.reset_confirmation = false;
    }
    pub fn reset_page(&mut self, page: SettingsPage) {
        self.draft.reset_page(page);
        self.errors.retain(|key| {
            settings_schema()
                .iter()
                .any(|spec| spec.key == *key && spec.page != page)
        });
    }
    pub fn request_reset_all(&mut self) {
        self.reset_confirmation = true;
    }
    pub fn reset_confirmation_pending(&self) -> bool {
        self.reset_confirmation
    }
    pub fn confirm_reset_all(&mut self, confirmed: bool) {
        if self.reset_confirmation && confirmed {
            for page in SettingsPage::ALL {
                self.reset_page(page);
            }
        }
        self.reset_confirmation = false;
    }
    pub fn appearance(&self, native: ThemeProfile) -> ThemeProfile {
        appearance_profile(&self.draft, native)
    }
    pub fn search(&self, query: &str, catalog: &Catalog) -> Vec<SettingsSearchHit> {
        let query = query.to_lowercase();
        settings_schema()
            .iter()
            .filter(|spec| self.available(spec))
            .filter_map(|spec| {
                let label = catalog.message(spec.label).ok()?.to_string();
                let text = format!(
                    "{} {} {} {} {}",
                    label,
                    catalog.message(spec.page.label()).ok()?,
                    catalog.message(spec.group).ok()?,
                    spec.aliases,
                    spec.key
                )
                .to_lowercase();
                query
                    .split_whitespace()
                    .all(|word| text.contains(word))
                    .then_some(SettingsSearchHit {
                        key: spec.key,
                        page: spec.page,
                        label,
                    })
            })
            .collect()
    }
    pub fn navigate_to(&mut self, key: &str) -> Result<(), SettingsError> {
        let spec = settings_schema()
            .iter()
            .find(|spec| spec.key == key && self.available(spec))
            .ok_or_else(|| SettingsError::InvalidValue { key: key.into() })?;
        self.page = spec.page;
        self.focused = Some(spec.key);
        Ok(())
    }
    pub fn page_controls(&self) -> Vec<&'static SettingSpec> {
        let controls = match self.page {
            SettingsPage::General | SettingsPage::Layout | SettingsPage::Shortcuts => {
                general::controls(self.page)
            }
            SettingsPage::Appearance => appearance::controls(),
            SettingsPage::Files => files::controls(),
            SettingsPage::Search => search::controls(),
            SettingsPage::Operations => operations::controls(),
            SettingsPage::Integrations => terminal::controls()
                .into_iter()
                .chain(remote::controls())
                .collect(),
            SettingsPage::Advanced => advanced::controls(),
        };
        controls
            .into_iter()
            .filter(|spec| self.available(spec))
            .collect()
    }
}

fn controls_for(page: SettingsPage) -> Vec<&'static SettingSpec> {
    settings_schema()
        .iter()
        .filter(|spec| spec.page == page)
        .collect()
}

/// Saved preferences consumed when new browser windows are constructed.
pub(crate) struct RuntimeSettings(pub SettingsDocument);
impl gpui_kit::Global for RuntimeSettings {}
