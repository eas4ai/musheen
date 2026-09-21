use crate::i18n::Catalog;
use crate::operations::{OperationHub, spawn_ready_hub_operations};
use crate::{ApplicationIdentity, DropError, LocalOperationQueue, PermissionsPageModel};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, KeyBinding, Role, SharedString,
    Subscription, Task, TestSupportExt, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    px, size,
};
use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityState, CommandTargetRef,
    DisplayPath, ItemKind, Store, StorePath,
};
use musheen_desktop::{
    AclEntry, AclQualifier, AclState, AggregateValue, ChecksumAlgorithm, ChecksumResult,
    ChecksumService, PropertyError, PropertyRefresh, PropertySnapshot, PropertyTimestamp,
    RecursiveSize, TagError, XattrState,
};
use musheen_local::LocalStore;
use musheen_ops::{JobId, MetadataChange, MetadataScope};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const LIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

type RefreshWorkResult = Result<(PropertyRefresh, Option<PropertySnapshot>), PropertyError>;

gpui_kit::actions!(properties, [CancelProperties, ConfirmProperties]);

pub(crate) fn install_properties_key_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", ConfirmProperties, Some("PropertiesWindow")),
        KeyBinding::new("escape", CancelProperties, Some("PropertiesWindow")),
    ]);
}

pub(crate) fn properties_window_options(title: impl Into<SharedString>, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(760.), px(620.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitlebarOptions::default()
        }),
        app_id: Some(format!("{}.Properties", ApplicationIdentity::ID)),
        window_min_size: Some(size(px(560.), px(420.))),
        ..WindowOptions::default()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PropertiesPage {
    General,
    Permissions,
    OpenWith,
    Tags,
    Checksums,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PropertiesState {
    Ready,
    Replaced,
    Missing,
}

fn properties_state(refresh: PropertyRefresh) -> PropertiesState {
    match refresh {
        PropertyRefresh::Current | PropertyRefresh::MetadataChanged => PropertiesState::Ready,
        PropertyRefresh::Replaced => PropertiesState::Replaced,
        PropertyRefresh::Missing => PropertiesState::Missing,
    }
}

#[derive(Debug)]
pub struct PropertiesDialogModel {
    snapshot: PropertySnapshot,
    permissions: PermissionsPageModel,
    pages: Vec<PropertiesPage>,
    page: PropertiesPage,
    state: PropertiesState,
    original_tags: BTreeSet<Box<str>>,
    tags: BTreeSet<Box<str>>,
    mixed_tags: BTreeSet<Box<str>>,
    added_tags: BTreeSet<Box<str>>,
    removed_tags: BTreeSet<Box<str>>,
}

impl PropertiesDialogModel {
    pub fn new(snapshot: PropertySnapshot) -> Self {
        let permissions = PermissionsPageModel::from_snapshot(&snapshot);
        let mut pages = vec![
            PropertiesPage::General,
            PropertiesPage::Permissions,
            PropertiesPage::Tags,
        ];
        if snapshot
            .items()
            .iter()
            .all(|item| item.kind() == ItemKind::RegularFile)
        {
            pages.insert(2, PropertiesPage::OpenWith);
            pages.push(PropertiesPage::Checksums);
        }
        Self {
            snapshot,
            permissions,
            pages,
            page: PropertiesPage::General,
            state: PropertiesState::Ready,
            original_tags: BTreeSet::new(),
            tags: BTreeSet::new(),
            mixed_tags: BTreeSet::new(),
            added_tags: BTreeSet::new(),
            removed_tags: BTreeSet::new(),
        }
    }

    pub fn snapshot(&self) -> &PropertySnapshot {
        &self.snapshot
    }

    pub fn state(&self) -> PropertiesState {
        self.state
    }

    pub fn page(&self) -> PropertiesPage {
        self.page
    }

    pub fn pages(&self) -> &[PropertiesPage] {
        &self.pages
    }

    pub fn select_page(&mut self, page: PropertiesPage) -> Result<(), PropertiesModelError> {
        if !self.pages.contains(&page) {
            return Err(PropertiesModelError::UnavailablePage);
        }
        self.page = page;
        Ok(())
    }

    pub fn permissions(&self) -> &PermissionsPageModel {
        &self.permissions
    }

    pub fn permissions_mut(&mut self) -> &mut PermissionsPageModel {
        &mut self.permissions
    }

    pub fn set_tags<'a>(&mut self, tags: impl IntoIterator<Item = &'a str>) {
        self.set_tag_states(tags, std::iter::empty());
    }

    pub fn set_tag_states<'a>(
        &mut self,
        common: impl IntoIterator<Item = &'a str>,
        mixed: impl IntoIterator<Item = &'a str>,
    ) {
        self.tags = common.into_iter().map(Box::<str>::from).collect();
        self.original_tags = self.tags.clone();
        self.mixed_tags = mixed.into_iter().map(Box::<str>::from).collect();
        self.added_tags.clear();
        self.removed_tags.clear();
    }

    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(AsRef::as_ref)
    }

    pub fn mixed_tags(&self) -> impl Iterator<Item = &str> {
        self.mixed_tags.iter().map(AsRef::as_ref)
    }

    pub fn added_tags(&self) -> impl Iterator<Item = &str> {
        self.added_tags.iter().map(AsRef::as_ref)
    }

    pub fn removed_tags(&self) -> impl Iterator<Item = &str> {
        self.removed_tags.iter().map(AsRef::as_ref)
    }

    pub fn assign_tag(&mut self, tag: &str) -> Result<bool, TagError> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Err(TagError::Empty);
        }
        if tag.len() > 128 {
            return Err(TagError::TooLong);
        }
        let was_mixed = self.mixed_tags.remove(tag);
        let changed = self.tags.insert(tag.into()) || was_mixed;
        if changed {
            self.removed_tags.remove(tag);
            self.added_tags.insert(tag.into());
        }
        Ok(changed)
    }

    pub fn remove_tag(&mut self, tag: &str) -> bool {
        let changed = self.tags.remove(tag) || self.mixed_tags.remove(tag);
        if changed {
            self.added_tags.remove(tag);
            self.removed_tags.insert(tag.into());
        }
        changed
    }

    #[must_use]
    pub fn tags_dirty(&self) -> bool {
        !self.added_tags.is_empty() || !self.removed_tags.is_empty()
    }

    pub fn accept_tags(&mut self) {
        self.original_tags = self.tags.clone();
        self.added_tags.clear();
        self.removed_tags.clear();
    }

    pub fn apply_visible(&self) -> bool {
        self.state == PropertiesState::Ready
            && self.permissions.is_dirty()
            && self.permissions.is_valid()
    }

    pub fn submit_permissions(
        &self,
        queue: &mut LocalOperationQueue,
    ) -> Result<Vec<JobId>, DropError> {
        let (roots, scope, change) = self.permission_request()?;
        queue.submit_metadata_changes(roots, scope, change)
    }

    fn permission_request(
        &self,
    ) -> Result<(Vec<StorePath>, MetadataScope, MetadataChange), DropError> {
        if self.state != PropertiesState::Ready {
            return Err(DropError::Mutation(
                musheen_ops::MutationError::SourceChanged,
            ));
        }
        let roots = self
            .snapshot
            .items()
            .iter()
            .map(|item| StorePath::from_unix_path(item.path().as_os_str()))
            .collect();
        Ok((
            roots,
            self.permissions.scope(),
            self.permissions.change().clone(),
        ))
    }

    pub fn refresh(&mut self) -> Result<PropertyRefresh, PropertyError> {
        let refresh = self.snapshot.refresh_state()?;
        if refresh == PropertyRefresh::MetadataChanged && self.permissions.is_dirty() {
            self.state = PropertiesState::Replaced;
            return Ok(refresh);
        }
        self.state = properties_state(refresh);
        if refresh == PropertyRefresh::MetadataChanged {
            let paths = self
                .snapshot
                .items()
                .iter()
                .map(|item| item.path().to_path_buf())
                .collect::<Vec<_>>();
            self.snapshot = PropertySnapshot::load(&paths)?;
            self.permissions = PermissionsPageModel::from_snapshot(&self.snapshot);
        }
        Ok(refresh)
    }

    fn replace_snapshot(&mut self, snapshot: PropertySnapshot) {
        let selected_page = self.page;
        let mut replacement = Self::new(snapshot);
        if replacement.pages.contains(&selected_page) {
            replacement.page = selected_page;
        }
        *self = replacement;
    }

    fn clear_permission_edits(&mut self) {
        self.permissions = PermissionsPageModel::from_snapshot(&self.snapshot);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PropertiesModelError {
    UnavailablePage,
}

#[derive(Clone, Debug)]
pub struct ProviderPropertiesDialogModel {
    targets: Vec<CommandTargetRef>,
    capabilities: Vec<CapabilityMatrix>,
    pages: Vec<PropertiesPage>,
    page: PropertiesPage,
    original_tags: BTreeSet<Box<str>>,
    tags: BTreeSet<Box<str>>,
    mixed_tags: BTreeSet<Box<str>>,
    added_tags: BTreeSet<Box<str>>,
    removed_tags: BTreeSet<Box<str>>,
}

impl ProviderPropertiesDialogModel {
    #[must_use]
    pub fn new(targets: Vec<(CommandTargetRef, CapabilityMatrix)>) -> Self {
        let (targets, capabilities): (Vec<_>, Vec<_>) = targets.into_iter().unzip();
        let mut pages = vec![PropertiesPage::General];
        if capabilities.iter().all(|matrix: &CapabilityMatrix| {
            matches!(matrix.get(CapabilityKind::Tags), CapabilityState::Supported)
        }) {
            pages.push(PropertiesPage::Tags);
        }
        Self {
            targets,
            capabilities,
            pages,
            page: PropertiesPage::General,
            original_tags: BTreeSet::new(),
            tags: BTreeSet::new(),
            mixed_tags: BTreeSet::new(),
            added_tags: BTreeSet::new(),
            removed_tags: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn targets(&self) -> &[CommandTargetRef] {
        &self.targets
    }

    #[must_use]
    pub fn capabilities(&self) -> &[CapabilityMatrix] {
        &self.capabilities
    }

    #[must_use]
    pub fn pages(&self) -> &[PropertiesPage] {
        &self.pages
    }

    #[must_use]
    pub const fn page(&self) -> PropertiesPage {
        self.page
    }

    pub fn select_page(&mut self, page: PropertiesPage) -> Result<(), PropertiesModelError> {
        if !self.pages.contains(&page) {
            return Err(PropertiesModelError::UnavailablePage);
        }
        self.page = page;
        Ok(())
    }

    pub fn set_tags<'a>(&mut self, tags: impl IntoIterator<Item = &'a str>) {
        self.set_tag_states(tags, std::iter::empty());
    }

    pub fn set_tag_states<'a>(
        &mut self,
        common: impl IntoIterator<Item = &'a str>,
        mixed: impl IntoIterator<Item = &'a str>,
    ) {
        self.tags = common.into_iter().map(Box::<str>::from).collect();
        self.original_tags = self.tags.clone();
        self.mixed_tags = mixed.into_iter().map(Box::<str>::from).collect();
        self.added_tags.clear();
        self.removed_tags.clear();
    }

    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(AsRef::as_ref)
    }

    pub fn mixed_tags(&self) -> impl Iterator<Item = &str> {
        self.mixed_tags.iter().map(AsRef::as_ref)
    }

    pub fn assign_tag(&mut self, tag: &str) -> Result<bool, TagError> {
        let tag = tag.trim();
        if tag.is_empty() {
            return Err(TagError::Empty);
        }
        if tag.len() > 128 {
            return Err(TagError::TooLong);
        }
        let was_mixed = self.mixed_tags.remove(tag);
        let changed = self.tags.insert(tag.into()) || was_mixed;
        if changed {
            self.removed_tags.remove(tag);
            self.added_tags.insert(tag.into());
        }
        Ok(changed)
    }

    pub fn remove_tag(&mut self, tag: &str) -> bool {
        let changed = self.tags.remove(tag) || self.mixed_tags.remove(tag);
        if changed {
            self.added_tags.remove(tag);
            self.removed_tags.insert(tag.into());
        }
        changed
    }

    fn accept_tags(&mut self) {
        self.original_tags = self.tags.clone();
        self.added_tags.clear();
        self.removed_tags.clear();
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TagDelta {
    pub(crate) added: BTreeSet<Box<str>>,
    pub(crate) removed: BTreeSet<Box<str>>,
}

pub(crate) struct ProviderPropertiesWindowData {
    model: ProviderPropertiesDialogModel,
    tag_writer: Option<TagWriter>,
    catalog: Catalog,
}

impl ProviderPropertiesWindowData {
    pub(crate) fn new(
        targets: Vec<(CommandTargetRef, CapabilityMatrix)>,
        common_tags: impl IntoIterator<Item = Box<str>>,
        mixed_tags: impl IntoIterator<Item = Box<str>>,
        tag_writer: Option<TagWriter>,
        catalog: Catalog,
    ) -> Self {
        let mut model = ProviderPropertiesDialogModel::new(targets);
        let common_tags = common_tags.into_iter().collect::<Vec<_>>();
        let mixed_tags = mixed_tags.into_iter().collect::<Vec<_>>();
        model.set_tag_states(
            common_tags.iter().map(AsRef::as_ref),
            mixed_tags.iter().map(AsRef::as_ref),
        );
        Self {
            model,
            tag_writer,
            catalog,
        }
    }
}

pub(crate) struct ProviderPropertiesWindow {
    model: ProviderPropertiesDialogModel,
    tag_writer: Option<TagWriter>,
    tag_input: Entity<InputState>,
    tag_error: Option<Box<str>>,
    page_notice: Option<Box<str>>,
    catalog: Catalog,
}

impl ProviderPropertiesWindow {
    pub(crate) fn new(
        data: ProviderPropertiesWindowData,
        page: PropertiesPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut model = data.model;
        let page_notice = model
            .select_page(page)
            .err()
            .map(|_| provider_page_unavailable_message(&model, page, &data.catalog));
        let tag_name = data
            .catalog
            .message("catalog-tag-name")
            .expect("the tag-name catalog message exists")
            .to_owned();
        Self {
            model,
            tag_writer: data.tag_writer,
            tag_input: cx.new(|cx| InputState::new(window, cx).placeholder(tag_name)),
            tag_error: None,
            page_notice,
            catalog: data.catalog,
        }
    }

    fn select_page(&mut self, page: PropertiesPage, cx: &mut Context<Self>) {
        match self.model.select_page(page) {
            Ok(()) => self.page_notice = None,
            Err(_) => {
                self.page_notice = Some(provider_page_unavailable_message(
                    &self.model,
                    page,
                    &self.catalog,
                ));
            }
        }
        cx.notify();
    }

    fn apply_tags(&mut self, cx: &mut Context<Self>) {
        let Some(writer) = self.tag_writer.as_ref() else {
            self.tag_error = Some(
                self.catalog
                    .message("provider-properties-tag-storage-unavailable")
                    .expect("the tag-storage catalog message exists")
                    .into(),
            );
            cx.notify();
            return;
        };
        let delta = TagDelta {
            added: self.model.added_tags.clone(),
            removed: self.model.removed_tags.clone(),
        };
        // Provider and local windows use the same app-owned writer policy.
        match writer(&delta, cx) {
            Ok(()) => {
                self.model.accept_tags();
                self.tag_error = None;
            }
            Err(error) => self.tag_error = Some(error),
        }
        cx.notify();
    }

    fn render_page_navigation(&self, cx: &mut Context<Self>) -> AnyElement {
        let buttons = [PropertiesPage::General, PropertiesPage::Tags]
            .into_iter()
            .map(|page| {
                let available = self.model.pages().contains(&page);
                Button::new(SharedString::from(format!(
                    "provider-properties-page-{}",
                    page_id(page)
                )))
                .label(provider_page_label(page, &self.catalog))
                .selected(self.model.page() == page)
                .disabled(!available)
                .on_click(cx.listener(move |this, _, _, cx| this.select_page(page, cx)))
            })
            .collect::<Vec<_>>();
        div()
            .id("provider-properties-pages")
            .test_support()
            .role(Role::TabList)
            .flex()
            .gap_2()
            .children(buttons)
            .into_any_element()
    }

    fn render_tags_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let tags = self.model.tags().map(str::to_owned).collect::<Vec<_>>();
        let mixed_tags = self
            .model
            .mixed_tags()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let remove_label = self
            .catalog
            .message("provider-properties-remove")
            .expect("the remove catalog message exists")
            .to_owned();
        let some_items = self
            .catalog
            .message("provider-properties-some-items")
            .expect("the some-items catalog message exists")
            .to_owned();
        div()
            .id("properties-tags-page")
            .test_support()
            .flex()
            .flex_col()
            .gap_2()
            .children(tags.into_iter().enumerate().map(|(index, tag)| {
                let remove = tag.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .when(self.catalog.locale() == crate::Locale::Ar, |row| {
                        row.flex_row_reverse()
                    })
                    .child(tag)
                    .child(
                        Button::new(SharedString::from(format!(
                            "provider-properties-remove-tag-{index}"
                        )))
                        .label(remove_label.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.remove_tag(&remove);
                            cx.notify();
                        })),
                    )
            }))
            .children(mixed_tags.into_iter().enumerate().map(|(index, tag)| {
                let remove = tag.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .when(self.catalog.locale() == crate::Locale::Ar, |row| {
                        row.flex_row_reverse()
                    })
                    .child(format!("{tag} ({some_items})"))
                    .child(
                        Button::new(SharedString::from(format!(
                            "provider-properties-remove-mixed-tag-{index}"
                        )))
                        .label(remove_label.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.remove_tag(&remove);
                            cx.notify();
                        })),
                    )
            }))
            .child(Input::new(&self.tag_input).id("provider-properties-tag-input"))
            .when_some(self.tag_error.as_deref(), |view, error| {
                let error = self.catalog.localize_reason(error);
                view.child(
                    div()
                        .id("provider-properties-tag-error")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .child(error),
                )
            })
            .child(
                Button::new("provider-properties-add-tag")
                    .label(
                        self.catalog
                            .message("provider-properties-add-tag")
                            .expect("the add-tag catalog message exists"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let tag = this.tag_input.read(cx).value().to_string();
                        this.tag_error = this
                            .model
                            .assign_tag(&tag)
                            .err()
                            .map(|error| error.to_string().into());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("provider-properties-apply-tags")
                    .label(
                        self.catalog
                            .message("provider-properties-apply")
                            .expect("the apply catalog message exists"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.apply_tags(cx))),
            )
            .into_any_element()
    }

    fn render_general_page(&self) -> AnyElement {
        let provider_label = self
            .catalog
            .message("provider-properties-provider")
            .expect("the provider catalog message exists")
            .to_owned();
        let identity_label = self
            .catalog
            .message("provider-properties-stable-identity")
            .expect("the stable-identity catalog message exists")
            .to_owned();
        let location_label = self
            .catalog
            .message("provider-properties-location")
            .expect("the location catalog message exists")
            .to_owned();
        let targets = self
            .model
            .targets()
            .iter()
            .enumerate()
            .map(|(index, target)| {
                div()
                    .id(SharedString::from(format!(
                        "provider-properties-target-{index}"
                    )))
                    .test_support()
                    .flex()
                    .flex_col()
                    .child(provider_property_row(
                        format!("provider-properties-provider-{index}"),
                        format!("{provider_label}: {}", target.id().provider().as_str()),
                    ))
                    .child(provider_property_row(
                        format!("provider-properties-identity-{index}"),
                        format!("{identity_label}: {:?}", target.id().opaque_key()),
                    ))
                    .child(provider_property_row(
                        format!("provider-properties-location-{index}"),
                        format!(
                            "{location_label}: {}",
                            DisplayPath::from_store_path(target.path()).as_str()
                        ),
                    ))
            });
        let capabilities = CapabilityKind::ALL
            .iter()
            .copied()
            .enumerate()
            .map(|(index, kind)| {
                let values = self
                    .model
                    .capabilities()
                    .iter()
                    .map(|matrix| provider_capability_state_label(matrix.get(kind), &self.catalog))
                    .collect::<Vec<_>>();
                div()
                    .id(SharedString::from(format!(
                        "provider-properties-capability-{index}"
                    )))
                    .test_support()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(provider_capability_kind_label(kind, &self.catalog))
                    .child(provider_aggregate_strings(&values, &self.catalog))
            });
        div()
            .id("provider-properties-general-page")
            .test_support()
            .flex()
            .flex_col()
            .gap_2()
            .children(targets)
            .children(capabilities)
            .into_any_element()
    }
}

impl Render for ProviderPropertiesWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match self.model.page() {
            PropertiesPage::Tags => self.render_tags_page(cx),
            _ => self.render_general_page(),
        };
        div()
            .id("provider-properties")
            .test_support()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.render_page_navigation(cx))
            .when_some(self.page_notice.clone(), |view, notice| {
                view.child(
                    div()
                        .id("provider-properties-page-notice")
                        .test_support()
                        .child(notice.to_string()),
                )
            })
            .child(content)
    }
}

fn provider_property_row(id: String, value: String) -> AnyElement {
    div()
        .id(SharedString::from(id))
        .test_support()
        .aria_label(value.clone())
        .child(value)
        .into_any_element()
}

fn provider_page_unavailable_message(
    model: &ProviderPropertiesDialogModel,
    page: PropertiesPage,
    catalog: &Catalog,
) -> Box<str> {
    if page == PropertiesPage::Tags {
        let states = model
            .capabilities()
            .iter()
            .map(|matrix| {
                provider_capability_state_label(matrix.get(CapabilityKind::Tags), catalog)
            })
            .collect::<Vec<_>>();
        return format!(
            "{}: {}",
            catalog
                .message("provider-properties-tags-unavailable")
                .expect("the unavailable-tags catalog message exists"),
            provider_aggregate_strings(&states, catalog)
        )
        .into();
    }
    format!(
        "{} {}",
        provider_page_label(page, catalog),
        catalog
            .message("provider-properties-page-unavailable")
            .expect("the unavailable-page catalog message exists")
    )
    .into()
}

pub struct PropertiesWindowData {
    snapshot: PropertySnapshot,
    filesystem_rows: Vec<(Box<str>, Box<str>)>,
    capability_rows: Vec<(Box<str>, Box<str>)>,
    tags: BTreeSet<Box<str>>,
    mixed_tags: BTreeSet<Box<str>>,
    tag_writer: Option<TagWriter>,
    catalog: Catalog,
}

pub(crate) type TagWriter =
    Arc<dyn Fn(&TagDelta, &mut App) -> Result<(), Box<str>> + Send + Sync + 'static>;

impl PropertiesWindowData {
    pub fn load(paths: &[PathBuf]) -> Result<Self, PropertyError> {
        let snapshot = PropertySnapshot::load(paths)?;
        let store = LocalStore::new();
        let locations = snapshot
            .items()
            .iter()
            .map(|item| StorePath::from_unix_path(item.path().as_os_str()))
            .collect::<Vec<_>>();
        let filesystem_information = locations
            .iter()
            .map(|location| store.probe(location))
            .collect::<Result<Vec<_>, _>>();
        let (filesystem_rows, capability_matrices) = match filesystem_information {
            Ok(information) => {
                let summaries = information
                    .iter()
                    .map(|info| {
                        vec![
                            ("Filesystem", info.filesystem_type().to_owned()),
                            ("Mount source", display_os(info.source().as_os_str())),
                            ("Mount point", display_os(info.mount_point().as_os_str())),
                            (
                                "Mount mode",
                                if info.is_read_only() {
                                    "Read only".to_owned()
                                } else {
                                    "Read and write".to_owned()
                                },
                            ),
                            ("Available", format_size(info.available_bytes())),
                        ]
                    })
                    .collect::<Vec<_>>();
                let capabilities = information
                    .iter()
                    .map(|info| info.capabilities())
                    .collect::<Vec<_>>();
                (aggregate_named_rows(&summaries), capabilities)
            }
            Err(error) => (
                vec![("Filesystem".into(), format!("Unavailable: {error}").into())],
                locations
                    .iter()
                    .map(|location| store.capabilities(location))
                    .collect(),
            ),
        };
        let capability_rows = CapabilityKind::ALL
            .iter()
            .copied()
            .map(|kind| {
                let values = capability_matrices
                    .iter()
                    .map(|matrix| capability_state_label(matrix.get(kind)))
                    .collect::<Vec<_>>();
                (
                    format!("Capability: {}", capability_kind_label(kind)).into(),
                    aggregate_strings(&values).into(),
                )
            })
            .collect();
        Ok(Self {
            snapshot,
            filesystem_rows,
            capability_rows,
            tags: BTreeSet::new(),
            mixed_tags: BTreeSet::new(),
            tag_writer: None,
            catalog: Catalog::system().expect("the built-in locale catalogs are valid"),
        })
    }

    #[must_use]
    pub fn with_tags<'a>(mut self, tags: impl IntoIterator<Item = &'a str>) -> Self {
        self.tags = tags.into_iter().map(Box::<str>::from).collect();
        self
    }

    #[must_use]
    pub(crate) fn with_tag_states(
        mut self,
        common: impl IntoIterator<Item = Box<str>>,
        mixed: impl IntoIterator<Item = Box<str>>,
    ) -> Self {
        self.tags = common.into_iter().collect();
        self.mixed_tags = mixed.into_iter().collect();
        self
    }

    #[must_use]
    pub(crate) fn with_tag_writer(mut self, writer: TagWriter) -> Self {
        self.tag_writer = Some(writer);
        self
    }

    #[must_use]
    pub(crate) fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }
}

#[derive(Debug)]
struct PropertyRow {
    label: Box<str>,
    value: Box<str>,
    input: Entity<InputState>,
}

#[derive(Debug)]
struct PermissionInputs {
    owner: Entity<InputState>,
    group: Entity<InputState>,
    file_mode: Entity<InputState>,
    directory_mode: Entity<InputState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PermissionBatchOutcome {
    Ignored,
    Pending,
    Succeeded,
    Failed,
}

#[derive(Debug, Default)]
struct PermissionBatchState {
    pending: BTreeSet<JobId>,
    failed: bool,
}

impl PermissionBatchState {
    fn is_active(&self) -> bool {
        !self.pending.is_empty()
    }

    fn begin(&mut self, pending: impl IntoIterator<Item = JobId>) {
        self.pending = pending.into_iter().collect();
        self.failed = false;
    }

    fn finish(&mut self, id: JobId, succeeded: bool) -> PermissionBatchOutcome {
        if !self.pending.remove(&id) {
            return PermissionBatchOutcome::Ignored;
        }
        self.failed |= !succeeded;
        if !self.pending.is_empty() {
            PermissionBatchOutcome::Pending
        } else if self.failed {
            PermissionBatchOutcome::Failed
        } else {
            PermissionBatchOutcome::Succeeded
        }
    }
}

#[derive(Debug)]
enum RecursiveSizeState {
    Idle,
    Running,
    Ready(RecursiveSize),
    Failed(Box<str>),
}

#[derive(Debug)]
enum ChecksumState {
    Idle,
    Running(ChecksumAlgorithm),
    Ready(ChecksumResult),
    Failed(Box<str>),
}

pub(crate) struct PropertiesFailureWindow {
    message: Box<str>,
    catalog: Catalog,
    focus: FocusHandle,
    pending_focus: bool,
}

impl PropertiesFailureWindow {
    pub fn new(message: impl Into<Box<str>>, catalog: Catalog, cx: &mut Context<Self>) -> Self {
        Self {
            message: message.into(),
            catalog,
            focus: cx.focus_handle(),
            pending_focus: true,
        }
    }
}

impl Render for PropertiesFailureWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.focus.focus(window, cx);
            self.pending_focus = false;
        }
        div()
            .id("properties-dialog")
            .test_support()
            .key_context("PropertiesWindow")
            .role(Role::Dialog)
            .aria_label(
                self.catalog
                    .message("properties-load-failed")
                    .expect("the Properties failure message exists"),
            )
            .track_focus(&self.focus)
            .tab_index(0)
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .p_6()
            .bg(cx.theme().colors.background)
            .text_color(cx.theme().colors.foreground)
            .on_action(|_: &ConfirmProperties, window, _| window.remove_window())
            .on_action(|_: &CancelProperties, window, _| window.remove_window())
            .on_action(|_: &Escape, window, _| window.remove_window())
            .child(
                div()
                    .id("properties-validation-summary")
                    .test_support()
                    .role(Role::Alert)
                    .aria_label(self.message.to_string())
                    .child(self.message.to_string()),
            )
            .child(
                Button::new("properties-close")
                    .label(
                        self.catalog
                            .message("properties-close")
                            .expect("the Properties close message exists"),
                    )
                    .primary()
                    .on_click(|_, window, _| window.remove_window()),
            )
    }
}

pub(crate) struct PropertiesWindow {
    model: PropertiesDialogModel,
    filesystem_rows: Vec<(Box<str>, Box<str>)>,
    capability_rows: Vec<(Box<str>, Box<str>)>,
    rows: Vec<PropertyRow>,
    focus: FocusHandle,
    pending_focus: bool,
    refreshing: bool,
    refresh_error: Option<Box<str>>,
    recursive_size: RecursiveSizeState,
    recursive_cancellation: Option<CancellationToken>,
    checksum: ChecksumState,
    checksum_cancellation: Option<CancellationToken>,
    close_requested: bool,
    permission_inputs: PermissionInputs,
    permission_inputs_need_sync: bool,
    permission_subscriptions: Vec<Subscription>,
    operation_hub: OperationHub,
    permission_error: Option<Box<str>>,
    permission_batch: PermissionBatchState,
    tag_input: Entity<InputState>,
    tag_writer: Option<TagWriter>,
    tag_error: Option<Box<str>>,
    catalog: Catalog,
}

impl PropertiesWindow {
    #[cfg(test)]
    pub fn new(data: PropertiesWindowData, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::with_hub(
            data,
            OperationHub::new(&musheen_core::ResourceLimits::default()),
            window,
            cx,
        )
    }

    pub(crate) fn with_hub(
        data: PropertiesWindowData,
        operation_hub: OperationHub,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let owner = editable_u32(data.snapshot.aggregate().owner());
        let group = editable_u32(data.snapshot.aggregate().group());
        let mode = editable_mode(data.snapshot.aggregate().mode());
        let permission_inputs = PermissionInputs {
            owner: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(owner)
                    .placeholder("Numeric user ID")
            }),
            group: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(group)
                    .placeholder("Numeric group ID")
            }),
            file_mode: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(mode.clone())
                    .placeholder("0644")
            }),
            directory_mode: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(mode)
                    .placeholder("0755")
            }),
        };
        let tag_name = data
            .catalog
            .message("catalog-tag-name")
            .expect("the tag-name catalog message exists")
            .to_owned();
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder(tag_name));
        let mut model = PropertiesDialogModel::new(data.snapshot);
        model.set_tag_states(
            data.tags.iter().map(AsRef::as_ref),
            data.mixed_tags.iter().map(AsRef::as_ref),
        );
        let mut this = Self {
            model,
            filesystem_rows: data.filesystem_rows,
            capability_rows: data.capability_rows,
            rows: Vec::new(),
            focus: cx.focus_handle(),
            pending_focus: true,
            refreshing: false,
            refresh_error: None,
            recursive_size: RecursiveSizeState::Idle,
            recursive_cancellation: None,
            checksum: ChecksumState::Idle,
            checksum_cancellation: None,
            close_requested: false,
            permission_inputs,
            permission_inputs_need_sync: false,
            permission_subscriptions: Vec::new(),
            operation_hub,
            permission_error: None,
            permission_batch: PermissionBatchState::default(),
            tag_input,
            tag_writer: data.tag_writer,
            tag_error: None,
            catalog: data.catalog,
        };
        this.subscribe_permission_inputs(window, cx);
        this.sync_rows(window, cx);
        this.start_live_refresh(cx);
        this
    }

    /// Opens a real properties surface on a non-mutating requested page.
    /// Invalid pages remain on General; capability-bearing pages are still
    /// gated by the loaded snapshot rather than being forced into existence.
    pub(crate) fn with_hub_page(
        data: PropertiesWindowData,
        operation_hub: OperationHub,
        initial_page: PropertiesPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::with_hub(data, operation_hub, window, cx);
        let _ = this.model.select_page(initial_page);
        this
    }

    fn subscribe_permission_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let owner = self.permission_inputs.owner.clone();
        self.permission_subscriptions.push(cx.subscribe_in(
            &owner,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                this.model.permissions_mut().set_owner_text(&value);
                cx.notify();
            },
        ));
        let group = self.permission_inputs.group.clone();
        self.permission_subscriptions.push(cx.subscribe_in(
            &group,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                this.model.permissions_mut().set_group_text(&value);
                cx.notify();
            },
        ));
        let file_mode = self.permission_inputs.file_mode.clone();
        self.permission_subscriptions.push(cx.subscribe_in(
            &file_mode,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                this.model.permissions_mut().set_file_mode_text(&value);
                cx.notify();
            },
        ));
        let directory_mode = self.permission_inputs.directory_mode.clone();
        self.permission_subscriptions.push(cx.subscribe_in(
            &directory_mode,
            window,
            |this, input, event: &InputEvent, _, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value().to_string();
                this.model.permissions_mut().set_directory_mode_text(&value);
                cx.notify();
            },
        ));
    }

    fn sync_pristine_permission_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let permissions = self.model.permissions();
        if permissions.is_dirty() || permissions.edit_disabled_reason().is_some() {
            return;
        }
        let owner = editable_u32(permissions.owner().clone());
        let group = editable_u32(permissions.group().clone());
        let mode = editable_mode(permissions.mode().clone());
        for (input, value) in [
            (&self.permission_inputs.owner, owner),
            (&self.permission_inputs.group, group),
            (&self.permission_inputs.file_mode, mode.clone()),
            (&self.permission_inputs.directory_mode, mode),
        ] {
            if input.read(cx).value().as_ref() != value {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
    }

    fn apply_permissions(&mut self, cx: &mut Context<Self>) {
        if self.permission_batch.is_active() {
            return;
        }
        let submitted: Result<Vec<JobId>, Box<str>> = self
            .model
            .permission_request()
            .map_err(|error| error.to_string().into())
            .and_then(|(roots, scope, change)| {
                self.operation_hub
                    .submit_metadata_changes(roots, scope, change)
                    .map_err(|error| error.to_string().into())
            });
        match submitted {
            Ok(jobs) => {
                self.permission_error = self.operation_hub.persistence_error();
                self.permission_batch.begin(jobs);
                self.pump_operation_queue(cx);
            }
            Err(error) => {
                self.permission_error = Some(error);
                cx.notify();
            }
        }
    }

    fn pump_operation_queue(&mut self, cx: &mut Context<Self>) {
        let result = spawn_ready_hub_operations(
            self.operation_hub.clone(),
            cx,
            |state: &mut Self, id, outcome, error, cx| {
                let succeeded = outcome.is_some() && error.is_none();
                let outcome = state.permission_batch.finish(id, succeeded);
                if outcome == PermissionBatchOutcome::Ignored {
                    return;
                }
                if let Some(error) = error {
                    state.permission_error = Some(error);
                }
                if outcome == PermissionBatchOutcome::Succeeded {
                    state.model.clear_permission_edits();
                }
                state.pump_operation_queue(cx);
                cx.notify();
            },
        );
        if let Err(error) = result {
            self.permission_error = Some(error.to_string().into());
            cx.notify();
        }
    }

    fn start_live_refresh(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(LIVE_REFRESH_INTERVAL).await;
                let Some(this) = this.upgrade() else {
                    return;
                };
                let work = this.update(cx, Self::begin_live_refresh);
                let Some(work) = work else {
                    continue;
                };
                let result = work.await;
                this.update(cx, |state, cx| state.finish_live_refresh(result, cx));
            }
        })
        .detach();
    }

    fn begin_live_refresh(&mut self, cx: &mut Context<Self>) -> Option<Task<RefreshWorkResult>> {
        if self.refreshing
            || matches!(
                self.model.state(),
                PropertiesState::Missing | PropertiesState::Replaced
            )
        {
            return None;
        }

        self.refreshing = true;
        let probe = self.model.snapshot().refresh_probe();
        let paths = self
            .model
            .snapshot()
            .items()
            .iter()
            .map(|item| item.path().to_path_buf())
            .collect::<Vec<_>>();
        Some(cx.background_spawn(async move {
            let refresh = probe.refresh_state()?;
            let replacement = if refresh == PropertyRefresh::MetadataChanged {
                Some(PropertySnapshot::load(&paths)?)
            } else {
                None
            };
            Ok((refresh, replacement))
        }))
    }

    fn finish_live_refresh(&mut self, result: RefreshWorkResult, cx: &mut Context<Self>) {
        self.refreshing = false;
        match result {
            Ok((refresh, replacement)) => {
                if refresh != PropertyRefresh::Current
                    && matches!(self.checksum, ChecksumState::Ready(_))
                {
                    self.checksum = ChecksumState::Failed(
                        "Invalidated because the file changed after calculation".into(),
                    );
                }
                if let Some(snapshot) = replacement {
                    if self.model.permissions().is_dirty() {
                        self.model.state = PropertiesState::Replaced;
                        self.refresh_error = Some(
                              "The selected item changed while you were editing. Close and reopen Properties."
                                  .into(),
                          );
                    } else {
                        self.model.replace_snapshot(snapshot);
                        self.permission_inputs_need_sync = true;
                        self.refresh_error = None;
                    }
                } else {
                    self.model.state = properties_state(refresh);
                    self.refresh_error = None;
                }
            }
            Err(error) => self.refresh_error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn select_page(&mut self, page: PropertiesPage, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.select_page(page).is_ok() {
            self.sync_rows(window, cx);
            cx.notify();
        }
    }

    fn close(&mut self, window: &mut Window) {
        self.close_requested = true;
        window.remove_window();
    }

    fn start_recursive_size(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.single_directory_path() else {
            return;
        };
        if let Some(cancellation) = self.recursive_cancellation.take() {
            cancellation.cancel();
        }
        let cancellation = CancellationToken::new();
        self.recursive_cancellation = Some(cancellation.clone());
        self.recursive_size = RecursiveSizeState::Running;
        let work =
            cx.background_spawn(async move { RecursiveSize::calculate(&path, cancellation) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                state.recursive_cancellation = None;
                state.recursive_size = match result {
                    Ok(size) => RecursiveSizeState::Ready(size),
                    Err(error) => RecursiveSizeState::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn start_checksum(&mut self, algorithm: ChecksumAlgorithm, cx: &mut Context<Self>) {
        let Some(path) = self.single_regular_file_path() else {
            return;
        };
        if let Some(cancellation) = self.checksum_cancellation.take() {
            cancellation.cancel();
        }
        let cancellation = CancellationToken::new();
        self.checksum_cancellation = Some(cancellation.clone());
        self.checksum = ChecksumState::Running(algorithm);
        let work =
            cx.background_spawn(
                async move { ChecksumService::compute(&path, algorithm, cancellation) },
            );
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                state.checksum_cancellation = None;
                state.checksum = match result {
                    Ok(checksum) => ChecksumState::Ready(checksum),
                    Err(error) => ChecksumState::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn single_directory_path(&self) -> Option<PathBuf> {
        let [item] = self.model.snapshot().items() else {
            return None;
        };
        (item.kind() == ItemKind::Directory).then(|| item.path().to_path_buf())
    }

    fn single_regular_file_path(&self) -> Option<PathBuf> {
        let [item] = self.model.snapshot().items() else {
            return None;
        };
        (item.kind() == ItemKind::RegularFile).then(|| item.path().to_path_buf())
    }

    fn desired_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        match self.model.page() {
            PropertiesPage::General => self.general_rows(),
            PropertiesPage::Permissions => self.permission_rows(),
            PropertiesPage::OpenWith => self.open_with_rows(),
            PropertiesPage::Tags => self.tag_rows(),
            PropertiesPage::Checksums => self.checksum_rows(),
        }
    }

    fn message(&self, key: &str) -> Box<str> {
        self.catalog
            .message(key)
            .expect("the local Properties catalog message exists")
            .into()
    }

    fn localized_value(&self, value: impl AsRef<str>) -> Box<str> {
        self.catalog.localize_reason(value.as_ref()).into()
    }

    fn sync_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let desired = self.desired_rows();
        if desired.len() != self.rows.len()
            || desired
                .iter()
                .zip(&self.rows)
                .any(|((label, _), row)| label != &row.label)
        {
            self.rows = desired
                .into_iter()
                .map(|(label, value)| PropertyRow {
                    label,
                    input: cx
                        .new(|cx| InputState::new(window, cx).default_value(value.to_string())),
                    value,
                })
                .collect();
            return;
        }
        for ((_, value), row) in desired.into_iter().zip(&mut self.rows) {
            if value != row.value {
                row.input.update(cx, |input, cx| {
                    input.set_value(value.to_string(), window, cx)
                });
                row.value = value;
            }
        }
    }

    fn general_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        let snapshot = self.model.snapshot();
        let aggregate = snapshot.aggregate();
        let mut rows = vec![
            (
                self.message("properties-items"),
                snapshot.items().len().to_string().into(),
            ),
            (
                self.message("properties-type"),
                self.localized_value(aggregate_kind(aggregate.kind())),
            ),
            (
                self.message("properties-mime-type"),
                self.localized_value(aggregate_text(aggregate.mime_type())),
            ),
            (
                self.message("properties-location"),
                aggregate_location(snapshot).into(),
            ),
            (
                self.message("properties-identity"),
                self.localized_value(aggregate_identity(snapshot)),
            ),
            (
                self.message("properties-logical-size"),
                format_size(aggregate.logical_size()).into(),
            ),
            (
                self.message("properties-allocated-size"),
                format_size(aggregate.allocated_size()).into(),
            ),
            (
                self.message("properties-modified"),
                self.localized_value(aggregate_time(aggregate.modified())),
            ),
            (
                self.message("properties-accessed"),
                self.localized_value(aggregate_time(aggregate.accessed())),
            ),
            (
                self.message("properties-metadata-changed"),
                self.localized_value(aggregate_time(aggregate.changed())),
            ),
        ];
        if self.single_directory_path().is_some() {
            rows.push((
                self.message("properties-contained-items"),
                self.localized_value(recursive_size_label(&self.recursive_size)),
            ));
        }
        rows.extend(
            self.filesystem_rows
                .iter()
                .map(|(label, value)| (self.localized_value(label), self.localized_value(value))),
        );
        rows.extend(self.capability_rows.iter().map(|(label, value)| {
            (
                localize_property_label(&self.catalog, label).into(),
                localize_property_value(&self.catalog, value).into(),
            )
        }));
        rows
    }

    fn permission_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        let permissions = self.model.permissions();
        let mut rows = vec![
            (
                self.message("properties-owner"),
                self.localized_value(aggregate_u32(permissions.owner())),
            ),
            (
                self.message("properties-group"),
                self.localized_value(aggregate_u32(permissions.group())),
            ),
            (
                self.message("properties-mode"),
                self.localized_value(aggregate_mode(permissions.mode())),
            ),
            (
                self.message("properties-editing"),
                self.localized_value(
                    permissions
                        .edit_disabled_reason()
                        .unwrap_or("Ready to edit"),
                ),
            ),
        ];
        for (index, item) in self.model.snapshot().items().iter().enumerate() {
            rows.push((
                indexed_label(
                    self.catalog
                        .message("properties-access-acl")
                        .expect("the access ACL message exists"),
                    index,
                    self.model.snapshot().items().len(),
                )
                .into(),
                self.localized_value(acl_state_label(item.permissions().acl())),
            ));
            if let Some(default_acl) = item.permissions().default_acl() {
                rows.push((
                    indexed_label(
                        self.catalog
                            .message("properties-default-acl")
                            .expect("the default ACL message exists"),
                        index,
                        self.model.snapshot().items().len(),
                    )
                    .into(),
                    self.localized_value(acl_state_label(default_acl)),
                ));
            }
        }
        rows
    }

    fn open_with_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        vec![
            (
                self.message("properties-mime-type"),
                self.localized_value(aggregate_text(
                    self.model.snapshot().aggregate().mime_type(),
                )),
            ),
            (
                self.message("properties-association"),
                self.message("properties-association-help"),
            ),
        ]
    }

    fn tag_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        let mut rows = self
            .model
            .tags()
            .map(|tag| (self.message("properties-tag"), Box::<str>::from(tag)))
            .collect::<Vec<_>>();
        for (item_index, item) in self.model.snapshot().items().iter().enumerate() {
            match item.xattrs() {
                XattrState::Available(entries) if entries.is_empty() => rows.push((
                    indexed_label(
                        self.catalog
                            .message("properties-extended-attributes")
                            .expect("the extended-attributes message exists"),
                        item_index,
                        self.model.snapshot().items().len(),
                    )
                    .into(),
                    self.message("properties-none"),
                )),
                XattrState::Available(entries) => {
                    for entry in entries {
                        rows.push((
                            format!(
                                "{}: {}",
                                indexed_label(
                                    self.catalog
                                        .message("properties-attribute")
                                        .expect("the attribute message exists"),
                                    item_index,
                                    self.model.snapshot().items().len()
                                ),
                                display_os(entry.name())
                            )
                            .into(),
                            display_bytes(entry.value()).into(),
                        ));
                    }
                }
                XattrState::Unavailable(reason) => rows.push((
                    indexed_label(
                        self.catalog
                            .message("properties-extended-attributes")
                            .expect("the extended-attributes message exists"),
                        item_index,
                        self.model.snapshot().items().len(),
                    )
                    .into(),
                    format!(
                        "{}: {reason}",
                        self.catalog
                            .message("properties-unavailable")
                            .expect("the unavailable message exists")
                    )
                    .into(),
                )),
            }
        }
        rows
    }

    fn checksum_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        match &self.checksum {
            ChecksumState::Idle => vec![(
                self.message("properties-checksum"),
                self.message("properties-not-calculated"),
            )],
            ChecksumState::Running(algorithm) => vec![(
                self.message("properties-checksum"),
                format!(
                    "{} {}",
                    self.catalog
                        .message("properties-calculating")
                        .expect("the calculating message exists"),
                    algorithm.label()
                )
                .into(),
            )],
            ChecksumState::Failed(error) => {
                vec![(
                    self.message("properties-checksum"),
                    format!(
                        "{}: {error}",
                        self.catalog
                            .message("properties-failed")
                            .expect("the failed message exists")
                    )
                    .into(),
                )]
            }
            ChecksumState::Ready(result) => {
                let fingerprint = result.fingerprint();
                vec![
                    (
                        self.message("properties-algorithm"),
                        result.algorithm().label().into(),
                    ),
                    (
                        self.message("properties-digest"),
                        result.hex_digest().into(),
                    ),
                    (
                        self.message("properties-stable-identity"),
                        format!("{}:{}", fingerprint.device(), fingerprint.inode()).into(),
                    ),
                    (
                        self.message("properties-size-at-read"),
                        format_size(fingerprint.size()).into(),
                    ),
                    (
                        self.message("properties-modified-at-read"),
                        format_timestamp(
                            fingerprint.modified_seconds(),
                            fingerprint.modified_nanoseconds(),
                        )
                        .into(),
                    ),
                ]
            }
        }
    }

    fn render_page_navigation(&self, cx: &mut Context<Self>) -> AnyElement {
        let buttons = self
            .model
            .pages()
            .iter()
            .copied()
            .map(|page| {
                Button::new(SharedString::from(format!(
                    "properties-page-{}",
                    page_id(page)
                )))
                .label(provider_page_label(page, &self.catalog))
                .selected(self.model.page() == page)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.select_page(page, window, cx);
                }))
            })
            .collect::<Vec<_>>();
        div()
            .id("properties-pages")
            .test_support()
            .role(Role::TabList)
            .aria_label(
                self.catalog
                    .message("properties-pages")
                    .expect("the Properties pages message exists"),
            )
            .w(px(180.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_stretch()
            .gap_1()
            .p_3()
            .border_r_1()
            .border_color(cx.theme().colors.border)
            .children(buttons)
            .into_any_element()
    }

    fn render_rows(&self) -> AnyElement {
        div()
            .id("properties-values")
            .test_support()
            .role(Role::Region)
            .aria_label(format!(
                "{} {}",
                self.catalog
                    .message("properties-for")
                    .expect("the Properties message exists"),
                provider_page_label(self.model.page(), &self.catalog)
            ))
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .children(self.rows.iter().enumerate().map(|(index, row)| {
                let input_id = SharedString::from(format!("property-value-{index}"));
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_xs().child(row.label.to_string()))
                    .child(
                        Input::new(&row.input)
                            .id(input_id)
                            .aria_label(row.label.to_string())
                            .readonly(true)
                            .small(),
                    )
            }))
            .into_any_element()
    }

    fn render_permission_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let disabled = self.model.state() != PropertiesState::Ready;
        let scope = self.model.permissions().scope();
        let recursive = scope.is_recursive();
        let review_needed = recursive && !scope.is_reviewed();
        let field = |label: String, id: &'static str, input: &Entity<InputState>| {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().child(label.clone()))
                .child(
                    Input::new(input)
                        .id(id)
                        .aria_label(label)
                        .disabled(disabled)
                        .small(),
                )
        };
        div()
            .id("permissions-editor")
            .test_support()
            .role(Role::Region)
            .aria_label(
                self.catalog
                    .message("properties-permission-editor")
                    .expect("the permission-editor message exists"),
            )
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(field(
                self.catalog
                    .message("properties-owner")
                    .expect("the owner message exists")
                    .to_owned(),
                "permissions-owner",
                &self.permission_inputs.owner,
            ))
            .child(field(
                self.catalog
                    .message("properties-group")
                    .expect("the group message exists")
                    .to_owned(),
                "permissions-group",
                &self.permission_inputs.group,
            ))
            .child(field(
                self.catalog
                    .message("properties-file-mode")
                    .expect("the file-mode message exists")
                    .to_owned(),
                "permissions-file-mode",
                &self.permission_inputs.file_mode,
            ))
            .child(field(
                self.catalog
                    .message("properties-directory-mode")
                    .expect("the directory-mode message exists")
                    .to_owned(),
                "permissions-directory-mode",
                &self.permission_inputs.directory_mode,
            ))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new("permissions-scope-single")
                            .label(
                                self.catalog
                                    .message("properties-selected-only")
                                    .expect("the selected-only message exists"),
                            )
                            .selected(!recursive)
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.permissions_mut().set_single();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("permissions-scope-recursive")
                            .label(
                                self.catalog
                                    .message("properties-include-descendants")
                                    .expect("the descendants message exists"),
                            )
                            .selected(recursive && !scope.includes_nested_mounts())
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.permissions_mut().set_recursive(false);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("permissions-scope-mounts")
                            .label(
                                self.catalog
                                    .message("properties-include-nested-mounts")
                                    .expect("the nested-mounts message exists"),
                            )
                            .selected(recursive && scope.includes_nested_mounts())
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.permissions_mut().set_recursive(true);
                                cx.notify();
                            })),
                    ),
            )
            .when(review_needed, |editor| {
                editor.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(div().text_sm().child(if scope.includes_nested_mounts() {
                            self.catalog
                                .message("properties-review-nested")
                                .expect("the nested review message exists")
                                .to_owned()
                        } else {
                            self.catalog
                                .message("properties-review-descendants")
                                .expect("the descendant review message exists")
                                .to_owned()
                        }))
                        .child(
                            Button::new("permissions-review-scope")
                                .label(
                                    self.catalog
                                        .message("properties-reviewed-scope")
                                        .expect("the reviewed-scope message exists"),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.model.permissions_mut().review_recursive_scope();
                                    cx.notify();
                                })),
                        ),
                )
            })
            .when_some(
                self.model
                    .permissions()
                    .edit_disabled_reason()
                    .or(self.permission_error.as_deref()),
                |editor, message| {
                    editor.child(
                        div()
                            .id("permissions-validation")
                            .test_support()
                            .role(Role::Alert)
                            .aria_label(message.to_owned())
                            .child(message.to_owned()),
                    )
                },
            )
            .child(div().mt_2().child(self.render_rows()))
            .into_any_element()
    }

    fn render_tag_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let tags = self.model.tags().map(str::to_owned).collect::<Vec<_>>();
        let mixed_tags = self
            .model
            .mixed_tags()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let tag_editor = self
            .catalog
            .message("catalog-tag-editor")
            .expect("the tag-editor catalog message exists");
        let tag_name = self
            .catalog
            .message("catalog-tag-name")
            .expect("the tag-name catalog message exists");
        let add_tag = self
            .catalog
            .message("provider-properties-add-tag")
            .expect("the add-tag catalog message exists");
        let remove_label = self
            .catalog
            .message("provider-properties-remove")
            .expect("the remove catalog message exists")
            .to_owned();
        let some_items = self
            .catalog
            .message("provider-properties-some-items")
            .expect("the some-items catalog message exists")
            .to_owned();
        div()
            .id("properties-tag-editor")
            .test_support()
            .role(Role::Region)
            .aria_label(tag_editor)
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Input::new(&self.tag_input)
                            .id("properties-tag-input")
                            .aria_label(tag_name),
                    )
                    .child(
                        Button::new("properties-tag-add")
                            .label(add_tag)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let tag = this.tag_input.read(cx).value().to_string();
                                match this.model.assign_tag(&tag) {
                                    Ok(_) => {
                                        this.tag_error = None;
                                        this.tag_input.update(cx, |input, cx| {
                                            input.set_value("", window, cx);
                                        });
                                    }
                                    Err(error) => this.tag_error = Some(error.to_string().into()),
                                }
                                cx.notify();
                            })),
                    ),
            )
            .children(tags.into_iter().enumerate().map(|(index, tag)| {
                let remove = tag.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .when(self.catalog.locale() == crate::Locale::Ar, |row| {
                        row.flex_row_reverse()
                    })
                    .child(tag)
                    .child(
                        Button::new(SharedString::from(format!("properties-tag-remove-{index}")))
                            .label(remove_label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model.remove_tag(&remove);
                                cx.notify();
                            })),
                    )
            }))
            .children(mixed_tags.into_iter().enumerate().map(|(index, tag)| {
                let remove = tag.clone();
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .when(self.catalog.locale() == crate::Locale::Ar, |row| {
                        row.flex_row_reverse()
                    })
                    .child(format!("{tag} ({some_items})"))
                    .child(
                        Button::new(SharedString::from(format!(
                            "properties-tag-remove-mixed-{index}"
                        )))
                        .label(remove_label.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.remove_tag(&remove);
                            cx.notify();
                        })),
                    )
            }))
            .when_some(self.tag_error.as_deref(), |editor, error| {
                let error = self.catalog.localize_reason(error);
                editor.child(
                    div()
                        .id("properties-tag-error")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(error.clone())
                        .child(error),
                )
            })
            .child(self.render_rows())
            .into_any_element()
    }

    fn apply_tags(&mut self, cx: &mut Context<Self>) {
        let Some(writer) = self.tag_writer.as_ref() else {
            self.tag_error = Some(
                self.catalog
                    .message("provider-properties-tag-storage-unavailable")
                    .expect("the tag-storage catalog message exists")
                    .into(),
            );
            cx.notify();
            return;
        };
        let delta = TagDelta {
            added: self.model.added_tags.clone(),
            removed: self.model.removed_tags.clone(),
        };
        match writer(&delta, cx) {
            Ok(()) => {
                self.model.accept_tags();
                self.tag_error = None;
            }
            Err(error) => self.tag_error = Some(error),
        }
        cx.notify();
    }

    fn render_page_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut actions = div().flex().items_center().gap_2();
        if self.model.page() == PropertiesPage::Permissions
            && self.model.apply_visible()
            && !self.permission_batch.is_active()
        {
            actions = actions.child(
                Button::new("properties-apply")
                    .label(
                        self.catalog
                            .message("properties-apply")
                            .expect("the apply message exists"),
                    )
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| this.apply_permissions(cx))),
            );
        }
        if self.model.page() == PropertiesPage::Tags
            && self.model.tags_dirty()
            && self.tag_writer.is_some()
        {
            actions = actions.child(
                Button::new("properties-tags-apply")
                    .label(
                        self.catalog
                            .message("catalog-apply-tags")
                            .expect("the apply-tags catalog message exists"),
                    )
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| this.apply_tags(cx))),
            );
        }
        if self.model.page() == PropertiesPage::General && self.single_directory_path().is_some() {
            actions = actions.child(
                Button::new("properties-calculate-size")
                    .label(
                        if matches!(self.recursive_size, RecursiveSizeState::Running) {
                            self.catalog
                                .message("properties-calculating")
                                .expect("the calculating message exists")
                        } else {
                            self.catalog
                                .message("properties-calculate-size")
                                .expect("the calculate-size message exists")
                        },
                    )
                    .disabled(matches!(self.recursive_size, RecursiveSizeState::Running))
                    .on_click(cx.listener(|this, _, _, cx| this.start_recursive_size(cx))),
            );
        }
        if self.model.page() == PropertiesPage::Checksums
            && self.single_regular_file_path().is_some()
        {
            actions = actions
                .child(
                    Button::new("properties-checksum-blake3")
                        .label(
                            self.catalog
                                .message("properties-calculate-blake3")
                                .expect("the BLAKE3 message exists"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_checksum(ChecksumAlgorithm::Blake3, cx);
                        })),
                )
                .child(
                    Button::new("properties-checksum-sha256")
                        .label(
                            self.catalog
                                .message("properties-calculate-sha256")
                                .expect("the SHA-256 message exists"),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_checksum(ChecksumAlgorithm::Sha256, cx);
                        })),
                );
        }
        actions.into_any_element()
    }
}

impl Drop for PropertiesWindow {
    fn drop(&mut self) {
        if let Some(cancellation) = self.recursive_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(cancellation) = self.checksum_cancellation.take() {
            cancellation.cancel();
        }
    }
}

impl Render for PropertiesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.focus.focus(window, cx);
            self.pending_focus = false;
        }
        if self.permission_inputs_need_sync {
            self.sync_pristine_permission_inputs(window, cx);
            self.permission_inputs_need_sync = false;
        }
        self.sync_rows(window, cx);
        let title = identity_title(self.model.snapshot());
        let location = aggregate_location(self.model.snapshot());
        let state_message = match self.model.state() {
            PropertiesState::Ready => self.refresh_error.as_deref(),
            PropertiesState::Replaced => self
                .refresh_error
                .as_deref()
                .or_else(|| self.catalog.message("properties-replaced").ok()),
            PropertiesState::Missing => self.catalog.message("properties-missing").ok(),
        };
        let properties_for = self
            .catalog
            .message("properties-for")
            .expect("the Properties title message exists");
        let colors = cx.theme().colors;
        div()
            .id("properties-dialog")
            .test_support()
            .key_context("PropertiesWindow")
            .role(Role::Dialog)
            .aria_label(format!("{properties_for} {title}"))
            .track_focus(&self.focus)
            .tab_index(0)
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.foreground)
            .on_action(cx.listener(|this, _: &ConfirmProperties, window, _| {
                this.close(window);
            }))
            .on_action(cx.listener(|this, _: &CancelProperties, window, _| {
                this.close(window);
            }))
            .on_action(cx.listener(|this, _: &Escape, window, _| {
                this.close(window);
            }))
            .child(
                div()
                    .id("properties-identity")
                    .test_support()
                    .role(Role::Heading)
                    .aria_label(format!("{properties_for} {title}, {location}"))
                    .p_4()
                    .border_b_1()
                    .border_color(colors.border)
                    .child(div().text_lg().child(title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(colors.muted_foreground)
                            .child(location),
                    ),
            )
            .when_some(state_message, |dialog, message| {
                dialog.child(
                    div()
                        .id("properties-validation-summary")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(message.to_owned())
                        .px_4()
                        .py_2()
                        .child(message.to_owned()),
                )
            })
            .child(
                div()
                    .flex_grow(1.0)
                    .min_h(px(0.))
                    .flex()
                    .child(self.render_page_navigation(cx))
                    .child(
                        div()
                            .flex_grow(1.0)
                            .min_w(px(0.))
                            .p_4()
                            .overflow_x_hidden()
                            .child(if self.model.page() == PropertiesPage::Permissions {
                                self.render_permission_editor(cx)
                            } else if self.model.page() == PropertiesPage::Tags {
                                self.render_tag_editor(cx)
                            } else {
                                self.render_rows()
                            })
                            .overflow_y_scrollbar(),
                    ),
            )
            .child(
                div()
                    .id("properties-actions")
                    .test_support()
                    .role(Role::Toolbar)
                    .aria_label(
                        self.catalog
                            .message("properties-actions")
                            .expect("the Properties actions message exists"),
                    )
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(colors.border)
                    .child(self.render_page_actions(cx))
                    .child(
                        Button::new("properties-close")
                            .label(
                                self.catalog
                                    .message("properties-close")
                                    .expect("the close message exists"),
                            )
                            .primary()
                            .on_click(cx.listener(|this, _, window, _| this.close(window))),
                    ),
            )
    }
}

fn aggregate_named_rows(rows: &[Vec<(&'static str, String)>]) -> Vec<(Box<str>, Box<str>)> {
    let Some(first) = rows.first() else {
        return Vec::new();
    };
    first
        .iter()
        .enumerate()
        .map(|(index, (label, _))| {
            let values = rows
                .iter()
                .filter_map(|row| row.get(index).map(|(_, value)| value.clone()))
                .collect::<Vec<_>>();
            ((*label).into(), aggregate_strings(&values).into())
        })
        .collect()
}

fn aggregate_strings(values: &[String]) -> String {
    let Some(first) = values.first() else {
        return "Unavailable".to_owned();
    };
    if values.iter().all(|value| value == first) {
        first.clone()
    } else {
        "Mixed".to_owned()
    }
}

fn provider_aggregate_strings(values: &[String], catalog: &Catalog) -> String {
    let Some(first) = values.first() else {
        return catalog
            .message("provider-properties-unavailable")
            .expect("the unavailable catalog message exists")
            .to_owned();
    };
    if values.iter().all(|value| value == first) {
        first.clone()
    } else {
        catalog
            .message("provider-properties-mixed")
            .expect("the mixed catalog message exists")
            .to_owned()
    }
}

fn provider_capability_state_label(state: &CapabilityState, catalog: &Catalog) -> String {
    match state {
        CapabilityState::Supported => catalog
            .message("provider-properties-supported")
            .expect("the supported catalog message exists")
            .to_owned(),
        CapabilityState::Unsupported(reason) => format!(
            "{}: {}",
            catalog
                .message("provider-properties-unsupported")
                .expect("the unsupported catalog message exists"),
            reason.as_str()
        ),
        CapabilityState::Unknown(reason) => format!(
            "{}: {}",
            catalog
                .message("provider-properties-unknown")
                .expect("the unknown catalog message exists"),
            reason.as_str()
        ),
    }
}

fn provider_capability_kind_label(kind: CapabilityKind, catalog: &Catalog) -> String {
    let key = match kind {
        CapabilityKind::Permissions => "provider-capability-permissions",
        CapabilityKind::Ownership => "provider-capability-ownership",
        CapabilityKind::SymbolicLinks => "provider-capability-symbolic-links",
        CapabilityKind::HardLinks => "provider-capability-hard-links",
        CapabilityKind::SparseFiles => "provider-capability-sparse-files",
        CapabilityKind::ExtendedAttributes => "provider-capability-extended-attributes",
        CapabilityKind::Tags => "provider-capability-tags",
        CapabilityKind::ReflinkCopies => "provider-capability-reflink-copies",
        CapabilityKind::Trash => "provider-capability-trash",
        CapabilityKind::AtomicRename => "provider-capability-atomic-rename",
        CapabilityKind::Watching => "provider-capability-watching",
        CapabilityKind::CaseSensitivity => "provider-capability-case-sensitivity",
    };
    catalog
        .message(key)
        .expect("the provider capability catalog message exists")
        .to_owned()
}

fn capability_state_label(state: &CapabilityState) -> String {
    match state {
        CapabilityState::Supported => "Supported".to_owned(),
        CapabilityState::Unsupported(reason) => format!("Unsupported: {}", reason.as_str()),
        CapabilityState::Unknown(reason) => format!("Unknown: {}", reason.as_str()),
    }
}

fn capability_kind_label(kind: CapabilityKind) -> &'static str {
    match kind {
        CapabilityKind::Permissions => "Permissions",
        CapabilityKind::Ownership => "Ownership",
        CapabilityKind::SymbolicLinks => "Symbolic links",
        CapabilityKind::HardLinks => "Hard links",
        CapabilityKind::SparseFiles => "Sparse files",
        CapabilityKind::ExtendedAttributes => "Extended attributes",
        CapabilityKind::Tags => "Tags",
        CapabilityKind::ReflinkCopies => "Reflink copies",
        CapabilityKind::Trash => "Trash",
        CapabilityKind::AtomicRename => "Atomic rename",
        CapabilityKind::Watching => "Watching",
        CapabilityKind::CaseSensitivity => "Case sensitivity",
    }
}

fn page_id(page: PropertiesPage) -> &'static str {
    match page {
        PropertiesPage::General => "general",
        PropertiesPage::Permissions => "permissions",
        PropertiesPage::OpenWith => "open-with",
        PropertiesPage::Tags => "tags",
        PropertiesPage::Checksums => "checksums",
    }
}

fn page_label(page: PropertiesPage) -> &'static str {
    match page {
        PropertiesPage::General => "General",
        PropertiesPage::Permissions => "Permissions",
        PropertiesPage::OpenWith => "Open With",
        PropertiesPage::Tags => "Tags",
        PropertiesPage::Checksums => "Checksums",
    }
}

fn provider_page_label(page: PropertiesPage, catalog: &Catalog) -> String {
    let key = match page {
        PropertiesPage::General => "properties-page-general",
        PropertiesPage::Permissions => "properties-page-permissions",
        PropertiesPage::OpenWith => "properties-page-open-with",
        PropertiesPage::Tags => "properties-page-tags",
        PropertiesPage::Checksums => "properties-page-checksums",
    };
    catalog
        .message(key)
        .unwrap_or_else(|_| page_label(page))
        .to_owned()
}

fn localize_property_label(catalog: &Catalog, label: &str) -> String {
    if let Some(kind) = label.strip_prefix("Capability: ") {
        return format!(
            "{}: {}",
            catalog
                .message("properties-capability")
                .expect("the capability message exists"),
            catalog.localize_reason(kind)
        );
    }
    catalog.localize_reason(label)
}

fn localize_property_value(catalog: &Catalog, value: &str) -> String {
    for (prefix, key) in [
        ("Unsupported: ", "properties-unsupported"),
        ("Unknown: ", "properties-unknown"),
        ("Unavailable: ", "properties-unavailable"),
        ("Failed: ", "properties-failed"),
    ] {
        if let Some(reason) = value.strip_prefix(prefix) {
            return format!(
                "{}: {reason}",
                catalog
                    .message(key)
                    .expect("the property-state catalog message exists")
            );
        }
    }
    catalog.localize_reason(value)
}

fn identity_title(snapshot: &PropertySnapshot) -> String {
    if let [item] = snapshot.items() {
        item.path()
            .file_name()
            .map(display_os)
            .unwrap_or_else(|| display_os(item.path().as_os_str()))
    } else {
        format!("{} items", snapshot.items().len())
    }
}

fn aggregate_location(snapshot: &PropertySnapshot) -> String {
    if let [item] = snapshot.items() {
        item.path()
            .parent()
            .map(|path| display_os(path.as_os_str()))
            .unwrap_or_else(|| display_os(item.path().as_os_str()))
    } else {
        aggregate_strings(
            &snapshot
                .items()
                .iter()
                .map(|item| {
                    item.path()
                        .parent()
                        .map(|path| display_os(path.as_os_str()))
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>(),
        )
    }
}

fn aggregate_identity(snapshot: &PropertySnapshot) -> String {
    if let [item] = snapshot.items() {
        format!("{}:{}", item.identity().device(), item.identity().inode())
    } else {
        "Multiple stable identities".to_owned()
    }
}

fn aggregate_kind(value: AggregateValue<ItemKind>) -> String {
    match value {
        AggregateValue::Same(kind) => match kind {
            ItemKind::RegularFile => "File",
            ItemKind::Directory => "Folder",
            ItemKind::SymbolicLink => "Symbolic link",
            ItemKind::Other => "Other",
        }
        .to_owned(),
        AggregateValue::Mixed => "Mixed".to_owned(),
        AggregateValue::Unavailable => "Unavailable".to_owned(),
    }
}

fn aggregate_text(value: AggregateValue<Box<str>>) -> String {
    match value {
        AggregateValue::Same(value) => value.into(),
        AggregateValue::Mixed => "Mixed".to_owned(),
        AggregateValue::Unavailable => "Unavailable".to_owned(),
    }
}

fn aggregate_u32(value: &AggregateValue<u32>) -> String {
    match value {
        AggregateValue::Same(value) => value.to_string(),
        AggregateValue::Mixed => "Mixed".to_owned(),
        AggregateValue::Unavailable => "Unavailable".to_owned(),
    }
}

fn editable_u32(value: AggregateValue<u32>) -> String {
    match value {
        AggregateValue::Same(value) => value.to_string(),
        AggregateValue::Mixed | AggregateValue::Unavailable => String::new(),
    }
}

fn aggregate_mode(value: &AggregateValue<u32>) -> String {
    match value {
        AggregateValue::Same(value) => format!("{value:04o}"),
        AggregateValue::Mixed => "Mixed".to_owned(),
        AggregateValue::Unavailable => "Unavailable".to_owned(),
    }
}

fn editable_mode(value: AggregateValue<u32>) -> String {
    match value {
        AggregateValue::Same(value) => format!("{value:04o}"),
        AggregateValue::Mixed | AggregateValue::Unavailable => String::new(),
    }
}

fn aggregate_time(value: AggregateValue<PropertyTimestamp>) -> String {
    match value {
        AggregateValue::Same(value) => format_timestamp(value.seconds(), value.nanoseconds()),
        AggregateValue::Mixed => "Mixed".to_owned(),
        AggregateValue::Unavailable => "Unavailable".to_owned(),
    }
}

fn format_timestamp(seconds: i64, nanoseconds: i64) -> String {
    format!("{seconds}.{nanoseconds:09} Unix seconds")
}

fn recursive_size_label(state: &RecursiveSizeState) -> String {
    match state {
        RecursiveSizeState::Idle => "Not calculated".to_owned(),
        RecursiveSizeState::Running => "Calculating…".to_owned(),
        RecursiveSizeState::Failed(error) => format!("Failed: {error}"),
        RecursiveSizeState::Ready(size) => format!(
            "{} files, {} folders; {} logical, {} allocated{}",
            size.file_count(),
            size.directory_count(),
            format_size(size.logical_bytes()),
            format_size(size.allocated_bytes()),
            if size.errors().is_empty() && size.dropped_error_count() == 0 {
                String::new()
            } else {
                format!(
                    "; {} errors",
                    size.errors()
                        .len()
                        .saturating_add(size.dropped_error_count())
                )
            }
        ),
    }
}

fn acl_state_label(state: &AclState) -> String {
    match state {
        AclState::Available(entries) if entries.is_empty() => "No ACL entries".to_owned(),
        AclState::Available(entries) => entries
            .iter()
            .map(acl_entry_label)
            .collect::<Vec<_>>()
            .join(", "),
        AclState::Unsupported(reason) => format!("Unsupported: {reason}"),
        AclState::Unavailable(reason) => format!("Unavailable: {reason}"),
    }
}

fn acl_entry_label(entry: &AclEntry) -> String {
    let qualifier = match entry.qualifier() {
        AclQualifier::Owner => "owner".to_owned(),
        AclQualifier::OwningGroup => "group".to_owned(),
        AclQualifier::Other => "other".to_owned(),
        AclQualifier::User(id) => format!("user:{id}"),
        AclQualifier::Group(id) => format!("group:{id}"),
        AclQualifier::Mask => "mask".to_owned(),
        AclQualifier::Unknown => "unknown".to_owned(),
    };
    let permissions = [
        if entry.read() { 'r' } else { '-' },
        if entry.write() { 'w' } else { '-' },
        if entry.execute() { 'x' } else { '-' },
    ]
    .iter()
    .collect::<String>();
    format!("{qualifier}:{permissions}")
}

fn indexed_label(base: &str, index: usize, count: usize) -> String {
    if count == 1 {
        base.to_owned()
    } else {
        format!("{base} (item {})", index + 1)
    }
}

fn display_os(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

fn display_bytes(value: &[u8]) -> String {
    String::from_utf8(value.to_vec()).unwrap_or_else(|_| {
        value
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<Vec<_>>()
            .join("")
    })
}

fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {} ({bytes} bytes)", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::Root;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use standard_library::fs as filesystem;
    use std as standard_library;

    #[gpui_kit::test]
    async fn properties_failure_window_localizes_aria_and_close_at_200_percent(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let handle = cx.open_window(size(px(760.), px(620.)), |window, cx| {
            let view = cx.new(|cx| {
                PropertiesFailureWindow::new(
                    "provider unavailable",
                    Catalog::load(crate::Locale::Ar).unwrap(),
                    cx,
                )
            });
            Root::new(view, window, cx)
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.set_scale_factor(2.0);
            window.render_frame(cx);
            assert_eq!(window.scale_factor(), 2.0);
            assert_eq!(
                window.find("properties-dialog").label(),
                Some("تعذّر تحميل الخصائص")
            );
            assert_eq!(window.find("properties-close").label(), Some("إغلاق"));
        })
        .unwrap();
    }

    #[test]
    fn permission_batches_clear_edits_only_after_every_job_succeeds() {
        let mut batch = PermissionBatchState::default();
        let first = JobId::new(11).expect("non-zero job ID");
        let second = JobId::new(12).expect("non-zero job ID");
        let unrelated = JobId::new(99).expect("non-zero job ID");
        assert!(!batch.is_active());
        batch.begin([first, second]);
        assert!(batch.is_active());

        assert_eq!(
            batch.finish(unrelated, true),
            PermissionBatchOutcome::Ignored
        );
        assert_eq!(batch.finish(first, true), PermissionBatchOutcome::Pending);
        assert!(batch.is_active());
        assert_eq!(batch.finish(second, false), PermissionBatchOutcome::Failed);
        assert!(!batch.is_active());

        batch.begin([first, second]);
        assert_eq!(batch.finish(first, true), PermissionBatchOutcome::Pending);
        assert_eq!(
            batch.finish(second, true),
            PermissionBatchOutcome::Succeeded
        );
    }

    #[gpui_kit::test]
    async fn properties_window_is_semantic_read_only_and_keyboard_closable(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery/notes.txt");
        let data =
            PropertiesWindowData::load(std::slice::from_ref(&path)).expect("properties load");
        let mut properties = None;
        let handle = cx.open_window(size(px(760.), px(620.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            properties = Some(view.clone());
            Root::new(view, window, cx)
        });
        let properties = properties.expect("the Properties view is constructed");

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("properties-dialog").label(),
                Some("Properties for notes.txt")
            );
            assert!(window.find("properties-identity").visible());
            assert!(window.find("properties-pages").visible());
            assert!(window.find("properties-values").visible());
            assert!(window.find("properties-close").visible());

            let original = window
                .find("property-value-0")
                .value()
                .expect("a property value is exposed")
                .to_owned();
            window.click("property-value-0", cx);
            window.input("must not edit", cx);
            assert_eq!(
                window.find("property-value-0").value(),
                Some(original.as_str())
            );

            window.click("properties-page-permissions", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("properties-values").label(),
                Some("Properties for Permissions")
            );
            assert!(
                window
                    .find("property-value-4")
                    .value()
                    .is_some_and(|value| !value.is_empty()),
                "the access ACL is exposed as a readable value"
            );
            properties.update(cx, |state, cx| state.focus.focus(window, cx));
            window.render_frame(cx);
            window.press("escape", cx);
        })
        .expect("Properties window stays open through the interaction");
        cx.run_until_parked();
        assert!(cx.update(|cx| properties.read(cx).close_requested));

        let data = PropertiesWindowData::load(&[path]).expect("properties reload");
        let mut default_action_view = None;
        let default_action_handle = cx.open_window(size(px(760.), px(620.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            default_action_view = Some(view.clone());
            Root::new(view, window, cx)
        });
        let default_action_view =
            default_action_view.expect("the default-action Properties view is constructed");
        cx.update_window(default_action_handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press("enter", cx);
        })
        .expect("the default action is dispatched");
        assert!(cx.update(|cx| default_action_view.read(cx).close_requested));
    }

    #[gpui_kit::test]
    async fn local_properties_localizes_arabic_and_pseudo_chrome_at_200_percent(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery/notes.txt");
        let arabic = PropertiesWindowData::load(std::slice::from_ref(&path))
            .unwrap()
            .with_catalog(Catalog::load(crate::Locale::Ar).unwrap());
        let arabic_handle = cx.open_window(size(px(1520.), px(1240.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(arabic, window, cx));
            Root::new(view, window, cx)
        });
        cx.update_window(arabic_handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("properties-dialog").label(),
                Some("خصائص notes.txt")
            );
            assert_eq!(
                window.find("properties-page-permissions").label(),
                Some("الأذونات")
            );
            window.click("properties-page-permissions", cx);
            window.render_frame(cx);
            assert_eq!(
                window.find("properties-values").label(),
                Some("خصائص الأذونات")
            );
            assert_eq!(
                window.find("permissions-editor").label(),
                Some("محرر الأذونات والملكية")
            );
            assert_eq!(
                window.find("permissions-owner").label(),
                Some("المالك (UID)")
            );
            assert!(window.find("properties-close").visible());
        })
        .unwrap();

        let pseudo = PropertiesWindowData::load(&[path])
            .unwrap()
            .with_catalog(Catalog::load(crate::Locale::EnXa).unwrap());
        let pseudo_handle = cx.open_window(size(px(1520.), px(1240.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(pseudo, window, cx));
            Root::new(view, window, cx)
        });
        cx.update_window(pseudo_handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find("properties-dialog")
                    .label()
                    .is_some_and(|label| label.starts_with('⟦'))
            );
            assert!(
                window
                    .find("properties-page-general")
                    .label()
                    .is_some_and(|label| label.starts_with('⟦'))
            );
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn recursive_permission_edits_require_review_and_run_through_the_queue(
        cx: &mut TestAppContext,
    ) {
        use std::os::unix::fs::PermissionsExt;

        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let directory = temporary.path().join("folder");
        let child = directory.join("child.txt");
        filesystem::create_dir(&directory).unwrap();
        filesystem::write(&child, b"child").unwrap();
        filesystem::set_permissions(&directory, filesystem::Permissions::from_mode(0o755)).unwrap();
        filesystem::set_permissions(&child, filesystem::Permissions::from_mode(0o644)).unwrap();
        let data = PropertiesWindowData::load(std::slice::from_ref(&directory)).unwrap();
        let mut properties = None;
        let handle = cx.open_window(size(px(760.), px(620.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            properties = Some(view.clone());
            Root::new(view, window, cx)
        });
        let properties = properties.expect("the Properties view is constructed");

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("properties-page-permissions", cx);
            window.render_frame(cx);
            assert!(window.find("permissions-file-mode").visible());
            assert!(window.find("permissions-directory-mode").visible());
            window.click("permissions-file-mode", cx);
        })
        .expect("the Properties window remains open while focusing");
        cx.update(|cx| {
            assert!(
                !properties.read(cx).model.permissions().is_dirty(),
                "focusing a permission field must not dirty the plan"
            );
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.press("ctrl-a", cx);
            window.input("0600", cx);
            window.click("permissions-directory-mode", cx);
            window.press("ctrl-a", cx);
            window.input("0700", cx);
        })
        .expect("the Properties window remains open while editing");

        cx.update(|cx| {
            assert_eq!(
                properties
                    .read(cx)
                    .permission_inputs
                    .file_mode
                    .read(cx)
                    .value()
                    .as_ref(),
                "0600"
            );
            assert_eq!(
                properties
                    .read(cx)
                    .permission_inputs
                    .directory_mode
                    .read(cx)
                    .value()
                    .as_ref(),
                "0700"
            );
            assert!(
                properties.read(cx).model.permissions().is_dirty(),
                "editing the modes must dirty the permission plan"
            );
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("permissions-scope-recursive", cx);
            assert!(
                properties
                    .read(cx)
                    .model
                    .permissions()
                    .scope()
                    .is_recursive()
            );
            window.render_frame(cx);
            let review = window.find("permissions-review-scope");
            assert!(review.visible());
            assert!(window.try_find("properties-apply").is_none());
            window.click("permissions-review-scope", cx);
            let permissions = properties.read(cx).model.permissions();
            assert!(permissions.scope().is_reviewed());
            assert!(
                permissions.is_dirty(),
                "permission changes must remain dirty"
            );
            assert_eq!(permissions.edit_disabled_reason(), None);
            assert!(
                permissions.is_valid(),
                "permission changes must become valid"
            );
            window.render_frame(cx);
            assert!(window.find("properties-apply").visible());
            window.click("properties-apply", cx);
        })
        .expect("the Properties window remains open");

        cx.wait_for(handle.into(), Duration::from_secs(2), |_, _| {
            let directory_mode = filesystem::metadata(&directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o7777;
            let child_mode = filesystem::metadata(&child).unwrap().permissions().mode() & 0o7777;
            directory_mode == 0o700 && child_mode == 0o600
        })
        .await;
        assert!(cx.update(|cx| !properties.read(cx).model.permissions().is_dirty()));
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn external_permission_changes_refresh_pristine_and_block_dirty_editors(
        cx: &mut TestAppContext,
    ) {
        use std::os::unix::fs::PermissionsExt;

        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let path = temporary.path().join("file.txt");
        filesystem::write(&path, b"content").unwrap();
        filesystem::set_permissions(&path, filesystem::Permissions::from_mode(0o644)).unwrap();
        let data = PropertiesWindowData::load(std::slice::from_ref(&path)).unwrap();
        let mut properties = None;
        let handle = cx.open_window(size(px(760.), px(620.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            properties = Some(view.clone());
            Root::new(view, window, cx)
        });
        let properties = properties.expect("the Properties view is constructed");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("properties-page-permissions", cx);
        })
        .expect("the Properties window remains open");

        filesystem::set_permissions(&path, filesystem::Permissions::from_mode(0o600)).unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            properties
                .read(cx)
                .permission_inputs
                .file_mode
                .read(cx)
                .value()
                .as_ref()
                == "0600"
        })
        .await;
        assert!(cx.update(|cx| !properties.read(cx).model.permissions().is_dirty()));

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("permissions-file-mode", cx);
            window.press("ctrl-a", cx);
            window.input("0640", cx);
        })
        .expect("the Properties window remains open while editing");
        assert!(cx.update(|cx| properties.read(cx).model.permissions().is_dirty()));
        filesystem::set_permissions(&path, filesystem::Permissions::from_mode(0o660)).unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            properties.read(cx).model.state() == PropertiesState::Replaced
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                properties
                    .read(cx)
                    .permission_inputs
                    .file_mode
                    .read(cx)
                    .value()
                    .as_ref(),
                "0640"
            );
            assert!(window.try_find("properties-apply").is_none());
            assert_eq!(
                window.find("properties-validation-summary").label(),
                Some(
                    "The selected item changed while you were editing. Close and reopen Properties."
                )
            );
        })
        .expect("the Properties window remains open after external change");
    }
}
