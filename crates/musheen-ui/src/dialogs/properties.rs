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
    CancellationToken, CapabilityKind, CapabilityState, ItemKind, Store, StorePath,
};
use musheen_desktop::{
    AclEntry, AclQualifier, AclState, AggregateValue, ChecksumAlgorithm, ChecksumResult,
    ChecksumService, PropertyError, PropertyRefresh, PropertySnapshot, PropertyTimestamp,
    RecursiveSize, XattrState,
};
use musheen_local::LocalStore;
use musheen_ops::{JobId, MetadataChange, MetadataScope};
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::PathBuf;
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

#[derive(Debug)]
pub struct PropertiesWindowData {
    snapshot: PropertySnapshot,
    filesystem_rows: Vec<(Box<str>, Box<str>)>,
    capability_rows: Vec<(Box<str>, Box<str>)>,
}

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
        })
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
    focus: FocusHandle,
    pending_focus: bool,
}

impl PropertiesFailureWindow {
    pub fn new(message: impl Into<Box<str>>, cx: &mut Context<Self>) -> Self {
        Self {
            message: message.into(),
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
            .aria_label("Properties could not be loaded")
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
                    .label("Close")
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
        let mut this = Self {
            model: PropertiesDialogModel::new(data.snapshot),
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
        };
        this.subscribe_permission_inputs(window, cx);
        this.sync_rows(window, cx);
        this.start_live_refresh(cx);
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
            |state: &mut Self, id, succeeded, error, cx| {
                let succeeded = succeeded && error.is_none();
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
            ("Items".into(), snapshot.items().len().to_string().into()),
            ("Type".into(), aggregate_kind(aggregate.kind()).into()),
            (
                "MIME type".into(),
                aggregate_text(aggregate.mime_type()).into(),
            ),
            ("Location".into(), aggregate_location(snapshot).into()),
            ("Identity".into(), aggregate_identity(snapshot).into()),
            (
                "Logical size".into(),
                format_size(aggregate.logical_size()).into(),
            ),
            (
                "Allocated size".into(),
                format_size(aggregate.allocated_size()).into(),
            ),
            (
                "Modified".into(),
                aggregate_time(aggregate.modified()).into(),
            ),
            (
                "Accessed".into(),
                aggregate_time(aggregate.accessed()).into(),
            ),
            (
                "Metadata changed".into(),
                aggregate_time(aggregate.changed()).into(),
            ),
        ];
        if self.single_directory_path().is_some() {
            rows.push((
                "Contained items".into(),
                recursive_size_label(&self.recursive_size).into(),
            ));
        }
        rows.extend(self.filesystem_rows.iter().cloned());
        rows.extend(self.capability_rows.iter().cloned());
        rows
    }

    fn permission_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        let permissions = self.model.permissions();
        let mut rows = vec![
            (
                "Owner (UID)".into(),
                aggregate_u32(permissions.owner()).into(),
            ),
            (
                "Group (GID)".into(),
                aggregate_u32(permissions.group()).into(),
            ),
            ("Mode".into(), aggregate_mode(permissions.mode()).into()),
            (
                "Editing".into(),
                permissions
                    .edit_disabled_reason()
                    .unwrap_or("Ready to edit")
                    .to_owned()
                    .into(),
            ),
        ];
        for (index, item) in self.model.snapshot().items().iter().enumerate() {
            rows.push((
                indexed_label("Access ACL", index, self.model.snapshot().items().len()).into(),
                acl_state_label(item.permissions().acl()).into(),
            ));
            if let Some(default_acl) = item.permissions().default_acl() {
                rows.push((
                    indexed_label("Default ACL", index, self.model.snapshot().items().len()).into(),
                    acl_state_label(default_acl).into(),
                ));
            }
        }
        rows
    }

    fn open_with_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        vec![
            (
                "MIME type".into(),
                aggregate_text(self.model.snapshot().aggregate().mime_type()).into(),
            ),
            (
                "Association".into(),
                "Choose an application for one launch or set the desktop default".into(),
            ),
        ]
    }

    fn tag_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        let mut rows = Vec::new();
        for (item_index, item) in self.model.snapshot().items().iter().enumerate() {
            match item.xattrs() {
                XattrState::Available(entries) if entries.is_empty() => rows.push((
                    indexed_label(
                        "Extended attributes",
                        item_index,
                        self.model.snapshot().items().len(),
                    )
                    .into(),
                    "None".into(),
                )),
                XattrState::Available(entries) => {
                    for entry in entries {
                        rows.push((
                            format!(
                                "{}: {}",
                                indexed_label(
                                    "Attribute",
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
                        "Extended attributes",
                        item_index,
                        self.model.snapshot().items().len(),
                    )
                    .into(),
                    format!("Unavailable: {reason}").into(),
                )),
            }
        }
        rows
    }

    fn checksum_rows(&self) -> Vec<(Box<str>, Box<str>)> {
        match &self.checksum {
            ChecksumState::Idle => vec![("Checksum".into(), "Not calculated".into())],
            ChecksumState::Running(algorithm) => vec![(
                "Checksum".into(),
                format!("Calculating {}…", algorithm.label()).into(),
            )],
            ChecksumState::Failed(error) => {
                vec![("Checksum".into(), format!("Failed: {error}").into())]
            }
            ChecksumState::Ready(result) => {
                let fingerprint = result.fingerprint();
                vec![
                    ("Algorithm".into(), result.algorithm().label().into()),
                    ("Digest".into(), result.hex_digest().into()),
                    (
                        "Stable identity".into(),
                        format!("{}:{}", fingerprint.device(), fingerprint.inode()).into(),
                    ),
                    (
                        "Size at read".into(),
                        format_size(fingerprint.size()).into(),
                    ),
                    (
                        "Modified at read".into(),
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
                .label(page_label(page))
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
            .aria_label("Properties pages")
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
            .aria_label(format!("{} properties", page_label(self.model.page())))
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
        let field = |label: &'static str, id: &'static str, input: &Entity<InputState>| {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_1()
                .child(div().text_xs().child(label))
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
            .aria_label("Permission and ownership editor")
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(field(
                "Owner (UID)",
                "permissions-owner",
                &self.permission_inputs.owner,
            ))
            .child(field(
                "Group (GID)",
                "permissions-group",
                &self.permission_inputs.group,
            ))
            .child(field(
                "File mode (octal)",
                "permissions-file-mode",
                &self.permission_inputs.file_mode,
            ))
            .child(field(
                "Directory mode (octal)",
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
                            .label("Selected items only")
                            .selected(!recursive)
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.permissions_mut().set_single();
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("permissions-scope-recursive")
                            .label("Include descendants")
                            .selected(recursive && !scope.includes_nested_mounts())
                            .disabled(disabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.permissions_mut().set_recursive(false);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("permissions-scope-mounts")
                            .label("Include descendants and nested mounts")
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
                            "Review: this will include nested filesystems."
                        } else {
                            "Review: this will include descendants but stop at nested filesystems."
                        }))
                        .child(
                            Button::new("permissions-review-scope")
                                .label("I reviewed this scope")
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

    fn render_page_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut actions = div().flex().items_center().gap_2();
        if self.model.page() == PropertiesPage::Permissions
            && self.model.apply_visible()
            && !self.permission_batch.is_active()
        {
            actions = actions.child(
                Button::new("properties-apply")
                    .label("Apply")
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| this.apply_permissions(cx))),
            );
        }
        if self.model.page() == PropertiesPage::General && self.single_directory_path().is_some() {
            actions = actions.child(
                Button::new("properties-calculate-size")
                    .label(
                        if matches!(self.recursive_size, RecursiveSizeState::Running) {
                            "Calculating…"
                        } else {
                            "Calculate contained size"
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
                        .label("Calculate BLAKE3")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.start_checksum(ChecksumAlgorithm::Blake3, cx);
                        })),
                )
                .child(
                    Button::new("properties-checksum-sha256")
                        .label("Calculate SHA-256")
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
            PropertiesState::Replaced => self.refresh_error.as_deref().or(Some(
                "The selected item was replaced. Close and reopen Properties.",
            )),
            PropertiesState::Missing => Some("The selected item no longer exists."),
        };
        let colors = cx.theme().colors;
        div()
            .id("properties-dialog")
            .test_support()
            .key_context("PropertiesWindow")
            .role(Role::Dialog)
            .aria_label(format!("Properties for {title}"))
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
                    .aria_label(format!("{title}, {location}"))
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
                    .aria_label("Properties actions")
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
                            .label("Close")
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
                Some("Permissions properties")
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
