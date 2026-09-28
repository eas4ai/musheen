use crate::i18n::Catalog;
use crate::operations::{OperationHub, spawn_ready_hub_operations};
use crate::{
    Access, AccessClass, Accounts, AclList, AclName, AclRight, ApplicationIdentity, DropError,
    LocalOperationQueue, MODE_BITS, OwnershipEdit, PermissionsPageModel, PrivilegeBackend,
    Tristate,
};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, KeyBinding, Role, SharedString,
    Task, TestSupportExt, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};
use musheen_core::{
    CancellationToken, CapabilityKind, CapabilityMatrix, CapabilityState, CommandTargetRef,
    DisplayPath, ItemKind, Store, StorePath,
};
use musheen_desktop::SecretBuffer;
use musheen_desktop::privilege::{
    BrokerOutput, BrokerRequest, OwnershipContents, OwnershipItem, PrivilegeProvider,
};
use musheen_desktop::{
    AclEntry, AclQualifier, AclState, AggregateValue, Capacity, ChecksumAlgorithm, ChecksumResult,
    ChecksumService, PropertyError, PropertyRefresh, PropertySnapshot, PropertyTimestamp,
    RecursiveSize, TagError, Volume, VolumeCapabilities, VolumeId, XattrState,
};
use musheen_local::{LocalStore, OwnershipOperationRoute};
use musheen_ops::{JobId, MetadataChange, MetadataScope};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const LIVE_REFRESH_INTERVAL: Duration = Duration::from_secs(1);

type RefreshWorkResult =
    Result<(PropertyRefresh, Option<(PropertySnapshot, Accounts)>), PropertyError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VolumePropertiesModel {
    id: VolumeId,
    label: Box<str>,
    device: PathBuf,
    mount_points: Vec<PathBuf>,
    filesystem_type: Option<Box<str>>,
    capacity: Option<Capacity>,
    read_only: bool,
    capabilities: VolumeCapabilities,
    available: bool,
}

impl VolumePropertiesModel {
    #[must_use]
    pub fn new(volume: &Volume) -> Self {
        Self {
            id: volume.id().clone(),
            label: volume.label().into(),
            device: volume.device().to_path_buf(),
            mount_points: volume.mount_points().to_vec(),
            filesystem_type: volume.filesystem_type().map(Into::into),
            capacity: volume.capacity(),
            read_only: volume.is_read_only(),
            capabilities: volume.capabilities(),
            available: true,
        }
    }

    /// Replace live device facts while preserving the dialog identity.
    /// Returns `false` for a different volume or an unchanged snapshot.
    pub fn update(&mut self, volume: &Volume) -> bool {
        if volume.id() != &self.id {
            return false;
        }
        let replacement = Self::new(volume);
        if *self == replacement {
            return false;
        }
        *self = replacement;
        true
    }

    #[must_use]
    pub const fn id(&self) -> &VolumeId {
        &self.id
    }

    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    #[must_use]
    pub fn device(&self) -> &std::path::Path {
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
    pub const fn available_bytes(&self) -> Option<u64> {
        match self.capacity {
            Some(capacity) => Some(capacity.available_bytes()),
            None => None,
        }
    }

    #[must_use]
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    #[must_use]
    pub const fn capabilities(&self) -> VolumeCapabilities {
        self.capabilities
    }

    #[must_use]
    pub const fn is_available(&self) -> bool {
        self.available
    }

    pub fn mark_unavailable(&mut self) -> bool {
        if !self.available {
            return false;
        }
        self.available = false;
        self.capabilities = VolumeCapabilities::default();
        true
    }
}

/// A device Properties surface whose identity survives mount-table and
/// UDisks2 updates. The application owns the subscription and pushes each
/// changed `Volume` into every open window; this view never polls.
pub(crate) struct VolumePropertiesWindow {
    model: VolumePropertiesModel,
    catalog: Catalog,
    focus: FocusHandle,
    pending_focus: bool,
}

struct VolumePropertyValues {
    mounts: String,
    capacity: String,
    access: String,
    filesystem: String,
}

impl VolumePropertiesWindow {
    #[must_use]
    pub(crate) fn new(
        model: VolumePropertiesModel,
        catalog: Catalog,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            model,
            catalog,
            focus: cx.focus_handle(),
            pending_focus: true,
        }
    }

    pub(crate) fn update_volume(&mut self, volume: &Volume, cx: &mut Context<Self>) {
        self.update_model(VolumePropertiesModel::new(volume), cx);
    }

    pub(crate) fn mark_unavailable(&mut self, cx: &mut Context<Self>) {
        if self.model.mark_unavailable() {
            cx.notify();
        }
    }

    fn update_model(&mut self, replacement: VolumePropertiesModel, cx: &mut Context<Self>) {
        if replacement.id() == self.model.id() && replacement != self.model {
            self.model = replacement;
            cx.notify();
        }
    }

    #[cfg(test)]
    fn model(&self) -> &VolumePropertiesModel {
        &self.model
    }

    fn row(&self, id: &'static str, label: &'static str, value: String) -> AnyElement {
        let label = self.catalog.message(label).unwrap_or(label);
        let accessible_label = format!("{label}: {value}");
        div()
            .id(id)
            .test_support()
            .aria_label(accessible_label)
            .flex()
            .items_center()
            .justify_between()
            .gap_4()
            .child(label.to_owned())
            .child(value)
            .into_any_element()
    }

    fn values(&self) -> VolumePropertyValues {
        let unavailable = || {
            self.catalog
                .message("properties-unavailable")
                .unwrap_or("Unavailable")
                .to_owned()
        };
        let mounts = if self.model.mount_points().is_empty() {
            unavailable()
        } else {
            self.model
                .mount_points()
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let capacity = self.model.capacity().map_or_else(unavailable, |capacity| {
            format!(
                "{} / {}",
                format_size(capacity.available_bytes()),
                format_size(capacity.total_bytes())
            )
        });
        let access_key = if self.model.is_read_only() {
            "properties-read-only"
        } else {
            "properties-read-write"
        };
        VolumePropertyValues {
            mounts,
            capacity,
            access: self
                .catalog
                .message(access_key)
                .unwrap_or(access_key)
                .to_owned(),
            filesystem: self
                .model
                .filesystem_type()
                .map_or_else(unavailable, str::to_owned),
        }
    }
}

impl Render for VolumePropertiesWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.focus.focus(window, cx);
            self.pending_focus = false;
        }
        let values = self.values();

        div()
            .id("volume-properties")
            .test_support()
            .key_context("PropertiesWindow")
            .role(Role::Dialog)
            .aria_label(
                self.catalog
                    .message("volume-properties-dialog")
                    .expect("the volume Properties dialog is localized"),
            )
            .track_focus(&self.focus)
            .tab_index(0)
            .on_action(|_: &ConfirmProperties, window, _| window.remove_window())
            .on_action(|_: &CancelProperties, window, _| window.remove_window())
            .on_action(|_: &Escape, window, _| window.remove_window())
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_xl().child(self.model.label().to_owned()))
            .when(!self.model.is_available(), |dialog| {
                let message = self
                    .catalog
                    .message("volume-properties-disappeared")
                    .expect("the disappeared-volume message is localized")
                    .to_owned();
                dialog.child(
                    div()
                        .id("volume-properties-unavailable")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(message.clone())
                        .child(message),
                )
            })
            .child(self.row(
                "volume-properties-device",
                "properties-mount-source",
                self.model.device().display().to_string(),
            ))
            .child(self.row(
                "volume-properties-mounts",
                "properties-mount-point",
                values.mounts,
            ))
            .child(self.row(
                "volume-properties-filesystem",
                "properties-filesystem",
                values.filesystem,
            ))
            .child(self.row(
                "volume-properties-capacity",
                "properties-available",
                values.capacity,
            ))
            .child(self.row(
                "volume-properties-access",
                "properties-mount-mode",
                values.access,
            ))
    }
}

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
        app_id: Some(ApplicationIdentity::ID.into()),
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
    /// Names for the Permissions page, looked up once.
    accounts: Accounts,
    /// The filesystem's permission support; anything else makes the
    /// Permissions page read-only (SEARCH-019).
    permission_capability: CapabilityState,
    pages: Vec<PropertiesPage>,
    page: PropertiesPage,
    state: PropertiesState,
    original_tags: BTreeSet<Box<str>>,
    tags: BTreeSet<Box<str>>,
    mixed_tags: BTreeSet<Box<str>>,
    added_tags: BTreeSet<Box<str>>,
    removed_tags: BTreeSet<Box<str>>,
    /// Whether every item is on a filesystem where the broker changes
    /// owners and groups (SYS-037), checked off the UI thread.
    admin_ownership: bool,
}

impl PropertiesDialogModel {
    /// Loads the account names and probes the filesystems' permission
    /// support on the calling thread. The Properties window does both off
    /// the UI thread, in `PropertiesWindowData::load`.
    pub fn new(snapshot: PropertySnapshot) -> Self {
        let accounts = Accounts::load(&snapshot);
        let store = LocalStore::new();
        let matrices = snapshot
            .items()
            .iter()
            .map(|item| store.capabilities(&StorePath::from_unix_path(item.path().as_os_str())))
            .collect::<Vec<_>>();
        let capability = permission_capability(&matrices);
        Self::with_accounts(snapshot, accounts, capability)
    }

    /// A model whose account names were loaded off the UI thread.
    pub(crate) fn with_accounts(
        snapshot: PropertySnapshot,
        accounts: Accounts,
        permission_capability: CapabilityState,
    ) -> Self {
        let permissions = PermissionsPageModel::from_snapshot(
            &snapshot,
            accounts.clone(),
            &permission_capability,
        );
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
            accounts,
            permission_capability,
            pages,
            page: PropertiesPage::General,
            state: PropertiesState::Ready,
            original_tags: BTreeSet::new(),
            tags: BTreeSet::new(),
            mixed_tags: BTreeSet::new(),
            added_tags: BTreeSet::new(),
            removed_tags: BTreeSet::new(),
            admin_ownership: true,
        }
    }

    /// Records whether the items are on filesystems where the broker changes
    /// owners and groups.
    pub(crate) fn set_admin_ownership(&mut self, supported: bool) {
        self.admin_ownership = supported;
        self.permissions = self.permissions.clone().with_admin_ownership(supported);
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

    /// Sets the filesystem's permission support: anything but supported
    /// makes the Permissions page read-only, with the reason.
    #[must_use]
    pub fn with_permission_capability(mut self, capability: CapabilityState) -> Self {
        self.permission_capability = capability;
        self.clear_permission_edits();
        self
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

    /// Marks the tags one write stored as saved. Edits made while the
    /// write ran stay unsaved.
    pub(crate) fn accept_written_tags(&mut self, written: &TagDelta) {
        for tag in &written.added {
            self.original_tags.insert(tag.clone());
            self.added_tags.remove(tag);
        }
        for tag in &written.removed {
            self.original_tags.remove(tag);
            self.removed_tags.remove(tag);
        }
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
        Ok((roots, self.permissions.scope(), self.permissions.change()))
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
            self.accounts = Accounts::load(&self.snapshot);
            self.clear_permission_edits();
        }
        Ok(refresh)
    }

    fn replace_snapshot(&mut self, snapshot: PropertySnapshot, accounts: Accounts) {
        let selected_page = self.page;
        let mut replacement =
            Self::with_accounts(snapshot, accounts, self.permission_capability.clone());
        if replacement.pages.contains(&selected_page) {
            replacement.page = selected_page;
        }
        replacement.set_admin_ownership(self.admin_ownership);
        *self = replacement;
    }

    fn clear_permission_edits(&mut self) {
        self.permissions = PermissionsPageModel::from_snapshot(
            &self.snapshot,
            self.accounts.clone(),
            &self.permission_capability,
        )
        .with_admin_ownership(self.admin_ownership);
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

    /// Marks the tags one write stored as saved. Edits made while the
    /// write ran stay unsaved.
    fn accept_written_tags(&mut self, written: &TagDelta) {
        for tag in &written.added {
            self.original_tags.insert(tag.clone());
            self.added_tags.remove(tag);
        }
        for tag in &written.removed {
            self.original_tags.remove(tag);
            self.removed_tags.remove(tag);
        }
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
    /// A tag write runs; Apply waits for it.
    tag_write_pending: bool,
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
            tag_write_pending: false,
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
        if self.tag_write_pending {
            return;
        }
        let writer = Arc::clone(writer);
        let delta = TagDelta {
            added: self.model.added_tags.clone(),
            removed: self.model.removed_tags.clone(),
        };
        self.tag_write_pending = true;
        let window = cx.entity().downgrade();
        let written = delta.clone();
        // Provider and local windows use the same app-owned writer policy.
        writer(
            &delta,
            cx,
            Box::new(move |result, cx| {
                let _ = window.update(cx, |this, cx| {
                    this.tag_write_pending = false;
                    match result {
                        Ok(()) => {
                            this.model.accept_written_tags(&written);
                            this.tag_error = None;
                        }
                        Err(error) => this.tag_error = Some(error),
                    }
                    cx.notify();
                });
            }),
        );
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
                    .disabled(self.tag_write_pending)
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
    /// The names the Permissions page shows, loaded with the snapshot.
    accounts: Accounts,
    filesystem_rows: Vec<(Box<str>, Box<str>)>,
    capability_rows: Vec<(Box<str>, Box<str>)>,
    /// Whether every item's filesystem supports POSIX permissions.
    permission_capability: CapabilityState,
    tags: BTreeSet<Box<str>>,
    mixed_tags: BTreeSet<Box<str>>,
    tag_writer: Option<TagWriter>,
    catalog: Catalog,
    /// Makes owner and group changes as administrator (SYS-037).
    privilege_backend: Option<Arc<dyn PrivilegeBackend>>,
    /// Whether every item is on a filesystem where the broker changes
    /// owners and groups, checked here, off the UI thread.
    admin_ownership: bool,
}

/// Hears the result of one tag write once the catalog took it.
pub(crate) type TagWriteDone = Box<dyn FnOnce(Result<(), Box<str>>, &mut App) + 'static>;

/// Saves a Properties window's tag edits. The write runs off the UI thread;
/// the writer returns at once and calls `done` with the result.
pub(crate) type TagWriter = Arc<dyn Fn(&TagDelta, &mut App, TagWriteDone) + Send + Sync + 'static>;

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
        let permission_capability = permission_capability(&capability_matrices);
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
        let admin_ownership = snapshot_admin_ownership(&snapshot);
        Ok(Self {
            accounts: Accounts::load(&snapshot),
            snapshot,
            filesystem_rows,
            capability_rows,
            permission_capability,
            tags: BTreeSet::new(),
            mixed_tags: BTreeSet::new(),
            tag_writer: None,
            catalog: Catalog::system().expect("the built-in locale catalogs are valid"),
            privilege_backend: None,
            admin_ownership,
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
    pub(crate) fn with_privilege_backend(mut self, backend: Arc<dyn PrivilegeBackend>) -> Self {
        self.privilege_backend = Some(backend);
        self
    }

    pub(crate) fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }

    /// Replaces the filesystem's permission support the window reports.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_permission_capability(mut self, capability: CapabilityState) -> Self {
        self.permission_capability = capability;
        self
    }
}

#[derive(Debug)]
struct PropertyRow {
    label: Box<str>,
    value: Box<str>,
    input: Entity<InputState>,
}

/// Why the last Apply did not finish, for the page's error line.
#[derive(Clone, Debug, Eq, PartialEq)]
enum PermissionError {
    /// A message from submitting or running the change.
    Message(Box<str>),
    /// A job failed; the status center holds its error.
    JobFailed,
    /// Text already in the window's language, such as an ownership change's
    /// failure, which names its item (SYS-037).
    Localized(Box<str>),
}

impl PermissionError {
    /// Keeps `message` for the page, and logs it, as the page may show only
    /// a general failure when the message is the system's own text.
    fn message(message: impl Into<Box<str>>) -> Self {
        let message = message.into();
        eprintln!("Musheen could not change permissions: {message}");
        Self::Message(message)
    }
}

/// Whether the broker would change the owner of every item of `snapshot`:
/// all on local filesystems with POSIX ownership (SYS-037). It asks the
/// filesystems, so it runs off the UI thread.
fn snapshot_admin_ownership(snapshot: &PropertySnapshot) -> bool {
    snapshot
        .items()
        .iter()
        .all(|item| musheen_desktop::privilege::ownership_supported_at(item.path()))
}

/// An owner and group change as administrator: the reviewed items, what
/// changes, and Apply to contents (SYS-037).
#[derive(Clone, Debug)]
struct OwnershipJob {
    items: Vec<OwnershipItem>,
    edit: OwnershipEdit,
    contents: Option<OwnershipContents>,
}

/// What Apply as Administrator shows before it asks for authorization:
/// each item with its current and new owner and group, and the sudo
/// password field when sudo authorizes.
struct OwnershipReview {
    rows: Vec<String>,
    job: OwnershipJob,
    password: Option<Entity<InputState>>,
}

/// Runs one reviewed ownership change through the privilege broker as a
/// job of the operations queue, with one authorization (SYS-037).
struct OwnershipRoute {
    backend: Arc<dyn PrivilegeBackend>,
    job: OwnershipJob,
    /// The user's own change of the same Apply, made first as the user:
    /// the selected items, their scope and the change.
    user_change: Option<(Vec<StorePath>, MetadataScope, MetadataChange)>,
    authentication: Mutex<Option<SecretBuffer>>,
    catalog: Catalog,
}

impl std::fmt::Debug for OwnershipRoute {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OwnershipRoute")
            .field("job", &self.job)
            .finish_non_exhaustive()
    }
}

impl OwnershipRoute {
    /// `reason` for the item at `path`, from the catalog's template.
    fn failed_item(&self, path: &std::path::Path, reason: &str) -> Box<str> {
        fill_template(
            &self.catalog,
            "ownership-failed-item",
            &[("path", &path.display().to_string()), ("reason", reason)],
        )
        .into()
    }

    /// Makes the user's own mode change, as the user, before the owner and
    /// group change (SEARCH-019).
    fn change_modes(&self, cancellation: &CancellationToken) -> Result<(), Box<str>> {
        use musheen_ops::MutationProvider as _;

        let Some((roots, scope, change)) = &self.user_change else {
            return Ok(());
        };
        let reason = |error: musheen_ops::MutationError| {
            let error = error.to_string();
            self.catalog
                .localize_known_reason(&error)
                .unwrap_or_else(|| {
                    self.catalog
                        .message("permissions-apply-failed")
                        .expect("the apply failure is localized")
                        .to_owned()
                })
        };
        let mut store = LocalStore::new();
        for root in roots {
            let path = root.as_unix_path().unwrap_or(std::path::Path::new(""));
            let identity = store
                .identity(root)
                .and_then(|identity| identity.ok_or(musheen_ops::MutationError::Missing))
                .map_err(|error| self.failed_item(path, &reason(error)))?;
            let plan = musheen_ops::MetadataPlan::preflight(
                &mut store,
                root.clone(),
                identity.to_vec(),
                *scope,
                change.clone(),
            );
            match plan {
                Ok(plan) => plan
                    .execute_controlled(&mut store, cancellation)
                    .map_err(|error| self.failed_item(path, &reason(error)))?,
                // An item the mode change leaves as it is does not fail it.
                Err(musheen_ops::MutationError::NoChanges) => {}
                Err(error) => return Err(self.failed_item(path, &reason(error))),
            }
        }
        Ok(())
    }
}

impl OwnershipOperationRoute for OwnershipRoute {
    fn execute_ownership(&self, cancellation: &CancellationToken) -> Result<(), Box<str>> {
        self.change_modes(cancellation)?;
        if self.job.items.is_empty() {
            return Ok(());
        }
        let localized = |error| crate::app::localized_privilege_error(&self.catalog, &error);
        let request = BrokerRequest::change_ownership(
            self.job.items.clone(),
            self.job.edit.owner,
            self.job.edit.group,
            self.job.contents,
        )
        .map_err(localized)?;
        let authentication = self
            .authentication
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let answer = futures_lite::future::block_on(self.backend.perform(
            &request,
            cancellation.clone(),
            authentication,
        ));
        match answer {
            Ok(BrokerOutput::OwnershipChanged(report)) => match report.failure() {
                None => Ok(()),
                Some(failure) => Err(self.failed_item(failure.path(), &localized(failure.error()))),
            },
            Ok(_) => Err(self
                .catalog
                .message("privilege-response-invalid")
                .expect("the invalid privilege response is localized")
                .into()),
            Err(error) => Err(localized(error)),
        }
    }
}

/// The catalog's message `key` with each `{name}` replaced by its value.
/// The catalog has no placeables of its own; the templates keep the order,
/// the punctuation and the arrow of each language.
fn fill_template(catalog: &Catalog, key: &str, values: &[(&str, &str)]) -> String {
    let mut text = catalog
        .message(key)
        .expect("the ownership templates are localized")
        .to_owned();
    for (name, value) in values {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
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
    /// The Permissions page's owner list is open.
    owner_picker_open: bool,
    /// The Permissions page's group list is open.
    group_picker_open: bool,
    /// The review Apply as Administrator shows before it asks for
    /// authorization (SYS-037).
    ownership_review: Option<OwnershipReview>,
    /// The queued ownership change, whose error is already localized.
    ownership_job: Option<JobId>,
    privilege_backend: Option<Arc<dyn PrivilegeBackend>>,
    /// The Permissions page's Advanced section is open.
    advanced_open: bool,
    /// The ACL list whose add chooser is open (SEARCH-020).
    acl_add_open: Option<AclList>,
    /// This window's own Apply ended, so the next refresh loads the items
    /// again even when their mode and times are as they were: an ACL change
    /// leaves them so (SEARCH-020).
    reload_after_apply: bool,
    operation_hub: OperationHub,
    permission_error: Option<PermissionError>,
    permission_batch: PermissionBatchState,
    tag_input: Entity<InputState>,
    tag_writer: Option<TagWriter>,
    /// A tag write runs; Apply waits for it.
    tag_write_pending: bool,
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
        let tag_name = data
            .catalog
            .message("catalog-tag-name")
            .expect("the tag-name catalog message exists")
            .to_owned();
        let tag_input = cx.new(|cx| InputState::new(window, cx).placeholder(tag_name));
        let mut model = PropertiesDialogModel::with_accounts(
            data.snapshot,
            data.accounts,
            data.permission_capability,
        );
        model.set_admin_ownership(data.admin_ownership);
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
            owner_picker_open: false,
            group_picker_open: false,
            ownership_review: None,
            ownership_job: None,
            privilege_backend: data.privilege_backend,
            advanced_open: false,
            acl_add_open: None,
            reload_after_apply: false,
            operation_hub,
            permission_error: None,
            permission_batch: PermissionBatchState::default(),
            tag_input,
            tag_writer: data.tag_writer,
            tag_write_pending: false,
            tag_error: None,
            catalog: data.catalog,
        };
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

    fn apply_permissions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.permission_batch.is_active() || self.ownership_review.is_some() {
            return;
        }
        if self.model.permissions().ownership_edit().is_some() {
            self.open_ownership_review(window, cx);
            return;
        }
        self.submit_user_permissions(cx);
    }

    /// Submits the change the user makes alone: the modes, and a group
    /// they may set themselves.
    fn submit_user_permissions(&mut self, cx: &mut Context<Self>) {
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
                self.permission_error = self
                    .operation_hub
                    .persistence_error()
                    .map(PermissionError::message);
                self.permission_batch.begin(jobs);
                self.pump_operation_queue(cx);
            }
            Err(error) => {
                self.permission_error = Some(PermissionError::message(error));
                cx.notify();
            }
        }
    }

    /// Shows each selected item with its current and new owner and group
    /// before Apply as Administrator asks for authorization (SYS-037). The
    /// page's edits stay as they are until the review ends.
    fn open_ownership_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let permissions = self.model.permissions();
        let Some(edit) = permissions.ownership_edit() else {
            return;
        };
        let accounts = permissions.accounts();
        let contents = match permissions.scope() {
            MetadataScope::Recursive {
                include_nested_mounts,
                ..
            } => Some(OwnershipContents {
                nested_mounts: include_nested_mounts,
            }),
            MetadataScope::Single => None,
        };
        let mut rows = Vec::new();
        let mut items = Vec::new();
        for item in self.model.snapshot().items() {
            // Sockets, pipes and devices are left as they are (SEARCH-019).
            if item.kind() == ItemKind::Other {
                continue;
            }
            let current = item.permissions();
            let owner = accounts.user_name(current.owner());
            let group = accounts.group_name(current.group());
            let new_owner = edit
                .owner
                .map_or_else(|| owner.clone(), |uid| accounts.user_name(uid));
            let new_group = edit
                .group
                .map_or_else(|| group.clone(), |gid| accounts.group_name(gid));
            let template = if contents.is_some() && item.kind() == ItemKind::Directory {
                "ownership-review-row-contents"
            } else {
                "ownership-review-row"
            };
            rows.push(fill_template(
                &self.catalog,
                template,
                &[
                    ("path", &item.path().display().to_string()),
                    ("owner", &owner),
                    ("group", &group),
                    ("new-owner", &new_owner),
                    ("new-group", &new_group),
                ],
            ));
            items.push(OwnershipItem::new(
                item.path(),
                item.identity().device(),
                item.identity().inode(),
            ));
        }
        let sudo = self
            .privilege_backend
            .as_ref()
            .is_some_and(|backend| backend.provider() == PrivilegeProvider::Sudo);
        let password = sudo.then(|| {
            let placeholder = self.message("dialog-sudo-password").to_string();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .masked(true)
            })
        });
        self.owner_picker_open = false;
        self.group_picker_open = false;
        self.acl_add_open = None;
        // Apply, which had focus, is hidden now; the window takes it, so
        // Escape closes the review.
        self.focus.focus(window, cx);
        self.ownership_review = Some(OwnershipReview {
            rows,
            job: OwnershipJob {
                items,
                edit,
                contents,
            },
            password,
        });
        cx.notify();
    }

    /// Queues the reviewed change as one job: the user's own mode change
    /// first, then the owner and group change as administrator. The job
    /// runs in the operations queue whether or not this window stays open.
    fn confirm_ownership_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.state() != PropertiesState::Ready {
            return;
        }
        let Some(review) = self.ownership_review.take() else {
            return;
        };
        let authentication = review.password.map(|password| {
            let secret = SecretBuffer::new(password.read(cx).value().as_bytes().to_vec());
            password.update(cx, |password, cx| password.set_value("", window, cx));
            secret
        });
        let Some(backend) = self.privilege_backend.clone() else {
            self.permission_error = Some(PermissionError::Localized(
                self.message("privilege-ownership-unavailable"),
            ));
            cx.notify();
            return;
        };
        let roots = self
            .model
            .snapshot()
            .items()
            .iter()
            .map(|item| StorePath::from_unix_path(item.path().as_os_str()))
            .collect::<Vec<_>>();
        let change = self.model.permissions().change();
        let user_change = change
            .is_dirty()
            .then(|| (roots.clone(), self.model.permissions().scope(), change));
        let route = Arc::new(OwnershipRoute {
            backend,
            job: review.job,
            user_change,
            authentication: Mutex::new(authentication),
            catalog: self.catalog.clone(),
        });
        match self.operation_hub.submit_ownership(roots, route) {
            Ok(id) => {
                self.ownership_job = Some(id);
                self.permission_batch.begin([id]);
                self.pump_operation_queue(cx);
            }
            Err(error) => {
                self.permission_error = Some(PermissionError::message(error.to_string()));
            }
        }
        cx.notify();
    }

    fn cancel_ownership_review(&mut self, cx: &mut Context<Self>) {
        self.ownership_review = None;
        cx.notify();
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
                let ownership = state.ownership_job == Some(id);
                if ownership {
                    state.ownership_job = None;
                }
                if let Some(error) = error {
                    state.permission_error = Some(if ownership {
                        PermissionError::Localized(error)
                    } else {
                        PermissionError::message(error)
                    });
                } else if outcome == PermissionBatchOutcome::Failed {
                    state.permission_error = Some(PermissionError::JobFailed);
                }
                if outcome == PermissionBatchOutcome::Succeeded {
                    state.model.clear_permission_edits();
                    state.reload_after_apply = true;
                    state.refresh_now(cx);
                }
                state.pump_operation_queue(cx);
                cx.notify();
            },
        );
        if let Err(error) = result {
            self.permission_error = Some(PermissionError::message(error.to_string()));
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

    /// Starts a refresh now instead of at the next tick; one that is
    /// running already picks up a reload requested meanwhile at the next
    /// tick.
    fn refresh_now(&mut self, cx: &mut Context<Self>) {
        let Some(work) = self.begin_live_refresh(cx) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| state.finish_live_refresh(result, cx));
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
        let reload = std::mem::take(&mut self.reload_after_apply);
        let probe = self.model.snapshot().refresh_probe();
        let paths = self
            .model
            .snapshot()
            .items()
            .iter()
            .map(|item| item.path().to_path_buf())
            .collect::<Vec<_>>();
        // The account lists stay as the window loaded them; only the names of
        // new owners and groups are looked up.
        let known = self.model.permissions().accounts().clone();
        Some(cx.background_spawn(async move {
            let refresh = probe.refresh_state()?;
            let replacement = if refresh == PropertyRefresh::MetadataChanged
                || (reload && refresh == PropertyRefresh::Current)
            {
                let snapshot = PropertySnapshot::load(&paths)?;
                let accounts = known.reload_for(&snapshot);
                Some((snapshot, accounts))
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
                if let Some((snapshot, accounts)) = replacement {
                    if self.permission_batch.is_active() {
                        // This window's own Apply changed the items; the page
                        // is loaded again once the Apply ends.
                    } else if self.model.permissions().is_dirty() {
                        self.model.state = PropertiesState::Replaced;
                        self.refresh_error = Some("properties-changed-while-editing".into());
                    } else {
                        self.model.replace_snapshot(snapshot, accounts);
                        self.refresh_error = None;
                    }
                } else {
                    self.model.state = properties_state(refresh);
                    self.refresh_error = None;
                }
            }
            Err(error) => {
                eprintln!("Musheen could not refresh Properties: {error}");
                self.refresh_error = Some(error.to_string().into());
            }
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
        let accounts = permissions.accounts();
        let named = |value: &AggregateValue<u32>, name: &dyn Fn(u32) -> String| match value {
            AggregateValue::Same(id) => name(*id).into_boxed_str(),
            other => self.localized_value(aggregate_u32(other)),
        };
        vec![
            (
                self.message("properties-owner"),
                named(permissions.owner(), &|uid| accounts.user_name(uid)),
            ),
            (
                self.message("properties-group"),
                named(permissions.group(), &|gid| accounts.group_name(gid)),
            ),
            (
                self.message("properties-mode"),
                self.localized_value(aggregate_mode(permissions.mode())),
            ),
        ]
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
        let disabled = self.model.state() != PropertiesState::Ready
            || self.model.permissions().read_only_reason().is_some();
        let scope = self.model.permissions().scope();
        let recursive = scope.is_recursive();
        let review_needed = recursive && !scope.is_reviewed();
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
            .children(self.render_permission_controls(cx))
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
                        .child(permission_note(
                            "permissions-special-inside",
                            self.message("permissions-special-inside").to_string(),
                            Role::Label,
                        ))
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
            .when_some(self.permission_error_text(), |editor, message| {
                editor.child(
                    div()
                        .id("permissions-validation")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(message.to_owned())
                        .child(message.to_owned()),
                )
            })
            .child(div().mt_2().child(self.render_rows()))
            .into_any_element()
    }

    /// The Permissions page's controls (SEARCH-019): the access choices of
    /// the three classes, the executable checkbox, the owner and group, and
    /// the Advanced section.
    fn render_permission_controls(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let permissions = self.model.permissions();
        // While the review is open, the page's edits stay as reviewed.
        let ready = self.model.state() == PropertiesState::Ready && self.ownership_review.is_none();
        let editable = ready && permissions.modes_editable();
        let varies = self.message("permissions-varies").to_string();
        let mut controls = Vec::new();
        if let Some(review) = &self.ownership_review {
            controls.push(self.render_ownership_review(review, cx));
        }
        if let Some(reason) = permissions.read_only_reason() {
            let reason = format!(
                "{} {}",
                self.message("permissions-read-only"),
                self.catalog.localize_reason(reason)
            );
            controls.push(permission_note(
                "permissions-read-only",
                reason,
                Role::Alert,
            ));
        } else if let Some(lock) = permissions.mode_lock() {
            controls.push(permission_note(
                "permissions-mode-lock",
                self.message(lock.message_key()).to_string(),
                Role::Label,
            ));
        }
        if permissions.has_special_items() {
            controls.push(permission_note(
                "permissions-special-items",
                self.message("permissions-special-items").to_string(),
                Role::Label,
            ));
        }
        let folders = permissions.only_folders();
        for class in AccessClass::ALL {
            let shown = permissions.access(class);
            let chosen = permissions.access_chosen(class);
            let class_label = self.message(&format!("permissions-class-{}", class.key()));
            let mut row = div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(div().w(px(96.)).child(class_label.to_string()));
            for access in Access::ALL {
                let label = self.message(match (access, folders) {
                    (Access::None, _) => "permissions-access-no-access",
                    (Access::View, false) => "permissions-access-can-view",
                    (Access::View, true) => "permissions-access-folder-can-view",
                    (Access::Modify, false) => "permissions-access-can-modify",
                    (Access::Modify, true) => "permissions-access-folder-can-modify",
                });
                row = row.child(
                    Button::new(SharedString::from(format!(
                        "permissions-{}-{}",
                        class.key(),
                        access.key()
                    )))
                    .label(label.to_string())
                    .small()
                    .selected(shown == Some(access))
                    .disabled(!editable)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.permissions_mut().set_access(class, access);
                        cx.notify();
                    })),
                );
            }
            // Varies shows while the items differ, and stays offered after
            // a choice so the user can go back to leaving them as they are.
            if shown.is_none() || (chosen && permissions.access_varies_unchanged(class)) {
                row = row.child(
                    Button::new(SharedString::from(format!(
                        "permissions-{}-varies",
                        class.key()
                    )))
                    .label(varies.clone())
                    .small()
                    .selected(shown.is_none())
                    .disabled(!editable || !chosen)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.permissions_mut().clear_access(class);
                        cx.notify();
                    })),
                );
            }
            controls.push(row.into_any_element());
        }
        if permissions.has_files() {
            let executable = permissions.executable();
            controls.push(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("permissions-executable")
                            .label(self.message("permissions-executable").to_string())
                            .small()
                            .selected(executable == Tristate::On)
                            .disabled(!editable)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.permissions_mut().toggle_executable();
                                cx.notify();
                            })),
                    )
                    .when(executable == Tristate::Varies, |row| {
                        row.child(varies_mark("permissions-executable-varies", &varies))
                    })
                    .into_any_element(),
            );
        }
        let owner = permissions.shown_owner().map_or_else(
            || varies.clone(),
            |uid| permissions.accounts().user_name(uid),
        );
        let owner_editable = ready && permissions.owner_editable();
        let owner_picker = div()
            .id("permissions-owner-picker")
            .test_support()
            .px_2()
            .border_1()
            .rounded_md()
            .aria_label(owner.clone())
            .child(owner);
        let owner_picker = if owner_editable {
            owner_picker
                .role(Role::Button)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.owner_picker_open = !this.owner_picker_open;
                    this.group_picker_open = false;
                    cx.notify();
                }))
        } else {
            owner_picker.role(Role::Label)
        };
        controls.push(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(96.))
                        .child(self.message("permissions-class-owner").to_string()),
                )
                .child(owner_picker)
                .into_any_element(),
        );
        if self.owner_picker_open && owner_editable {
            let shown = permissions.shown_owner();
            let options = permissions.accounts().all_users().to_vec();
            controls.push(
                div()
                    .id("permissions-owner-options")
                    .test_support()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(options.into_iter().map(|(uid, name)| {
                        Button::new(SharedString::from(format!(
                            "permissions-owner-option-{uid}"
                        )))
                        .label(name)
                        .small()
                        .selected(shown == Some(uid))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.permissions_mut().set_owner(uid);
                            this.owner_picker_open = false;
                            cx.notify();
                        }))
                    }))
                    .into_any_element(),
            );
        }
        let group = permissions.shown_group().map_or_else(
            || varies.clone(),
            |gid| permissions.accounts().group_name(gid),
        );
        let group_editable = ready && permissions.group_editable();
        let choose = self.message("permissions-group-choose").to_string();
        let picker = div()
            .id("permissions-group-picker")
            .test_support()
            .px_2()
            .border_1()
            .rounded_md()
            .child(group.clone());
        // Only a group the user may choose is a button; otherwise the group
        // is a label, with the reason beside it.
        let picker = if group_editable {
            picker
                .role(Role::Button)
                .aria_label(choose)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.group_picker_open = !this.group_picker_open;
                    this.owner_picker_open = false;
                    cx.notify();
                }))
        } else {
            picker.role(Role::Label).aria_label(group)
        };
        controls.push(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .w(px(96.))
                        .child(self.message("permissions-class-group").to_string()),
                )
                .child(picker)
                .into_any_element(),
        );
        if permissions.read_only_reason().is_none() && !permissions.admin_changes_available() {
            controls.push(permission_note(
                "permissions-admin-unavailable",
                self.message("permissions-admin-unavailable").to_string(),
                Role::Label,
            ));
        }
        if permissions.needs_administrator() {
            controls.push(permission_note(
                "permissions-needs-admin",
                self.message("permissions-needs-admin").to_string(),
                Role::Label,
            ));
        }
        if permissions.has_named_acl() {
            controls.push(permission_note(
                "permissions-acl-mask-note",
                self.message("permissions-acl-mask-note").to_string(),
                Role::Label,
            ));
        }
        if self.group_picker_open && group_editable {
            let shown = permissions.shown_group();
            let options = permissions.group_choices().to_vec();
            controls.push(
                div()
                    .id("permissions-group-options")
                    .test_support()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(options.into_iter().map(|(gid, name)| {
                        Button::new(SharedString::from(format!(
                            "permissions-group-option-{gid}"
                        )))
                        .label(name)
                        .small()
                        .selected(shown == Some(gid))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.permissions_mut().set_group(gid);
                            this.group_picker_open = false;
                            cx.notify();
                        }))
                    }))
                    .into_any_element(),
            );
        }
        controls.push(
            Button::new("permissions-advanced")
                .label(self.message("permissions-advanced").to_string())
                .small()
                .selected(self.advanced_open)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.advanced_open = !this.advanced_open;
                    cx.notify();
                }))
                .into_any_element(),
        );
        if self.advanced_open {
            controls.push(self.render_permission_bits(editable, cx));
            controls.push(self.render_access_entries(cx));
        }
        controls
    }

    /// The review Apply as Administrator shows before it asks for
    /// authorization: each item with its current and new owner and group
    /// (SYS-037).
    fn render_ownership_review(
        &self,
        review: &OwnershipReview,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = self.message("ownership-review-title").to_string();
        div()
            .id("ownership-review")
            .test_support()
            .role(Role::Dialog)
            .aria_label(title.clone())
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .border_1()
            .rounded_md()
            .child(div().child(title))
            .when_some(self.privilege_backend.as_ref(), |panel, backend| {
                let provider = match backend.provider() {
                    PrivilegeProvider::Polkit => "Polkit",
                    PrivilegeProvider::Sudo => "sudo",
                };
                let line = format!(
                    "{}: {provider}",
                    self.message("dialog-authorization-provider")
                );
                panel.child(
                    div()
                        .id("ownership-review-provider")
                        .test_support()
                        .role(Role::Label)
                        .aria_label(line.clone())
                        .text_sm()
                        .child(line),
                )
            })
            .child(permission_note(
                "ownership-review-kernel",
                self.message("ownership-review-kernel").to_string(),
                Role::Label,
            ))
            .children(review.rows.iter().enumerate().map(|(index, row)| {
                div()
                    .id(SharedString::from(format!("ownership-review-item-{index}")))
                    .test_support()
                    .role(Role::Label)
                    .aria_label(row.clone())
                    .text_sm()
                    .child(row.clone())
            }))
            .when_some(review.password.clone(), |panel, password| {
                let label = self.message("dialog-sudo-password").to_string();
                panel.child(
                    div()
                        .id("ownership-review-password")
                        .test_support()
                        .role(Role::Group)
                        .aria_label(label.clone())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().child(label))
                        .child(Input::new(&password)),
                )
            })
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("ownership-review-cancel")
                            .label(self.message("ownership-review-cancel").to_string())
                            .small()
                            .on_click(
                                cx.listener(|this, _, _, cx| this.cancel_ownership_review(cx)),
                            ),
                    )
                    .child(
                        Button::new("ownership-review-confirm")
                            .label(self.message("ownership-review-confirm").to_string())
                            .small()
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_ownership_review(window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    /// The Advanced section's mode bits, each a toggle, with Varies where
    /// the selected items differ.
    fn render_permission_bits(&self, editable: bool, cx: &mut Context<Self>) -> AnyElement {
        let permissions = self.model.permissions();
        let varies = self.message("permissions-varies").to_string();
        div()
            .id("permissions-bits")
            .flex()
            .flex_wrap()
            .gap_1()
            .children(MODE_BITS.into_iter().map(|bit| {
                let label = match bit.key.split_once('-') {
                    Some((class, kind)) => format!(
                        "{} {}",
                        self.message(&format!("permissions-class-{class}")),
                        self.message(&format!("permissions-bit-{kind}"))
                    ),
                    None => self
                        .message(&format!("permissions-bit-{}", bit.key))
                        .to_string(),
                };
                let mask = bit.mask;
                let shown = permissions.bit(mask);
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("permissions-bit-{}", bit.key)))
                            .label(label)
                            .small()
                            .selected(shown == Tristate::On)
                            .disabled(!editable)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.model.permissions_mut().toggle_bit(mask);
                                cx.notify();
                            })),
                    )
                    .when(shown == Tristate::Varies, |cell| {
                        cell.child(varies_mark(
                            SharedString::from(format!("permissions-bit-{}-varies", bit.key)),
                            &varies,
                        ))
                    })
            }))
            .into_any_element()
    }

    /// The name of the user or group of a named ACL entry, as the page
    /// shows it.
    fn acl_name_label(&self, name: AclName) -> String {
        let accounts = self.model.permissions().accounts();
        match name {
            AclName::User(uid) => format!(
                "{} {}",
                self.message("permissions-acl-user"),
                accounts.user_name(uid)
            ),
            AclName::Group(gid) => format!(
                "{} {}",
                self.message("permissions-acl-group"),
                accounts.group_name(gid)
            ),
        }
    }

    /// The Advanced section's ACL entries (SEARCH-020): an editor for the
    /// named entries every selected item shares, and each item's entries as
    /// Apply would leave them, with names.
    fn render_access_entries(&self, cx: &mut Context<Self>) -> AnyElement {
        let permissions = self.model.permissions();
        let separator = self.catalog.list_separator();
        let rights = |read: bool, write: bool, execute: bool| {
            [
                if read { 'r' } else { '-' },
                if write { 'w' } else { '-' },
                if execute { 'x' } else { '-' },
            ]
            .iter()
            .collect::<String>()
        };
        let desktop_label = |entry: &AclEntry| {
            let qualifier = match entry.qualifier() {
                AclQualifier::Owner => self.message("permissions-acl-owner").to_string(),
                AclQualifier::OwningGroup => {
                    self.message("permissions-acl-owning-group").to_string()
                }
                AclQualifier::Other => self.message("permissions-acl-other").to_string(),
                AclQualifier::User(uid) => self.acl_name_label(AclName::User(*uid)),
                AclQualifier::Group(gid) => self.acl_name_label(AclName::Group(*gid)),
                AclQualifier::Mask => self.message("permissions-acl-mask").to_string(),
                AclQualifier::Unknown => self.message("permissions-acl-unknown").to_string(),
            };
            format!(
                "{qualifier}: {}",
                rights(entry.read(), entry.write(), entry.execute())
            )
        };
        let projected_label = |entry: &musheen_ops::AclEntry| {
            let qualifier = match entry.qualifier() {
                musheen_ops::AclQualifier::Owner => {
                    self.message("permissions-acl-owner").to_string()
                }
                musheen_ops::AclQualifier::OwningGroup => {
                    self.message("permissions-acl-owning-group").to_string()
                }
                musheen_ops::AclQualifier::Other => {
                    self.message("permissions-acl-other").to_string()
                }
                musheen_ops::AclQualifier::User(uid) => self.acl_name_label(AclName::User(*uid)),
                musheen_ops::AclQualifier::Group(gid) => self.acl_name_label(AclName::Group(*gid)),
                musheen_ops::AclQualifier::Mask => self.message("permissions-acl-mask").to_string(),
            };
            format!(
                "{qualifier}: {}",
                rights(entry.read(), entry.write(), entry.execute())
            )
        };
        let entries_label = |labels: Vec<String>| {
            if labels.is_empty() {
                self.message("properties-no-acl").to_string()
            } else {
                labels.join(separator)
            }
        };
        let state_label = |state: &AclState| match state {
            AclState::Available(entries) => {
                entries_label(entries.iter().map(desktop_label).collect())
            }
            AclState::Unsupported(reason) => format!(
                "{}: {}",
                self.message("properties-unsupported"),
                self.catalog.localize_reason(reason)
            ),
            AclState::Unavailable(reason) => format!(
                "{}: {}",
                self.message("properties-unavailable"),
                self.catalog.localize_reason(reason)
            ),
        };
        let list_label = |index: usize, list: AclList, state: &AclState| {
            permissions.projected_acl(index, list).map_or_else(
                || state_label(state),
                |entries| entries_label(entries.iter().map(projected_label).collect()),
            )
        };
        let mut sections = Vec::new();
        if let Some(reason) = permissions.acl_read_only_reason()
            && permissions.read_only_reason().is_none()
        {
            sections.push(permission_note(
                "permissions-acl-read-only",
                format!(
                    "{}: {}",
                    self.message("permissions-acl-read-only"),
                    self.catalog.localize_reason(reason)
                ),
                Role::Label,
            ));
        }
        for list in [AclList::Access, AclList::Default] {
            if permissions.acl_editable(list) || permissions.acl_varies(list) {
                sections.push(self.render_acl_list(list, cx));
            }
        }
        for (index, item) in self.model.snapshot().items().iter().enumerate() {
            let name = item
                .path()
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut text = format!(
                "{name}{separator}{}: {}",
                self.message("properties-access-acl"),
                list_label(index, AclList::Access, item.permissions().acl())
            );
            if let Some(default_acl) = item.permissions().default_acl() {
                text.push_str(&format!(
                    "{separator}{}: {}",
                    self.message("properties-default-acl"),
                    list_label(index, AclList::Default, default_acl)
                ));
            }
            sections.push(
                div()
                    .id(SharedString::from(format!("permissions-acl-item-{index}")))
                    .test_support()
                    .role(Role::Label)
                    .aria_label(text.clone())
                    .text_sm()
                    .child(text)
                    .into_any_element(),
            );
        }
        div()
            .id("permissions-acl-entries")
            .test_support()
            .role(Role::Region)
            .aria_label(self.message("properties-access-acl").to_string())
            .flex()
            .flex_col()
            .gap_1()
            .children(sections)
            .into_any_element()
    }

    /// The editor of one ACL list: each named entry with its rights and a
    /// remove button, and the chooser that adds one, or Varies when the
    /// selected items' entries differ (SEARCH-020).
    fn render_acl_list(&self, list: AclList, cx: &mut Context<Self>) -> AnyElement {
        let permissions = self.model.permissions();
        let key = list.key();
        let title = self
            .message(match list {
                AclList::Access => "properties-access-acl",
                AclList::Default => "properties-default-acl",
            })
            .to_string();
        let mut rows = vec![div().text_sm().child(title.clone()).into_any_element()];
        let Some(entries) = permissions.acl_entries(list) else {
            rows.push(varies_mark(
                format!("permissions-acl-{key}-varies"),
                &self.message("permissions-varies"),
            ));
            return acl_list_region(key, title, rows);
        };
        let editable = permissions.acl_editable(list);
        for (name, shown) in entries {
            let id = format!("permissions-acl-{key}-{}", name.key());
            let label = self.acl_name_label(name);
            let mut row = div().flex().flex_wrap().items_center().gap_1().child(
                div()
                    .id(SharedString::from(id.clone()))
                    .test_support()
                    .role(Role::Label)
                    .aria_label(label.clone())
                    .w(px(160.))
                    .child(label),
            );
            for right in AclRight::ALL {
                row = row.child(
                    Button::new(SharedString::from(format!("{id}-{}", right.key())))
                        .label(
                            self.message(&format!("permissions-bit-{}", right.key()))
                                .to_string(),
                        )
                        .small()
                        .selected(shown.has(right))
                        .disabled(!editable)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model
                                .permissions_mut()
                                .toggle_acl_right(list, name, right);
                            cx.notify();
                        })),
                );
            }
            row = row.child(
                Button::new(SharedString::from(format!("{id}-remove")))
                    .label(self.message("permissions-acl-remove").to_string())
                    .small()
                    .disabled(!editable)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.model.permissions_mut().remove_acl_entry(list, name);
                        cx.notify();
                    })),
            );
            rows.push(row.into_any_element());
        }
        if editable {
            rows.push(
                Button::new(SharedString::from(format!("permissions-acl-{key}-add")))
                    .label(self.message("permissions-acl-add").to_string())
                    .small()
                    .selected(self.acl_add_open == Some(list))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.acl_add_open = (this.acl_add_open != Some(list)).then_some(list);
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        if editable && self.acl_add_open == Some(list) {
            rows.push(
                div()
                    .id(SharedString::from(format!(
                        "permissions-acl-{key}-add-options"
                    )))
                    .test_support()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .children(permissions.acl_choices(list).into_iter().map(|(name, _)| {
                        Button::new(SharedString::from(format!(
                            "permissions-acl-{key}-add-{}",
                            name.key()
                        )))
                        .label(self.acl_name_label(name))
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.model.permissions_mut().add_acl_entry(list, name);
                            this.acl_add_open = None;
                            cx.notify();
                        }))
                    }))
                    .into_any_element(),
            );
        }
        acl_list_region(key, title, rows)
    }

    /// The page's error line, in the window's language.
    fn permission_error_text(&self) -> Option<String> {
        self.permission_error.as_ref().map(|error| match error {
            PermissionError::Message(message) => {
                self.localized_error(message, "permissions-apply-failed")
            }
            PermissionError::JobFailed => self.message("permissions-apply-failed").to_string(),
            PermissionError::Localized(message) => message.to_string(),
        })
    }

    /// `error` in the window's language. A message the catalog knows is
    /// translated; any other, such as the system's own text, shows
    /// `fallback`.
    fn localized_error(&self, error: &str, fallback: &str) -> String {
        self.catalog
            .localize_known_reason(error)
            .unwrap_or_else(|| self.message(fallback).to_string())
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
        if self.tag_write_pending {
            return;
        }
        let writer = Arc::clone(writer);
        let delta = TagDelta {
            added: self.model.added_tags.clone(),
            removed: self.model.removed_tags.clone(),
        };
        self.tag_write_pending = true;
        let window = cx.entity().downgrade();
        let written = delta.clone();
        writer(
            &delta,
            cx,
            Box::new(move |result, cx| {
                let _ = window.update(cx, |this, cx| {
                    this.tag_write_pending = false;
                    match result {
                        Ok(()) => {
                            this.model.accept_written_tags(&written);
                            this.tag_error = None;
                        }
                        Err(error) => this.tag_error = Some(error),
                    }
                    cx.notify();
                });
            }),
        );
        cx.notify();
    }

    fn render_page_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut actions = div().flex().items_center().gap_2();
        if self.model.page() == PropertiesPage::Permissions
            && self.model.apply_visible()
            && !self.permission_batch.is_active()
            && self.ownership_review.is_none()
        {
            let label = if self.model.permissions().needs_administrator() {
                "properties-apply-as-administrator"
            } else {
                "properties-apply"
            };
            actions = actions.child(
                Button::new("properties-apply")
                    .label(self.message(label).to_string())
                    .primary()
                    .on_click(
                        cx.listener(|this, _, window, cx| this.apply_permissions(window, cx)),
                    ),
            );
        }
        if self.model.page() == PropertiesPage::Tags
            && self.model.tags_dirty()
            && self.tag_writer.is_some()
            && !self.tag_write_pending
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
        self.sync_rows(window, cx);
        let title = identity_title(self.model.snapshot());
        let location = aggregate_location(self.model.snapshot());
        let refresh_error = self
            .refresh_error
            .as_deref()
            .map(|error| self.localized_error(error, "properties-refresh-failed"));
        let state_message = match self.model.state() {
            PropertiesState::Ready => refresh_error,
            PropertiesState::Replaced => {
                refresh_error.or_else(|| Some(self.message("properties-replaced").to_string()))
            }
            PropertiesState::Missing => Some(self.message("properties-missing").to_string()),
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
                // Enter does not close the window under an open review.
                if this.ownership_review.is_none() {
                    this.close(window);
                }
            }))
            .on_action(cx.listener(|this, _: &CancelProperties, window, cx| {
                if this.ownership_review.is_some() {
                    this.cancel_ownership_review(cx);
                } else {
                    this.close(window);
                }
            }))
            .on_action(cx.listener(|this, _: &Escape, window, cx| {
                if this.ownership_review.is_some() {
                    this.cancel_ownership_review(cx);
                } else {
                    this.close(window);
                }
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

/// The selection's POSIX permission support: the first item's filesystem
/// that does not support them decides, with its reason.
fn permission_capability(matrices: &[CapabilityMatrix]) -> CapabilityState {
    matrices
        .iter()
        .map(|matrix| matrix.get(CapabilityKind::Permissions).clone())
        .find(|state| *state != CapabilityState::Supported)
        .unwrap_or(CapabilityState::Supported)
}

/// A labelled line of text on the Permissions page.
fn permission_note(id: impl Into<SharedString>, text: String, role: Role) -> AnyElement {
    div()
        .id(id.into())
        .test_support()
        .role(role)
        .aria_label(text.clone())
        .text_sm()
        .child(text)
        .into_any_element()
}

/// The Varies mark beside a control whose selected items differ.
fn varies_mark(id: impl Into<SharedString>, varies: &str) -> AnyElement {
    permission_note(id, varies.to_owned(), Role::Label)
}

/// The region of one ACL list's editor, labelled `title`.
fn acl_list_region(key: &str, title: String, rows: Vec<AnyElement>) -> AnyElement {
    div()
        .id(SharedString::from(format!("permissions-acl-{key}")))
        .test_support()
        .role(Role::Region)
        .aria_label(title)
        .flex()
        .flex_col()
        .gap_1()
        .children(rows)
        .into_any_element()
}

fn aggregate_u32(value: &AggregateValue<u32>) -> String {
    match value {
        AggregateValue::Same(value) => value.to_string(),
        AggregateValue::Mixed => "Mixed".to_owned(),
        AggregateValue::Unavailable => "Unavailable".to_owned(),
    }
}

fn aggregate_mode(value: &AggregateValue<u32>) -> String {
    match value {
        AggregateValue::Same(value) => format!("{value:04o}"),
        AggregateValue::Mixed => "Mixed".to_owned(),
        AggregateValue::Unavailable => "Unavailable".to_owned(),
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
    use musheen_core::CapabilityReason;
    use musheen_ops::MutationError;
    use standard_library::fs as filesystem;
    use std as standard_library;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn volume_properties_fixture(available_bytes: u64, read_only: bool) -> VolumePropertiesModel {
        VolumePropertiesModel {
            id: VolumeId::new("fixture-volume").unwrap(),
            label: "Fixture volume".into(),
            device: "/dev/sdz1".into(),
            mount_points: vec!["/media/fixture".into()],
            filesystem_type: Some("ext4".into()),
            capacity: Some(Capacity::new(1_000, available_bytes)),
            read_only,
            capabilities: VolumeCapabilities::default(),
            available: true,
        }
    }

    #[gpui_kit::test]
    async fn open_volume_properties_window_updates_live_without_reopening(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let slot = Rc::new(RefCell::new(None));
        let opened = Rc::clone(&slot);
        let handle = cx.open_window(size(px(760.), px(620.)), move |window, cx| {
            let view = cx.new(|cx| {
                VolumePropertiesWindow::new(
                    volume_properties_fixture(400, false),
                    Catalog::system().unwrap(),
                    cx,
                )
            });
            opened.borrow_mut().replace(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow().clone().expect("the volume view opened");

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("volume-properties-capacity").label(),
                Some("Available: 400 B / 1000 B")
            );
        })
        .unwrap();

        cx.update(|cx| {
            view.update(cx, |window, cx| {
                window.update_model(volume_properties_fixture(125, true), cx);
            });
        });
        cx.update(|cx| {
            assert_eq!(view.read(cx).model().available_bytes(), Some(125));
            assert!(view.read(cx).model().is_read_only());
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("volume-properties-capacity").label(),
                Some("Available: 125 B / 1000 B")
            );
            assert_eq!(
                window.find("volume-properties-access").label(),
                Some("Mount mode: Read only")
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn open_volume_properties_window_reports_removal_and_closes_with_escape(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let slot = Rc::new(RefCell::new(None));
        let opened = Rc::clone(&slot);
        let handle = cx.open_window(size(px(760.), px(620.)), move |window, cx| {
            let view = cx.new(|cx| {
                VolumePropertiesWindow::new(
                    volume_properties_fixture(400, false),
                    Catalog::system().unwrap(),
                    cx,
                )
            });
            opened.borrow_mut().replace(view.clone());
            Root::new(view, window, cx)
        });
        let view = slot.borrow().clone().expect("the volume view opened");
        cx.update(|cx| view.update(cx, |window, cx| window.mark_unavailable(cx)));
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("volume-properties").role(), Some(Role::Dialog));
            assert_eq!(
                window.find("volume-properties-unavailable").role(),
                Some(Role::Alert)
            );
            window.dispatch_action(Box::new(CancelProperties), cx);
        })
        .unwrap();
        cx.run_until_parked();
        assert!(cx.update_window(handle.into(), |_, _, _| ()).is_err());
    }

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
                    .find("property-value-2")
                    .value()
                    .is_some_and(|value| !value.is_empty()),
                "the mode is exposed as a readable value"
            );
            window.click("permissions-advanced", cx);
            window.render_frame(cx);
            assert!(
                window
                    .find("permissions-acl-item-0")
                    .label()
                    .is_some_and(|label| label.starts_with("notes.txt")),
                "the access ACL is exposed as a readable label"
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
                window.find("permissions-owner-can-modify").label(),
                Some("يمكنه العرض والتعديل")
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
        let handle = cx.open_window(size(px(900.), px(900.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            properties = Some(view.clone());
            Root::new(view, window, cx)
        });
        let properties = properties.expect("the Properties view is constructed");

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("properties-page-permissions", cx);
            window.render_frame(cx);
            assert!(window.find("permissions-group-no-access").visible());
            assert!(window.find("permissions-others-no-access").visible());
        })
        .expect("the Properties window remains open while opening the page");
        cx.update(|cx| {
            assert!(
                !properties.read(cx).model.permissions().is_dirty(),
                "opening the Permissions page must not dirty the plan"
            );
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.click("permissions-group-no-access", cx);
            window.render_frame(cx);
            window.click("permissions-others-no-access", cx);
        })
        .expect("the Properties window remains open while editing");

        cx.update(|cx| {
            assert!(
                properties.read(cx).model.permissions().is_dirty(),
                "choosing an access must dirty the permission plan"
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

    /// Opens the Properties window of `paths` on its Permissions page.
    fn open_permissions_page(
        paths: &[PathBuf],
        catalog: Option<Catalog>,
        cx: &mut TestAppContext,
    ) -> (gpui_kit::WindowHandle<Root>, Entity<PropertiesWindow>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        let mut data = PropertiesWindowData::load(paths).expect("the items load");
        if let Some(catalog) = catalog {
            data = data.with_catalog(catalog);
        }
        let mut properties = None;
        let handle = cx.open_window(size(px(1000.), px(1600.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            properties = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("properties-page-permissions", cx);
            window.render_frame(cx);
        })
        .expect("the Properties window is open");
        (
            handle,
            properties.expect("the Properties view is constructed"),
        )
    }

    /// A privilege backend that records each request, with the mode its
    /// first item had then, and answers an ownership change, failing on
    /// `failing` when given.
    #[derive(Clone)]
    struct RecordingBackend {
        requests: Arc<Mutex<Vec<(BrokerRequest, u32)>>>,
        failing: Option<PathBuf>,
    }

    impl RecordingBackend {
        fn new(failing: Option<PathBuf>) -> Self {
            Self {
                requests: Arc::default(),
                failing,
            }
        }

        fn requests(&self) -> Vec<(BrokerRequest, u32)> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl PrivilegeBackend for RecordingBackend {
        fn provider(&self) -> PrivilegeProvider {
            PrivilegeProvider::Polkit
        }

        fn perform<'a>(
            &'a self,
            request: &'a BrokerRequest,
            _cancellation: CancellationToken,
            _authentication: Option<SecretBuffer>,
        ) -> musheen_core::BoxFuture<
            'a,
            Result<BrokerOutput, musheen_desktop::privilege::BrokerError>,
        > {
            use std::os::unix::fs::PermissionsExt as _;

            let mode = filesystem::symlink_metadata(request.target())
                .map_or(0, |metadata| metadata.permissions().mode() & 0o7777);
            self.requests.lock().unwrap().push((request.clone(), mode));
            let report = match &self.failing {
                None => musheen_desktop::privilege::OwnershipReport::new(1, None),
                Some(path) => musheen_desktop::privilege::OwnershipReport::new(
                    0,
                    Some(musheen_desktop::privilege::OwnershipFailure::new(
                        path,
                        &musheen_desktop::privilege::BrokerError::Io,
                    )),
                ),
            };
            Box::pin(async move { Ok(BrokerOutput::OwnershipChanged(report)) })
        }

        fn open_window<'a>(
            &'a self,
            _request: &'a BrokerRequest,
            _cancellation: CancellationToken,
            _authentication: Option<SecretBuffer>,
        ) -> musheen_core::BoxFuture<
            'a,
            Result<Arc<dyn crate::ElevatedSession>, musheen_desktop::privilege::BrokerError>,
        > {
            Box::pin(async { Err(musheen_desktop::privilege::BrokerError::InvalidRequest) })
        }
    }

    fn open_permissions_page_with_backend(
        paths: &[PathBuf],
        backend: RecordingBackend,
        cx: &mut TestAppContext,
    ) -> (gpui_kit::WindowHandle<Root>, Entity<PropertiesWindow>) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        let data = PropertiesWindowData::load(paths)
            .expect("the items load")
            .with_privilege_backend(Arc::new(backend));
        let mut properties = None;
        let handle = cx.open_window(size(px(1000.), px(1600.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            properties = Some(view.clone());
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("properties-page-permissions", cx);
            window.render_frame(cx);
        })
        .expect("the Properties window is open");
        (
            handle,
            properties.expect("the Properties view is constructed"),
        )
    }

    /// Clicks each of `ids` on the page, in order.
    fn click_all(handle: gpui_kit::WindowHandle<Root>, ids: &[&str], cx: &mut TestAppContext) {
        cx.update_window(handle.into(), |_, window, cx| {
            for id in ids {
                window.render_frame(cx);
                window.click(SharedString::from(id.to_string()), cx);
            }
            window.render_frame(cx);
        })
        .expect("the Properties window is open");
    }

    fn file_mode(path: &std::path::Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        filesystem::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    fn set_file_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        filesystem::set_permissions(path, filesystem::Permissions::from_mode(mode)).unwrap();
    }

    /// Clicks Apply and waits until `done` holds.
    async fn apply_until(
        handle: gpui_kit::WindowHandle<Root>,
        cx: &mut TestAppContext,
        done: impl Fn() -> bool + 'static,
    ) {
        click_all(handle, &["properties-apply"], cx);
        cx.wait_for(handle.into(), Duration::from_secs(2), move |_, _| done())
            .await;
    }

    fn command_output(program: &str, arguments: &[&str]) -> String {
        let output = std::process::Command::new(program)
            .args(arguments)
            .output()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_access_choices_set_the_described_modes(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        set_file_mode(&file, 0o600);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert_eq!(
                window.find("permissions-group-can-view").label(),
                Some("Can View")
            );
        })
        .unwrap();
        click_all(
            handle,
            &["permissions-group-can-view", "permissions-others-can-view"],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o644).await;

        let folder = temporary.path().join("folder");
        filesystem::create_dir(&folder).unwrap();
        set_file_mode(&folder, 0o700);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&folder), None, cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert_eq!(
                window.find("permissions-group-can-view").label(),
                Some("Can View Content")
            );
        })
        .unwrap();
        click_all(handle, &["permissions-group-can-view"], cx);
        let checked = folder.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o750).await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_executable_checkbox_follows_read_access(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        for (name, before, after) in [
            ("everyone-reads", 0o644, 0o755),
            ("others-do-not", 0o640, 0o750),
            ("executable", 0o755, 0o644),
        ] {
            let file = temporary.path().join(name);
            filesystem::write(&file, b"#!/bin/sh\n").unwrap();
            set_file_mode(&file, before);
            let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
            click_all(handle, &["permissions-executable"], cx);
            let checked = file.clone();
            apply_until(handle, cx, move || file_mode(&checked) == after).await;
        }
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_mixed_selection_shows_varies_and_changes_only_what_the_user_picks(
        cx: &mut TestAppContext,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join("first.txt");
        let second = temporary.path().join("second.txt");
        filesystem::write(&first, b"first").unwrap();
        filesystem::write(&second, b"second").unwrap();
        set_file_mode(&first, 0o640);
        set_file_mode(&second, 0o600);
        let (handle, _) = open_permissions_page(&[first.clone(), second.clone()], None, cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.find("permissions-group-varies").visible());
        })
        .unwrap();
        click_all(handle, &["permissions-others-can-view"], cx);
        let (checked_first, checked_second) = (first.clone(), second.clone());
        apply_until(handle, cx, move || {
            file_mode(&checked_first) == 0o644 && file_mode(&checked_second) == 0o604
        })
        .await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_advanced_bits_set_exact_modes(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("shared");
        filesystem::create_dir(&folder).unwrap();
        set_file_mode(&folder, 0o755);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&folder), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-bit-others-write",
                "permissions-bit-setgid",
            ],
            cx,
        );
        let checked = folder.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o2757).await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_offers_every_account_and_group(cx: &mut TestAppContext) {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        let user = command_output("/usr/bin/id", &["-un"]);
        let ids = |database: &str| -> Vec<u32> {
            command_output("/usr/bin/getent", &[database])
                .lines()
                .filter_map(|line| line.split(':').nth(2)?.parse().ok())
                .collect()
        };
        let (users, groups) = (ids("passwd"), ids("group"));
        assert!(users.contains(&0) && groups.contains(&0));
        let own_groups = command_output("/usr/bin/id", &["-G"])
            .split_whitespace()
            .map(|gid| gid.parse::<u32>().unwrap())
            .collect::<Vec<_>>();
        let current = filesystem::metadata(&file).unwrap().gid();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find("permissions-owner-picker").label(),
                Some(user.as_str())
            );
            window.click("permissions-owner-picker", cx);
            window.render_frame(cx);
            for uid in &users {
                assert!(
                    window
                        .try_find(SharedString::from(format!(
                            "permissions-owner-option-{uid}"
                        )))
                        .is_some(),
                    "user {uid} is offered as the owner"
                );
            }
            window.click("permissions-group-picker", cx);
            window.render_frame(cx);
            for gid in &groups {
                assert!(
                    window
                        .try_find(SharedString::from(format!(
                            "permissions-group-option-{gid}"
                        )))
                        .is_some(),
                    "group {gid} is offered"
                );
            }
            assert!(
                window.try_find("permissions-needs-admin").is_none(),
                "nothing chosen yet needs administrator rights"
            );
        })
        .unwrap();
        // One of the user's own groups on their own file still applies
        // without authorization, as before.
        if let Some(other) = own_groups.iter().copied().find(|gid| *gid != current) {
            let option = format!("permissions-group-option-{other}");
            click_all(handle, &[option.as_str()], cx);
            cx.update_window(handle.into(), |_, window, _| {
                assert!(window.try_find("permissions-needs-admin").is_none());
            })
            .unwrap();
            let checked = file.clone();
            apply_until(handle, cx, move || {
                filesystem::metadata(&checked).unwrap().gid() == other
            })
            .await;
        }
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_says_when_a_change_needs_administrator(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let (handle, properties) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &["permissions-owner-picker", "permissions-owner-option-0"],
            cx,
        );
        let english = Catalog::load(crate::Locale::EnUs).unwrap();
        let needs_admin = english
            .message("permissions-needs-admin")
            .unwrap()
            .to_owned();
        let apply_as_administrator = english
            .message("properties-apply-as-administrator")
            .unwrap()
            .to_owned();
        cx.update_window(handle.into(), |_, window, _| {
            assert_eq!(
                window.find("permissions-needs-admin").label(),
                Some(needs_admin.as_str()),
                "the page says the owner change needs administrator rights before Apply"
            );
            assert_eq!(
                window.find("properties-apply").label(),
                Some(apply_as_administrator.as_str())
            );
        })
        .unwrap();
        assert!(is_dirty(&properties, cx));
        assert!(
            owned_by_user(&file),
            "choosing an owner changes nothing before Apply"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn change_ownership_review_shows_each_item_before_authorization(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join("first.txt");
        let second = temporary.path().join("second.txt");
        filesystem::write(&first, b"first").unwrap();
        filesystem::write(&second, b"second").unwrap();
        let (handle, _) = open_permissions_page(&[first.clone(), second.clone()], None, cx);
        let user = command_output("/usr/bin/id", &["-un"]);
        let group = command_output("/usr/bin/id", &["-gn"]);
        let root_user = command_output("/usr/bin/getent", &["passwd", "0"])
            .split(':')
            .next()
            .unwrap()
            .to_owned();
        let root_group = command_output("/usr/bin/getent", &["group", "0"])
            .split(':')
            .next()
            .unwrap()
            .to_owned();
        click_all(
            handle,
            &[
                "permissions-owner-picker",
                "permissions-owner-option-0",
                "permissions-group-picker",
                "permissions-group-option-0",
                "properties-apply",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window.find("ownership-review").visible(),
                "Apply as Administrator shows the review before it asks for authorization"
            );
            for (index, path) in [&first, &second].into_iter().enumerate() {
                let row = window
                    .find(SharedString::from(format!("ownership-review-item-{index}")))
                    .label()
                    .unwrap_or_default()
                    .to_owned();
                for part in [
                    path.display().to_string(),
                    user.clone(),
                    group.clone(),
                    root_user.clone(),
                    root_group.clone(),
                ] {
                    assert!(row.contains(&part), "{row:?} names {part:?}");
                }
            }
            window.click("ownership-review-cancel", cx);
            window.render_frame(cx);
            assert!(window.try_find("ownership-review").is_none());
        })
        .unwrap();
        assert!(
            owned_by_user(&first) && owned_by_user(&second),
            "Cancel changes nothing"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn change_ownership_runs_after_the_mode_change_in_the_queue(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        set_file_mode(&file, 0o644);
        let backend = RecordingBackend::new(None);
        let (handle, properties) =
            open_permissions_page_with_backend(std::slice::from_ref(&file), backend.clone(), cx);
        click_all(
            handle,
            &[
                "permissions-others-no-access",
                "permissions-owner-picker",
                "permissions-owner-option-0",
                "properties-apply",
            ],
            cx,
        );
        assert!(
            backend.requests().is_empty(),
            "nothing is asked before the review is confirmed"
        );
        click_all(handle, &["ownership-review-confirm"], cx);
        let seen = backend.clone();
        cx.wait_for(handle.into(), Duration::from_secs(5), move |_, _| {
            seen.requests().len() == 1
        })
        .await;
        let (request, mode_then) = backend.requests().remove(0);
        assert_eq!(mode_then, 0o640, "the user's own mode change ran first");
        match request.operation() {
            musheen_desktop::privilege::BrokerOperation::ChangeOwnership {
                items,
                owner,
                group,
                contents,
            } => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].path(), file);
                assert_eq!(*owner, Some(0));
                assert_eq!(*group, None);
                assert_eq!(*contents, None);
            }
            other => panic!("Apply as Administrator asked for {other:?}"),
        }
        let edited = properties.clone();
        cx.wait_for(handle.into(), Duration::from_secs(5), move |_, cx| {
            !edited.read(cx).model.permissions().is_dirty()
        })
        .await;
        assert_eq!(
            backend.requests().len(),
            1,
            "one authorization for the Apply"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn change_ownership_review_holds_the_page_and_shows_the_provider(
        cx: &mut TestAppContext,
    ) {
        use std::os::unix::fs::PermissionsExt as _;

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        set_file_mode(&file, 0o644);
        // A socket in the selection is left as it is, and not reviewed.
        let socket = temporary.path().join("socket");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let backend = RecordingBackend::new(None);
        let (handle, properties) = open_permissions_page_with_backend(
            &[file.clone(), socket.clone()],
            backend.clone(),
            cx,
        );
        click_all(
            handle,
            &[
                "permissions-owner-picker",
                "permissions-owner-option-0",
                "properties-apply",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(
                window
                    .find("ownership-review-provider")
                    .label()
                    .is_some_and(|label| label.contains("Polkit")),
                "the review names how it will ask"
            );
            assert!(window.find("ownership-review-kernel").visible());
            assert!(window.try_find("ownership-review-item-0").is_some());
            assert!(
                window.try_find("ownership-review-item-1").is_none(),
                "the socket is not in the review"
            );
            // The page keeps what was reviewed while the review is open.
            window.click("permissions-others-no-access", cx);
            window.render_frame(cx);
            assert!(window.try_find("properties-apply").is_none());
            window.dispatch_action(Box::new(CancelProperties), cx);
        })
        .unwrap();
        // A dispatched action runs once the update ends.
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window.try_find("ownership-review").is_none(),
                "Escape closes the review, not the window"
            );
        })
        .expect("the window stays open");
        cx.update(|cx| {
            let permissions = properties.read(cx).model.permissions();
            assert!(permissions.is_dirty());
            assert!(
                !permissions.access_chosen(AccessClass::Others),
                "the mode choice made while the review was open did nothing"
            );
        });
        assert_eq!(
            filesystem::metadata(&file).unwrap().permissions().mode() & 0o7777,
            0o644
        );
        assert!(backend.requests().is_empty());
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn change_ownership_runs_after_its_window_closes(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        set_file_mode(&file, 0o644);
        let backend = RecordingBackend::new(None);
        let (handle, _) =
            open_permissions_page_with_backend(std::slice::from_ref(&file), backend.clone(), cx);
        click_all(
            handle,
            &[
                "permissions-others-no-access",
                "permissions-owner-picker",
                "permissions-owner-option-0",
                "properties-apply",
                "ownership-review-confirm",
            ],
            cx,
        );
        // The reviewed change is a queued job; closing the window does not
        // drop it.
        cx.update_window(handle.into(), |_, window, _| window.remove_window())
            .unwrap();
        cx.run_until_parked();
        let requests = backend.requests();
        assert_eq!(requests.len(), 1, "the job ran after its window closed");
        assert_eq!(
            requests[0].1, 0o640,
            "with the user's own mode change first"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn change_ownership_own_apply_is_not_an_outside_change(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let (handle, properties) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &["permissions-owner-picker", "permissions-owner-option-0"],
            cx,
        );
        let snapshot = PropertySnapshot::load(std::slice::from_ref(&file)).unwrap();
        let accounts = Accounts::load(&snapshot);
        properties.update(cx, |properties, cx| {
            // While this window's own Apply runs, a metadata change the live
            // refresh sees is that Apply, not an outside change.
            properties
                .permission_batch
                .begin([JobId::new(7).expect("a job ID is not zero")]);
            let first = PropertySnapshot::load(std::slice::from_ref(&file)).unwrap();
            properties.finish_live_refresh(
                Ok((
                    PropertyRefresh::MetadataChanged,
                    Some((first, accounts.clone())),
                )),
                cx,
            );
            assert_ne!(properties.model.state(), PropertiesState::Replaced);
            // Without an Apply running, the same change while editing is one.
            properties.permission_batch = PermissionBatchState::default();
            properties.finish_live_refresh(
                Ok((PropertyRefresh::MetadataChanged, Some((snapshot, accounts)))),
                cx,
            );
            assert_eq!(properties.model.state(), PropertiesState::Replaced);
        });
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn change_ownership_failure_names_its_item_on_the_page(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let backend = RecordingBackend::new(Some(file.clone()));
        let (handle, _) =
            open_permissions_page_with_backend(std::slice::from_ref(&file), backend, cx);
        click_all(
            handle,
            &[
                "permissions-owner-picker",
                "permissions-owner-option-0",
                "properties-apply",
                "ownership-review-confirm",
            ],
            cx,
        );
        let expected = file.display().to_string();
        cx.wait_for(handle.into(), Duration::from_secs(5), move |window, _| {
            window
                .try_find("permissions-validation")
                .and_then(|error| error.label().map(str::to_owned))
                .is_some_and(|label| label.contains(&expected))
        })
        .await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_changes_nothing_before_apply_and_skips_items_needing_no_change(
        cx: &mut TestAppContext,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let unchanged = temporary.path().join("unchanged.txt");
        let changed = temporary.path().join("changed.txt");
        filesystem::write(&unchanged, b"unchanged").unwrap();
        filesystem::write(&changed, b"changed").unwrap();
        set_file_mode(&unchanged, 0o600);
        set_file_mode(&changed, 0o640);
        let (handle, properties) =
            open_permissions_page(&[unchanged.clone(), changed.clone()], None, cx);
        click_all(handle, &["permissions-group-no-access"], cx);
        assert_eq!(file_mode(&unchanged), 0o600, "nothing changes before Apply");
        assert_eq!(file_mode(&changed), 0o640, "nothing changes before Apply");
        let checked = changed.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o600).await;
        assert_eq!(file_mode(&unchanged), 0o600);
        cx.update(|cx| {
            assert!(
                !properties.read(cx).model.permissions().is_dirty(),
                "an item that needs no change does not fail the Apply"
            );
        });
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_no_access_on_own_file_can_be_undone(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("locked.txt");
        filesystem::write(&file, b"locked").unwrap();
        set_file_mode(&file, 0o644);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &[
                "permissions-owner-no-access",
                "permissions-group-no-access",
                "permissions-others-no-access",
            ],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0).await;

        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(handle, &["permissions-owner-can-modify"], cx);
        let checked = file.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o600).await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_no_access_takes_execute_away(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("tool");
        filesystem::write(&file, b"#!/bin/sh\n").unwrap();
        set_file_mode(&file, 0o751);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window.find("permissions-others-varies").visible(),
                "execute without read is no choice"
            );
        })
        .unwrap();
        click_all(handle, &["permissions-others-no-access"], cx);
        let checked = file.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o750).await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_names_acl_entries_and_the_group_row_sets_their_mask(
        cx: &mut TestAppContext,
    ) {
        use posix_acl::{ACL_READ, ACL_WRITE, PosixACL, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o660);
        let mut acl = PosixACL::read_acl(&file).expect("the temporary filesystem has ACLs");
        acl.set(
            Qualifier::User(musheen_desktop::effective_user()),
            ACL_READ | ACL_WRITE,
        );
        acl.fix_mask();
        acl.write_acl(&file)
            .expect("the temporary filesystem takes a named ACL entry");
        let user = command_output("/usr/bin/id", &["-un"]);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window.find("permissions-acl-mask-note").visible(),
                "the note shows beside the Group row, with Advanced closed"
            );
        })
        .unwrap();
        click_all(handle, &["permissions-advanced"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window
                    .find("permissions-acl-item-0")
                    .label()
                    .is_some_and(|label| label.contains(&format!("User {user}: rw-"))),
                "a named entry shows its user's name"
            );
        })
        .unwrap();
        click_all(handle, &["permissions-group-can-view"], cx);
        let checked = file.clone();
        apply_until(handle, cx, move || {
            PosixACL::read_acl(&checked)
                .ok()
                .and_then(|acl| acl.get(Qualifier::Mask))
                == Some(ACL_READ)
        })
        .await;
    }

    /// Gives `path` the named ACL entries `entries`, with the mask setfacl
    /// sets.
    fn set_named_acl(path: &std::path::Path, entries: &[(posix_acl::Qualifier, u32)]) {
        let mut acl =
            posix_acl::PosixACL::read_acl(path).expect("the temporary filesystem has ACLs");
        for (qualifier, permissions) in entries {
            acl.set(*qualifier, *permissions);
        }
        acl.fix_mask();
        acl.write_acl(path)
            .expect("the temporary filesystem takes named ACL entries");
    }

    /// The entries of `path`'s access ACL, or of its default ACL.
    fn acl_entries(path: &std::path::Path, default: bool) -> Vec<(posix_acl::Qualifier, u32)> {
        let acl = if default {
            posix_acl::PosixACL::read_default_acl(path)
        } else {
            posix_acl::PosixACL::read_acl(path)
        };
        acl.expect("the ACL reads")
            .entries()
            .into_iter()
            .map(|entry| (entry.qual, entry.perm))
            .collect()
    }

    /// The rights `path`'s access or default ACL gives `qualifier`.
    fn acl_right(
        path: &std::path::Path,
        default: bool,
        qualifier: posix_acl::Qualifier,
    ) -> Option<u32> {
        acl_entries(path, default)
            .into_iter()
            .find_map(|(entry, permissions)| (entry == qualifier).then_some(permissions))
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_lists_acl_entries_for_editing(cx: &mut TestAppContext) {
        use posix_acl::{ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o660);
        let me = musheen_desktop::effective_user();
        set_named_acl(&file, &[(Qualifier::User(me), ACL_READ | ACL_WRITE)]);
        let user = command_output("/usr/bin/id", &["-un"]);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(handle, &["permissions-advanced"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert_eq!(
                window
                    .find(SharedString::from(format!(
                        "permissions-acl-access-user-{me}"
                    )))
                    .label(),
                Some(format!("User {user}").as_str()),
                "the entry shows its user's name"
            );
            for control in ["read", "write", "execute", "remove"] {
                assert!(
                    window
                        .find(SharedString::from(format!(
                            "permissions-acl-access-user-{me}-{control}"
                        )))
                        .visible(),
                    "the entry offers {control}"
                );
            }
            assert!(window.find("permissions-acl-access-add").visible());
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_adds_changes_and_removes_named_entries(cx: &mut TestAppContext) {
        use posix_acl::{ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o640);
        set_named_acl(&file, &[(Qualifier::User(12_345), ACL_READ)]);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-add",
                "permissions-acl-access-add-user-0",
                "permissions-acl-access-user-0-write",
                "permissions-acl-access-user-12345-remove",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window
                    .try_find("permissions-acl-access-user-12345")
                    .is_none(),
                "a removed entry leaves the list"
            );
        })
        .unwrap();
        let checked = file.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, false, Qualifier::User(0)).is_some()
        })
        .await;
        assert_eq!(
            acl_entries(&file, false),
            vec![
                (Qualifier::UserObj, ACL_READ | ACL_WRITE),
                (Qualifier::User(0), ACL_READ | ACL_WRITE),
                (Qualifier::GroupObj, ACL_READ),
                (Qualifier::Mask, ACL_READ | ACL_WRITE),
                (Qualifier::Other, 0),
            ],
            "the new entry has the rights chosen, the removed one is gone, the mode's entries \
             stay, and the mask is the union of the group class"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_sets_the_mask_as_setfacl_does_unless_the_group_row_is_set(
        cx: &mut TestAppContext,
    ) {
        use posix_acl::{ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o640);
        set_named_acl(&file, &[(Qualifier::User(12_345), ACL_READ)]);

        // An edit alone: the mask becomes the union of the group-class
        // entries.
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-add",
                "permissions-acl-access-add-group-0",
                "permissions-acl-access-group-0-write",
            ],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, false, Qualifier::Group(0)).is_some()
        })
        .await;
        assert_eq!(
            acl_right(&file, false, Qualifier::Group(0)),
            Some(ACL_READ | ACL_WRITE)
        );
        assert_eq!(
            acl_right(&file, false, Qualifier::Mask),
            Some(ACL_READ | ACL_WRITE)
        );
        assert_eq!(
            acl_right(&file, false, Qualifier::GroupObj),
            Some(ACL_READ),
            "the owning group's entry stays"
        );
        assert_eq!(file_mode(&file), 0o660, "the group bits show the mask");

        // With the Group row set in the same Apply, the row sets the mask,
        // and the Others choice is kept.
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-add",
                "permissions-acl-access-add-user-0",
                "permissions-acl-access-user-0-write",
                "permissions-group-no-access",
                "permissions-others-can-view",
            ],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, false, Qualifier::User(0)).is_some()
        })
        .await;
        assert_eq!(
            acl_right(&file, false, Qualifier::User(0)),
            Some(ACL_READ | ACL_WRITE)
        );
        assert_eq!(
            acl_right(&file, false, Qualifier::Mask),
            Some(0),
            "the Group row sets the mask"
        );
        assert_eq!(
            acl_right(&file, false, Qualifier::Other),
            Some(ACL_READ),
            "the mode change of the same Apply is kept"
        );
        assert_eq!(file_mode(&file), 0o604);
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_shows_varies_and_leaves_differing_entries(cx: &mut TestAppContext) {
        use posix_acl::{ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join("first.txt");
        let second = temporary.path().join("second.txt");
        for (path, rights) in [(&first, ACL_READ), (&second, ACL_READ | ACL_WRITE)] {
            filesystem::write(path, b"shared").unwrap();
            set_file_mode(path, 0o640);
            set_named_acl(path, &[(Qualifier::User(12_345), rights)]);
        }
        let (handle, _) = open_permissions_page(&[first.clone(), second.clone()], None, cx);
        click_all(handle, &["permissions-advanced"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.find("permissions-acl-access-varies").visible());
            for id in [
                "permissions-acl-access-add",
                "permissions-acl-access-user-12345-write",
                "permissions-acl-access-user-12345-remove",
            ] {
                assert!(window.try_find(id).is_none(), "{id} is not offered");
            }
        })
        .unwrap();
        click_all(handle, &["permissions-others-can-view"], cx);
        let (checked_first, checked_second) = (first.clone(), second.clone());
        apply_until(handle, cx, move || {
            file_mode(&checked_first) & 0o7 == 0o4 && file_mode(&checked_second) & 0o7 == 0o4
        })
        .await;
        for (path, rights) in [(&first, ACL_READ), (&second, ACL_READ | ACL_WRITE)] {
            assert_eq!(
                acl_right(path, false, Qualifier::User(12_345)),
                Some(rights),
                "{path:?} keeps its entry"
            );
            assert_eq!(acl_right(path, false, Qualifier::Mask), Some(rights));
        }
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_applies_to_the_entries_each_item_has_when_it_applies(
        cx: &mut TestAppContext,
    ) {
        use musheen_ops::MutationProvider as _;
        use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o640);
        set_named_acl(&file, &[(Qualifier::User(12_345), ACL_READ)]);
        let (handle, properties) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-user-12345-write",
            ],
            cx,
        );
        let Ok((roots, scope, change)) =
            cx.update(|cx| properties.read(cx).model.permission_request())
        else {
            panic!("the page has a change to apply");
        };
        // Another program adds an entry after the page read the item.
        set_named_acl(&file, &[(Qualifier::Group(23_456), ACL_READ | ACL_EXECUTE)]);
        let mut store = LocalStore::new();
        for root in roots {
            let identity = store.identity(&root).unwrap().unwrap();
            musheen_ops::MetadataPlan::preflight(
                &mut store,
                root,
                identity.to_vec(),
                scope,
                change.clone(),
            )
            .unwrap()
            .execute(&mut store)
            .unwrap();
        }
        assert_eq!(
            acl_right(&file, false, Qualifier::User(12_345)),
            Some(ACL_READ | ACL_WRITE)
        );
        assert_eq!(
            acl_right(&file, false, Qualifier::Group(23_456)),
            Some(ACL_READ | ACL_EXECUTE),
            "an entry the user did not edit stays"
        );
        assert_eq!(
            acl_right(&file, false, Qualifier::Mask),
            Some(ACL_READ | ACL_WRITE | ACL_EXECUTE)
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_edits_a_folders_default_entries(cx: &mut TestAppContext) {
        use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("shared");
        filesystem::create_dir(&folder).unwrap();
        set_file_mode(&folder, 0o750);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&folder), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-default-add",
                "permissions-acl-default-add-group-0",
            ],
            cx,
        );
        let checked = folder.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, true, Qualifier::Group(0)).is_some()
        })
        .await;
        let all = ACL_READ | ACL_WRITE | ACL_EXECUTE;
        let view = ACL_READ | ACL_EXECUTE;
        assert_eq!(
            acl_entries(&folder, true),
            vec![
                (Qualifier::UserObj, all),
                (Qualifier::GroupObj, view),
                (Qualifier::Group(0), view),
                (Qualifier::Mask, view),
                (Qualifier::Other, 0),
            ],
            "a new default entry starts from the folder's mode, as setfacl does"
        );
        assert_eq!(
            acl_entries(&folder, false).len(),
            3,
            "the access entries stay as they were"
        );

        // Removing the entry keeps the other default entries, as setfacl
        // does.
        let (handle, _) = open_permissions_page(std::slice::from_ref(&folder), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-default-group-0-remove",
            ],
            cx,
        );
        let checked = folder.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, true, Qualifier::Group(0)).is_none()
        })
        .await;
        assert_eq!(
            acl_entries(&folder, true),
            vec![
                (Qualifier::UserObj, all),
                (Qualifier::GroupObj, view),
                (Qualifier::Mask, view),
                (Qualifier::Other, 0),
            ]
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_apply_to_contents_gives_execute_only_where_an_execute_bit_is(
        cx: &mut TestAppContext,
    ) {
        use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("folder");
        let inner = folder.join("inner");
        let plain = folder.join("plain.txt");
        let program = folder.join("program");
        filesystem::create_dir(&folder).unwrap();
        filesystem::create_dir(&inner).unwrap();
        filesystem::write(&plain, b"plain").unwrap();
        filesystem::write(&program, b"program").unwrap();
        set_file_mode(&folder, 0o755);
        set_file_mode(&inner, 0o755);
        set_file_mode(&plain, 0o644);
        set_file_mode(&program, 0o755);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&folder), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-add",
                "permissions-acl-access-add-user-0",
                "permissions-acl-access-user-0-write",
                "permissions-acl-default-add",
                "permissions-acl-default-add-user-0",
                "permissions-acl-default-user-0-write",
                "permissions-scope-recursive",
                "permissions-review-scope",
            ],
            cx,
        );
        // The folder changes after what it contains.
        let checked = folder.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, true, Qualifier::User(0)).is_some()
        })
        .await;
        let all = ACL_READ | ACL_WRITE | ACL_EXECUTE;
        for path in [&folder, &inner] {
            assert_eq!(
                acl_right(path, false, Qualifier::User(0)),
                Some(all),
                "{path:?}"
            );
            assert_eq!(
                acl_right(path, true, Qualifier::User(0)),
                Some(all),
                "{path:?} takes the default entry"
            );
        }
        assert_eq!(
            acl_right(&program, false, Qualifier::User(0)),
            Some(all),
            "a file with an execute bit takes execute"
        );
        assert_eq!(
            acl_right(&plain, false, Qualifier::User(0)),
            Some(ACL_READ | ACL_WRITE),
            "a file without an execute bit does not"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_offers_edits_only_on_the_users_own_items(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(handle, &["permissions-advanced"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.find("permissions-acl-access-add").visible());
        })
        .unwrap();

        // Another user's file: its entries show, and none can change. The
        // superuser owns it.
        let other = PathBuf::from("/etc/passwd");
        if filesystem::symlink_metadata(&other).is_err() || owned_by_user(&other) {
            return;
        }
        let (handle, _) = open_permissions_page(std::slice::from_ref(&other), None, cx);
        click_all(handle, &["permissions-advanced"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.find("permissions-acl-item-0").visible());
            assert!(window.try_find("permissions-acl-access-add").is_none());
        })
        .unwrap();
    }

    const ACL_SCENARIO_ROOT: &str = "MUSHEEN_ACL_SCENARIO_ROOT";

    /// A filesystem that takes modes but refuses ACLs: ramfs, mounted as
    /// root in a user and mount namespace. `unshare` starts this test binary
    /// as `acl_editing_unsupported_child`. Where user namespaces are not
    /// allowed, the test says so and passes.
    #[cfg(unix)]
    #[test]
    fn acl_editing_is_read_only_without_acl_support() {
        let allowed = std::process::Command::new("unshare")
            .args(["--map-auto", "--map-root-user", "true"])
            .status()
            .is_ok_and(|status| status.success());
        if !allowed {
            eprintln!("user namespaces are not allowed here; not checked");
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let output = std::process::Command::new("unshare")
            .args(["--map-auto", "--map-root-user", "--mount"])
            .arg(std::env::current_exe().unwrap())
            .args([
                "dialogs::properties::tests::acl_editing_unsupported_child",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ACL_SCENARIO_ROOT, root.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "the check failed inside the namespace:\n{stdout}\n{stderr}"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_unsupported_child(cx: &mut TestAppContext) {
        let Some(root) = std::env::var_os(ACL_SCENARIO_ROOT) else {
            return;
        };
        let root = PathBuf::from(root);
        let inner = root.join("inner");
        let mount = |kind: &str, path: &std::path::Path| {
            let mounted = std::process::Command::new("mount")
                .args(["-t", kind, "none"])
                .arg(path)
                .status()
                .unwrap();
            assert!(mounted.success(), "{kind} mounts");
        };
        mount("tmpfs", &root);
        filesystem::create_dir(&inner).unwrap();
        mount("ramfs", &inner);
        let file = inner.join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        set_file_mode(&file, 0o644);
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        // POSIX permissions are supported here; only ACLs are not.
        let data = PropertiesWindowData::load(std::slice::from_ref(&file))
            .unwrap()
            .with_permission_capability(CapabilityState::Supported);
        let handle = cx.open_window(size(px(1000.), px(1600.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            Root::new(view, window, cx)
        });
        click_all(
            handle,
            &["properties-page-permissions", "permissions-advanced"],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window
                    .find("permissions-acl-read-only")
                    .label()
                    .is_some_and(|label| label.contains("does not support")),
                "the page says why"
            );
            assert!(window.try_find("permissions-acl-access-add").is_none());
            for id in ["permissions-read-only", "permissions-mode-lock"] {
                assert!(window.try_find(id).is_none(), "the mode can still change");
            }
        })
        .unwrap();

        // Apply to contents reaches the ramfs through a nested mount: an
        // edit that only removes entries leaves its items as they are, and
        // one that adds an entry fails there.
        let run = |step: musheen_ops::AclEditStep| {
            use musheen_ops::MutationProvider as _;

            let mut store = LocalStore::new();
            let target = StorePath::from_unix_path(root.as_os_str());
            let identity = store.identity(&target).unwrap().unwrap();
            musheen_ops::MetadataPlan::preflight(
                &mut store,
                target,
                identity.to_vec(),
                MetadataScope::recursive(true, true),
                MetadataChange::new().with_access_acl(musheen_ops::AclChange::Edit(
                    musheen_ops::AclEdit::new(vec![step]),
                )),
            )
            .and_then(|plan| plan.execute(&mut store))
        };
        let named = musheen_ops::AclQualifier::User(12_345);
        assert_eq!(run(musheen_ops::AclEditStep::Remove(named.clone())), Ok(()));
        assert_eq!(
            run(musheen_ops::AclEditStep::Set(musheen_ops::AclEntry::new(
                named, true, false, false
            ))),
            Err(MutationError::Unsupported)
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_every_string_is_localized(cx: &mut TestAppContext) {
        use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("shared");
        filesystem::create_dir(&folder).unwrap();
        set_file_mode(&folder, 0o750);
        set_named_acl(
            &folder,
            &[(Qualifier::User(12_345), ACL_READ | ACL_EXECUTE)],
        );
        let pseudo = || Some(Catalog::load(crate::Locale::EnXa).unwrap());
        let (handle, _) = open_permissions_page(std::slice::from_ref(&folder), pseudo(), cx);
        click_all(handle, &["permissions-advanced"], cx);
        let localized = |window: &Window, id: &str| {
            assert!(
                window
                    .find(SharedString::from(id.to_owned()))
                    .label()
                    .is_some_and(|label| label.starts_with('⟦')),
                "{id} is localized"
            );
        };
        cx.update_window(handle.into(), |_, window, _| {
            for id in [
                "permissions-acl-access",
                "permissions-acl-default",
                "permissions-acl-access-user-12345",
                "permissions-acl-access-user-12345-read",
                "permissions-acl-access-user-12345-write",
                "permissions-acl-access-user-12345-execute",
                "permissions-acl-access-user-12345-remove",
                "permissions-acl-access-add",
                "permissions-acl-default-add",
            ] {
                localized(window, id);
            }
        })
        .unwrap();

        let other = temporary.path().join("other");
        filesystem::create_dir(&other).unwrap();
        set_named_acl(&other, &[(Qualifier::User(12_345), ACL_READ | ACL_WRITE)]);
        let (handle, _) = open_permissions_page(&[folder, other], pseudo(), cx);
        click_all(handle, &["permissions-advanced"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            localized(window, "permissions-acl-access-varies");
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_keeps_a_group_row_set_back_to_the_value_it_had(cx: &mut TestAppContext) {
        use posix_acl::{ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o640);
        set_named_acl(&file, &[(Qualifier::User(12_345), ACL_READ)]);
        // The edit widens the mask to rw-; the Group row sets it back to
        // r--, the value the group bits had.
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-user-12345-write",
                "permissions-group-can-view",
            ],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, false, Qualifier::User(12_345)) == Some(ACL_READ | ACL_WRITE)
                && acl_right(&checked, false, Qualifier::Mask) == Some(ACL_READ)
        })
        .await;
        assert_eq!(file_mode(&file), 0o640);
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_leaves_links_in_the_selection_as_they_are(cx: &mut TestAppContext) {
        use posix_acl::{ACL_READ, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        let link = temporary.path().join("link");
        filesystem::write(&file, b"notes").unwrap();
        std::os::unix::fs::symlink("notes.txt", &link).unwrap();
        let (handle, properties) = open_permissions_page(&[file.clone(), link], None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-add",
                "permissions-acl-access-add-user-0",
            ],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, false, Qualifier::User(0)) == Some(ACL_READ)
        })
        .await;
        cx.update(|cx| {
            assert!(
                properties.read(cx).permission_error.is_none(),
                "the link the edit does not reach does not fail it"
            );
        });
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn acl_editing_shows_what_its_own_apply_left(cx: &mut TestAppContext) {
        use posix_acl::{ACL_EXECUTE, ACL_READ, ACL_WRITE, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o660);
        set_named_acl(&file, &[(Qualifier::User(12_345), ACL_READ)]);
        // Write for 12345 leaves the mask rw- and the mode 0660, so only the
        // ACL changes.
        let (handle, properties) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-user-12345-write",
            ],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, false, Qualifier::User(12_345)) == Some(ACL_READ | ACL_WRITE)
        })
        .await;
        let shown = properties.clone();
        cx.wait_for(handle.into(), Duration::from_secs(2), move |_, cx| {
            shown
                .read(cx)
                .model
                .permissions()
                .acl_entries(AclList::Access)
                .is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|(name, rights)| *name == AclName::User(12_345) && rights.write)
                })
        })
        .await;
        // The next edit starts from what the Apply left.
        click_all(handle, &["permissions-acl-access-user-12345-execute"], cx);
        let checked = file.clone();
        apply_until(handle, cx, move || {
            acl_right(&checked, false, Qualifier::User(12_345))
                .is_some_and(|rights| rights & ACL_EXECUTE != 0)
        })
        .await;
        assert_eq!(
            acl_right(&file, false, Qualifier::User(12_345)),
            Some(ACL_READ | ACL_WRITE | ACL_EXECUTE)
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_says_the_group_row_sets_the_mask_of_a_new_entry(
        cx: &mut TestAppContext,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        set_file_mode(&file, 0o640);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.try_find("permissions-acl-mask-note").is_none());
        })
        .unwrap();
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-acl-access-add",
                "permissions-acl-access-add-user-0",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window.find("permissions-acl-mask-note").visible(),
                "the first named entry makes the Group row set the mask"
            );
        })
        .unwrap();
    }

    /// The page's model for `file`, owned by the user, with a named entry
    /// for user 12345, as `effective_user` sees it.
    fn acl_page(file: &std::path::Path, effective_user: u32) -> PermissionsPageModel {
        let snapshot = PropertySnapshot::load(&[file.to_path_buf()]).unwrap();
        let accounts = Accounts::fixed(
            effective_user,
            Vec::new(),
            vec![(0, "root".into()), (12_345, "alice".into())],
            vec![(0, "root".into())],
        );
        PermissionsPageModel::from_snapshot(&snapshot, accounts, &CapabilityState::Supported)
    }

    #[cfg(unix)]
    #[test]
    fn acl_editing_offers_another_users_items_only_to_the_superuser() {
        use posix_acl::{ACL_READ, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_named_acl(&file, &[(Qualifier::User(12_345), ACL_READ)]);
        let owner = musheen_desktop::effective_user();

        // Another user sees the entries and changes none.
        let mut other = acl_page(&file, owner + 1);
        assert!(!other.acl_editable(AclList::Access));
        other.add_acl_entry(AclList::Access, AclName::User(0));
        other.toggle_acl_right(AclList::Access, AclName::User(12_345), AclRight::Write);
        other.remove_acl_entry(AclList::Access, AclName::User(12_345));
        assert!(!other.is_dirty());
        assert!(!other.change().is_dirty());

        // The superuser edits any item.
        let mut superuser = acl_page(&file, 0);
        assert!(superuser.acl_editable(AclList::Access));
        superuser.toggle_acl_right(AclList::Access, AclName::User(12_345), AclRight::Write);
        assert!(superuser.is_dirty());
        assert!(superuser.change().requires_permissions());
    }

    #[cfg(unix)]
    #[test]
    fn acl_editing_shows_the_mask_an_edit_leaves_in_the_group_row() {
        use posix_acl::{ACL_READ, Qualifier};

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("shared.txt");
        filesystem::write(&file, b"shared").unwrap();
        set_file_mode(&file, 0o640);
        set_named_acl(&file, &[(Qualifier::User(12_345), ACL_READ)]);
        let mut page = acl_page(&file, musheen_desktop::effective_user());
        assert!(
            !page
                .acl_choices(AclList::Access)
                .iter()
                .any(|(name, _)| *name == AclName::User(12_345)),
            "an account with an entry is not offered again"
        );
        page.add_acl_entry(AclList::Access, AclName::User(0));
        page.toggle_acl_right(AclList::Access, AclName::User(0), AclRight::Write);
        assert_eq!(
            page.access(AccessClass::Group),
            Some(Access::Modify),
            "the Group row shows the mask the new entry gives"
        );
        assert_eq!(page.bit(0o020), Tristate::On);

        // Removing the new entry gives back what the file has.
        page.remove_acl_entry(AclList::Access, AclName::User(0));
        assert!(!page.is_dirty());
        assert_eq!(page.access(AccessClass::Group), Some(Access::View));
    }

    /// Whether `path` is owned by the current user.
    fn owned_by_user(path: &std::path::Path) -> bool {
        use std::os::unix::fs::MetadataExt;

        filesystem::symlink_metadata(path).unwrap().uid() == musheen_desktop::effective_user()
    }

    fn is_dirty(properties: &Entity<PropertiesWindow>, cx: &mut TestAppContext) -> bool {
        cx.update(|cx| properties.read(cx).model.permissions().is_dirty())
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_later_changes_win_over_advanced_bits(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let notes = temporary.path().join("notes.txt");
        filesystem::write(&notes, b"notes").unwrap();
        set_file_mode(&notes, 0o600);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&notes), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-bit-others-read",
                "permissions-others-no-access",
                "permissions-group-can-view",
            ],
            cx,
        );
        let checked = notes.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o640).await;

        let tool = temporary.path().join("tool");
        filesystem::write(&tool, b"#!/bin/sh\n").unwrap();
        set_file_mode(&tool, 0o600);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&tool), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-bit-group-read",
                "permissions-bit-owner-execute",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window.find("permissions-executable-varies").visible(),
                "0o740 executes where the group may not"
            );
        })
        .unwrap();
        click_all(
            handle,
            &["permissions-executable", "permissions-executable"],
            cx,
        );
        let checked = tool.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o640).await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_bits_show_varies_and_clicks_return_to_unchanged(
        cx: &mut TestAppContext,
    ) {
        let temporary = tempfile::tempdir().unwrap();
        let first = temporary.path().join("first.txt");
        let second = temporary.path().join("second.txt");
        filesystem::write(&first, b"first").unwrap();
        filesystem::write(&second, b"second").unwrap();
        set_file_mode(&first, 0o640);
        set_file_mode(&second, 0o600);
        let (handle, properties) =
            open_permissions_page(&[first.clone(), second.clone()], None, cx);
        click_all(handle, &["permissions-advanced"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.find("permissions-bit-group-read-varies").visible());
            assert!(
                window
                    .try_find("permissions-bit-owner-read-varies")
                    .is_none()
            );
        })
        .unwrap();
        click_all(
            handle,
            &[
                "permissions-bit-group-read",
                "permissions-bit-group-read",
                "permissions-bit-group-read",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.find("permissions-bit-group-read-varies").visible());
            assert!(window.try_find("properties-apply").is_none());
        })
        .unwrap();
        assert!(
            !is_dirty(&properties, cx),
            "set, cleared, then Varies again"
        );
        click_all(
            handle,
            &["permissions-group-can-view", "permissions-group-varies"],
            cx,
        );
        assert!(!is_dirty(&properties, cx), "Varies drops the group choice");

        let (handle, properties) = open_permissions_page(std::slice::from_ref(&first), None, cx);
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-bit-others-write",
                "permissions-bit-others-write",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.try_find("properties-apply").is_none());
        })
        .unwrap();
        assert!(
            !is_dirty(&properties, cx),
            "a second click gives the bit back"
        );
        assert_eq!(file_mode(&first), 0o640);
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_locks_what_the_user_may_not_change(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        let link = temporary.path().join("link");
        filesystem::write(&file, b"notes").unwrap();
        std::os::unix::fs::symlink("notes.txt", &link).unwrap();
        set_file_mode(&file, 0o644);

        // A link has no mode of its own, but its owner may change its group.
        let (handle, properties) = open_permissions_page(std::slice::from_ref(&link), None, cx);
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(window.find("permissions-mode-lock").visible());
            window.click("permissions-others-no-access", cx);
            window.render_frame(cx);
            window.click("permissions-group-picker", cx);
            window.render_frame(cx);
            assert!(window.try_find("permissions-group-options").is_some());
        })
        .unwrap();
        assert!(!is_dirty(&properties, cx));

        // Items another user owns, judged with POSIX permissions supported,
        // as /etc and /dev may sit on filesystems the probe does not know:
        // /etc for the mode, and the link /dev/stdin for the group. The
        // superuser owns both.
        for other in [PathBuf::from("/etc"), PathBuf::from("/dev/stdin")] {
            if filesystem::symlink_metadata(&other).is_err() || owned_by_user(&other) {
                continue;
            }
            let snapshot = PropertySnapshot::load(&[file.clone(), other.clone()]).unwrap();
            let accounts = Accounts::load(&snapshot);
            let mut permissions = PermissionsPageModel::from_snapshot(
                &snapshot,
                accounts,
                &CapabilityState::Supported,
            );
            permissions.set_access(AccessClass::Others, Access::None);
            let expected = other.is_dir().then_some(crate::ModeLock::NotOwner);
            assert_eq!(permissions.mode_lock(), expected, "{other:?}");
            assert_eq!(
                permissions.is_dirty(),
                expected.is_none(),
                "a mode change reaches only the user's own items"
            );
            assert!(
                permissions.group_editable(),
                "{other:?}: another user's item takes a group as administrator"
            );
            permissions.set_group(0);
            assert!(
                permissions.is_dirty(),
                "{other:?}: the group changes as administrator"
            );
        }
    }

    #[test]
    fn permissions_page_superuser_sets_the_owner_without_the_broker() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let snapshot = PropertySnapshot::load(std::slice::from_ref(&file)).unwrap();
        let accounts = Accounts::fixed(
            0,
            vec![(0, "root".into())],
            vec![(0, "root".into()), (4242, "alice".into())],
            vec![(0, "root".into()), (4343, "staff".into())],
        );
        let mut permissions =
            PermissionsPageModel::from_snapshot(&snapshot, accounts, &CapabilityState::Supported)
                .with_admin_ownership(false);
        permissions.set_owner(4242);
        permissions.set_group(4343);
        assert!(
            !permissions.needs_administrator(),
            "the superuser needs none"
        );
        assert_eq!(permissions.ownership_edit(), None);
        let change = permissions.change();
        assert!(change.is_dirty() && change.requires_ownership());
    }

    #[test]
    fn permissions_page_offers_only_the_users_groups_where_the_broker_cannot_go() {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let snapshot = PropertySnapshot::load(std::slice::from_ref(&file)).unwrap();
        let user = musheen_desktop::effective_user();
        let own_group = snapshot.items()[0].permissions().group();
        let accounts = Accounts::fixed(
            user,
            vec![(own_group, "own".into()), (4344, "second".into())],
            vec![(user, "me".into()), (4242, "alice".into())],
            vec![
                (own_group, "own".into()),
                (4344, "second".into()),
                (4343, "staff".into()),
            ],
        );
        let mut permissions =
            PermissionsPageModel::from_snapshot(&snapshot, accounts, &CapabilityState::Supported)
                .with_admin_ownership(false);
        assert!(
            !permissions.owner_editable(),
            "no owner change on such a filesystem"
        );
        assert_eq!(
            permissions.group_choices().len(),
            2,
            "only the user's groups"
        );
        permissions.set_group(4343);
        assert!(
            !permissions.is_dirty(),
            "a group the user is not in is not offered"
        );
        permissions.set_group(4344);
        assert!(permissions.is_dirty());
        assert!(!permissions.needs_administrator());
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_apply_to_contents_changes_contents_first(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let folder = temporary.path().join("shared");
        let file = folder.join("notes.txt");
        let socket = folder.join("socket");
        filesystem::create_dir(&folder).unwrap();
        filesystem::write(&file, b"notes").unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        set_file_mode(&file, 0o644);
        set_file_mode(&folder, 0o755);
        let socket_mode = file_mode(&socket);
        let (handle, _) = open_permissions_page(std::slice::from_ref(&folder), None, cx);
        click_all(
            handle,
            &[
                "permissions-owner-no-access",
                "permissions-advanced",
                "permissions-bit-setgid",
                "permissions-scope-recursive",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.find("permissions-special-inside").visible());
        })
        .unwrap();
        click_all(handle, &["permissions-review-scope"], cx);
        let checked = folder.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o2055).await;
        // The folder no longer lets its owner in; give it back to read its
        // contents.
        set_file_mode(&folder, 0o755);
        assert_eq!(
            file_mode(&file),
            0o044,
            "the file changed before its folder, without the folder's setgid"
        );
        assert_eq!(
            file_mode(&socket),
            socket_mode,
            "the socket is left as it is"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_execute_follows_read_while_checked(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let program = temporary.path().join("program");
        let partial = temporary.path().join("partial");
        filesystem::write(&program, b"#!/bin/sh\n").unwrap();
        filesystem::write(&partial, b"#!/bin/sh\n").unwrap();
        set_file_mode(&program, 0o700);
        set_file_mode(&partial, 0o744);

        let (handle, _) = open_permissions_page(std::slice::from_ref(&partial), None, cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window.find("permissions-executable-varies").visible(),
                "group and others may read 0o744 but not execute it"
            );
        })
        .unwrap();

        let (handle, _) = open_permissions_page(std::slice::from_ref(&program), None, cx);
        click_all(handle, &["permissions-group-can-view"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(window.try_find("permissions-executable-varies").is_none());
        })
        .unwrap();
        let checked = program.clone();
        apply_until(handle, cx, move || file_mode(&checked) == 0o750).await;
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_changes_modes_before_the_group(cx: &mut TestAppContext) {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("tool");
        filesystem::write(&file, b"#!/bin/sh\n").unwrap();
        set_file_mode(&file, 0o755);
        let current = filesystem::metadata(&file).unwrap().gid();
        let groups = command_output("/usr/bin/id", &["-G"])
            .split_whitespace()
            .map(|gid| gid.parse::<u32>().unwrap())
            .collect::<Vec<_>>();
        let Some(other) = groups.iter().copied().find(|gid| *gid != current) else {
            eprintln!("skipped: the user belongs to one group");
            return;
        };
        let (handle, _) = open_permissions_page(std::slice::from_ref(&file), None, cx);
        let option = format!("permissions-group-option-{other}");
        click_all(
            handle,
            &[
                "permissions-advanced",
                "permissions-bit-setuid",
                "permissions-group-picker",
                option.as_str(),
            ],
            cx,
        );
        let checked = file.clone();
        apply_until(handle, cx, move || {
            filesystem::metadata(&checked).unwrap().gid() == other
        })
        .await;
        assert_eq!(
            file_mode(&file),
            0o755,
            "the mode changed first, so the group change cleared the setuid bit set with it"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_group_change_skips_members_and_shows_cleared_setuid(
        cx: &mut TestAppContext,
    ) {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir().unwrap();
        let moving = temporary.path().join("moving");
        let member = temporary.path().join("member");
        filesystem::write(&moving, b"#!/bin/sh\n").unwrap();
        filesystem::write(&member, b"#!/bin/sh\n").unwrap();
        let current = filesystem::metadata(&moving).unwrap().gid();
        let groups = command_output("/usr/bin/id", &["-G"])
            .split_whitespace()
            .map(|gid| gid.parse::<u32>().unwrap())
            .collect::<Vec<_>>();
        let Some(other) = groups.iter().copied().find(|gid| *gid != current) else {
            eprintln!("skipped: the user belongs to one group");
            return;
        };
        std::os::unix::fs::chown(&member, None, Some(other)).unwrap();
        set_file_mode(&moving, 0o4755);
        set_file_mode(&member, 0o4755);
        let (handle, _) = open_permissions_page(&[moving.clone(), member.clone()], None, cx);
        let option = format!("permissions-group-option-{other}");
        click_all(
            handle,
            &[
                "permissions-group-picker",
                option.as_str(),
                "permissions-advanced",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window.find("permissions-bit-setuid-varies").visible(),
                "the file that changes group loses setuid; the member keeps it"
            );
        })
        .unwrap();
        let (checked_moving, checked_member) = (moving.clone(), member.clone());
        apply_until(handle, cx, move || {
            filesystem::metadata(&checked_moving).unwrap().gid() == other
                && file_mode(&checked_moving) == 0o755
        })
        .await;
        assert_eq!(
            file_mode(&checked_member),
            0o4755,
            "no chown ran on the member"
        );
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_localizes_notes_reasons_and_errors(cx: &mut TestAppContext) {
        use posix_acl::{ACL_READ, PosixACL, Qualifier};

        let pseudo = Catalog::load(crate::Locale::EnXa).unwrap();
        for error in [
            MutationError::PermissionDenied,
            MutationError::Missing,
            MutationError::SourceChanged,
            MutationError::Unsupported,
            MutationError::Cancelled,
            MutationError::ScopeNotReviewed,
            MutationError::NoChanges,
            MutationError::InvalidMetadata,
            MutationError::InvalidScope,
        ] {
            assert!(
                pseudo
                    .localize_known_reason(&error.to_string())
                    .is_some_and(|message| message.starts_with('⟦')),
                "{error} is localized"
            );
        }
        for reason in [
            "properties-changed-while-editing",
            "permissions-apply-failed",
            "properties-refresh-failed",
            "the filesystem could not be probed",
            "the filesystem does not support POSIX ACLs",
            "the ACL could not be read",
        ] {
            assert!(
                pseudo
                    .localize_known_reason(reason)
                    .is_some_and(|message| message.starts_with('⟦')),
                "{reason} is localized"
            );
        }

        let temporary = tempfile::tempdir().unwrap();
        let shared = temporary.path().join("shared.txt");
        let private = temporary.path().join("private.txt");
        let link = temporary.path().join("link");
        let socket = temporary.path().join("socket");
        filesystem::write(&shared, b"shared").unwrap();
        filesystem::write(&private, b"private").unwrap();
        std::os::unix::fs::symlink("shared.txt", &link).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        set_file_mode(&shared, 0o640);
        set_file_mode(&private, 0o600);
        let mut acl = PosixACL::read_acl(&shared).expect("the temporary filesystem has ACLs");
        acl.set(Qualifier::User(musheen_desktop::effective_user()), ACL_READ);
        acl.fix_mask();
        acl.write_acl(&shared)
            .expect("the temporary filesystem takes a named ACL entry");
        let (handle, _) = open_permissions_page(
            &[
                shared.clone(),
                private.clone(),
                link.clone(),
                socket.clone(),
            ],
            Some(pseudo.clone()),
            cx,
        );
        click_all(handle, &["permissions-advanced"], cx);
        let symlink_reason = pseudo
            .localize_known_reason("POSIX ACLs are not read through symbolic links")
            .unwrap();
        cx.update_window(handle.into(), |_, window, _| {
            for id in [
                "permissions-group-varies",
                "permissions-special-items",
                "permissions-acl-mask-note",
                "permissions-bit-group-read-varies",
                "permissions-acl-entries",
            ] {
                assert!(
                    window
                        .find(id)
                        .label()
                        .is_some_and(|label| label.starts_with('⟦')),
                    "{id} is localized"
                );
            }
            assert!(
                window
                    .find("permissions-acl-item-2")
                    .label()
                    .is_some_and(|label| label.contains(&symlink_reason)),
                "the link's ACL reason is localized"
            );
        })
        .unwrap();

        let (handle, _) = open_permissions_page(std::slice::from_ref(&link), Some(pseudo), cx);
        cx.update_window(handle.into(), |_, window, _| {
            assert!(
                window
                    .find("permissions-mode-lock")
                    .label()
                    .is_some_and(|label| label.starts_with('⟦'))
            );
        })
        .unwrap();
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_is_read_only_without_posix_permissions(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_properties_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("on-vfat.txt");
        filesystem::write(&file, b"contents").unwrap();
        set_file_mode(&file, 0o644);
        let reason = CapabilityReason::new("the filesystem does not store POSIX permissions")
            .expect("the reason is not empty");
        let data = PropertiesWindowData::load(std::slice::from_ref(&file))
            .expect("the item loads")
            .with_permission_capability(CapabilityState::Unsupported(reason));
        let mut properties = None;
        let handle = cx.open_window(size(px(900.), px(900.)), |window, cx| {
            let view = cx.new(|cx| PropertiesWindow::new(data, window, cx));
            properties = Some(view.clone());
            Root::new(view, window, cx)
        });
        let properties = properties.expect("the Properties view is constructed");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("properties-page-permissions", cx);
            window.render_frame(cx);
            assert!(
                window
                    .find("permissions-read-only")
                    .label()
                    .is_some_and(|label| label.contains("does not store POSIX permissions")),
                "the page says why it is read-only"
            );
            window.click("permissions-others-no-access", cx);
            window.click("permissions-executable", cx);
            window.click("permissions-advanced", cx);
            window.render_frame(cx);
            window.click("permissions-bit-sticky", cx);
            window.render_frame(cx);
            assert!(window.try_find("properties-apply").is_none());
        })
        .expect("the Properties window is open");
        cx.update(|cx| {
            assert!(!properties.read(cx).model.permissions().is_dirty());
        });
        assert_eq!(file_mode(&file), 0o644);
    }

    #[cfg(unix)]
    #[gpui_kit::test]
    async fn permissions_page_every_string_is_localized(cx: &mut TestAppContext) {
        let temporary = tempfile::tempdir().unwrap();
        let file = temporary.path().join("notes.txt");
        filesystem::write(&file, b"notes").unwrap();
        let (handle, _) = open_permissions_page(
            std::slice::from_ref(&file),
            Some(Catalog::load(crate::Locale::EnXa).unwrap()),
            cx,
        );
        // Another owner makes the page say it needs administrator rights.
        click_all(
            handle,
            &[
                "permissions-owner-picker",
                "permissions-owner-option-0",
                "permissions-advanced",
            ],
            cx,
        );
        cx.update_window(handle.into(), |_, window, _| {
            for id in [
                "permissions-owner-no-access",
                "permissions-group-can-view",
                "permissions-others-can-modify",
                "permissions-executable",
                "permissions-needs-admin",
                "properties-apply",
                "permissions-group-picker",
                "permissions-advanced",
                "permissions-bit-owner-read",
                "permissions-bit-setuid",
            ] {
                assert!(
                    window
                        .find(id)
                        .label()
                        .is_some_and(|label| label.starts_with('⟦')),
                    "{id} is localized"
                );
            }
        })
        .unwrap();
        // The review's rows come from the catalog's template.
        click_all(handle, &["properties-apply"], cx);
        cx.update_window(handle.into(), |_, window, _| {
            let row = window
                .find("ownership-review-item-0")
                .label()
                .unwrap_or_default()
                .to_owned();
            assert!(row.starts_with('⟦') && !row.contains('{'), "{row}");
            for id in ["ownership-review-kernel", "ownership-review-cancel"] {
                assert!(
                    window
                        .find(id)
                        .label()
                        .is_some_and(|label| label.starts_with('⟦')),
                    "{id} is localized"
                );
            }
        })
        .unwrap();
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
            let permissions = properties.read(cx).model.permissions();
            *permissions.mode() == AggregateValue::Same(0o600)
                && permissions.access(AccessClass::Group) == Some(Access::None)
        })
        .await;
        assert!(cx.update(|cx| !properties.read(cx).model.permissions().is_dirty()));

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("permissions-group-can-view", cx);
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
                    .model
                    .permissions()
                    .access(AccessClass::Group),
                Some(Access::View),
                "the user's choice survives the external change"
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
