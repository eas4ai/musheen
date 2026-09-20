use super::*;
use gpui_kit::WeakEntity;
use musheen_core::CapabilityMatrix;
use musheen_desktop::{HomeItemKind, HomeSection, MountShortcut, TagService, TagStorage};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub(super) struct CatalogBinding {
    store: Option<CatalogStore>,
    document: Arc<Mutex<CatalogDocument>>,
    xattr_opt_in: bool,
}

pub(super) type TagTarget = (ItemId, StorePath, CapabilityMatrix);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DirectoryObservation {
    Partial,
    Complete,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OrphanCleanupOutcome {
    Removed,
    StillPresent,
    NoLongerOrphaned,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct OrphanedTagRecord {
    pub(super) item: ItemId,
    pub(super) path: StorePath,
    pub(super) tags: BTreeSet<Box<str>>,
}

impl CatalogBinding {
    pub(super) fn in_memory() -> Self {
        Self::in_memory_with_xattr_opt_in(false)
    }

    pub(super) fn in_memory_with_xattr_opt_in(xattr_opt_in: bool) -> Self {
        Self {
            store: None,
            document: Arc::new(Mutex::new(CatalogDocument::default())),
            xattr_opt_in,
        }
    }

    #[cfg(test)]
    pub(super) fn persistent(store: CatalogStore, document: CatalogDocument) -> Self {
        Self::persistent_with_xattr_opt_in(store, document, false)
    }

    pub(super) fn persistent_with_xattr_opt_in(
        store: CatalogStore,
        document: CatalogDocument,
        xattr_opt_in: bool,
    ) -> Self {
        Self {
            store: Some(store),
            document: Arc::new(Mutex::new(document)),
            xattr_opt_in,
        }
    }

    pub(super) fn snapshot(&self) -> CatalogDocument {
        self.document
            .lock()
            .expect("catalog lock is not poisoned")
            .clone()
    }

    pub(super) fn update(&self, change: impl FnOnce(&mut CatalogDocument)) -> Result<(), Box<str>> {
        self.update_result(|document| {
            change(document);
            Ok(())
        })
    }

    fn update_result<T>(
        &self,
        change: impl FnOnce(&mut CatalogDocument) -> Result<T, Box<str>>,
    ) -> Result<T, Box<str>> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        if let Some(store) = &self.store {
            *document = store
                .load()
                .map_err(|error| Box::<str>::from(error.to_string()))?;
        }
        let previous = document.clone();
        let changed = match change(&mut document) {
            Ok(changed) => changed,
            Err(error) => {
                *document = previous;
                return Err(error);
            }
        };
        if let Some(store) = &self.store
            && let Err(error) = store.save(&document)
        {
            *document = previous;
            return Err(error.to_string().into());
        }
        Ok(changed)
    }

    pub(super) fn assign_tag(
        &self,
        item: &ItemId,
        path: &StorePath,
        capabilities: &CapabilityMatrix,
        tag: &str,
    ) -> Result<TagStorage, Box<str>> {
        self.update_result(|document| {
            TagService::new(document.tags_mut(), self.xattr_opt_in)
                .assign(item, path, capabilities, tag)
                .map_err(|error| error.to_string().into())
        })
    }

    pub(super) fn remove_tag(
        &self,
        item: &ItemId,
        path: &StorePath,
        capabilities: &CapabilityMatrix,
        tag: &str,
    ) -> Result<TagStorage, Box<str>> {
        self.update_result(|document| {
            TagService::new(document.tags_mut(), self.xattr_opt_in)
                .remove(item, path, capabilities, tag)
                .map_err(|error| error.to_string().into())
        })
    }

    pub(super) fn tags_for(
        &self,
        item: &ItemId,
        path: &StorePath,
        capabilities: &CapabilityMatrix,
    ) -> Result<BTreeSet<Box<str>>, Box<str>> {
        self.update_result(|document| {
            TagService::new(document.tags_mut(), self.xattr_opt_in)
                .tags(item, path, capabilities)
                .map_err(|error| error.to_string().into())
        })
    }

    pub(super) fn common_tags(
        &self,
        targets: &[TagTarget],
    ) -> Result<BTreeSet<Box<str>>, Box<str>> {
        let Some((first_item, first_path, first_capabilities)) = targets.first() else {
            return Ok(BTreeSet::new());
        };
        let mut common = self.tags_for(first_item, first_path, first_capabilities)?;
        for (item, path, capabilities) in targets.iter().skip(1) {
            let tags = self.tags_for(item, path, capabilities)?;
            common.retain(|tag| tags.contains(tag));
        }
        Ok(common)
    }

    pub(super) fn replace_tags(
        &self,
        targets: &[TagTarget],
        desired: &BTreeSet<Box<str>>,
    ) -> Result<(), Box<str>> {
        for (item, path, capabilities) in targets {
            let current = self.tags_for(item, path, capabilities)?;
            for tag in current.difference(desired) {
                self.remove_tag(item, path, capabilities, tag)?;
            }
            for tag in desired.difference(&current) {
                self.assign_tag(item, path, capabilities, tag)?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn tags_for_identity(&self, item: &ItemId) -> BTreeSet<Box<str>> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in).tags_for(item)
    }

    pub(super) fn tag_names(&self) -> BTreeSet<Box<str>> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in).tag_names()
    }

    pub(super) fn tag_name_from_target(
        &self,
        target: &CommandTargetRef,
    ) -> Result<Box<str>, Box<str>> {
        let Some((provider, key)) = target.path().provider_key() else {
            return Err("tag actions require an exact tag shortcut identity".into());
        };
        if provider.as_str() != "musheen-tag"
            || target.id().provider() != provider
            || target.id().opaque_key() != key
        {
            return Err("the captured tag identity is invalid".into());
        }
        let name = std::str::from_utf8(key)
            .map_err(|_| Box::<str>::from("the captured tag name is not valid UTF-8"))?;
        self.tag_names()
            .contains(name)
            .then(|| Box::<str>::from(name))
            .ok_or_else(|| Box::<str>::from("the captured tag no longer exists"))
    }

    pub(super) fn rename_tag(
        &self,
        old: &str,
        new: &str,
        mut capabilities: impl FnMut(&StorePath) -> CapabilityMatrix,
    ) -> Result<usize, Box<str>> {
        TagService::validate_tag(new).map_err(|error| Box::<str>::from(error.to_string()))?;
        self.update_result(|document| {
            let targets = document
                .tags()
                .tracked_items()
                .filter(|(item, _, _)| document.tags().tags_for(item).contains(old))
                .map(|(item, path, _)| (item.clone(), path.clone()))
                .collect::<Vec<_>>();
            let mut changed = 0;
            for (item, path) in targets {
                let matrix = capabilities(&path);
                let mut service = TagService::new(document.tags_mut(), self.xattr_opt_in);
                service
                    .remove(&item, &path, &matrix, old)
                    .map_err(|error| Box::<str>::from(error.to_string()))?;
                service
                    .assign(&item, &path, &matrix, new)
                    .map_err(|error| Box::<str>::from(error.to_string()))?;
                changed += 1;
            }
            Ok(changed)
        })
    }

    pub(super) fn delete_tag(
        &self,
        tag: &str,
        mut capabilities: impl FnMut(&StorePath) -> CapabilityMatrix,
    ) -> Result<usize, Box<str>> {
        self.update_result(|document| {
            let targets = document
                .tags()
                .tracked_items()
                .filter(|(item, _, _)| document.tags().tags_for(item).contains(tag))
                .map(|(item, path, _)| (item.clone(), path.clone()))
                .collect::<Vec<_>>();
            let changed = targets.len();
            for (item, path) in targets {
                let matrix = capabilities(&path);
                TagService::new(document.tags_mut(), self.xattr_opt_in)
                    .remove(&item, &path, &matrix, tag)
                    .map_err(|error| Box::<str>::from(error.to_string()))?;
            }
            Ok(changed)
        })
    }

    pub(super) fn items_with_tag(&self, tag: &str) -> BTreeSet<ItemId> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in).items_with_tag(tag)
    }

    pub(super) fn complete_move(
        &self,
        source: &ItemId,
        destination: ItemId,
        destination_path: StorePath,
        destination_capabilities: &CapabilityMatrix,
    ) -> Result<TagMoveOutcome, Box<str>> {
        if source == &destination {
            return self.complete_rename(source, destination_path, destination_capabilities);
        }
        self.update_result(|document| {
            Ok(document.note_completed_move(
                source,
                destination,
                destination_path,
                destination_capabilities,
            ))
        })
    }

    pub(super) fn complete_rename(
        &self,
        item: &ItemId,
        destination_path: StorePath,
        destination_capabilities: &CapabilityMatrix,
    ) -> Result<TagMoveOutcome, Box<str>> {
        self.update_result(|document| {
            Ok(document.note_completed_rename(item, destination_path, destination_capabilities))
        })
    }

    pub(super) fn observe_present(&self, item: &ItemId, path: StorePath) -> Result<(), Box<str>> {
        self.update(|document| {
            TagService::new(document.tags_mut(), self.xattr_opt_in).observe_present(item, path);
        })
    }

    pub(super) fn observe_missing(&self, item: &ItemId) -> Result<(), Box<str>> {
        self.update(|document| {
            TagService::new(document.tags_mut(), self.xattr_opt_in).observe_missing(item);
        })
    }

    #[cfg(test)]
    pub(super) fn cleanup_reviewed_orphans<'a>(
        &self,
        reviewed: impl IntoIterator<Item = &'a ItemId>,
    ) -> Result<usize, Box<str>> {
        self.update_result(|document| {
            Ok(TagService::new(document.tags_mut(), self.xattr_opt_in)
                .cleanup_reviewed_orphans(reviewed))
        })
    }

    pub(super) fn cleanup_reviewed_orphan(
        &self,
        reviewed: &ItemId,
        mut resolve: impl FnMut(&StorePath) -> Result<Option<StoreItem>, Box<str>>,
    ) -> Result<OrphanCleanupOutcome, Box<str>> {
        let snapshot = self.snapshot();
        if !snapshot.tags().is_orphaned(reviewed) {
            return Ok(OrphanCleanupOutcome::NoLongerOrphaned);
        }
        let path = snapshot
            .tags()
            .path_hint(reviewed)
            .cloned()
            .ok_or_else(|| Box::<str>::from("the reviewed tag record has no path hint"))?;
        if let Some(item) = resolve(&path)?
            && item.id() == reviewed
        {
            self.observe_present(reviewed, item.path().clone())?;
            return Ok(OrphanCleanupOutcome::StillPresent);
        }
        self.update_result(|document| {
            if !document.tags().is_orphaned(reviewed)
                || document.tags().path_hint(reviewed) != Some(&path)
            {
                return Ok(OrphanCleanupOutcome::NoLongerOrphaned);
            }
            let removed = TagService::new(document.tags_mut(), self.xattr_opt_in)
                .cleanup_reviewed_orphans([reviewed]);
            Ok(if removed == 1 {
                OrphanCleanupOutcome::Removed
            } else {
                OrphanCleanupOutcome::NoLongerOrphaned
            })
        })
    }

    #[cfg(test)]
    pub(super) fn path_hint(&self, item: &ItemId) -> Option<StorePath> {
        self.snapshot().tags().path_hint(item).cloned()
    }

    #[cfg(test)]
    pub(super) fn is_orphaned(&self, item: &ItemId) -> bool {
        self.snapshot().tags().is_orphaned(item)
    }

    pub(super) fn home_sections(&self, mounts: &[MountShortcut]) -> Vec<HomeSection> {
        self.snapshot().home_sections(mounts)
    }

    pub(super) fn orphaned_tags(&self) -> Vec<OrphanedTagRecord> {
        let mut document = self.document.lock().expect("catalog lock is not poisoned");
        TagService::new(document.tags_mut(), self.xattr_opt_in)
            .orphaned_items()
            .map(|(item, path, tags)| OrphanedTagRecord {
                item: item.clone(),
                path: path.clone(),
                tags: tags.clone(),
            })
            .collect()
    }

    pub(super) fn reconcile_pins(
        &self,
        mut resolve: impl FnMut(&StorePath) -> Result<Option<StoreItem>, Box<str>>,
    ) -> Result<(), Box<str>> {
        self.update(|document| {
            let pins = document.pins().entries().to_vec();
            for pin in pins {
                match resolve(pin.path_hint()) {
                    Ok(Some(item)) if item.id() == pin.item() => {
                        document
                            .pins_mut()
                            .mark_available(pin.item(), item.path().clone());
                    }
                    Ok(Some(_)) => {
                        document.pins_mut().mark_unavailable(
                            pin.item(),
                            "the stored path now identifies a different item",
                        );
                    }
                    Ok(None) => {
                        document
                            .pins_mut()
                            .mark_unavailable(pin.item(), "the target is missing");
                    }
                    Err(error) => {
                        document.pins_mut().mark_unavailable(pin.item(), error);
                    }
                }
            }
        })
    }

    pub(super) fn reconcile_directory(
        &self,
        location: &StorePath,
        items: &[StoreItem],
        observation: DirectoryObservation,
    ) -> Result<(), Box<str>> {
        let present = items
            .iter()
            .map(|item| item.id().clone())
            .collect::<BTreeSet<_>>();
        self.update(|document| {
            let tracked = TagService::new(document.tags_mut(), self.xattr_opt_in)
                .tracked_items()
                .map(|(item, path, _)| (item.clone(), path.clone()))
                .collect::<Vec<_>>();
            let mut service = TagService::new(document.tags_mut(), self.xattr_opt_in);
            for item in items {
                service.observe_present(item.id(), item.path().clone());
            }
            if observation != DirectoryObservation::Complete {
                return;
            }
            let Some(directory) = location.as_unix_path() else {
                return;
            };
            for (item, path) in tracked {
                if path
                    .as_unix_path()
                    .and_then(std::path::Path::parent)
                    .is_some_and(|parent| parent == directory)
                    && !present.contains(&item)
                {
                    service.observe_missing(&item);
                }
            }
        })
    }
}

impl MusheenApp {
    pub(super) fn apply_properties_tags(
        &mut self,
        targets: &[TagTarget],
        desired: &BTreeSet<Box<str>>,
    ) -> Result<(), Box<str>> {
        self.catalog_binding.replace_tags(targets, desired)?;
        self.sync_catalog_projection();
        Ok(())
    }

    pub(super) fn delete_captured_tag(
        &mut self,
        target: &CommandTargetRef,
    ) -> Result<(), Box<str>> {
        let tag = self.catalog_binding.tag_name_from_target(target)?;
        let store = Arc::clone(&self.store);
        self.catalog_binding
            .delete_tag(&tag, |path| store.capabilities(path))?;
        self.sync_catalog_projection();
        Ok(())
    }

    pub(super) fn rename_catalog_tag(&mut self, old: &str, new: &str) -> Result<(), Box<str>> {
        let store = Arc::clone(&self.store);
        let changed = self
            .catalog_binding
            .rename_tag(old, new, |path| store.capabilities(path))?;
        if changed == 0 {
            return Err("the captured tag no longer exists".into());
        }
        self.sync_catalog_projection();
        Ok(())
    }

    pub(super) fn open_captured_tag_rename(
        &mut self,
        target: &CommandTargetRef,
        cx: &mut Context<Self>,
    ) -> Result<(), Box<str>> {
        let tag = self.catalog_binding.tag_name_from_target(target)?;
        let app = cx.entity().downgrade();
        let options = properties_window_options("Rename Tag", cx);
        cx.open_window(options, move |window, cx| {
            let view = cx.new(|cx| TagRenameWindow::new(app, tag, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        })
        .map_err(|error| Box::<str>::from(error.to_string()))?;
        Ok(())
    }

    pub(super) fn sync_catalog_projection(&mut self) {
        let store = Arc::clone(&self.store);
        if let Err(error) = self.catalog_binding.reconcile_pins(|path| {
            store
                .resolve_item(path)
                .map_err(|error| Box::<str>::from(error.to_string()))
        }) {
            self.operation_error = Some(error);
        }
        let document = self.catalog_binding.snapshot();
        self.pins.replace_catalog(document.pins());
        let tag_names = self.catalog_binding.tag_names();
        for sidebar in self.sidebars.values_mut() {
            sidebar.set_tag_names(tag_names.iter().map(AsRef::as_ref));
        }
        let locations = self
            .navigation
            .panes()
            .iter()
            .flat_map(|pane| pane.tabs())
            .map(|tab| {
                (
                    tab.id(),
                    tab.location().clone(),
                    tab.view_preferences().clone(),
                )
            })
            .collect::<Vec<_>>();
        for (tab_id, location, base) in locations {
            let preferences = self.preferences_with_catalog(&location, base, &document);
            if let Some(directory) = self.directories.get_mut(&tab_id) {
                *directory.view_mut().preferences_mut() = preferences.clone();
            }
            if let Some(tab) = self.navigation.tab_mut(tab_id) {
                tab.set_view_preferences(preferences);
            }
        }
    }

    pub(super) fn folder_identity(&self, location: &StorePath) -> Option<FolderIdentity> {
        self.store
            .resolve_item(location)
            .ok()
            .flatten()
            .map(|item| FolderIdentity::from_item(item.id().clone()))
            .or_else(|| {
                self.catalog_binding
                    .snapshot()
                    .folder_preferences()
                    .identity_for_path(location)
                    .cloned()
            })
    }

    pub(super) fn parent_folder_identity(&self, location: &StorePath) -> Option<FolderIdentity> {
        let parent = location.as_unix_path()?.parent()?;
        self.folder_identity(&StorePath::from_unix_path(parent.as_os_str()))
    }

    pub(super) fn remember_folder_location(&mut self, location: &StorePath) {
        let Some(identity) = self.folder_identity(location) else {
            return;
        };
        let parent = self.parent_folder_identity(location);
        if let Err(error) = self.catalog_binding.update(|document| {
            document
                .folder_preferences_mut()
                .remember_location(identity, location.clone(), parent);
        }) {
            self.operation_error = Some(error);
        }
    }

    pub(super) fn preferences_with_catalog(
        &self,
        location: &StorePath,
        base: crate::views::ViewPreferences,
        catalog: &CatalogDocument,
    ) -> crate::views::ViewPreferences {
        let Some(identity) = self.folder_identity(location) else {
            return base;
        };
        let mut reconciled = ViewPreferenceStore::new(base.clone());
        reconciled.set(location.clone(), base);
        reconciled.apply_catalog(&identity, location.clone(), catalog.folder_preferences());
        reconciled.for_path(location).clone()
    }

    pub(super) fn catalog_models(
        session_binding: Option<&SessionBinding>,
    ) -> (CatalogBinding, PinStore, Vec<Box<str>>) {
        let binding = session_binding
            .map(|session| session.catalog.clone())
            .unwrap_or_else(CatalogBinding::in_memory);
        let snapshot = binding.snapshot();
        let pins = PinStore::default();
        pins.replace_catalog(snapshot.pins());
        let tags = binding.tag_names().into_iter().collect();
        (binding, pins, tags)
    }

    pub(super) fn apply_tag_filter(&mut self, tag: &str, cx: &mut Context<Self>) {
        let expression = format!("tag:{tag}");
        self.omnibar.enter(OmnibarMode::Filter, expression.clone());
        self.pending_omnibar_value = Some(expression.clone());
        self.apply_filter(expression, cx);
    }
}

struct TagRenameWindow {
    app: WeakEntity<MusheenApp>,
    old: Box<str>,
    input: Entity<InputState>,
    error: Option<Box<str>>,
}

impl TagRenameWindow {
    fn new(
        app: WeakEntity<MusheenApp>,
        old: Box<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(old.as_ref())
                .placeholder("Tag name")
        });
        Self {
            app,
            old,
            input,
            error: None,
        }
    }
}

impl Render for TagRenameWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let error = self.error.clone();
        div()
            .id("tag-rename-dialog")
            .test_support()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_lg().child(format!("Rename ‘{}’", self.old)))
            .child(Input::new(&self.input).id("tag-rename-input"))
            .children(error.map(|error| {
                div()
                    .id("tag-rename-error")
                    .test_support()
                    .role(Role::Alert)
                    .child(error.to_string())
            }))
            .child(
                Button::new("tag-rename-confirm")
                    .label("Rename")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let new = this.input.read(cx).value().to_string();
                        match this
                            .app
                            .update(cx, |app, _| app.rename_catalog_tag(&this.old, &new))
                        {
                            Ok(Ok(())) => window.remove_window(),
                            Ok(Err(error)) => this.error = Some(error),
                            Err(error) => this.error = Some(error.to_string().into()),
                        }
                        cx.notify();
                    })),
            )
    }
}

impl MusheenApp {
    pub(super) fn render_home_surface(
        &mut self,
        tab_id: TabId,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mounts = self
            .sidebars
            .get(&tab_id)
            .into_iter()
            .flat_map(SidebarModel::sections)
            .find(|section| section.kind() == SidebarSectionKind::Mounts)
            .into_iter()
            .flat_map(|section| section.items().to_vec())
            .filter_map(|entry| {
                self.folder_identity(entry.location()).map(|identity| {
                    MountShortcut::new(identity, entry.location().clone(), entry.label())
                })
            })
            .collect::<Vec<_>>();
        let sections = self.catalog_binding.home_sections(&mounts);
        let orphaned = self.catalog_binding.orphaned_tags();
        let rows = sections
            .into_iter()
            .map(|section| Self::render_home_section(section, cx))
            .collect::<Vec<_>>();
        let orphan_review = Self::render_orphan_review(orphaned, cx);
        div()
            .id("home-surface")
            .test_support()
            .role(Role::Main)
            .aria_label("Home")
            .size_full()
            .p_4()
            .flex()
            .flex_col()
            .gap_4()
            .children(rows)
            .when_some(orphan_review, |home, review| home.child(review))
            .into_any_element()
    }

    fn render_home_section(section: HomeSection, cx: &mut Context<Self>) -> AnyElement {
        let kind = section.kind();
        let (kind_id, heading) = match kind {
            HomeItemKind::Recent => ("recent", "Recent locations"),
            HomeItemKind::Pin => ("pin", "Pinned"),
            HomeItemKind::Mount => ("mount", "Storage"),
            HomeItemKind::Tag => ("tag", "Tags"),
        };
        let items = section.items().iter().enumerate().map(|(index, item)| {
            let path = item.path_hint().cloned();
            let pin_path = path.clone();
            let pin_identity = item.identity().cloned();
            let tag = (kind == HomeItemKind::Tag).then(|| item.label().to_owned());
            let unavailable = item.unavailable_reason().map(str::to_owned);
            div()
                .w_full()
                .flex()
                .items_center()
                .child(
                    Button::new(SharedString::from(format!("home-item-{kind_id}-{index}")))
                        .label(item.label().to_owned())
                        .accessibility_label(item.label().to_owned())
                        .ghost()
                        .small()
                        .disabled(unavailable.is_some())
                        .when_some(unavailable, |button, reason| button.tooltip(reason))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(tag) = tag.as_deref() {
                                this.apply_tag_filter(tag, cx);
                            } else if let Some(path) = &path {
                                this.navigate(path.clone(), true, cx);
                            }
                        })),
                )
                .when(
                    kind == HomeItemKind::Pin && pin_identity.is_some() && pin_path.is_some(),
                    |row| {
                        row.child(
                            Button::new(SharedString::from(format!("home-unpin-{index}")))
                                .label("Unpin")
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let (Some(item), Some(path)) =
                                        (pin_identity.clone(), pin_path.clone())
                                    else {
                                        return;
                                    };
                                    this.unpin_home_item(item, path, cx);
                                })),
                        )
                    },
                )
        });
        div()
            .id(SharedString::from(format!("home-section-{kind_id}")))
            .test_support()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_sm().child(heading))
            .children(items)
            .into_any_element()
    }

    fn render_orphan_review(
        orphaned: Vec<OrphanedTagRecord>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if orphaned.is_empty() {
            return None;
        }
        let rows = orphaned.into_iter().enumerate().map(|(index, orphan)| {
            let item = orphan.item.clone();
            let tags = orphan
                .tags
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>()
                .join(", ");
            div()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_grow(1.0).text_sm().child(format!(
                    "{} — {tags}",
                    DisplayPath::from_store_path(&orphan.path).as_str()
                )))
                .child(
                    Button::new(SharedString::from(format!("home-orphan-cleanup-{index}")))
                        .label("Remove metadata")
                        .accessibility_label("Remove reviewed orphaned tag metadata")
                        .ghost()
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.cleanup_reviewed_orphan(item.clone(), cx);
                        })),
                )
        });
        Some(
            div()
                .id("home-orphan-review")
                .test_support()
                .role(Role::Region)
                .aria_label("Orphaned tag metadata review")
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_sm().child("Tag metadata needing review"))
                .children(rows)
                .into_any_element(),
        )
    }

    fn unpin_home_item(&mut self, item: ItemId, path: StorePath, cx: &mut Context<Self>) {
        let Ok(target) = CommandTargetRef::new(item, path.clone()) else {
            return;
        };
        let menu = self.compose_context_menu_at(
            self.navigation.focused_tab().id(),
            MenuTarget::SidebarLocation,
            path,
            vec![target],
        );
        if let Some(entry) = menu.entry("directory.unpin").cloned() {
            self.dispatch_context_entry(entry, cx);
        }
    }

    fn cleanup_reviewed_orphan(&mut self, item: ItemId, cx: &mut Context<Self>) {
        let store = Arc::clone(&self.store);
        match self.catalog_binding.cleanup_reviewed_orphan(&item, |path| {
            store
                .resolve_item(path)
                .map_err(|error| Box::<str>::from(error.to_string()))
        }) {
            Ok(OrphanCleanupOutcome::Removed | OrphanCleanupOutcome::StillPresent) => {
                self.sync_catalog_projection();
            }
            Ok(OrphanCleanupOutcome::NoLongerOrphaned) => {
                self.operation_error = Some("the reviewed tag record is no longer orphaned".into());
            }
            Err(error) => self.operation_error = Some(error),
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::{CatalogBinding, DirectoryObservation, OrphanCleanupOutcome};
    use musheen_core::{
        CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, ItemId, ProviderId,
        StorePath,
    };
    use musheen_core::{DisplayPath, ItemKind, StoreItem};
    use musheen_desktop::{TagMoveOutcome, TagStorage};

    fn provider(name: &str) -> ProviderId {
        ProviderId::new(name).expect("valid provider")
    }

    fn item(provider_name: &str, key: &[u8]) -> ItemId {
        ItemId::new(provider(provider_name), key.to_vec()).expect("valid item identity")
    }

    fn remote_path(provider_name: &str, key: &[u8]) -> StorePath {
        StorePath::from_provider_key(provider(provider_name), key.to_vec()).expect("valid path")
    }

    fn capabilities(tags: bool, xattrs: bool) -> CapabilityMatrix {
        CapabilityMatrix::new(|kind| {
            let supported = match kind {
                CapabilityKind::Tags => tags,
                CapabilityKind::ExtendedAttributes => xattrs,
                _ => true,
            };
            if supported {
                CapabilityState::Supported
            } else {
                CapabilityState::Unsupported(
                    CapabilityReason::new("not supported by this provider").unwrap(),
                )
            }
        })
    }

    #[test]
    fn binding_routes_tag_writes_through_the_production_service() {
        let binding = CatalogBinding::in_memory_with_xattr_opt_in(false);
        let target = item("remote", b"stable-item");
        let path = remote_path("remote", b"folder/file");

        assert_eq!(
            binding
                .assign_tag(&target, &path, &capabilities(true, false), "blue")
                .unwrap(),
            TagStorage::AppCatalog
        );
        assert_eq!(
            binding
                .tags_for(&target, &path, &capabilities(true, false))
                .unwrap(),
            [Box::<str>::from("blue")].into_iter().collect()
        );
    }

    #[test]
    fn binding_preserves_provider_paths_and_refuses_unsupported_move_targets() {
        let binding = CatalogBinding::in_memory();
        let source = item("local", b"inode-1");
        let source_path = StorePath::from_unix_bytes(b"/source/file".to_vec());
        binding
            .assign_tag(&source, &source_path, &capabilities(true, false), "keep")
            .unwrap();

        let remote = item("remote", b"object-2");
        let destination_path = remote_path("remote", b"share/final");
        assert_eq!(
            binding
                .complete_move(
                    &source,
                    remote.clone(),
                    destination_path.clone(),
                    &capabilities(true, false),
                )
                .unwrap(),
            TagMoveOutcome::Preserved
        );
        assert_eq!(binding.path_hint(&remote), Some(destination_path));

        let archive = item("archive", b"entry-3");
        let archive_path = remote_path("archive", b"entry-3");
        assert_eq!(
            binding
                .complete_move(
                    &remote,
                    archive.clone(),
                    archive_path,
                    &capabilities(false, false),
                )
                .unwrap(),
            TagMoveOutcome::UnsupportedDestination
        );
        assert_eq!(binding.tags_for_identity(&remote).len(), 1);
        assert!(binding.tags_for_identity(&archive).is_empty());
    }

    #[test]
    fn binding_reconciles_external_changes_by_exact_identity() {
        let binding = CatalogBinding::in_memory();
        let target = item("local", b"inode-9");
        let old_path = StorePath::from_unix_bytes(b"/disk/old".to_vec());
        let renamed_path = StorePath::from_unix_bytes(b"/disk/new".to_vec());
        binding
            .assign_tag(&target, &old_path, &capabilities(true, false), "tracked")
            .unwrap();

        binding
            .observe_present(&target, renamed_path.clone())
            .unwrap();
        assert_eq!(binding.path_hint(&target), Some(renamed_path));
        binding.observe_missing(&target).unwrap();
        assert!(binding.is_orphaned(&target));

        let reused_path = item("local", b"inode-10");
        assert_eq!(binding.cleanup_reviewed_orphans([&reused_path]).unwrap(), 0);
        assert_eq!(binding.cleanup_reviewed_orphans([&target]).unwrap(), 1);
    }

    #[test]
    fn binding_reconciles_each_pin_without_blocking_siblings() {
        let binding = CatalogBinding::in_memory();
        let online = item("remote", b"online");
        let offline = item("remote", b"offline");
        let online_path = remote_path("remote", b"share/online");
        let offline_path = remote_path("remote", b"share/offline");
        binding
            .update(|document| {
                document
                    .pins_mut()
                    .pin(online.clone(), online_path.clone(), "Online")
                    .unwrap();
                document
                    .pins_mut()
                    .pin(offline.clone(), offline_path.clone(), "Offline")
                    .unwrap();
            })
            .unwrap();

        binding
            .reconcile_pins(|path| {
                if path == &online_path {
                    Ok(Some(StoreItem::new(
                        online.clone(),
                        online_path.clone(),
                        DisplayPath::new("Online"),
                        ItemKind::Directory,
                        None,
                    )))
                } else {
                    Err(Box::<str>::from("remote account offline"))
                }
            })
            .unwrap();

        let snapshot = binding.snapshot();
        assert!(matches!(
            snapshot.pins().entries()[0].state(),
            musheen_desktop::PinState::Available
        ));
        assert!(matches!(
            snapshot.pins().entries()[1].state(),
            musheen_desktop::PinState::Unavailable(reason)
                if reason.as_ref() == "remote account offline"
        ));
    }

    #[test]
    fn directory_reconciliation_requires_authoritative_completion_before_orphaning() {
        let binding = CatalogBinding::in_memory();
        let renamed = item("local", b"inode-41");
        let missing = item("local", b"inode-42");
        let directory = StorePath::from_unix_path("/scope");
        let old_path = StorePath::from_unix_path("/scope/old-name");
        let new_path = StorePath::from_unix_path("/scope/new-name");
        let missing_path = StorePath::from_unix_path("/scope/missing");
        for (item, path) in [(&renamed, &old_path), (&missing, &missing_path)] {
            binding
                .assign_tag(item, path, &capabilities(true, false), "tracked")
                .unwrap();
        }
        let listing = vec![StoreItem::new(
            renamed.clone(),
            new_path.clone(),
            DisplayPath::new("new-name"),
            ItemKind::RegularFile,
            None,
        )];

        binding
            .reconcile_directory(&directory, &listing, DirectoryObservation::Partial)
            .unwrap();

        assert_eq!(binding.path_hint(&renamed), Some(new_path));
        assert!(!binding.is_orphaned(&renamed));
        assert!(!binding.is_orphaned(&missing));

        binding
            .reconcile_directory(&directory, &listing, DirectoryObservation::Complete)
            .unwrap();
        assert!(binding.is_orphaned(&missing));
    }

    #[test]
    fn reviewed_orphan_cleanup_rechecks_exact_identity_liveness() {
        let binding = CatalogBinding::in_memory();
        let target = item("local", b"inode-99");
        let path = StorePath::from_unix_path("/scope/target");
        binding
            .assign_tag(&target, &path, &capabilities(true, false), "tracked")
            .unwrap();
        binding.observe_missing(&target).unwrap();

        assert_eq!(
            binding
                .cleanup_reviewed_orphan(&target, |_| {
                    Ok(Some(StoreItem::new(
                        target.clone(),
                        path.clone(),
                        DisplayPath::new("target"),
                        ItemKind::RegularFile,
                        None,
                    )))
                })
                .unwrap(),
            OrphanCleanupOutcome::StillPresent
        );
        assert!(!binding.is_orphaned(&target));

        binding.observe_missing(&target).unwrap();
        assert_eq!(
            binding
                .cleanup_reviewed_orphan(&target, |_| Ok(None))
                .unwrap(),
            OrphanCleanupOutcome::Removed
        );
        assert!(binding.tags_for_identity(&target).is_empty());
    }
}
