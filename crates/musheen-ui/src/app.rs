mod custom_actions;

use crate::dialogs::{
    ConflictDialog, ConflictDialogEvent, ConflictDialogModel, PropertiesFailureWindow,
    PropertiesPage, PropertiesWindow, PropertiesWindowData, conflict_window_options,
    install_properties_key_bindings, properties_window_options,
};
use crate::directory::{DirectoryLoad, DirectoryModel, DirectoryState, enumerate_directory};
use crate::i18n::Catalog;
use crate::icons::{ApplicationIdentity, ContentIdentity, freedesktop_icon_name};
use crate::info_pane::{
    InfoPaneDetails, InfoPaneModel, InfoPaneResult, InfoPaneState, InfoPaneWork,
    PreviewPresentation,
};
use crate::navigation::{
    ApplicationSession, BreadcrumbTrail, MAX_WINDOWS, NavigationError, OmnibarMode, OmnibarState,
    OmnibarSubmission, PaneId, TabId, WindowSession, resolve_path_input,
};
use crate::operations::{DropAction, FileDragPayload, OperationHub, spawn_ready_hub_operations};
use crate::search::{DirectoryFilter, SearchGeneration, SearchResultModel, SearchState};
use crate::sidebar::{PinStore, SidebarEntry, SidebarModel, SidebarSectionKind};
use crate::status_bar::status_text_with_size;
use crate::status_center::{OperationStatus, OperationStatusEntry, TrashItem, TrashSurfaceModel};
use crate::toolbar::COMMAND_IDS;
use crate::views::{
    AdaptiveLayout, ColumnKey, GroupKey, Layout, SelectionMode, SortDirection, SortKey, SortSpec,
};
use crate::{
    ContextMenu, ContextMenuDestinationResolver, MenuEntry, MenuInvocation, MenuTarget,
    PendingInvocation,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Root, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, DismissEvent, Entity, EventEmitter, FocusHandle,
    Focusable, ImageSource, IntoElement, KeyBinding, MouseButton, Pixels, Point, Render, Role,
    SharedString, Subscription, TestSupportExt, TitlebarOptions, Window, WindowBounds, WindowId,
    WindowOptions, div, img, px, size, uniform_list,
};
use musheen_core::{
    ActiveLayout, CancellationToken, CapabilityReason, CapabilityState, CommandAction,
    CommandContext, CommandDispatchError, CommandDispatcher, CommandParameters, CommandTarget,
    CommandTargetRef, DirectoryWatch, DisplayPath, ItemId, ItemKind, Page, ProviderActionMatrix,
    ProviderId, ResourceLimits, SEARCH_RESULT_LIMIT, SEARCH_RETAINED_RESULTS, SearchBatch,
    SearchCompletion, SearchQuery, SearchScopeError, SearchStream, Store, StoreError, StoreItem,
    StorePath, WatchEvent,
};
use musheen_desktop::{
    ConflictDecisionStore, MimeDetector, PreviewDocument, SessionStore, ThumbnailCache,
    ThumbnailLimits, ThumbnailLookup, ThumbnailMode, ThumbnailRequest, ThumbnailService,
    ThumbnailSize,
};
use musheen_local::LocalStore;
use musheen_ops::{
    ApplyScope, ConflictChoice, ConflictDecision, ConflictItemKind, ConflictPolicies,
    ConflictRecord, MutationError, MutationProvider, OperationKind,
};
use native_theme::SystemTheme;
use native_theme::icons::FreedesktopLoader;
use native_theme_gpui::NativeTheme;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

gpui_kit::assets::icon_assets!(
    pub MusheenAssets,
    [
        ArrowLeft,
        ArrowRight,
        ArrowUp,
        Copy,
        Columns2,
        Download,
        Eye,
        EyeOff,
        File,
        FileSymlink,
        Folder,
        Grid2x2,
        HardDrive,
        House,
        Info,
        List,
        ListChecks,
        Link2,
        MapPin,
        Network,
        PanelRight,
        Pin,
        PinOff,
        Plus,
        Puzzle,
        RefreshCw,
        RotateCcw,
        Scissors,
        Search,
        Settings,
        Shield,
        ShieldCheck,
        Star,
        Tag,
        Terminal,
        TextCursorInput,
        Trash,
        X,
    ]
);

const SIDEBAR_WIDTH: f32 = 220.0;
const CONTENT_PADDING: f32 = 32.0;
const GRID_ITEM_WIDTH: f32 = 128.0;
const GRID_GAP: f32 = 8.0;
const SESSION_SAVE_DELAY: Duration = Duration::from_millis(250);
const OPERATION_STATUS_REFRESH_INTERVAL: Duration = Duration::from_millis(125);

gpui_kit::actions!(
    musheen,
    [
        GoBack,
        GoForward,
        GoParent,
        Reload,
        EditLocation,
        SearchLocation,
        FilterLocation,
        OpenCommandMode,
        NewTabShortcut,
        CloseTabShortcut,
        ReopenClosedTabShortcut,
        SplitPaneShortcut,
        FocusNextPaneShortcut,
        SelectAllShortcut,
        ToggleHiddenShortcut,
        ViewDetailsShortcut,
        ViewListShortcut,
        ViewCardsShortcut,
        ViewGridShortcut,
        ViewColumnsShortcut,
        ViewAdaptiveShortcut,
        ToggleSidebarShortcut,
        OpenPropertiesShortcut,
        OpenContextMenuShortcut,
        FocusNextDirectoryItem,
        FocusPreviousDirectoryItem,
    ]
);

fn install_navigation_key_bindings(cx: &mut App) {
    install_properties_key_bindings(cx);
    cx.bind_keys([
        KeyBinding::new("alt-left", GoBack, None),
        KeyBinding::new("alt-right", GoForward, None),
        KeyBinding::new("alt-up", GoParent, None),
        KeyBinding::new("f5", Reload, None),
        KeyBinding::new("ctrl-l", EditLocation, None),
        KeyBinding::new("ctrl-f", SearchLocation, Some("!Input")),
        KeyBinding::new("ctrl-shift-f", FilterLocation, None),
        KeyBinding::new("ctrl-shift-p", OpenCommandMode, None),
        KeyBinding::new("ctrl-t", NewTabShortcut, None),
        KeyBinding::new("ctrl-w", CloseTabShortcut, None),
        KeyBinding::new("ctrl-shift-t", ReopenClosedTabShortcut, None),
        KeyBinding::new("f3", SplitPaneShortcut, None),
        KeyBinding::new("f6", FocusNextPaneShortcut, None),
        KeyBinding::new("ctrl-a", SelectAllShortcut, Some("!Input")),
        KeyBinding::new("escape", Escape, None),
        KeyBinding::new("ctrl-h", ToggleHiddenShortcut, Some("!Input")),
        KeyBinding::new("ctrl-1", ViewDetailsShortcut, None),
        KeyBinding::new("ctrl-2", ViewListShortcut, None),
        KeyBinding::new("ctrl-3", ViewCardsShortcut, None),
        KeyBinding::new("ctrl-4", ViewGridShortcut, None),
        KeyBinding::new("ctrl-5", ViewColumnsShortcut, None),
        KeyBinding::new("ctrl-6", ViewAdaptiveShortcut, None),
        KeyBinding::new("ctrl-b", ToggleSidebarShortcut, None),
        KeyBinding::new("alt-enter", OpenPropertiesShortcut, None),
        KeyBinding::new("shift-f10", OpenContextMenuShortcut, None),
        KeyBinding::new("menu", OpenContextMenuShortcut, None),
        KeyBinding::new("down", FocusNextDirectoryItem, Some("DirectoryContent")),
        KeyBinding::new("up", FocusPreviousDirectoryItem, Some("DirectoryContent")),
    ]);
}

#[derive(Clone, Copy)]
struct PaneRenderSpec {
    tab_id: TabId,
    pane_index: usize,
    focused: bool,
}

struct ItemRenderSpec {
    tab_id: TabId,
    pane_index: usize,
    index: usize,
    id: ItemId,
    path: StorePath,
    name: String,
    kind: ItemKind,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
    columns: Vec<(ColumnKey, u16)>,
    layout: Layout,
    selected: bool,
    focused: bool,
}

struct FileDragPreview {
    label: String,
    position: Point<Pixels>,
}

#[derive(Default)]
struct AppMenuDispatcher {
    dispatched: Option<(CommandAction, CommandParameters)>,
}

impl CommandDispatcher for AppMenuDispatcher {
    fn dispatch(
        &mut self,
        action: CommandAction,
        parameters: CommandParameters,
    ) -> Result<(), CommandDispatchError> {
        self.dispatched = Some((action, parameters));
        Ok(())
    }
}

impl Render for FileDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .ml(self.position.x)
            .mt(self.position.y)
            .max_w(px(280.))
            .px_3()
            .py_2()
            .rounded_md()
            .bg(cx.theme().colors.popover)
            .text_color(cx.theme().colors.popover_foreground)
            .shadow_md()
            .child(self.label.clone())
    }
}

#[derive(Clone, Copy)]
enum ColumnAction {
    Toggle(ColumnKey),
    MoveLeft(ColumnKey),
    MoveRight(ColumnKey),
    Narrower(ColumnKey),
    Wider(ColumnKey),
}

struct ActiveSearch {
    model: SearchResultModel,
    cancellation: CancellationToken,
    expression: String,
    error: Option<Box<str>>,
    retryable: bool,
}

impl ActiveSearch {
    fn status_text(&self) -> String {
        let total = self.model.total_results();
        let results = if total == 1 {
            "1 result".to_owned()
        } else {
            format!("{total} results")
        };
        let scope_errors = self.model.errors().len() + self.model.dropped_error_count();
        match self.model.state() {
            SearchState::Idle => "Idle".to_owned(),
            SearchState::Running => format!("Searching — {total} found"),
            SearchState::Complete => results,
            SearchState::Partial => format!("{results} — {scope_errors} scope errors"),
            SearchState::RefineRequired => format!("{results} — refine the search to continue"),
            SearchState::Cancelled => "Search cancelled".to_owned(),
            SearchState::Error => self.error.as_deref().unwrap_or("Search failed").to_owned(),
        }
    }
}

struct ActiveFilter {
    filter: Option<DirectoryFilter>,
    expression: String,
    error: Option<Box<str>>,
}

#[derive(Clone)]
struct SearchItemRenderSpec {
    index: usize,
    name: String,
    path: String,
    kind: ItemKind,
    mime: Option<Box<str>>,
}

#[derive(Clone, Debug)]
enum TrashState {
    Loading,
    Ready(TrashSurfaceModel),
    Error(Box<str>),
}

enum TrashRestoreResult {
    Restored,
    Conflict {
        receipt: musheen_ops::TrashReceipt,
        conflict: ConflictRecord,
    },
    Failed(MutationError),
}

struct PendingDrop {
    conflict_window: Option<WindowId>,
    // Keep the event source alive after its window closes until the queued
    // decision is delivered or the close observer cancels the workflow.
    conflict_dialog: Option<Entity<ConflictDialog>>,
    payload: FileDragPayload,
    target: StorePath,
    conflicts: Vec<ConflictRecord>,
    next_conflict: usize,
    decisions: Vec<ConflictDecision>,
    policies: ConflictPolicies,
    automatic_scope: bool,
}

struct PendingRestore {
    tab_id: TabId,
    receipt: musheen_ops::TrashReceipt,
    conflict: ConflictRecord,
    _dialog: Entity<ConflictDialog>,
}

#[derive(Clone)]
struct ContextDestinationChoice {
    label: String,
    location: StorePath,
}

#[derive(Clone)]
struct ContextDialogWindow {
    id: WindowId,
    origin_tab: TabId,
    focused_item: Option<ItemId>,
    restore_focus: FocusHandle,
}

#[derive(Clone)]
struct ContextDialogStrings {
    choose_destination: String,
    destination_explanation: String,
    destination_path: String,
    destination_path_invalid: String,
    cancel: String,
    review_operation: String,
    continue_action: String,
    authorization_unavailable: String,
    command: String,
    targets: String,
    move_review: String,
    operation_review: String,
}

impl ContextDialogStrings {
    fn from_catalog(catalog: &Catalog) -> Self {
        let message = |id| {
            catalog
                .message(id)
                .expect("context-dialog locale keys are present")
                .to_owned()
        };
        Self {
            choose_destination: message("dialog.choose-destination"),
            destination_explanation: message("dialog.destination-explanation"),
            destination_path: message("dialog.destination-path"),
            destination_path_invalid: message("dialog.destination-path-invalid"),
            cancel: message("dialog.cancel"),
            review_operation: message("dialog.review-operation"),
            continue_action: message("dialog.continue"),
            authorization_unavailable: message("dialog.authorization-unavailable"),
            command: message("dialog.command"),
            targets: message("dialog.targets"),
            move_review: message("dialog.move-review"),
            operation_review: message("dialog.operation-review"),
        }
    }
}

#[derive(Clone)]
enum ContextDestinationEvent {
    Chosen(StorePath),
    Cancelled,
}

struct ContextDestinationDialog {
    choices: Vec<ContextDestinationChoice>,
    strings: ContextDialogStrings,
    focus: FocusHandle,
    pending_focus: bool,
    location_input: Entity<InputState>,
    location_error: bool,
}

impl EventEmitter<ContextDestinationEvent> for ContextDestinationDialog {}

impl ContextDestinationDialog {
    fn new(
        choices: Vec<ContextDestinationChoice>,
        strings: ContextDialogStrings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = strings.destination_path.clone();
        Self {
            choices,
            strings,
            focus: cx.focus_handle(),
            pending_focus: true,
            location_input: cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)),
            location_error: false,
        }
    }
}

impl Render for ContextDestinationDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.focus.focus(window, cx);
            self.pending_focus = false;
        }
        let choices = self
            .choices
            .clone()
            .into_iter()
            .enumerate()
            .map(|(index, choice)| {
                let location = choice.location.clone();
                Button::new(SharedString::from(format!("context-destination-{index}")))
                    .label(choice.label)
                    .w_full()
                    .on_click(cx.listener(move |_, _, window, cx| {
                        cx.emit(ContextDestinationEvent::Chosen(location.clone()));
                        window.defer(cx, |window, _| window.remove_window());
                    }))
            });
        div()
            .id("context-destination-dialog")
            .test_support()
            .key_context("ContextDestinationDialog")
            .role(Role::Dialog)
            .aria_label(self.strings.choose_destination.clone())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .on_action(cx.listener(|_, _: &Escape, window, cx| {
                cx.emit(ContextDestinationEvent::Cancelled);
                window.defer(cx, |window, _| window.remove_window());
            }))
            .child(
                div()
                    .text_lg()
                    .child(self.strings.choose_destination.clone()),
            )
            .child(self.strings.destination_explanation.clone())
            .child(Input::new(&self.location_input).id("context-destination-path"))
            .child(
                Button::new("context-destination-use-path")
                    .label(self.strings.continue_action.clone())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let path = this.location_input.read(cx).value();
                        let path = Path::new(path.as_str());
                        if path.is_absolute() && !path.as_os_str().as_encoded_bytes().contains(&0) {
                            cx.emit(ContextDestinationEvent::Chosen(StorePath::from_unix_path(
                                path,
                            )));
                            window.defer(cx, |window, _| window.remove_window());
                        } else {
                            this.location_error = true;
                            cx.notify();
                        }
                    })),
            )
            .when(self.location_error, |this| {
                this.child(
                    div()
                        .id("context-destination-error")
                        .test_support()
                        .role(Role::Alert)
                        .child(self.strings.destination_path_invalid.clone()),
                )
            })
            .children(choices)
            .child(
                Button::new("context-destination-cancel")
                    .label(self.strings.cancel.clone())
                    .on_click(cx.listener(|_, _, window, cx| {
                        cx.emit(ContextDestinationEvent::Cancelled);
                        window.defer(cx, |window, _| window.remove_window());
                    })),
            )
    }
}

#[derive(Clone, Copy)]
enum ContextReviewEvent {
    Confirmed,
    Cancelled,
}

struct ContextReviewDialog {
    command: String,
    move_operation: bool,
    targets: Vec<String>,
    strings: ContextDialogStrings,
    focus: FocusHandle,
    pending_focus: bool,
}

impl EventEmitter<ContextReviewEvent> for ContextReviewDialog {}

impl ContextReviewDialog {
    fn new(
        command: String,
        move_operation: bool,
        targets: Vec<String>,
        strings: ContextDialogStrings,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            command,
            move_operation,
            targets,
            strings,
            focus: cx.focus_handle(),
            pending_focus: true,
        }
    }
}

impl Render for ContextReviewDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.focus.focus(window, cx);
            self.pending_focus = false;
        }
        let command = self.command.clone();
        let reversible = if self.move_operation {
            self.strings.move_review.clone()
        } else {
            self.strings.operation_review.clone()
        };
        div()
            .id("context-command-review")
            .test_support()
            .key_context("ContextReviewDialog")
            .role(Role::Dialog)
            .aria_label(self.strings.review_operation.clone())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .on_action(cx.listener(|_, _: &Escape, window, cx| {
                cx.emit(ContextReviewEvent::Cancelled);
                window.defer(cx, |window, _| window.remove_window());
            }))
            .child(div().text_lg().child(self.strings.review_operation.clone()))
            .child(
                div()
                    .id("context-review-command")
                    .test_support()
                    .role(Role::Label)
                    .aria_label(format!("{}: {command}", self.strings.command))
                    .child(format!("{}: {command}", self.strings.command)),
            )
            .child(
                div()
                    .id("context-review-targets")
                    .test_support()
                    .role(Role::Label)
                    .aria_label(format!(
                        "{}: {}",
                        self.strings.targets,
                        self.targets.join(", ")
                    ))
                    .child(format!(
                        "{}: {}",
                        self.strings.targets,
                        self.targets.join(", ")
                    )),
            )
            .child(
                div()
                    .id("context-review-explanation")
                    .test_support()
                    .role(Role::Label)
                    .aria_label(reversible.clone())
                    .child(reversible),
            )
            .child(self.strings.authorization_unavailable.clone())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("context-review-cancel")
                            .label(self.strings.cancel.clone())
                            .on_click(cx.listener(|_, _, window, cx| {
                                cx.emit(ContextReviewEvent::Cancelled);
                                window.defer(cx, |window, _| window.remove_window());
                            })),
                    )
                    .child(
                        Button::new("context-review-confirm")
                            .label(self.strings.continue_action.clone())
                            .primary()
                            .on_click(cx.listener(|_, _, window, cx| {
                                cx.emit(ContextReviewEvent::Confirmed);
                                window.defer(cx, |window, _| window.remove_window());
                            })),
                    ),
            )
    }
}

/// The menu layer asks this adapter for a provider-backed destination result;
/// it deliberately delegates the decision to the operation queue instead of
/// accepting a UI-provided writable flag.
struct ContextTransferDestinationResolver<'a> {
    operation_hub: &'a OperationHub,
    payload: &'a FileDragPayload,
    catalog: &'a Catalog,
}

impl ContextMenuDestinationResolver for ContextTransferDestinationResolver<'_> {
    fn resolve_context_menu_destination(
        &self,
        destination: &StorePath,
    ) -> musheen_core::ResolvedDestination {
        if self
            .operation_hub
            .can_accept_drop(self.payload, destination)
        {
            musheen_core::ResolvedDestination::writable(destination.clone())
        } else {
            musheen_core::ResolvedDestination::read_only(
                destination.clone(),
                self.catalog
                    .message("context.destination-refused")
                    .expect("destination refusal is localized"),
            )
        }
    }
}

pub fn run(initial_path: PathBuf) {
    gpui_kit::application()
        .with_assets(MusheenAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
            install_native_theme(cx);
            let settings = musheen_desktop::SettingsStore::for_current_user()
                .load()
                .unwrap_or_else(|error| {
                    eprintln!("Musheen could not load settings: {error}");
                    musheen_desktop::SettingsDocument::default()
                });
            if std::env::var_os("MUSHEEN_THEME_PREVIEW").is_none() {
                crate::settings::apply_appearance(&settings, cx);
            }
            let limits = settings
                .resource_limits_snapshot()
                .expect("loaded settings limits are validated");
            cx.set_global(crate::settings::RuntimeSettings(settings.clone()));
            let width = preview_window_width(
                std::env::var("MUSHEEN_PREVIEW_WINDOW_WIDTH")
                    .ok()
                    .as_deref(),
            );
            let height = preview_window_height(
                std::env::var("MUSHEEN_PREVIEW_WINDOW_HEIGHT")
                    .ok()
                    .as_deref(),
            );
            let fallback = StorePath::from_unix_path(initial_path.into_os_string());
            let store = SessionStore::for_current_user();
            let application = (settings.value("general.restore_session").as_deref()
                != Some("false")
                && settings.value("general.startup").as_deref() == Some("last-session"))
            .then(|| {
                restore_application_session(
                    &store,
                    fallback.clone(),
                    LocalStore::session_location_exists,
                )
            })
            .flatten()
            .unwrap_or_else(|| {
                ApplicationSession::new(vec![WindowSession::new(fallback)])
                    .expect("a single fallback window is a valid application session")
            });
            let coordinator = Arc::new(Mutex::new(SessionCoordinator::new(
                store,
                application.windows().to_vec(),
            )));
            let operation_hub = OperationHub::for_current_user(&limits);
            let windows = coordinator
                .lock()
                .expect("session coordinator lock is not poisoned")
                .entries()
                .into_iter()
                .map(|(window_id, navigation)| {
                    (
                        application_window_options(width, height, cx),
                        navigation,
                        SessionBinding {
                            coordinator: Arc::clone(&coordinator),
                            window_id,
                            operation_hub: operation_hub.clone(),
                        },
                    )
                })
                .collect::<Vec<_>>();
            cx.spawn(async move |cx| {
                for (window_options, navigation, binding) in windows {
                    let limits = limits.clone();
                    cx.open_window(window_options, move |window, cx| {
                        let view = cx.new(|cx| {
                            MusheenApp::new_with_navigation(
                                navigation,
                                Some(binding),
                                limits.clone(),
                                true,
                                cx,
                            )
                        });
                        cx.new(|cx| Root::new(view, window, cx))
                    })
                    .expect("Musheen could not restore a browsing window");
                }
            })
            .detach();
        });
}

fn application_window_options(width: f32, height: f32, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(width), px(height)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some("Musheen".into()),
            ..TitlebarOptions::default()
        }),
        app_id: Some(ApplicationIdentity::ID.into()),
        window_min_size: Some(size(px(720.), px(480.))),
        ..WindowOptions::default()
    }
}

fn detached_window_options() -> WindowOptions {
    WindowOptions {
        titlebar: Some(TitlebarOptions {
            title: Some("Musheen".into()),
            ..TitlebarOptions::default()
        }),
        app_id: Some(ApplicationIdentity::ID.into()),
        window_min_size: Some(size(px(720.), px(480.))),
        ..WindowOptions::default()
    }
}

fn install_native_theme(cx: &mut App) {
    if let Ok(requested) = std::env::var("MUSHEEN_THEME_PREVIEW")
        && let Some((is_dark, high_contrast)) = preview_theme(&requested)
    {
        let mut preferences = native_theme::AccessibilityPreferences::from_system();
        preferences.high_contrast = high_contrast;
        preferences.reduce_transparency |= high_contrast;
        if let Ok((theme, resolved)) =
            native_theme_gpui::from_preset("adwaita", is_dark, &preferences)
        {
            native_theme_gpui::apply(theme, &resolved, &preferences, cx);
            return;
        }
    }

    match SystemTheme::from_system() {
        Ok(system_theme) => native_theme_gpui::apply_system_theme(&system_theme, cx),
        Err(error) => {
            eprintln!("Musheen could not read the system theme: {error}. Using Adwaita.");
            let preferences = native_theme::AccessibilityPreferences::from_system();
            if let Ok((theme, resolved)) =
                native_theme_gpui::from_preset("adwaita", true, &preferences)
            {
                native_theme_gpui::apply(theme, &resolved, &preferences, cx);
            }
            if let Ok((theme, resolved)) =
                native_theme_gpui::from_preset("adwaita", false, &preferences)
            {
                native_theme_gpui::apply(theme, &resolved, &preferences, cx);
            }
        }
    }
    crate::theme::runtime::install(cx);
}

fn preview_theme(value: &str) -> Option<(bool, bool)> {
    match value {
        "light" => Some((false, false)),
        "dark" => Some((true, false)),
        "high-contrast" => Some((false, true)),
        _ => None,
    }
}

fn preview_window_width(value: Option<&str>) -> f32 {
    bounded_dimension(value, 1_180.0, 720.0, 1_920.0)
}

fn preview_window_height(value: Option<&str>) -> f32 {
    bounded_dimension(value, 760.0, 480.0, 1_200.0)
}

fn bounded_dimension(value: Option<&str>, fallback: f32, minimum: f32, maximum: f32) -> f32 {
    value
        .and_then(|value| value.parse::<f32>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(fallback)
        .clamp(minimum, maximum)
}

fn grid_column_count(viewport_width: f32) -> usize {
    let available = (viewport_width - SIDEBAR_WIDTH - CONTENT_PADDING).max(GRID_ITEM_WIDTH);
    (((available + GRID_GAP) / (GRID_ITEM_WIDTH + GRID_GAP)).floor() as usize).max(1)
}

fn grid_row_count(item_count: usize, columns: usize) -> usize {
    item_count.div_ceil(columns.max(1))
}

fn grid_item_range(row: usize, item_count: usize, columns: usize) -> std::ops::Range<usize> {
    let columns = columns.max(1);
    let start = row.saturating_mul(columns).min(item_count);
    start..start.saturating_add(columns).min(item_count)
}

async fn next_watch_event(
    mut watcher: Box<dyn DirectoryWatch>,
    cancellation: CancellationToken,
) -> Result<(Box<dyn DirectoryWatch>, WatchEvent), StoreError> {
    let event = watcher.next_event(cancellation).await?;
    Ok((watcher, event))
}

fn restore_application_session(
    store: &SessionStore,
    fallback: StorePath,
    exists: impl Fn(&StorePath) -> bool + Copy,
) -> Option<ApplicationSession> {
    for (label, document) in [("primary", store.load()), ("backup", store.load_backup())] {
        match document {
            Ok(Some(document)) => {
                match ApplicationSession::restore_compatible_json(
                    &document,
                    exists,
                    fallback.clone(),
                ) {
                    Ok(session) => return Some(session),
                    Err(error) => {
                        eprintln!(
                            "Musheen could not restore its {label} browsing session: {error}"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("Musheen could not read its {label} browsing session: {error}");
            }
        }
    }
    None
}

#[derive(Debug)]
struct SessionCoordinator {
    store: SessionStore,
    windows: Vec<(u64, WindowSession)>,
    next_window_id: u64,
    revision: u64,
}

impl SessionCoordinator {
    fn new(store: SessionStore, windows: Vec<WindowSession>) -> Self {
        let windows = windows
            .into_iter()
            .enumerate()
            .map(|(index, window)| (index as u64 + 1, window))
            .collect::<Vec<_>>();
        Self {
            store,
            next_window_id: windows.len() as u64 + 1,
            windows,
            revision: 0,
        }
    }

    fn entries(&self) -> Vec<(u64, WindowSession)> {
        self.windows.clone()
    }

    fn can_append(&self) -> bool {
        self.windows.len() < MAX_WINDOWS
    }

    fn append(&mut self, window: WindowSession) -> Result<u64, NavigationError> {
        if !self.can_append() {
            return Err(NavigationError::LimitReached("windows"));
        }
        let id = self.next_window_id;
        self.next_window_id = self.next_window_id.saturating_add(1);
        self.windows.push((id, window));
        Ok(id)
    }

    fn prepare_save(
        &mut self,
        window_id: u64,
        window: WindowSession,
    ) -> Result<(u64, Vec<u8>), NavigationError> {
        let Some((_, saved)) = self.windows.iter_mut().find(|(id, _)| *id == window_id) else {
            return Err(NavigationError::InvalidDocument(
                "window is not registered in the application session".into(),
            ));
        };
        *saved = window;
        let document = ApplicationSession::new(
            self.windows
                .iter()
                .map(|(_, window)| window.clone())
                .collect(),
        )?
        .to_json()?;
        self.revision = self.revision.wrapping_add(1);
        Ok((self.revision, document))
    }
}

#[derive(Clone, Debug)]
struct SessionBinding {
    coordinator: Arc<Mutex<SessionCoordinator>>,
    window_id: u64,
    operation_hub: OperationHub,
}

#[derive(Debug)]
struct PreparedSessionSave {
    binding: SessionBinding,
    revision: u64,
    document: Vec<u8>,
}

impl PreparedSessionSave {
    fn save_if_current(self) -> Result<bool, musheen_desktop::SessionStoreError> {
        let coordinator = self
            .binding
            .coordinator
            .lock()
            .expect("session coordinator lock is not poisoned");
        if coordinator.revision != self.revision {
            return Ok(false);
        }
        coordinator.store.save(&self.document)?;
        Ok(true)
    }
}

impl SessionBinding {
    fn prepare_save(&self, window: WindowSession) -> Result<PreparedSessionSave, NavigationError> {
        let (revision, document) = self
            .coordinator
            .lock()
            .expect("session coordinator lock is not poisoned")
            .prepare_save(self.window_id, window)?;
        Ok(PreparedSessionSave {
            binding: self.clone(),
            revision,
            document,
        })
    }

    fn can_append_window(&self) -> bool {
        self.coordinator
            .lock()
            .expect("session coordinator lock is not poisoned")
            .can_append()
    }

    fn append_window(&self, window: WindowSession) -> Result<Self, NavigationError> {
        let window_id = self
            .coordinator
            .lock()
            .expect("session coordinator lock is not poisoned")
            .append(window)?;
        Ok(Self {
            coordinator: Arc::clone(&self.coordinator),
            window_id,
            operation_hub: self.operation_hub.clone(),
        })
    }
}

struct MusheenApp {
    directories: HashMap<TabId, DirectoryModel>,
    searches: HashMap<TabId, ActiveSearch>,
    filters: HashMap<TabId, ActiveFilter>,
    sidebars: HashMap<TabId, SidebarModel>,
    pins: PinStore,
    limits: ResourceLimits,
    store: Arc<dyn Store>,
    shell: crate::ShellModel,
    navigation: WindowSession,
    omnibar: OmnibarState,
    omnibar_input: Option<Entity<InputState>>,
    omnibar_subscription: Option<Subscription>,
    requested_omnibar_mode: Option<OmnibarMode>,
    pending_omnibar_value: Option<String>,
    content_focus: FocusHandle,
    pending_content_focus: bool,
    pending_restored_focus: Option<FocusHandle>,
    browser_focus: Option<FocusHandle>,
    context_menu_focus: Option<FocusHandle>,
    session_binding: Option<SessionBinding>,
    session_save_generation: u64,
    watch_directories: bool,
    sidebar_visible: bool,
    customization_keys: Option<Subscription>,
    custom_actions: musheen_desktop::CustomActionDocument,
    custom_action_warning: Option<&'static str>,
    script_load_warning: Option<&'static str>,
    running_custom_actions: usize,
    script_actions: musheen_desktop::CustomActionDocument,
    scripts_enabled: bool,
    script_reload_revision: u64,
    custom_preflight: Option<custom_actions::ActionPreflight>,
    live_action_popups: Vec<custom_actions::LiveActionPopup>,
    icon_cache: HashMap<Box<str>, Option<ImageSource>>,
    info_panes: HashMap<TabId, InfoPaneModel>,
    catalog: Catalog,
    context_menu_theme: crate::ThemeProfile,
    operation_hub: OperationHub,
    operation_status_revision: u64,
    operation_error: Option<Box<str>>,
    status_center_open: bool,
    trash_states: HashMap<TabId, TrashState>,
    trash_focus: HashMap<TabId, CommandTargetRef>,
    pending_empty_trash: Option<MenuInvocation>,
    pending_restores: HashMap<WindowId, PendingRestore>,
    pending_drop: Option<PendingDrop>,
    pending_context_menu: Option<ContextMenu>,
    keyboard_context_popup: Option<Entity<PopupMenu>>,
    /// Dialog windows are tracked by their GPUI identity. The close observer
    /// clears only its own immutable pending workflow; opening a second review
    /// cannot overwrite the first one's command or targets.
    context_dialog_windows: Vec<ContextDialogWindow>,
    context_dialog_close_subscription: Option<Subscription>,
    conflict_subscriptions: Vec<Subscription>,
}

impl Drop for MusheenApp {
    fn drop(&mut self) {
        for directory in self.directories.values() {
            directory.cancel();
        }
        for search in self.searches.values() {
            search.cancellation.cancel();
        }
        for info_pane in self.info_panes.values_mut() {
            info_pane.cancel_active();
        }
    }
}

impl MusheenApp {
    fn high_contrast(cx: &Context<Self>) -> bool {
        cx.try_global::<NativeTheme>()
            .is_some_and(|theme| theme.accessibility().high_contrast)
    }

    #[cfg(test)]
    fn new_with_session_store(
        initial_path: PathBuf,
        session_store: Option<SessionStore>,
        cx: &mut Context<Self>,
    ) -> Self {
        let limits = ResourceLimits::default();
        let initial = StorePath::from_unix_path(initial_path.into_os_string());
        let (navigation, session_binding) = if let Some(store) = session_store {
            let application = restore_application_session(
                &store,
                initial.clone(),
                LocalStore::session_location_exists,
            )
            .unwrap_or_else(|| {
                ApplicationSession::new(vec![WindowSession::new(initial.clone())])
                    .expect("a single fallback window is a valid application session")
            });
            let coordinator = Arc::new(Mutex::new(SessionCoordinator::new(
                store,
                application.windows().to_vec(),
            )));
            let (window_id, navigation) = coordinator
                .lock()
                .expect("session coordinator lock is not poisoned")
                .entries()
                .into_iter()
                .next()
                .expect("an application session always contains a window");
            (
                navigation,
                Some(SessionBinding {
                    coordinator,
                    window_id,
                    operation_hub: OperationHub::new(&limits),
                }),
            )
        } else {
            (WindowSession::new(initial), None)
        };
        Self::new_with_navigation(navigation, session_binding, limits, false, cx)
    }

    fn new_with_navigation(
        mut navigation: WindowSession,
        session_binding: Option<SessionBinding>,
        limits: ResourceLimits,
        watch_directories: bool,
        cx: &mut Context<Self>,
    ) -> Self {
        let settings = cx
            .try_global::<crate::settings::RuntimeSettings>()
            .map(|runtime| runtime.0.clone());
        if let Some(settings) = &settings {
            crate::settings::general::configure_navigation(&mut navigation, settings);
        }
        let location = navigation.focused_tab().location().clone();
        let focused_tab = navigation.focused_tab().id();
        let pins = PinStore::default();
        let mut directories = HashMap::new();
        let mut directory = DirectoryModel::new(limits.snapshot());
        *directory.view_mut().preferences_mut() =
            navigation.focused_tab().view_preferences().clone();
        directories.insert(focused_tab, directory);
        let mut sidebars = HashMap::new();
        sidebars.insert(focused_tab, default_sidebar_model(pins.clone()));
        let operation_hub = session_binding
            .as_ref()
            .map(|binding| binding.operation_hub.clone())
            .unwrap_or_else(|| OperationHub::new(&limits));
        let operation_error = operation_hub.persistence_error();
        let operation_status_revision = operation_hub.status_revision();
        let mut this = Self {
            custom_actions: custom_actions::from_settings(settings.as_ref()),
            custom_action_warning: None,
            script_load_warning: None,
            running_custom_actions: 0,
            script_actions: musheen_desktop::CustomActionDocument::default(),
            scripts_enabled: false,
            script_reload_revision: 0,
            custom_preflight: None,
            live_action_popups: Vec::new(),
            customization_keys: None,
            directories,
            searches: HashMap::new(),
            filters: HashMap::new(),
            sidebars,
            pins,
            limits,
            store: Arc::new(LocalStore::new()),
            shell: crate::ShellModel::new(settings.as_ref().is_some_and(|settings| {
                settings.value("layout.info_pane").as_deref() == Some("true")
            })),
            navigation,
            omnibar: OmnibarState::default(),
            omnibar_input: None,
            omnibar_subscription: None,
            requested_omnibar_mode: None,
            pending_omnibar_value: None,
            content_focus: cx.focus_handle(),
            pending_content_focus: true,
            pending_restored_focus: None,
            browser_focus: None,
            context_menu_focus: None,
            session_binding,
            session_save_generation: 0,
            watch_directories,
            sidebar_visible: settings.as_ref().is_none_or(|settings| {
                settings.value("layout.sidebar").as_deref() != Some("false")
            }),
            icon_cache: HashMap::new(),
            info_panes: HashMap::new(),
            catalog: Catalog::system().expect("the built-in locale catalogs are valid"),
            context_menu_theme: crate::ThemeProfile::new(crate::AppearanceMode::Light, false),
            operation_hub,
            operation_status_revision,
            operation_error,
            status_center_open: false,
            trash_states: HashMap::new(),
            trash_focus: HashMap::new(),
            pending_empty_trash: None,
            pending_restores: HashMap::new(),
            pending_drop: None,
            pending_context_menu: None,
            keyboard_context_popup: None,
            context_dialog_windows: Vec::new(),
            context_dialog_close_subscription: None,
            conflict_subscriptions: Vec::new(),
        };
        this.start_load(location, cx);
        this.start_operation_status_refresh(cx);
        this
    }

    fn start_operation_status_refresh(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(OPERATION_STATUS_REFRESH_INTERVAL)
                    .await;
                let Some(this) = this.upgrade() else {
                    return;
                };
                this.update(cx, |state, cx| {
                    let revision = state.operation_hub.status_revision();
                    if revision != state.operation_status_revision {
                        state.operation_status_revision = revision;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    fn start_load(&mut self, location: StorePath, cx: &mut Context<Self>) {
        let tab_id = self.navigation.focused_tab().id();
        self.start_load_for_tab(tab_id, location, cx);
    }

    fn start_load_for_tab(&mut self, tab_id: TabId, location: StorePath, cx: &mut Context<Self>) {
        let trash = is_trash_location(&location);
        if !self.directories.contains_key(&tab_id) {
            let mut directory = DirectoryModel::new(self.limits.snapshot());
            if let Some(tab) = self.navigation.tab(tab_id) {
                *directory.view_mut().preferences_mut() =
                    self.navigation.preferences_for(tab.location()).clone();
            }
            self.directories.insert(tab_id, directory);
        }
        self.sidebars
            .entry(tab_id)
            .or_insert_with(|| default_sidebar_model(self.pins.clone()));
        let load = self
            .directories
            .entry(tab_id)
            .or_insert_with(|| DirectoryModel::new(self.limits.snapshot()))
            .begin_navigation(location);
        if trash {
            self.start_trash_load(tab_id, cx);
            return;
        }
        self.trash_states.remove(&tab_id);
        let worker_load = load.clone();
        let result_load = load.clone();
        let store = Arc::clone(&self.store);
        let limits = self.limits.snapshot();
        let work = cx.background_spawn(async move {
            enumerate_directory(store.as_ref(), &worker_load, &limits).await
        });

        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                state.apply_directory_result(tab_id, &result_load, result);
                cx.notify();
            });
        })
        .detach();
        if self.watch_directories {
            self.start_watch(tab_id, load, cx);
        }
    }

    fn start_trash_load(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        self.trash_states.insert(tab_id, TrashState::Loading);
        let work = cx.background_spawn(async move {
            let mut store = LocalStore::new();
            store.list_trash().map(|entries| {
                TrashSurfaceModel::new(
                    entries
                        .into_iter()
                        .map(|entry| {
                            TrashItem::with_kind(
                                entry.receipt().clone(),
                                entry.deleted_at_unix_seconds(),
                                entry.kind(),
                            )
                        })
                        .collect(),
                )
            })
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                if state
                    .navigation
                    .tab(tab_id)
                    .is_some_and(|tab| is_trash_location(tab.location()))
                {
                    state.trash_states.insert(
                        tab_id,
                        match result {
                            Ok(surface) => TrashState::Ready(surface),
                            Err(error) => TrashState::Error(error.to_string().into()),
                        },
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn start_watch(&mut self, tab_id: TabId, load: DirectoryLoad, cx: &mut Context<Self>) {
        let store = Arc::clone(&self.store);
        let location = load.location().clone();
        let cancellation = load.cancellation().clone();
        let work = cx.background_spawn(async move {
            let watcher = store
                .watch_directory(&location, cancellation.clone())
                .await?;
            next_watch_event(watcher, cancellation).await
        });
        cx.spawn(async move |this, cx| match work.await {
            Ok((watcher, event)) => {
                let Some(this) = this.upgrade() else {
                    return;
                };
                this.update(cx, |state, cx| {
                    if state.apply_watch_event(tab_id, &load, event) {
                        state.continue_watch(tab_id, load, watcher, cx);
                        cx.notify();
                    }
                });
            }
            Err(StoreError::Cancelled) => {}
            Err(error) => eprintln!("Musheen stopped watching a directory: {error}"),
        })
        .detach();
    }

    fn continue_watch(
        &mut self,
        tab_id: TabId,
        load: DirectoryLoad,
        watcher: Box<dyn DirectoryWatch>,
        cx: &mut Context<Self>,
    ) {
        let cancellation = load.cancellation().clone();
        let work = cx.background_spawn(next_watch_event(watcher, cancellation));
        cx.spawn(async move |this, cx| match work.await {
            Ok((watcher, event)) => {
                let Some(this) = this.upgrade() else {
                    return;
                };
                this.update(cx, |state, cx| {
                    if state.apply_watch_event(tab_id, &load, event) {
                        state.continue_watch(tab_id, load, watcher, cx);
                        cx.notify();
                    }
                });
            }
            Err(StoreError::Cancelled) => {}
            Err(error) => eprintln!("Musheen stopped watching a directory: {error}"),
        })
        .detach();
    }

    fn apply_watch_event(
        &mut self,
        tab_id: TabId,
        load: &DirectoryLoad,
        event: WatchEvent,
    ) -> bool {
        self.directories
            .get_mut(&tab_id)
            .is_some_and(|directory| directory.apply_watch_event(load, event))
    }

    fn apply_directory_result(
        &mut self,
        tab_id: TabId,
        load: &DirectoryLoad,
        result: Result<Vec<Page<StoreItem>>, StoreError>,
    ) {
        let Some(directory) = self.directories.get_mut(&tab_id) else {
            return;
        };
        match result {
            Ok(pages) => {
                for page in pages {
                    directory.apply_page(load, page);
                }
            }
            Err(error) => {
                directory.apply_error(load, error.to_string());
            }
        }
    }

    fn navigate(&mut self, location: StorePath, remember: bool, cx: &mut Context<Self>) {
        if self.browser_input_blocked() {
            return;
        }
        let tab_id = self.navigation.focused_tab().id();
        self.cancel_search(tab_id);
        self.info_panes.entry(tab_id).or_default().clear();
        self.filters.remove(&tab_id);
        if remember {
            self.navigation.navigate_focused(location.clone());
            self.schedule_session_save(cx);
        }
        let preferences = self.navigation.preferences_for(&location).clone();
        *self.focused_directory_mut().view_mut().preferences_mut() = preferences.clone();
        self.navigation
            .focused_tab_mut()
            .set_view_preferences(preferences);
        self.pending_omnibar_value =
            Some(DisplayPath::from_store_path(&location).as_str().to_owned());
        self.omnibar.enter(
            OmnibarMode::Path,
            self.pending_omnibar_value.clone().unwrap_or_default(),
        );
        self.pending_content_focus = true;
        self.start_load(location, cx);
        cx.notify();
    }

    fn load_focused_tab(&mut self, cx: &mut Context<Self>) {
        let location = self.navigation.focused_tab().location().clone();
        let display = DisplayPath::from_store_path(&location).as_str().to_owned();
        let tab_id = self.navigation.focused_tab().id();
        if let Some(search) = self.searches.get(&tab_id) {
            self.omnibar
                .enter(OmnibarMode::Search, search.expression.clone());
            self.pending_omnibar_value = Some(search.expression.clone());
        } else if let Some(filter) = self.filters.get(&tab_id) {
            self.omnibar
                .enter(OmnibarMode::Filter, filter.expression.clone());
            self.pending_omnibar_value = Some(filter.expression.clone());
        } else {
            self.omnibar.enter(OmnibarMode::Path, display.clone());
            self.pending_omnibar_value = Some(display);
        }
        self.pending_content_focus = true;
        let loaded = if is_trash_location(&location) {
            self.trash_states.contains_key(&tab_id)
        } else {
            self.directories
                .get(&tab_id)
                .and_then(DirectoryModel::location)
                == Some(&location)
        };
        if loaded {
            cx.notify();
        } else {
            self.start_load(location, cx);
            cx.notify();
        }
    }

    fn focused_directory(&self) -> &DirectoryModel {
        self.directories
            .get(&self.navigation.focused_tab().id())
            .expect("the focused tab owns a directory model")
    }

    fn focused_directory_mut(&mut self) -> &mut DirectoryModel {
        self.directories
            .get_mut(&self.navigation.focused_tab().id())
            .expect("the focused tab owns a directory model")
    }

    #[cfg(test)]
    fn active_search(&self) -> Option<&SearchResultModel> {
        self.searches
            .get(&self.navigation.focused_tab().id())
            .map(|search| &search.model)
    }

    fn cancel_search(&mut self, tab_id: TabId) {
        if let Some(search) = self.searches.remove(&tab_id) {
            search.cancellation.cancel();
        }
    }

    fn filtered_items(&self, tab_id: TabId) -> Vec<&StoreItem> {
        let Some(directory) = self.directories.get(&tab_id) else {
            return Vec::new();
        };
        self.filters
            .get(&tab_id)
            .and_then(|filter| filter.filter.as_ref())
            .map_or_else(
                || directory.view().visible_items(),
                |filter| filter.apply(directory.view()),
            )
    }

    fn activate_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        if self.browser_input_blocked() {
            return;
        }
        let previous = self.navigation.focused_tab().id();
        if self.navigation.focused_pane_mut().activate_tab(id).is_ok() {
            if previous != id {
                self.info_panes.entry(previous).or_default().clear();
            }
            self.schedule_session_save(cx);
            self.load_focused_tab(cx);
        }
    }

    fn focus_pane(&mut self, id: PaneId, cx: &mut Context<Self>) {
        if !self.context_dialog_windows.is_empty() {
            return;
        }
        let previous = self.navigation.focused_tab().id();
        if self.navigation.focus_pane(id).is_ok() {
            let focused = self.navigation.focused_tab().id();
            if previous != focused {
                self.info_panes.entry(previous).or_default().clear();
            }
            self.schedule_session_save(cx);
            self.load_focused_tab(cx);
        }
    }

    fn schedule_session_save(&mut self, cx: &mut Context<Self>) {
        if self.session_binding.is_none() {
            return;
        }
        self.session_save_generation = self.session_save_generation.wrapping_add(1);
        let generation = self.session_save_generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SESSION_SAVE_DELAY).await;
            let Some(this) = this.upgrade() else {
                return;
            };
            let prepared = this.update(cx, |state, _| {
                if state.session_save_generation != generation {
                    return None;
                }
                let binding = state.session_binding.clone()?;
                match binding.prepare_save(state.navigation.clone()) {
                    Ok(prepared) => Some(prepared),
                    Err(error) => {
                        eprintln!("Musheen could not serialize its browsing session: {error}");
                        None
                    }
                }
            });
            let Some(prepared) = prepared else {
                return;
            };
            if let Err(error) = cx
                .background_executor()
                .spawn(async move { prepared.save_if_current() })
                .await
            {
                eprintln!("Musheen could not save its browsing session: {error}");
            }
        })
        .detach();
    }

    fn dispatch_command(&mut self, command_id: &str, cx: &mut Context<Self>) {
        if !self.context_dialog_windows.is_empty() {
            // Context reviews are guarded modals: browser shortcuts must not
            // reach a background pane while an immutable decision is open.
            return;
        }
        let Some(command) = self.shell.commands().get(command_id) else {
            return;
        };
        let request = self.active_command_request(command.action());
        if !command.state(request.context()).is_enabled() {
            return;
        }
        let action = command.action();
        let menu = self.compose_context_request(request);
        if let Some(entry) = Self::menu_entry_by_id(&menu, command_id) {
            // Toolbar and browser shortcuts invoke the very same registry
            // projection as a context row, including parameter construction,
            // backend state and confirmation/chooser routing.
            self.dispatch_context_entry(entry.clone(), cx);
            return;
        }
        if is_contextual_command(action) {
            // An absent contextual command is inapplicable. Never use the
            // generic action switch as a back door around registry policy.
            return;
        }
        self.dispatch_action(action, cx);
    }

    fn menu_entry_by_id<'a>(menu: &'a ContextMenu, command_id: &str) -> Option<&'a MenuEntry> {
        menu.entries().iter().find_map(|entry| {
            (entry.command_id() == Some(command_id))
                .then_some(entry)
                .or_else(|| {
                    entry
                        .submenu()
                        .and_then(|submenu| Self::menu_entry_by_id(submenu, command_id))
                })
        })
    }

    fn open_selected_properties_page(&mut self, page: PropertiesPage, cx: &mut Context<Self>) {
        let view = self.focused_directory().view();
        let selected = view
            .selected_ids()
            .iter()
            .filter_map(|id| view.item(id))
            .filter_map(|item| {
                item.path()
                    .as_unix_path()
                    .map(|path| (item.display_name().as_str().to_owned(), path.to_path_buf()))
            })
            .collect::<Vec<_>>();
        if selected.is_empty() {
            return;
        }
        self.open_properties_paths(
            selected.into_iter().map(|(_, path)| path).collect(),
            page,
            cx,
        );
    }

    fn open_properties_targets(
        &mut self,
        targets: &[CommandTargetRef],
        page: PropertiesPage,
        cx: &mut Context<Self>,
    ) {
        self.open_properties_paths(
            targets
                .iter()
                .filter_map(|target| target.path().as_unix_path().map(Path::to_path_buf))
                .collect(),
            page,
            cx,
        );
    }

    fn open_properties_paths(
        &mut self,
        paths: Vec<PathBuf>,
        page: PropertiesPage,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        let title = if paths.len() == 1 {
            format!("{} Properties", paths[0].display())
        } else {
            format!("{} items — Properties", paths.len())
        };
        let options = properties_window_options(title, cx);
        let operation_hub = self.operation_hub.clone();
        let work = cx.background_spawn(async move { PropertiesWindowData::load(&paths) });
        cx.spawn(async move |_, cx| {
            let result = work.await;
            cx.open_window(options, move |window, cx| match result {
                Ok(data) => {
                    let view = cx.new(|cx| {
                        PropertiesWindow::with_hub_page(data, operation_hub, page, window, cx)
                    });
                    cx.new(|cx| Root::new(view, window, cx))
                }
                Err(error) => {
                    let view = cx.new(|cx| PropertiesFailureWindow::new(error.to_string(), cx));
                    cx.new(|cx| Root::new(view, window, cx))
                }
            })
            .expect("Musheen could not open a Properties window");
        })
        .detach();
    }

    fn open_selected_properties(&mut self, cx: &mut Context<Self>) {
        self.open_selected_properties_page(PropertiesPage::General, cx);
    }

    fn dispatch_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        match action {
            CommandAction::OpenSettings => crate::settings::open_settings_window(cx),
            CommandAction::NavigateBack
            | CommandAction::NavigateForward
            | CommandAction::NavigateParent
            | CommandAction::Refresh => self.dispatch_navigation_action(action, cx),
            CommandAction::FocusLocation
            | CommandAction::Search
            | CommandAction::Filter
            | CommandAction::FocusCommand => self.dispatch_omnibar_action(action, cx),
            CommandAction::ViewDetails
            | CommandAction::ViewList
            | CommandAction::ViewCards
            | CommandAction::ViewGrid
            | CommandAction::ViewColumns
            | CommandAction::ViewAdaptive
            | CommandAction::CycleSort
            | CommandAction::CycleGroup
            | CommandAction::ToggleDirectoriesFirst
            | CommandAction::ToggleHidden
            | CommandAction::ToggleSidebar
            | CommandAction::ToggleInfo => self.dispatch_view_action(action, cx),
            CommandAction::NewTab
            | CommandAction::CloseTab
            | CommandAction::DuplicateTab
            | CommandAction::ReopenClosedTab
            | CommandAction::MoveTabOtherPane
            | CommandAction::MoveTabLeft
            | CommandAction::MoveTabRight
            | CommandAction::TearOutTab => self.dispatch_tab_action(action, cx),
            CommandAction::SplitPane | CommandAction::FocusNextPane => {
                self.dispatch_pane_action(action, cx);
            }
            CommandAction::SelectAll | CommandAction::ClearSelection => {
                self.dispatch_selection_action(action, cx);
            }
            CommandAction::OpenProperties => self.open_selected_properties(cx),
            CommandAction::Permissions => {
                self.open_selected_properties_page(PropertiesPage::Permissions, cx);
            }
            action => {
                self.operation_error = Some(
                    format!(
                        "{}: {action:?}",
                        self.catalog
                            .message("context.dispatch-unavailable")
                            .expect("dispatcher refusal is localized")
                    )
                    .into(),
                );
                cx.notify();
            }
        }
    }

    fn dispatch_navigation_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        match action {
            CommandAction::NavigateBack => {
                if let Some(location) = self.navigation.go_back().cloned() {
                    self.navigate(location, false, cx);
                }
            }
            CommandAction::NavigateForward => {
                if let Some(location) = self.navigation.go_forward().cloned() {
                    self.navigate(location, false, cx);
                }
            }
            CommandAction::NavigateParent => {
                let parent = self
                    .focused_directory()
                    .location()
                    .and_then(StorePath::as_unix_path)
                    .and_then(Path::parent)
                    .map(|path| StorePath::from_unix_path(path.as_os_str()));
                if let Some(parent) = parent {
                    self.navigate(parent, true, cx);
                }
            }
            CommandAction::Refresh => {
                if let Some(location) = self.focused_directory().location().cloned() {
                    self.start_load(location, cx);
                }
            }
            _ => {}
        }
    }

    fn dispatch_omnibar_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        self.requested_omnibar_mode = match action {
            CommandAction::FocusLocation => Some(OmnibarMode::Path),
            CommandAction::Search => Some(OmnibarMode::Search),
            CommandAction::Filter => Some(OmnibarMode::Filter),
            CommandAction::FocusCommand => Some(OmnibarMode::Command),
            _ => None,
        };
        cx.notify();
    }

    fn dispatch_view_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        let changed = match action {
            CommandAction::ViewDetails => self.set_layout(Layout::Details),
            CommandAction::ViewList => self.set_layout(Layout::List),
            CommandAction::ViewCards => self.set_layout(Layout::Cards),
            CommandAction::ViewGrid => self.set_layout(Layout::Grid),
            CommandAction::ViewColumns => self.set_layout(Layout::Columns),
            CommandAction::ViewAdaptive => self.set_layout(Layout::Adaptive),
            CommandAction::CycleSort => {
                let preferences = self.focused_directory_mut().view_mut().preferences_mut();
                preferences.sort = SortSpec {
                    key: match preferences.sort.key {
                        SortKey::Name => SortKey::Size,
                        SortKey::Size => SortKey::Kind,
                        SortKey::Kind => SortKey::Modified,
                        SortKey::Modified => SortKey::Name,
                    },
                    direction: SortDirection::Ascending,
                };
                true
            }
            CommandAction::CycleGroup => {
                let group = &mut self
                    .focused_directory_mut()
                    .view_mut()
                    .preferences_mut()
                    .group;
                *group = match *group {
                    GroupKey::None => GroupKey::Kind,
                    GroupKey::Kind => GroupKey::FirstLetter,
                    GroupKey::FirstLetter => GroupKey::Modified,
                    GroupKey::Modified => GroupKey::None,
                };
                true
            }
            CommandAction::ToggleDirectoriesFirst => {
                let value = &mut self
                    .focused_directory_mut()
                    .view_mut()
                    .preferences_mut()
                    .directories_first;
                *value = !*value;
                true
            }
            CommandAction::ToggleHidden => {
                let value = &mut self
                    .focused_directory_mut()
                    .view_mut()
                    .preferences_mut()
                    .show_hidden;
                *value = !*value;
                true
            }
            CommandAction::ToggleSidebar => {
                self.sidebar_visible = !self.sidebar_visible;
                false
            }
            CommandAction::ToggleInfo => {
                self.shell.toggle_info();
                false
            }
            _ => false,
        };
        if changed {
            self.persist_focused_view_preferences(cx);
        }
        cx.notify();
    }

    fn set_layout(&mut self, layout: Layout) -> bool {
        self.focused_directory_mut()
            .view_mut()
            .preferences_mut()
            .layout = layout;
        true
    }

    fn persist_focused_view_preferences(&mut self, cx: &mut Context<Self>) {
        self.persist_view_preferences(self.navigation.focused_tab().id(), cx);
    }

    fn persist_view_preferences(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(preferences) = self
            .directories
            .get(&tab_id)
            .map(|directory| directory.view().preferences().clone())
        else {
            return;
        };
        let Some(location) = self
            .navigation
            .tab(tab_id)
            .map(|tab| tab.location().clone())
        else {
            return;
        };
        if let Some(tab) = self.navigation.tab_mut(tab_id) {
            tab.set_view_preferences(preferences.clone());
        }
        self.navigation.set_preferences_for(location, preferences);
        self.schedule_session_save(cx);
    }

    fn dispatch_selection_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        match action {
            CommandAction::SelectAll => {
                self.focused_directory_mut().view_mut().select_all_visible()
            }
            CommandAction::ClearSelection => {
                self.focused_directory_mut().view_mut().clear_selection()
            }
            _ => return,
        }
        let selected = self.focused_directory().view().selected_ids().to_vec();
        self.navigation.focused_tab_mut().set_selection(selected);
        let tab_id = self.navigation.focused_tab().id();
        self.refresh_info_pane(tab_id, cx);
        self.schedule_session_save(cx);
        cx.notify();
    }

    fn dispatch_selection_action_for_tab(
        &mut self,
        origin_tab: Option<TabId>,
        action: CommandAction,
        cx: &mut Context<Self>,
    ) {
        let tab_id = origin_tab.unwrap_or_else(|| self.navigation.focused_tab().id());
        let Some(directory) = self.directories.get_mut(&tab_id) else {
            return;
        };
        match action {
            CommandAction::SelectAll => directory.view_mut().select_all_visible(),
            CommandAction::ClearSelection => directory.view_mut().clear_selection(),
            _ => return,
        }
        let selected = directory.view().selected_ids().to_vec();
        if let Some(tab) = self.navigation.tab_mut(tab_id) {
            tab.set_selection(selected);
        }
        self.refresh_info_pane(tab_id, cx);
        self.schedule_session_save(cx);
        cx.notify();
    }

    fn dispatch_context_view_action(
        &mut self,
        origin_tab: Option<TabId>,
        action: CommandAction,
        cx: &mut Context<Self>,
    ) {
        let tab_id = origin_tab.unwrap_or_else(|| self.navigation.focused_tab().id());
        let mut changed = false;
        if let Some(directory) = self.directories.get_mut(&tab_id) {
            let preferences = directory.view_mut().preferences_mut();
            match action {
                CommandAction::ViewDetails => preferences.layout = Layout::Details,
                CommandAction::ViewList => preferences.layout = Layout::List,
                CommandAction::ViewCards => preferences.layout = Layout::Cards,
                CommandAction::ViewGrid => preferences.layout = Layout::Grid,
                CommandAction::ViewColumns => preferences.layout = Layout::Columns,
                CommandAction::ViewAdaptive => preferences.layout = Layout::Adaptive,
                CommandAction::CycleSort => {
                    preferences.sort = SortSpec {
                        key: match preferences.sort.key {
                            SortKey::Name => SortKey::Size,
                            SortKey::Size => SortKey::Kind,
                            SortKey::Kind => SortKey::Modified,
                            SortKey::Modified => SortKey::Name,
                        },
                        direction: SortDirection::Ascending,
                    };
                }
                CommandAction::CycleGroup => {
                    preferences.group = match preferences.group {
                        GroupKey::None => GroupKey::Kind,
                        GroupKey::Kind => GroupKey::FirstLetter,
                        GroupKey::FirstLetter => GroupKey::Modified,
                        GroupKey::Modified => GroupKey::None,
                    };
                }
                CommandAction::ToggleDirectoriesFirst => {
                    preferences.directories_first = !preferences.directories_first;
                }
                CommandAction::ToggleHidden => preferences.show_hidden = !preferences.show_hidden,
                CommandAction::ToggleSidebar => self.sidebar_visible = !self.sidebar_visible,
                CommandAction::ToggleInfo => self.shell.toggle_info(),
                _ => return,
            }
            changed = !matches!(
                action,
                CommandAction::ToggleSidebar | CommandAction::ToggleInfo
            );
        }
        if changed {
            self.persist_view_preferences(tab_id, cx);
        }
        cx.notify();
    }

    fn select_item(&mut self, tab_id: TabId, id: ItemId, cx: &mut Context<Self>) {
        if !self.context_dialog_windows.is_empty() {
            return;
        }
        let selected = {
            let Some(directory) = self.directories.get_mut(&tab_id) else {
                return;
            };
            directory.view_mut().select_item(id, SelectionMode::Replace);
            directory.view().selected_ids().to_vec()
        };
        if let Some(tab) = self.navigation.tab_mut(tab_id) {
            tab.set_selection(selected);
        }
        self.refresh_info_pane(tab_id, cx);
        self.schedule_session_save(cx);
        cx.notify();
    }

    fn focus_directory_item(&mut self, tab_id: TabId, id: Option<ItemId>, cx: &mut Context<Self>) {
        if !self.context_dialog_windows.is_empty() {
            return;
        }
        if id.is_none() {
            self.trash_focus.remove(&tab_id);
        }
        if let Some(directory) = self.directories.get_mut(&tab_id) {
            directory.view_mut().focus_item(id);
            cx.notify();
        }
    }

    fn move_directory_focus(&mut self, delta: isize, cx: &mut Context<Self>) {
        if !self.context_dialog_windows.is_empty() {
            return;
        }
        let tab_id = self.navigation.focused_tab().id();
        if is_trash_location(self.navigation.focused_tab().location()) {
            let Some(TrashState::Ready(surface)) = self.trash_states.get(&tab_id) else {
                return;
            };
            let targets = surface
                .items()
                .iter()
                .map(trash_command_target)
                .collect::<Vec<_>>();
            if targets.is_empty() {
                return;
            }
            let current = self
                .trash_focus
                .get(&tab_id)
                .and_then(|focused| targets.iter().position(|target| target == focused));
            let index = current
                .map(|index| (index as isize + delta).clamp(0, targets.len() as isize - 1) as usize)
                .unwrap_or_else(|| if delta < 0 { targets.len() - 1 } else { 0 });
            self.trash_focus.insert(tab_id, targets[index].clone());
            cx.notify();
            return;
        }
        let Some(directory) = self.directories.get_mut(&tab_id) else {
            return;
        };
        let items = directory.view().visible_items();
        if items.is_empty() {
            directory.view_mut().focus_item(None);
            return;
        }
        let current = directory
            .view()
            .focused_item_id()
            .and_then(|id| items.iter().position(|item| item.id() == id));
        let index = current
            .map(|index| (index as isize + delta).clamp(0, items.len() as isize - 1) as usize)
            .unwrap_or_else(|| if delta < 0 { items.len() - 1 } else { 0 });
        let next = items[index].id().clone();
        directory.view_mut().focus_item(Some(next));
        cx.notify();
    }

    /// Builds a menu request from the pane that received the pointer event.
    /// Capturing the pane's tab id here prevents the other pane's selection
    /// from becoming an accidental target after focus changes.
    fn item_context_menu(
        &mut self,
        tab_id: TabId,
        clicked: ItemId,
        cx: &mut Context<Self>,
    ) -> ContextMenu {
        let (clicked_target, selected) = {
            let Some(directory) = self.directories.get(&tab_id) else {
                return self.compose_context_menu(tab_id, MenuTarget::Background, Vec::new());
            };
            let view = directory.view();
            let Some(item) = view.item(&clicked) else {
                return self.compose_context_menu(tab_id, MenuTarget::Background, Vec::new());
            };
            let clicked_target = CommandTargetRef::new(item.id().clone(), item.path().clone())
                .expect("directory items have stable command targets");
            let selected = view
                .selected_ids()
                .iter()
                .filter_map(|id| view.item(id))
                .filter_map(|item| {
                    CommandTargetRef::new(item.id().clone(), item.path().clone()).ok()
                })
                .collect::<Vec<_>>();
            (clicked_target, selected)
        };
        let prepared = self
            .shell
            .context_menus()
            .prepare_pointer_target(&selected, &clicked_target);
        if !selected
            .iter()
            .any(|target| target.id() == clicked_target.id())
        {
            self.select_item(tab_id, clicked, cx);
        }
        self.focus_directory_item(tab_id, Some(clicked_target.id().clone()), cx);
        self.preflight_custom_actions(
            prepared.selection(),
            self.navigation
                .tab(tab_id)
                .expect("context tab exists")
                .location()
                .clone(),
            cx,
        );
        self.compose_context_menu(tab_id, MenuTarget::Item, prepared.selection().to_vec())
    }

    fn sidebar_context_menu(&self, tab_id: TabId) -> ContextMenu {
        self.compose_context_menu(tab_id, MenuTarget::SidebarLocation, Vec::new())
    }

    fn open_keyboard_context_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.context_dialog_windows.is_empty() {
            return;
        }
        self.remember_context_invocation_focus(window, cx);
        let tab_id = self.navigation.focused_tab().id();
        let trash = is_trash_location(self.navigation.focused_tab().location());
        let focused = if trash {
            self.trash_focus.get(&tab_id).cloned()
        } else {
            self.directories.get(&tab_id).and_then(|directory| {
                directory
                    .view()
                    .focused_item_id()
                    .and_then(|id| directory.view().item(id))
                    .and_then(|item| {
                        CommandTargetRef::new(item.id().clone(), item.path().clone()).ok()
                    })
            })
        };
        let prepared = self.shell.context_menus().prepare_keyboard_target(focused);
        let menu_target = if prepared.selection().is_empty() {
            MenuTarget::Background
        } else if trash {
            MenuTarget::TrashItem
        } else {
            MenuTarget::Item
        };
        let menu = self.compose_context_menu(tab_id, menu_target, prepared.selection().to_vec());
        let app = cx.entity().downgrade();
        let popup = PopupMenu::build(window, cx, move |popup, window, popup_cx| {
            Self::populate_context_popup(
                popup,
                menu,
                app.clone(),
                format!("keyboard-{tab_id:?}"),
                window,
                popup_cx,
            )
        });
        let popup_for_focus = popup.clone();
        popup.update(cx, |popup, cx| {
            popup.focus_handle(cx).focus(window, cx);
        });
        let subscription = cx.subscribe(&popup_for_focus, move |this, _, _: &DismissEvent, cx| {
            this.keyboard_context_popup = None;
            if !this.browser_input_blocked() {
                this.pending_restored_focus = this.browser_focus.clone();
            }
            cx.notify();
        });
        self.conflict_subscriptions.push(subscription);
        self.keyboard_context_popup = Some(popup);
        cx.notify();
    }

    fn sidebar_location_context_menu(
        &self,
        tab_id: TabId,
        target: MenuTarget,
        location: StorePath,
    ) -> ContextMenu {
        // A sidebar/mount row is a concrete provider location, not a generic
        // background. Capture its current identity when the provider exposes
        // one so later invocation cannot be redirected by a pane change.
        let selection = self
            .store
            .resolve_item(&location)
            .ok()
            .flatten()
            .and_then(|item| CommandTargetRef::new(item.id().clone(), item.path().clone()).ok())
            .into_iter()
            .collect();
        self.compose_context_menu_at(tab_id, target, location, selection)
    }

    fn compose_context_menu(
        &self,
        tab_id: TabId,
        target: MenuTarget,
        selection: Vec<CommandTargetRef>,
    ) -> ContextMenu {
        let location = self
            .navigation
            .tab(tab_id)
            .map(|tab| tab.location().clone())
            .unwrap_or_else(|| self.navigation.focused_tab().location().clone());
        self.compose_context_menu_at(tab_id, target, location, selection)
    }

    fn context_menu_request(
        &self,
        tab_id: TabId,
        target: MenuTarget,
        location: StorePath,
        selection: Vec<CommandTargetRef>,
    ) -> crate::ContextMenuRequest {
        let target = if target == MenuTarget::Background && is_trash_location(&location) {
            MenuTarget::TrashBackground
        } else {
            target
        };
        let selection = if target == MenuTarget::TrashBackground {
            match self.trash_states.get(&tab_id) {
                Some(TrashState::Ready(surface)) => {
                    surface.items().iter().map(trash_command_target).collect()
                }
                _ => Vec::new(),
            }
        } else {
            selection
        };
        let context = self.context_for_menu(tab_id, target, &location, &selection);
        let trash_contents = if target == MenuTarget::TrashBackground {
            selection.clone()
        } else {
            Vec::new()
        };
        let contributions = self.custom_action_contributions(&selection, &location);
        crate::ContextMenuRequest::new(context, target, location, selection)
            .with_trash_contents(trash_contents)
            .with_origin_tab(tab_id)
            .with_actions(&contributions)
    }

    fn compose_context_menu_at(
        &self,
        tab_id: TabId,
        target: MenuTarget,
        location: StorePath,
        selection: Vec<CommandTargetRef>,
    ) -> ContextMenu {
        self.compose_context_request(self.context_menu_request(tab_id, target, location, selection))
    }

    fn compose_context_request(&self, request: crate::ContextMenuRequest) -> ContextMenu {
        self.shell
            .context_menus()
            .clone()
            .with_locale(self.catalog.locale())
            .with_theme_profile(self.context_menu_theme)
            .compose(request)
    }

    fn context_for_menu(
        &self,
        tab_id: TabId,
        target: MenuTarget,
        location: &StorePath,
        selection: &[CommandTargetRef],
    ) -> CommandContext {
        let item_count = self
            .directories
            .get(&tab_id)
            .map_or(0, |directory| directory.view().items().len());
        let selected_item = selection.first().and_then(|target| {
            self.directories
                .get(&tab_id)
                .and_then(|directory| directory.view().item(target.id()))
        });
        let executable_state = selected_item.and_then(|item| {
            (item.kind() == ItemKind::RegularFile)
                .then(|| self.store.executable_state(item.path()).ok())
                .flatten()
        });
        let item_target = if target == MenuTarget::Mount {
            CommandTarget::Mount
        } else if target == MenuTarget::SidebarLocation {
            CommandTarget::Sidebar
        } else if target == MenuTarget::Tag {
            CommandTarget::Tag
        } else if target == MenuTarget::TrashItem {
            CommandTarget::TrashItem
        } else if target == MenuTarget::TrashBackground
            || is_trash_location(location) && selection.is_empty()
        {
            CommandTarget::TrashBackground
        } else if selection.len() > 1 {
            CommandTarget::MultiSelection
        } else if let Some(item) = selected_item {
            match item.kind() {
                ItemKind::Directory => CommandTarget::Directory,
                ItemKind::RegularFile
                    if matches!(executable_state, Some(CapabilityState::Supported)) =>
                {
                    CommandTarget::ExecutableFile
                }
                _ if is_archive_path(item.path()) => CommandTarget::Archive,
                _ => CommandTarget::File,
            }
        } else {
            CommandTarget::Background
        };
        // Location commands use the exact selected directory (or the pane
        // background), never the currently focused pane after the menu opens.
        let command_location =
            if matches!(item_target, CommandTarget::Directory | CommandTarget::Mount) {
                selection
                    .first()
                    .map_or_else(|| location.clone(), |target| target.path().clone())
            } else {
                location.clone()
            };
        let capabilities = self.store.capabilities(&command_location);
        let is_local = location.as_unix_path().is_some();
        let writable_state = self
            .store
            .location_writable(&command_location)
            .unwrap_or_else(|error| {
                CapabilityState::Unknown(
                    CapabilityReason::new(format!(
                        "{}: {error}",
                        self.catalog
                            .message("context.directory-verification")
                            .expect("directory refusal is localized")
                    ))
                    .expect("the writable-location failure is visible"),
                )
            });
        let writable = matches!(writable_state, CapabilityState::Supported);
        let writable_reason = writable_state.reason().unwrap_or(
            self.catalog
                .message("context.write-unknown")
                .expect("write refusal is localized"),
        );
        let unsupported_provider_action = CapabilityState::Unsupported(
            CapabilityReason::new(
                self.catalog
                    .message("context.provider-unavailable")
                    .expect("provider refusal is localized"),
            )
            .expect("the provider-action reason is valid"),
        );
        let pane = self.navigation.focused_pane();
        let active_tab = pane.active_tab().id();
        CommandContext {
            can_go_back: self
                .navigation
                .tab(tab_id)
                .is_some_and(|tab| tab.history().can_go_back()),
            can_go_forward: self
                .navigation
                .tab(tab_id)
                .is_some_and(|tab| tab.history().can_go_forward()),
            has_parent: location.as_unix_path().and_then(Path::parent).is_some(),
            can_close_tab: pane.tabs().len() > 1,
            can_reopen_closed_tab: pane.has_closed_tabs() && pane.has_tab_capacity(),
            can_move_tab_left: pane
                .tabs()
                .first()
                .is_some_and(|tab| tab.id() != active_tab),
            can_move_tab_right: pane.tabs().last().is_some_and(|tab| tab.id() != active_tab),
            can_create_tab: pane.has_tab_capacity(),
            can_move_tab_other_pane: pane.tabs().len() > 1
                && self
                    .navigation
                    .panes()
                    .iter()
                    .any(|other| other.id() != pane.id() && other.has_tab_capacity()),
            can_tear_out_tab: pane.tabs().len() > 1
                && self
                    .session_binding
                    .as_ref()
                    .is_some_and(SessionBinding::can_append_window),
            can_split_pane: self.navigation.can_split(),
            can_focus_next_pane: self.navigation.panes().len() > 1,
            target: item_target,
            item_count,
            selection_count: selection.len(),
            location_is_writable: writable,
            resolved_destination: Some(if writable {
                musheen_core::ResolvedDestination::writable(command_location.clone())
            } else {
                musheen_core::ResolvedDestination::read_only(
                    command_location.clone(),
                    writable_reason,
                )
            }),
            mutation_is_supported: writable,
            mutation_reason: (!writable).then(|| writable_reason.into()),
            is_local,
            supports_provider_uris: self
                .custom_actions
                .actions()
                .iter()
                .any(|action| action.supports_provider_uris),
            has_dot_name_semantics: is_local,
            target_is_hidden: selected_item.is_some_and(|item| is_hidden_path(item.path())),
            executable_run_enabled: matches!(executable_state, Some(CapabilityState::Supported)),
            capabilities,
            provider_actions: ProviderActionMatrix::from_states(
                unsupported_provider_action.clone(),
                unsupported_provider_action.clone(),
                unsupported_provider_action.clone(),
                unsupported_provider_action,
            ),
            show_hidden: self
                .directories
                .get(&tab_id)
                .is_some_and(|directory| directory.view().preferences().show_hidden),
            directories_first: self
                .directories
                .get(&tab_id)
                .is_some_and(|directory| directory.view().preferences().directories_first),
            sidebar_visible: self.sidebar_visible,
            info_visible: self.shell.info_visible(),
            active_layout: self
                .directories
                .get(&tab_id)
                .map(|directory| match directory.view().preferences().layout {
                    Layout::Details => ActiveLayout::Details,
                    Layout::List => ActiveLayout::List,
                    Layout::Cards => ActiveLayout::Cards,
                    Layout::Grid => ActiveLayout::Grid,
                    Layout::Columns => ActiveLayout::Columns,
                    Layout::Adaptive => ActiveLayout::Adaptive,
                })
                .unwrap_or_default(),
            backend_actions: Some(
                CommandAction::ALL
                    .iter()
                    .copied()
                    .map(|action| {
                        (
                            action,
                            self.context_backend_action_state(
                                action,
                                tab_id,
                                item_target,
                                selection,
                            ),
                        )
                    })
                    .collect(),
            ),
            ..CommandContext::default()
        }
    }

    fn populate_context_popup(
        popup: PopupMenu,
        menu: ContextMenu,
        app: gpui_kit::WeakEntity<Self>,
        path: String,
        window: &mut Window,
        cx: &mut Context<PopupMenu>,
    ) -> PopupMenu {
        let observer = app.clone();
        crate::menus::ContextMenuRenderer::populate_live(
            popup,
            menu,
            path,
            window,
            cx,
            move |entry, _, cx| {
                let _ = app.update(cx, |this, cx| this.dispatch_context_entry(entry, cx));
            },
            move |menu, path, window, cx| {
                MusheenApp::track_live_action_popup(observer.clone(), menu, path, window, cx);
            },
        )
    }

    fn dispatch_context_entry(&mut self, entry: MenuEntry, cx: &mut Context<Self>) {
        if !self.context_dialog_windows.is_empty() {
            return;
        }
        if entry.command_id().is_none() {
            return;
        }
        let surface = self.shell.context_menus().clone();
        let mut dispatcher = AppMenuDispatcher::default();
        match surface.invoke(&entry, &mut dispatcher) {
            crate::MenuInvocation::Dispatched => {
                if let Some((action, parameters)) = dispatcher.dispatched {
                    self.dispatch_typed_context_command(
                        action,
                        parameters,
                        entry.origin_tab(),
                        Some(entry.captured_targets()),
                        false,
                        cx,
                    );
                }
            }
            MenuInvocation::NeedsConfirmation(pending) => {
                if pending.command_id() == "trash.empty" {
                    self.pending_empty_trash = Some(MenuInvocation::NeedsConfirmation(pending));
                    cx.notify();
                } else {
                    self.open_context_review(MenuInvocation::NeedsConfirmation(pending), cx);
                }
            }
            MenuInvocation::NeedsDestinationChooser(pending) => {
                self.open_context_destination_chooser(pending, cx);
            }
            MenuInvocation::Cancelled | MenuInvocation::Rejected(_) => {}
        }
    }

    fn resolve_context_destination(
        &mut self,
        pending: PendingInvocation,
        destination: StorePath,
        cx: &mut Context<Self>,
    ) {
        let action = match pending.command_id() {
            "clipboard.copy_to" => DropAction::Copy,
            "clipboard.move_to" => DropAction::Move,
            unsupported => {
                self.operation_error = Some(
                    format!(
                        "{}: {unsupported}",
                        self.catalog
                            .message("context.workflow-unavailable")
                            .expect("workflow refusal is localized")
                    )
                    .into(),
                );
                cx.notify();
                return;
            }
        };
        let sources = pending
            .selection()
            .iter()
            .map(|target| target.path().clone())
            .collect();
        let payload = match FileDragPayload::new(sources, action) {
            Ok(payload) => payload,
            Err(error) => {
                self.operation_error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let resolver = ContextTransferDestinationResolver {
            operation_hub: &self.operation_hub,
            payload: &payload,
            catalog: &self.catalog,
        };
        let surface = self.shell.context_menus().clone();
        let mut dispatcher = AppMenuDispatcher::default();
        let origin_tab = pending.origin_tab();
        let captured_targets = pending.selection().to_vec();
        match surface.resolve_destination(pending, destination, &resolver, &mut dispatcher) {
            MenuInvocation::Dispatched => {
                if let Some((action, parameters)) = dispatcher.dispatched {
                    self.dispatch_typed_context_command(
                        action,
                        parameters,
                        origin_tab,
                        Some(&captured_targets),
                        false,
                        cx,
                    );
                }
            }
            MenuInvocation::NeedsConfirmation(pending) => {
                self.open_context_review(MenuInvocation::NeedsConfirmation(pending), cx);
            }
            MenuInvocation::Rejected(error) => {
                self.operation_error = Some(error.to_string().into());
                cx.notify();
            }
            MenuInvocation::Cancelled | MenuInvocation::NeedsDestinationChooser(_) => {}
        }
    }

    fn cancel_context_destination(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    fn browser_input_blocked(&self) -> bool {
        !self.context_dialog_windows.is_empty()
    }

    fn activate_context_dialog(&self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.context_dialog_windows.last()
            && let Some(handle) = cx
                .windows()
                .into_iter()
                .find(|window| window.window_id() == dialog.id)
        {
            // The close observer may have been queued between the input and
            // this update; in that case the browser is about to be unblocked.
            let _ = handle.update(cx, |_, window, _| window.activate_window());
        }
    }

    fn remember_browser_focus(&mut self, window: &Window, cx: &App) {
        if !self.browser_input_blocked() {
            self.browser_focus = window.focused(cx);
        }
    }

    fn remember_context_invocation_focus(&mut self, window: &Window, cx: &App) {
        self.remember_browser_focus(window, cx);
        self.context_menu_focus = self.browser_focus.clone();
    }

    fn track_context_dialog_window(
        &mut self,
        window_id: WindowId,
        origin_tab: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        let origin_tab = origin_tab
            .or_else(|| {
                self.context_dialog_windows
                    .first()
                    .map(|dialog| dialog.origin_tab)
            })
            .unwrap_or_else(|| self.navigation.focused_tab().id());
        let focused_item = self
            .context_dialog_windows
            .first()
            .filter(|dialog| dialog.origin_tab == origin_tab)
            .map(|dialog| dialog.focused_item.clone())
            .unwrap_or_else(|| {
                self.directories
                    .get(&origin_tab)
                    .and_then(|directory| directory.view().focused_item_id())
                    .cloned()
            });
        self.context_dialog_windows.push(ContextDialogWindow {
            id: window_id,
            origin_tab,
            focused_item,
            restore_focus: self
                .context_dialog_windows
                .first()
                .map(|dialog| dialog.restore_focus.clone())
                .or_else(|| self.context_menu_focus.take())
                .or_else(|| self.browser_focus.clone())
                .unwrap_or_else(|| self.content_focus.clone()),
        });
        if self.context_dialog_close_subscription.is_some() {
            return;
        }
        let app = cx.entity().downgrade();
        let subscription = cx.on_window_closed(move |cx, closed| {
            let app = app.clone();
            // A dialog emits its decision before removing its window. Let
            // that queued event advance the workflow before cancelling a
            // still-pending conflict as a window-manager close.
            cx.defer(move |cx| {
                let _ = app.update(cx, |this, cx| {
                    this.close_context_dialog_window(closed, cx);
                });
            });
        });
        self.context_dialog_close_subscription = Some(subscription);
    }

    fn close_context_dialog_window(&mut self, closed: WindowId, cx: &mut Context<Self>) {
        let Some(index) = self
            .context_dialog_windows
            .iter()
            .position(|dialog| dialog.id == closed)
        else {
            return;
        };
        let dialog = self.context_dialog_windows.remove(index);
        self.pending_restores.remove(&closed);
        if self
            .pending_drop
            .as_ref()
            .is_some_and(|pending| pending.conflict_window == Some(closed))
        {
            self.pending_drop = None;
        }
        // An unresolved window-manager close cancels only its own workflow.
        if self.context_dialog_windows.is_empty() {
            self.pending_content_focus = false;
            self.pending_restored_focus = Some(dialog.restore_focus);
            self.focus_directory_item(dialog.origin_tab, dialog.focused_item, cx);
        }
        cx.notify();
    }

    fn context_destination_choices(&self, tab_id: TabId) -> Vec<ContextDestinationChoice> {
        self.sidebars
            .get(&tab_id)
            .map(|sidebar| {
                sidebar
                    .sections()
                    .into_iter()
                    .flat_map(|section| section.items().to_vec())
                    .map(|entry| ContextDestinationChoice {
                        label: entry.label().to_owned(),
                        location: entry.location().clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn open_context_destination_chooser(
        &mut self,
        pending: PendingInvocation,
        cx: &mut Context<Self>,
    ) {
        let tab_id = pending
            .origin_tab()
            .unwrap_or_else(|| self.navigation.focused_tab().id());
        let choices = self.context_destination_choices(tab_id);
        let strings = ContextDialogStrings::from_catalog(&self.catalog);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(520.), px(540.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(strings.choose_destination.clone())),
                ..TitlebarOptions::default()
            }),
            window_min_size: Some(size(px(440.), px(300.))),
            ..WindowOptions::default()
        };
        let mut dialog = None;
        let dialog_window = cx
            .open_window(options, |window, cx| {
                let view = cx.new(|cx| ContextDestinationDialog::new(choices, strings, window, cx));
                dialog = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Musheen could not open a context destination dialog");
        self.track_context_dialog_window(dialog_window.window_id(), pending.origin_tab(), cx);
        let dialog = dialog.expect("the destination dialog constructs its view");
        let subscription = cx.subscribe(&dialog, move |this, _, event, cx| match event {
            ContextDestinationEvent::Chosen(destination) => {
                this.resolve_context_destination(pending.clone(), destination.clone(), cx);
            }
            ContextDestinationEvent::Cancelled => this.cancel_context_destination(cx),
        });
        self.conflict_subscriptions.push(subscription);
    }

    fn open_context_review(&mut self, invocation: MenuInvocation, cx: &mut Context<Self>) {
        let MenuInvocation::NeedsConfirmation(pending) = &invocation else {
            return;
        };
        let move_operation = pending.command_id() == "clipboard.move_to";
        let command = self
            .custom_action_review_label(pending.custom_action_id())
            .unwrap_or_else(|| {
                self.shell
                    .commands()
                    .get(pending.command_id())
                    .and_then(|command| self.catalog.message(command.label_key()).ok())
                    .unwrap_or(pending.command_id())
                    .to_owned()
            });
        let targets = pending
            .selection()
            .iter()
            .map(|target| {
                DisplayPath::from_store_path(target.path())
                    .as_str()
                    .to_owned()
            })
            .collect();
        let strings = ContextDialogStrings::from_catalog(&self.catalog);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(580.), px(360.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some(SharedString::from(strings.review_operation.clone())),
                ..TitlebarOptions::default()
            }),
            window_min_size: Some(size(px(480.), px(280.))),
            ..WindowOptions::default()
        };
        let mut dialog = None;
        let dialog_window = cx
            .open_window(options, |window, cx| {
                let view = cx.new(|cx| {
                    ContextReviewDialog::new(command, move_operation, targets, strings, cx)
                });
                dialog = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Musheen could not open a context review dialog");
        self.track_context_dialog_window(dialog_window.window_id(), pending.origin_tab(), cx);
        let dialog = dialog.expect("the context review dialog constructs its view");
        let subscription = cx.subscribe(&dialog, move |this, _, event, cx| match event {
            ContextReviewEvent::Confirmed => this.confirm_context_review(invocation.clone(), cx),
            ContextReviewEvent::Cancelled => {
                cx.notify();
            }
        });
        self.conflict_subscriptions.push(subscription);
    }

    fn confirm_context_review(&mut self, invocation: MenuInvocation, cx: &mut Context<Self>) {
        let surface = self.shell.context_menus().clone();
        let mut dispatcher = AppMenuDispatcher::default();
        let origin_tab = match &invocation {
            MenuInvocation::NeedsConfirmation(pending) => pending.origin_tab(),
            _ => None,
        };
        // The review consumes its invocation while constructing the dispatch.
        // Retain the immutable capture so a reviewed directory action cannot
        // fall back to whatever happens to be focused when it is confirmed.
        let captured_targets = match &invocation {
            MenuInvocation::NeedsConfirmation(pending) => Some(pending.selection().to_vec()),
            _ => None,
        };
        match surface.confirm(invocation, &mut dispatcher) {
            Ok(()) => {
                if let Some((action, parameters)) = dispatcher.dispatched {
                    self.dispatch_typed_context_command(
                        action,
                        parameters,
                        origin_tab,
                        captured_targets.as_deref(),
                        true,
                        cx,
                    );
                }
            }
            Err(error) => {
                self.operation_error = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    fn dispatch_typed_context_command(
        &mut self,
        action: CommandAction,
        parameters: CommandParameters,
        origin_tab: Option<TabId>,
        captured_targets: Option<&[CommandTargetRef]>,
        confirmed: bool,
        cx: &mut Context<Self>,
    ) {
        match (&action, &parameters) {
            (CommandAction::CustomAction, CommandParameters::CustomAction { .. }) => {
                self.run_custom_action(parameters, origin_tab, confirmed, cx);
            }
            (CommandAction::Restore, CommandParameters::Targets(targets)) => {
                if let Some(tab_id) = origin_tab
                    && targets.len() == 1
                    && let Some(items) = self.trash_items_for_targets(tab_id, targets)
                {
                    let item = &items[0];
                    self.restore_trash_item(tab_id, item.receipt().clone(), item.kind(), cx);
                } else {
                    self.operation_error = Some(
                        self.catalog
                            .message("context.target-changed")
                            .expect("target refusal is localized")
                            .into(),
                    );
                    cx.notify();
                }
            }
            (CommandAction::EmptyTrash, CommandParameters::None) => {
                if let Some(tab_id) = origin_tab
                    && let Some(targets) = captured_targets
                    && let Some(items) = self.trash_items_for_targets(tab_id, targets)
                {
                    self.empty_trash(
                        tab_id,
                        items.iter().map(|item| item.receipt().clone()).collect(),
                        cx,
                    );
                } else {
                    self.operation_error = Some(
                        self.catalog
                            .message("context.target-changed")
                            .expect("target refusal is localized")
                            .into(),
                    );
                    cx.notify();
                }
            }
            (
                CommandAction::CopyTo | CommandAction::MoveTo,
                CommandParameters::Destination {
                    targets,
                    destination,
                },
            ) => {
                if let Err(error) = self.revalidate_context_targets(origin_tab, targets) {
                    self.operation_error = Some(error);
                    cx.notify();
                    return;
                }
                let drop_action = if action == CommandAction::CopyTo {
                    DropAction::Copy
                } else {
                    DropAction::Move
                };
                match FileDragPayload::with_expected_identities(
                    targets.iter().map(|target| target.path().clone()).collect(),
                    targets.iter().map(|target| target.id().clone()).collect(),
                    drop_action,
                ) {
                    Ok(payload) => self.submit_reviewed_transfer(payload, destination.clone(), cx),
                    Err(error) => {
                        self.operation_error = Some(error.to_string().into());
                        cx.notify();
                    }
                }
            }
            (CommandAction::OpenProperties, CommandParameters::Targets(targets)) => {
                if let Err(error) = self.revalidate_context_targets(origin_tab, targets) {
                    self.operation_error = Some(error);
                    cx.notify();
                    return;
                }
                self.open_properties_targets(targets, PropertiesPage::General, cx);
            }
            (CommandAction::Permissions, CommandParameters::Targets(targets)) => {
                if let Err(error) = self.revalidate_context_targets(origin_tab, targets) {
                    self.operation_error = Some(error);
                    cx.notify();
                    return;
                }
                self.open_properties_targets(targets, PropertiesPage::Permissions, cx);
            }
            (CommandAction::DirectoryProperties, CommandParameters::Location(location)) => {
                if let Some(targets) = captured_targets
                    && let Err(error) = self.revalidate_context_targets(origin_tab, targets)
                {
                    self.operation_error = Some(error);
                    cx.notify();
                    return;
                }
                self.open_properties_paths(
                    location
                        .as_unix_path()
                        .map(Path::to_path_buf)
                        .into_iter()
                        .collect(),
                    PropertiesPage::General,
                    cx,
                );
            }
            (CommandAction::SelectAll, CommandParameters::None) => {
                self.dispatch_selection_action_for_tab(origin_tab, CommandAction::SelectAll, cx);
            }
            (
                CommandAction::ViewDetails
                | CommandAction::ViewList
                | CommandAction::ViewCards
                | CommandAction::ViewGrid
                | CommandAction::ViewColumns
                | CommandAction::ViewAdaptive
                | CommandAction::CycleSort
                | CommandAction::CycleGroup
                | CommandAction::ToggleDirectoriesFirst
                | CommandAction::ToggleHidden
                | CommandAction::ToggleSidebar
                | CommandAction::ToggleInfo,
                CommandParameters::None,
            ) => self.dispatch_context_view_action(origin_tab, action, cx),
            _ => {
                self.operation_error = Some(
                    self.catalog
                        .message("context.backend-unavailable")
                        .expect("backend refusal is localized")
                        .into(),
                );
                cx.notify();
            }
        }
    }

    fn revalidate_context_targets(
        &self,
        origin_tab: Option<TabId>,
        targets: &[CommandTargetRef],
    ) -> Result<(), Box<str>> {
        let Some(tab_id) = origin_tab else {
            return Ok(());
        };
        let Some(directory) = self.directories.get(&tab_id) else {
            return Err(self
                .catalog
                .message("context.origin-unavailable")
                .expect("origin refusal is localized")
                .into());
        };
        targets.iter().try_for_each(|target| {
            let cached = directory.view().item(target.id());
            if cached.is_some_and(|item| item.path() != target.path()) {
                return Err(self
                    .catalog
                    .message("context.target-changed")
                    .expect("target refusal is localized")
                    .into());
            }
            let current = self.store.resolve_item(target.path()).map_err(|error| {
                Box::<str>::from(format!(
                    "{}: {error}",
                    self.catalog
                        .message("context.target-verification")
                        .expect("verification refusal is localized")
                ))
            })?;
            match current {
                Some(item) if item.id() == target.id() && item.path() == target.path() => Ok(()),
                _ => Err(self
                    .catalog
                    .message("context.target-changed")
                    .expect("target refusal is localized")
                    .into()),
            }
        })
    }

    fn backend_action_state(action: CommandAction) -> CapabilityState {
        if matches!(
            action,
            CommandAction::OpenSettings
                | CommandAction::NavigateBack
                | CommandAction::NavigateForward
                | CommandAction::NavigateParent
                | CommandAction::Refresh
                | CommandAction::FocusLocation
                | CommandAction::Search
                | CommandAction::Filter
                | CommandAction::FocusCommand
                | CommandAction::NewTab
                | CommandAction::CloseTab
                | CommandAction::DuplicateTab
                | CommandAction::ReopenClosedTab
                | CommandAction::MoveTabOtherPane
                | CommandAction::MoveTabLeft
                | CommandAction::MoveTabRight
                | CommandAction::TearOutTab
                | CommandAction::SplitPane
                | CommandAction::FocusNextPane
                | CommandAction::ClearSelection
                | CommandAction::SelectAll
                | CommandAction::ViewDetails
                | CommandAction::ViewList
                | CommandAction::ViewCards
                | CommandAction::ViewGrid
                | CommandAction::ViewColumns
                | CommandAction::ViewAdaptive
                | CommandAction::CycleSort
                | CommandAction::CycleGroup
                | CommandAction::ToggleDirectoriesFirst
                | CommandAction::ToggleHidden
                | CommandAction::ToggleSidebar
                | CommandAction::ToggleInfo
                | CommandAction::OpenProperties
                | CommandAction::Permissions
                | CommandAction::DirectoryProperties
                | CommandAction::CopyTo
                | CommandAction::MoveTo
                | CommandAction::Restore
                | CommandAction::EmptyTrash
                | CommandAction::CustomAction
        ) {
            return CapabilityState::Supported;
        }
        let reason = match action {
            CommandAction::Extract | CommandAction::ExtractHere => {
                "Archive extraction is unavailable because no archive operation provider is installed"
            }
            CommandAction::OpenAsAdministrator | CommandAction::RunAsAdministrator => {
                "Privilege elevation is unavailable because no authorization broker is installed"
            }
            CommandAction::OpenWith
            | CommandAction::ChooseApplication
            | CommandAction::SetDefaultApplication => {
                "Desktop application association is unavailable because no association backend is installed"
            }
            _ => "This command is not available in the current desktop backend",
        };
        CapabilityState::Unsupported(
            CapabilityReason::new(reason).expect("the desktop-backend reason is valid"),
        )
    }

    fn context_backend_action_state(
        &self,
        action: CommandAction,
        tab_id: TabId,
        target: CommandTarget,
        selection: &[CommandTargetRef],
    ) -> CapabilityState {
        // Trash renders receipts in deletion order, independently of the
        // directory view. Do not advertise preferences it cannot render.
        if self
            .navigation
            .tab(tab_id)
            .is_some_and(|tab| is_trash_location(tab.location()))
            && matches!(
                action,
                CommandAction::ViewDetails
                    | CommandAction::ViewList
                    | CommandAction::ViewCards
                    | CommandAction::ViewGrid
                    | CommandAction::ViewColumns
                    | CommandAction::ViewAdaptive
                    | CommandAction::CycleSort
                    | CommandAction::CycleGroup
                    | CommandAction::ToggleDirectoriesFirst
                    | CommandAction::ToggleHidden
            )
        {
            return CapabilityState::Unsupported(
                CapabilityReason::new(
                    self.catalog
                        .message("context.backend-unavailable")
                        .expect("backend refusal is localized"),
                )
                .expect("backend refusal is nonempty"),
            );
        }
        let target_matches = match action {
            CommandAction::Restore => target == CommandTarget::TrashItem && selection.len() == 1,
            CommandAction::EmptyTrash => target == CommandTarget::TrashBackground,
            _ => return self.localized_backend_action_state(action),
        };
        if target_matches && self.trash_items_for_targets(tab_id, selection).is_some() {
            self.localized_backend_action_state(action)
        } else {
            CapabilityState::Unsupported(
                CapabilityReason::new(
                    self.catalog
                        .message("context.target-changed")
                        .expect("target refusal is localized"),
                )
                .expect("target refusal is nonempty"),
            )
        }
    }

    fn localized_backend_action_state(&self, action: CommandAction) -> CapabilityState {
        match Self::backend_action_state(action) {
            CapabilityState::Supported => CapabilityState::Supported,
            CapabilityState::Unsupported(reason) | CapabilityState::Unknown(reason) => {
                CapabilityState::Unsupported(
                    CapabilityReason::new(self.catalog.localize_reason(reason.as_str()))
                        .expect("localized refusal is nonempty"),
                )
            }
        }
    }

    fn drag_payload(&self, spec: &ItemRenderSpec) -> Option<FileDragPayload> {
        if self.browser_input_blocked() {
            return None;
        }
        let view = self.directories.get(&spec.tab_id)?.view();
        let sources = if spec.selected {
            view.selected_ids()
                .iter()
                .filter_map(|id| view.item(id))
                .map(|item| item.path().clone())
                .collect()
        } else {
            vec![spec.path.clone()]
        };
        FileDragPayload::new(sources, DropAction::Move).ok()
    }

    fn submit_file_drop(
        &mut self,
        payload: FileDragPayload,
        target: StorePath,
        cx: &mut Context<Self>,
    ) {
        if !self.context_dialog_windows.is_empty() {
            return;
        }
        self.submit_reviewed_transfer(payload, target, cx);
    }

    fn submit_reviewed_transfer(
        &mut self,
        payload: FileDragPayload,
        target: StorePath,
        cx: &mut Context<Self>,
    ) {
        if self.pending_drop.is_some() {
            return;
        }
        let conflicts = match self.operation_hub.conflicts_for_drop(&payload, &target) {
            Ok(conflicts) => conflicts,
            Err(error) => {
                self.operation_error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        if !conflicts.is_empty() {
            self.pending_drop = Some(PendingDrop {
                conflict_window: None,
                conflict_dialog: None,
                payload,
                target,
                conflicts,
                next_conflict: 0,
                decisions: Vec::new(),
                policies: ConflictPolicies::default(),
                automatic_scope: false,
            });
            self.advance_pending_drop(cx);
            return;
        }
        let submitted = self
            .operation_hub
            .submit_drop(payload, target)
            .map_err(|error| Box::<str>::from(error.to_string()));
        self.finish_drop_submission(submitted, cx);
    }

    fn finish_drop_submission(
        &mut self,
        submitted: Result<Vec<musheen_ops::JobId>, Box<str>>,
        cx: &mut Context<Self>,
    ) {
        match submitted {
            Ok(_) => {
                self.operation_error = self.operation_hub.persistence_error();
                self.pump_operation_queue(cx);
            }
            Err(error) => {
                self.operation_error = Some(error);
                cx.notify();
            }
        }
    }

    fn advance_pending_drop(&mut self, cx: &mut Context<Self>) {
        loop {
            let Some(pending) = self.pending_drop.as_ref() else {
                return;
            };
            if pending.next_conflict >= pending.conflicts.len() {
                let pending = self
                    .pending_drop
                    .take()
                    .expect("the completed drop remains pending");
                let submitted = self
                    .operation_hub
                    .submit_drop_resolved(pending.payload, pending.target, pending.decisions)
                    .map_err(|error| Box::<str>::from(error.to_string()));
                self.finish_drop_submission(submitted, cx);
                return;
            }
            let conflict = pending.conflicts[pending.next_conflict].clone();
            if pending.automatic_scope {
                match self.resolve_saved_drop_decision(&conflict) {
                    Ok(Some(decision)) => {
                        let pending = self
                            .pending_drop
                            .as_mut()
                            .expect("the drop remains pending while applying a policy");
                        pending.decisions.push(decision);
                        pending.next_conflict += 1;
                        continue;
                    }
                    Ok(None) => {}
                    Err(error) => {
                        self.pending_drop = None;
                        self.operation_error = Some(error);
                        cx.notify();
                        return;
                    }
                }
            }
            self.open_drop_conflict(conflict, cx);
            return;
        }
    }

    fn resolve_saved_drop_decision(
        &mut self,
        conflict: &ConflictRecord,
    ) -> Result<Option<ConflictDecision>, Box<str>> {
        let mut store = LocalStore::new();
        let source = MutationProvider::identity(&mut store, conflict.source())
            .map_err(|error| Box::<str>::from(error.to_string()))?
            .ok_or_else(|| Box::<str>::from("the conflict source no longer exists"))?;
        let destination = MutationProvider::identity(&mut store, conflict.destination())
            .map_err(|error| Box::<str>::from(error.to_string()))?
            .ok_or_else(|| Box::<str>::from("the conflict destination no longer exists"))?;
        let mut journal = ConflictDecisionStore::for_current_user()
            .map_err(|error| Box::<str>::from(error.to_string()))?;
        self.pending_drop
            .as_mut()
            .expect("a saved decision is resolved only for a pending drop")
            .policies
            .resolve_saved_decision(conflict, &source, &destination, &mut journal)
            .map_err(|error| error.to_string().into())
    }

    fn open_drop_conflict(&mut self, conflict: ConflictRecord, cx: &mut Context<Self>) {
        let options = conflict_window_options(cx);
        let model = ConflictDialogModel::new(conflict);
        let mut dialog = None;
        let dialog_window = cx
            .open_window(options, |window, cx| {
                let view = cx.new(|cx| ConflictDialog::new(model, cx));
                dialog = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Musheen could not open a conflict dialog");
        let window_id = dialog_window.window_id();
        let dialog = dialog.expect("the conflict window constructs its view");
        let pending = self
            .pending_drop
            .as_mut()
            .expect("a conflict dialog belongs to a pending drop");
        pending.conflict_window = Some(window_id);
        pending.conflict_dialog = Some(dialog.clone());
        self.track_context_dialog_window(window_id, None, cx);
        let subscription = cx.subscribe(&dialog, move |this, _, event, cx| {
            this.handle_drop_conflict_event(window_id, event, cx);
        });
        self.conflict_subscriptions.push(subscription);
    }

    fn handle_drop_conflict_event(
        &mut self,
        window_id: WindowId,
        event: &ConflictDialogEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_drop.as_mut() else {
            return;
        };
        if pending.conflict_window != Some(window_id) {
            return;
        }
        // Closing this resolved window must not cancel the next conflict.
        pending.conflict_window = None;
        pending.conflict_dialog = None;
        match event {
            ConflictDialogEvent::Resolved(choice, scope) => {
                self.resolve_pending_drop_conflict(*choice, *scope, cx);
            }
            ConflictDialogEvent::Cancelled => {
                self.pending_drop = None;
                cx.notify();
            }
        }
    }

    fn resolve_pending_drop_conflict(
        &mut self,
        choice: ConflictChoice,
        scope: ApplyScope,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_drop.as_mut() else {
            return;
        };
        let Some(conflict) = pending.conflicts.get(pending.next_conflict) else {
            return;
        };
        let decision = ConflictDecisionStore::for_current_user()
            .map_err(|error| error.to_string())
            .and_then(|mut journal| {
                pending
                    .policies
                    .decide(conflict, choice, scope, &mut journal)
                    .map_err(|error| error.to_string())
            });
        match decision {
            Ok(decision) => {
                pending.decisions.push(decision);
                pending.next_conflict += 1;
                pending.automatic_scope |= scope == ApplyScope::CompatibleRemaining;
                self.advance_pending_drop(cx);
            }
            Err(error) => {
                self.pending_drop = None;
                self.operation_error = Some(error.into());
                cx.notify();
            }
        }
    }

    fn pump_operation_queue(&mut self, cx: &mut Context<Self>) {
        let result = spawn_ready_hub_operations(
            self.operation_hub.clone(),
            cx,
            |state: &mut Self, _, succeeded, error, cx| {
                if let Some(error) = error {
                    state.operation_error = Some(error);
                }
                if succeeded {
                    // A background completion refreshes the current tab; it
                    // must not steal focus restored when a review closes.
                    let pending_content_focus = state.pending_content_focus;
                    state.load_focused_tab(cx);
                    state.pending_content_focus = pending_content_focus;
                }
                state.pump_operation_queue(cx);
                cx.notify();
            },
        );
        if let Err(error) = result {
            self.operation_error = Some(error.to_string().into());
            cx.notify();
        }
    }

    fn refresh_info_pane(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let selected = self
            .directories
            .get(&tab_id)
            .map(|directory| directory.view().selected_ids().to_vec())
            .unwrap_or_default();
        match selected.as_slice() {
            [] => self.info_panes.entry(tab_id).or_default().clear(),
            [id] => {
                let item = self
                    .directories
                    .get(&tab_id)
                    .and_then(|directory| directory.view().item(id))
                    .cloned();
                let Some(item) = item else {
                    self.info_panes.entry(tab_id).or_default().clear();
                    return;
                };
                let details = InfoPaneDetails::new(
                    item.display_name().as_str(),
                    item.kind(),
                    item.size(),
                    item.modified_unix_seconds(),
                );
                let Some(path) = item.path().as_unix_path().map(Path::to_path_buf) else {
                    let model = self.info_panes.entry(tab_id).or_default();
                    let work = model.begin(details, PathBuf::new());
                    model.complete(
                        work.generation(),
                        InfoPaneResult::Details {
                            mime_type: "application/octet-stream".into(),
                        },
                    );
                    return;
                };
                let work = self
                    .info_panes
                    .entry(tab_id)
                    .or_default()
                    .begin(details, path);
                self.start_info_work(tab_id, work, cx);
            }
            _ => self
                .info_panes
                .entry(tab_id)
                .or_default()
                .show_multiple(selected.len()),
        }
    }

    fn start_info_work(&mut self, tab_id: TabId, work: InfoPaneWork, cx: &mut Context<Self>) {
        let generation = work.generation();
        let task = cx.background_spawn(async move { load_info_pane(work) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                let model = state.info_panes.entry(tab_id).or_default();
                let changed = match result {
                    Ok(result) => model.complete(generation, result),
                    Err(message) => model.fail(generation, message),
                };
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn retry_info_pane(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let work = self.info_panes.entry(tab_id).or_default().retry();
        if let Some(work) = work {
            self.start_info_work(tab_id, work, cx);
            cx.notify();
        }
    }

    fn load_more_info_pane(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
        let Some(mut work) = self.info_panes.entry(tab_id).or_default().begin_load_more() else {
            return;
        };
        let generation = work.generation();
        let cancellation = work.cancellation().clone();
        let task = cx.background_spawn(async move {
            work.document_mut()
                .load_more(cancellation)
                .map_err(|error| error.to_string().into_boxed_str())?;
            let (mime_type, document) = work.into_result_parts();
            Ok::<_, Box<str>>(InfoPaneResult::Preview {
                mime_type,
                document,
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                let model = state.info_panes.entry(tab_id).or_default();
                let changed = match result {
                    Ok(result) => model.complete(generation, result),
                    Err(message) => model.fail(generation, message),
                };
                if changed {
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn dispatch_tab_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        match action {
            CommandAction::NewTab => {
                let location = self.navigation.focused_tab().location().clone();
                if self.navigation.new_tab(location).is_ok() {
                    self.finish_navigation_change(cx);
                }
            }
            CommandAction::CloseTab if self.navigation.close_active_tab().is_ok() => {
                self.finish_navigation_change(cx);
            }
            CommandAction::DuplicateTab if self.navigation.duplicate_active_tab().is_ok() => {
                self.finish_navigation_change(cx);
            }
            CommandAction::ReopenClosedTab if self.navigation.reopen_closed_tab().is_ok() => {
                self.finish_navigation_change(cx);
            }
            CommandAction::MoveTabOtherPane => {
                self.move_active_tab_to_other_pane(cx);
            }
            CommandAction::MoveTabLeft => {
                self.reorder_active_tab(-1, cx);
            }
            CommandAction::MoveTabRight => {
                self.reorder_active_tab(1, cx);
            }
            CommandAction::TearOutTab => {
                self.tear_out_active_tab(cx);
            }
            _ => {}
        }
    }

    fn dispatch_pane_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        match action {
            CommandAction::SplitPane => {
                let location = self.navigation.focused_tab().location().clone();
                if self.navigation.split_focused(location).is_ok() {
                    self.finish_navigation_change(cx);
                }
            }
            CommandAction::FocusNextPane => {
                let panes = self.navigation.panes();
                if panes.len() > 1 {
                    let current = self.navigation.focused_pane_id();
                    let index = panes
                        .iter()
                        .position(|pane| pane.id() == current)
                        .unwrap_or(0);
                    let next = panes[(index + 1) % panes.len()].id();
                    if self.navigation.focus_pane(next).is_ok() {
                        self.finish_navigation_change(cx);
                    }
                }
            }
            _ => {}
        }
    }

    fn finish_navigation_change(&mut self, cx: &mut Context<Self>) {
        self.schedule_session_save(cx);
        self.load_focused_tab(cx);
    }

    fn reorder_active_tab(&mut self, offset: isize, cx: &mut Context<Self>) {
        let pane = self.navigation.focused_pane();
        let active = pane.active_tab().id();
        let Some(index) = pane.tabs().iter().position(|tab| tab.id() == active) else {
            return;
        };
        let destination = index.saturating_add_signed(offset);
        if destination < pane.tabs().len()
            && self.navigation.reorder_active_tab(destination).is_ok()
        {
            self.schedule_session_save(cx);
            cx.notify();
        }
    }

    fn move_active_tab_to_other_pane(&mut self, cx: &mut Context<Self>) {
        if self.navigation.panes().len() != 2 {
            return;
        }
        let source = self.navigation.focused_pane_id();
        let destination = self
            .navigation
            .panes()
            .iter()
            .find(|pane| pane.id() != source)
            .map(|pane| pane.id())
            .expect("a two-pane window has another pane");
        let old_id = self.navigation.focused_tab().id();
        if let Ok(new_id) = self.navigation.move_active_tab_to(destination) {
            if let Some(directory) = self.directories.remove(&old_id) {
                self.directories.insert(new_id, directory);
            }
            self.schedule_session_save(cx);
            self.load_focused_tab(cx);
        }
    }

    fn tear_out_active_tab(&mut self, cx: &mut Context<Self>) {
        let Some(source_binding) = self.session_binding.clone() else {
            return;
        };
        if !source_binding.can_append_window() {
            return;
        }
        let mut retained = self.navigation.clone();
        let Ok(detached) = retained.tear_out_active_tab() else {
            return;
        };
        let Ok(binding) = source_binding.append_window(detached.clone()) else {
            return;
        };
        self.navigation = retained;
        self.schedule_session_save(cx);
        self.load_focused_tab(cx);
        cx.spawn(async move |_, cx| {
            cx.open_window(detached_window_options(), move |window, cx| {
                let view = cx.new(|cx| {
                    MusheenApp::new_with_navigation(
                        detached,
                        Some(binding),
                        ResourceLimits::default(),
                        true,
                        cx,
                    )
                });
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Musheen could not open a detached tab window");
        })
        .detach();
    }

    fn ensure_omnibar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.omnibar_input.is_some() {
            return;
        }

        let value = self.current_location_text().to_string();
        self.omnibar.enter(OmnibarMode::Path, value.clone());
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(value)
                .placeholder("Enter a path")
        });
        let subscription = cx.subscribe(&input, |this, input, event: &InputEvent, cx| {
            let value = input.read(cx).value().to_string();
            this.omnibar.enter(this.omnibar.mode(), value);
            if matches!(event, InputEvent::PressEnter { .. }) {
                this.submit_omnibar(cx);
            } else if this.omnibar.mode() == OmnibarMode::Filter {
                this.apply_filter(this.omnibar.text().to_owned(), cx);
            } else {
                cx.notify();
            }
        });
        self.omnibar_input = Some(input);
        self.omnibar_subscription = Some(subscription);
    }

    fn activate_omnibar(&mut self, mode: OmnibarMode, window: &mut Window, cx: &mut Context<Self>) {
        let value = match mode {
            OmnibarMode::Path => self.current_location_text().to_string(),
            OmnibarMode::Search | OmnibarMode::Filter | OmnibarMode::Command => String::new(),
        };
        let placeholder = match mode {
            OmnibarMode::Path => "Enter a path",
            OmnibarMode::Search => "Search this location",
            OmnibarMode::Filter => "Filter loaded items",
            OmnibarMode::Command => "Run a command",
        };
        self.omnibar.enter(mode, value.clone());
        if let Some(input) = self.omnibar_input.as_ref() {
            input.update(cx, |input, cx| {
                input.set_placeholder(placeholder, window, cx);
                input.set_value(value, window, cx);
                input.focus(window, cx);
                input.select_all(window, cx);
            });
        }
    }

    fn submit_omnibar(&mut self, cx: &mut Context<Self>) {
        match self.omnibar.submit() {
            OmnibarSubmission::Path(value) => {
                if let Some(location) =
                    resolve_path_input(self.navigation.focused_tab().location(), &value)
                {
                    self.navigate(location, true, cx);
                }
            }
            OmnibarSubmission::Search(expression) => {
                self.start_search(expression, cx);
                self.pending_content_focus = true;
                cx.notify();
            }
            OmnibarSubmission::Filter(expression) => {
                self.apply_filter(expression, cx);
                self.pending_content_focus = true;
                cx.notify();
            }
            OmnibarSubmission::Command(value) => {
                let query = value.trim();
                let command_id = self
                    .shell
                    .commands()
                    .get(query)
                    .map(|command| command.id().as_str().to_owned())
                    .or_else(|| {
                        self.shell
                            .commands()
                            .commands()
                            .iter()
                            .find(|command| {
                                command
                                    .label_key()
                                    .rsplit('.')
                                    .next()
                                    .is_some_and(|label| label == query)
                            })
                            .map(|command| command.id().as_str().to_owned())
                    });
                if let Some(command_id) = command_id {
                    self.dispatch_command(&command_id, cx);
                    if self.requested_omnibar_mode.is_none() {
                        self.pending_content_focus = true;
                    }
                }
            }
        }
    }

    fn start_search(&mut self, expression: String, cx: &mut Context<Self>) {
        let tab_id = self.navigation.focused_tab().id();
        self.cancel_search(tab_id);
        self.filters.remove(&tab_id);
        let scope = self.navigation.focused_tab().location().clone();
        let include_hidden = self.focused_directory().view().preferences().show_hidden;
        let query = match SearchQuery::parse(&expression)
            .map(|query| query.with_default_hidden_policy(include_hidden))
            .and_then(|query| {
                self.store
                    .search_capabilities(&scope)
                    .validate(&query)
                    .map(|()| query)
            }) {
            Ok(query) => query,
            Err(error) => {
                let query = SearchQuery::parse("name:__invalid_search__")
                    .expect("the static fallback search is valid")
                    .with_default_hidden_policy(include_hidden);
                let mut model = SearchResultModel::new(
                    scope,
                    query,
                    SEARCH_RETAINED_RESULTS,
                    SEARCH_RESULT_LIMIT,
                );
                let generation = model.begin();
                model.fail(generation);
                self.searches.insert(
                    tab_id,
                    ActiveSearch {
                        model,
                        cancellation: CancellationToken::new(),
                        expression,
                        error: Some(error.to_string().into()),
                        retryable: false,
                    },
                );
                cx.notify();
                return;
            }
        };
        let cancellation = CancellationToken::new();
        let mut model = SearchResultModel::new(
            scope.clone(),
            query.clone(),
            SEARCH_RETAINED_RESULTS,
            SEARCH_RESULT_LIMIT,
        );
        let generation = model.begin();
        self.searches.insert(
            tab_id,
            ActiveSearch {
                model,
                cancellation: cancellation.clone(),
                expression,
                error: None,
                retryable: false,
            },
        );
        let store = Arc::clone(&self.store);
        let worker_cancellation = cancellation.clone();
        let work = cx.background_spawn(async move {
            let mut stream = store
                .search(&scope, query, worker_cancellation.clone())
                .await?;
            let batch = stream.next_batch(worker_cancellation).await?;
            Ok::<_, StoreError>((stream, batch))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                state.apply_search_step(tab_id, generation, cancellation, result, cx);
            });
        })
        .detach();
    }

    fn apply_filter(&mut self, expression: String, cx: &mut Context<Self>) {
        let tab_id = self.navigation.focused_tab().id();
        if expression.trim().is_empty() {
            self.filters.remove(&tab_id);
            cx.notify();
            return;
        }
        let active = match SearchQuery::parse(&expression).and_then(DirectoryFilter::from_query) {
            Ok(filter) => ActiveFilter {
                filter: Some(filter),
                expression,
                error: None,
            },
            Err(error) => ActiveFilter {
                filter: None,
                expression,
                error: Some(error.to_string().into()),
            },
        };
        self.filters.insert(tab_id, active);
        cx.notify();
    }

    fn continue_search(
        &mut self,
        tab_id: TabId,
        generation: SearchGeneration,
        cancellation: CancellationToken,
        mut stream: Box<dyn SearchStream>,
        cx: &mut Context<Self>,
    ) {
        let worker_cancellation = cancellation.clone();
        let work = cx.background_spawn(async move {
            let batch = stream.next_batch(worker_cancellation).await?;
            Ok::<_, StoreError>((stream, batch))
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| {
                state.apply_search_step(tab_id, generation, cancellation, result, cx);
            });
        })
        .detach();
    }

    fn apply_search_step(
        &mut self,
        tab_id: TabId,
        generation: SearchGeneration,
        cancellation: CancellationToken,
        result: Result<(Box<dyn SearchStream>, Option<SearchBatch>), StoreError>,
        cx: &mut Context<Self>,
    ) {
        let Some(search) = self.searches.get_mut(&tab_id) else {
            return;
        };
        match result {
            Ok((stream, Some(batch))) => {
                let complete = batch.completion() != SearchCompletion::Running;
                if search.model.apply(generation, batch)
                    && search.model.state() == SearchState::Running
                    && !complete
                {
                    self.continue_search(tab_id, generation, cancellation, stream, cx);
                }
            }
            Ok((_, None)) => {
                if search.model.fail(generation) {
                    search.error = Some(
                        "search provider ended before reporting completion"
                            .to_owned()
                            .into(),
                    );
                    search.retryable = true;
                }
            }
            Err(StoreError::Cancelled) => search.model.cancel(),
            Err(error) => {
                search.model.fail(generation);
                search.error = Some(error.to_string().into());
                search.retryable = true;
            }
        }
        cx.notify();
    }

    fn cancel_omnibar(&mut self, cx: &mut Context<Self>) {
        let tab_id = self.navigation.focused_tab().id();
        self.cancel_search(tab_id);
        self.filters.remove(&tab_id);
        let value = self.current_location_text().to_string();
        self.omnibar.cancel();
        self.omnibar.enter(OmnibarMode::Path, value.clone());
        self.pending_omnibar_value = Some(value);
        self.pending_content_focus = true;
        cx.notify();
    }

    fn handle_escape(&mut self, cx: &mut Context<Self>) {
        if self.keyboard_context_popup.take().is_some() {
            self.pending_content_focus = true;
            cx.notify();
        } else if self.omnibar.mode() != OmnibarMode::Path {
            self.cancel_omnibar(cx);
        } else if !self.focused_directory().view().selected_ids().is_empty() {
            self.dispatch_selection_action(CommandAction::ClearSelection, cx);
        } else {
            self.cancel_omnibar(cx);
        }
    }

    fn current_location_text(&self) -> SharedString {
        self.focused_directory()
            .location()
            .map(DisplayPath::from_store_path)
            .map(|path| SharedString::from(path.as_str().to_owned()))
            .unwrap_or_else(|| SharedString::from("Musheen"))
    }

    fn toolbar_button(&self, id: &'static str, cx: &mut Context<Self>) -> Button {
        self.named_toolbar_button(id, id.into(), cx)
    }

    fn named_toolbar_button(
        &self,
        id: &str,
        control_id: SharedString,
        cx: &mut Context<Self>,
    ) -> Button {
        let command = self
            .shell
            .commands()
            .get(id)
            .expect("toolbar command is registered");
        let state = command
            .state(&self.active_command_context(command.action()))
            .map_disabled_reason(|reason| self.catalog.localize_reason(reason));
        let label = self
            .catalog
            .message(command.label_key())
            .expect("toolbar label is localized")
            .to_owned();
        let tooltip = state
            .disabled_reason()
            .map_or_else(|| label.clone(), |reason| format!("{label}: {reason}"));
        let toggled = matches!(
            command.action(),
            CommandAction::ToggleHidden
                | CommandAction::ToggleDirectoriesFirst
                | CommandAction::ToggleSidebar
                | CommandAction::ToggleInfo
                | CommandAction::SplitPane
                | CommandAction::ViewDetails
                | CommandAction::ViewList
                | CommandAction::ViewCards
                | CommandAction::ViewGrid
                | CommandAction::ViewColumns
                | CommandAction::ViewAdaptive
        )
        .then_some(state.is_checked());
        let id = id.to_owned();
        Button::new(control_id)
            .icon(menu_icon(Some(command.icon_key())))
            .accessibility_label(label.clone())
            .tooltip(tooltip)
            .ghost()
            .small()
            .compact()
            .disabled(!state.is_enabled())
            .selected(toggled.unwrap_or(false))
            .when_some(toggled, |button, toggled| button.toggled(toggled))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.remember_context_invocation_focus(window, cx);
                this.dispatch_command(&id, cx);
            }))
    }

    fn active_command_context(&self, action: CommandAction) -> CommandContext {
        self.active_command_request(action).context().clone()
    }

    fn active_command_request(&self, action: CommandAction) -> crate::ContextMenuRequest {
        let tab = self.navigation.focused_tab();
        if is_trash_location(tab.location()) {
            let target = if action == CommandAction::EmptyTrash || tab.selection().is_empty() {
                MenuTarget::TrashBackground
            } else {
                MenuTarget::TrashItem
            };
            let selection = match self.trash_states.get(&tab.id()) {
                Some(TrashState::Ready(surface)) => surface
                    .items()
                    .iter()
                    .map(trash_command_target)
                    .filter(|target| tab.selection().contains(target.id()))
                    .collect(),
                _ => Vec::new(),
            };
            return self.context_menu_request(tab.id(), target, tab.location().clone(), selection);
        }
        let selection = self
            .directories
            .get(&tab.id())
            .map(|directory| {
                directory
                    .view()
                    .selected_ids()
                    .iter()
                    .filter_map(|id| directory.view().item(id))
                    .filter_map(|item| {
                        CommandTargetRef::new(item.id().clone(), item.path().clone()).ok()
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        self.context_menu_request(
            tab.id(),
            if selection.is_empty() {
                MenuTarget::Background
            } else {
                MenuTarget::Item
            },
            tab.location().clone(),
            selection,
        )
    }

    fn render_omnibar(&self, cx: &mut Context<Self>) -> AnyElement {
        let input = self
            .omnibar_input
            .as_ref()
            .expect("the omnibar is initialized before rendering");
        let mode = self.omnibar.mode();
        let suggestions = self
            .active_path_suggestions()
            .into_iter()
            .enumerate()
            .map(|(index, (label, target))| {
                Button::new(SharedString::from(format!("omnibar-suggestion-{index}")))
                    .label(label.clone())
                    .accessibility_label(format!("Navigate to {label}"))
                    .tooltip(label)
                    .secondary()
                    .small()
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.navigate(target.clone(), true, cx);
                    }))
            })
            .collect::<Vec<_>>();
        let breadcrumbs = BreadcrumbTrail::from_path(self.navigation.focused_tab().location(), 4);
        let hidden_crumbs = breadcrumbs
            .hidden()
            .iter()
            .enumerate()
            .map(|(index, crumb)| {
                let target = crumb.target().clone();
                Button::new(SharedString::from(format!("breadcrumb-overflow-{index}")))
                    .label("…")
                    .accessibility_label(format!("Go to {}", crumb.label().as_str()))
                    .tooltip(crumb.label().as_str().to_owned())
                    .ghost()
                    .small()
                    .compact()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.navigate(target.clone(), true, cx);
                    }))
            })
            .collect::<Vec<_>>();
        let last_crumb = breadcrumbs.visible().len().saturating_sub(1);
        let visible_crumbs = breadcrumbs
            .visible()
            .iter()
            .enumerate()
            .map(|(index, crumb)| {
                let target = crumb.target().clone();
                let id = if index == last_crumb {
                    SharedString::from("breadcrumb-current")
                } else {
                    SharedString::from(format!("breadcrumb-{index}"))
                };
                Button::new(id)
                    .label(crumb.label().as_str().to_owned())
                    .accessibility_label(format!("Go to {}", crumb.label().as_str()))
                    .tooltip(crumb.label().as_str().to_owned())
                    .ghost()
                    .small()
                    .compact()
                    .selected(index == last_crumb)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.navigate(target.clone(), true, cx);
                    }))
            })
            .collect::<Vec<_>>();
        let mode_button =
            |id: &'static str, label: &'static str, target: OmnibarMode, cx: &mut Context<Self>| {
                Button::new(id)
                    .label(label)
                    .accessibility_label(format!("{label} mode"))
                    .tooltip(format!("Use {label} mode"))
                    .ghost()
                    .small()
                    .compact()
                    .selected(mode == target)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.requested_omnibar_mode = Some(target);
                        cx.notify();
                    }))
            };

        div()
            .id("omnibar")
            .test_support()
            .min_w(px(180.))
            .flex_grow(1.0)
            .flex_shrink_1()
            .flex()
            .items_center()
            .gap_1()
            .child(
                div()
                    .id("breadcrumbs")
                    .test_support()
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap_1()
                    .flex_shrink_1()
                    .overflow_hidden()
                    .children(hidden_crumbs)
                    .children(visible_crumbs),
            )
            .child(mode_button("omnibar-path", "Path", OmnibarMode::Path, cx))
            .child(mode_button(
                "omnibar-search",
                "Search",
                OmnibarMode::Search,
                cx,
            ))
            .child(mode_button(
                "omnibar-filter",
                "Filter",
                OmnibarMode::Filter,
                cx,
            ))
            .child(mode_button(
                "omnibar-command",
                "Command",
                OmnibarMode::Command,
                cx,
            ))
            .child(
                Input::new(input)
                    .id("omnibar-input")
                    .aria_label("Path, search, or command")
                    .small(),
            )
            .children(suggestions)
            .into_any_element()
    }

    fn active_path_suggestions(&self) -> Vec<(String, StorePath)> {
        if self.omnibar.mode() != OmnibarMode::Path {
            return Vec::new();
        }
        let query = Path::new(self.omnibar.text())
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if query.is_empty() || self.omnibar.text() == self.current_location_text().as_ref() {
            return Vec::new();
        }
        self.focused_directory()
            .items()
            .iter()
            .filter(|item| item.display_name().as_str().starts_with(query))
            .take(3)
            .map(|item| (item.display_name().as_str().to_owned(), item.path().clone()))
            .collect()
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.sidebar_border
        };
        let active = self.navigation.focused_tab().id();
        let tabs = self
            .navigation
            .focused_pane()
            .tabs()
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let id = tab.id();
                let label = tab
                    .location()
                    .as_unix_path()
                    .and_then(Path::file_name)
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Files".into());
                Button::new(SharedString::from(format!("tab-{index}")))
                    .label(label)
                    .accessibility_label("Open location")
                    .tooltip("Switch to tab")
                    .secondary()
                    .small()
                    .selected(id == active)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.activate_tab(id, cx);
                    }))
            });
        div()
            .id("tab-strip")
            .test_support()
            .role(Role::TabList)
            .aria_label("Open locations")
            .h(px(42.))
            .flex()
            .items_end()
            .px_3()
            .gap_1()
            .bg(colors.sidebar)
            .border_b_1()
            .border_color(boundary)
            .child(Icon::default().data(ApplicationIdentity::ICON_SVG).small())
            .children(tabs)
            .child(
                Button::new("tab.new")
                    .icon(IconName::Plus)
                    .accessibility_label("New tab")
                    .tooltip("New tab")
                    .ghost()
                    .small()
                    .compact()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dispatch_command("tab.new", cx);
                    })),
            )
    }

    fn render_toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.border
        };
        debug_assert_eq!(COMMAND_IDS.len(), 21);
        let compact = window.viewport_size().width.as_f32() < 960.0;

        div()
            .id("navigation-toolbar")
            .test_support()
            .role(Role::Toolbar)
            .aria_label("Navigation and view controls")
            .h(px(52.))
            .flex()
            .items_center()
            .gap_1()
            .px_3()
            .bg(colors.background)
            .border_b_1()
            .border_color(boundary)
            .child(self.toolbar_button("navigation.back", cx))
            .child(self.toolbar_button("navigation.forward", cx))
            .child(self.toolbar_button("navigation.parent", cx))
            .child(self.toolbar_button("navigation.refresh", cx))
            .child(self.render_omnibar(cx))
            .child(self.toolbar_button("view.search", cx))
            .when(compact, |toolbar| {
                toolbar.child(self.render_view_overflow(cx))
            })
            .when(!compact, |toolbar| {
                toolbar.child(self.render_view_controls(cx))
            })
            .child(self.toolbar_button("view.sidebar", cx))
            .child(self.toolbar_button("view.info", cx))
            .child(self.toolbar_button("pane.split", cx))
            .child(self.toolbar_button("pane.focus_next", cx))
            .child(self.toolbar_button("app.settings", cx))
    }

    fn render_custom_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = cx
            .try_global::<crate::settings::RuntimeSettings>()
            .map(|settings| crate::settings::toolbar::toolbar_from_document(&settings.0))
            .unwrap_or_default();
        let overflow = layout.ids().iter().skip(8).cloned().collect::<Vec<_>>();
        let host = cx.entity();
        let overflow_items = overflow
            .iter()
            .map(|id| {
                let command = self.shell.commands().get(id.as_str());
                let label = command
                    .map(|command| {
                        self.catalog
                            .message(command.label_key())
                            .expect("localized command")
                            .to_owned()
                    })
                    .unwrap_or_else(|| {
                        format!(
                            "{}: {}",
                            self.catalog
                                .message("customization-unavailable")
                                .expect("localized unavailable"),
                            id.as_str()
                        )
                    });
                let enabled = command.is_some_and(|command| {
                    command
                        .state(&self.active_command_context(command.action()))
                        .is_enabled()
                });
                (
                    id.clone(),
                    label,
                    command.map(|command| menu_icon(Some(command.icon_key()))),
                    enabled,
                )
            })
            .collect::<Vec<_>>();
        div()
            .id("custom-toolbar")
            .test_support()
            .role(Role::Toolbar)
            .aria_label(
                self.catalog
                    .message("setting-layout-toolbar")
                    .expect("localized toolbar")
                    .to_owned(),
            )
            .flex()
            .flex_wrap()
            .gap_1()
            .px_3()
            .py_1()
            .bg(cx.theme().colors.background)
            .children(
                layout
                    .ids()
                    .iter()
                    .take(8)
                    .filter(|id| self.shell.commands().get(id.as_str()).is_some())
                    .map(|id| {
                        self.named_toolbar_button(
                            id.as_str(),
                            format!("custom-toolbar-{}", id.as_str()).into(),
                            cx,
                        )
                    })
                    .collect::<Vec<_>>(),
            )
            .when(!overflow.is_empty(), |bar| {
                bar.child(
                    Button::new("custom-toolbar-overflow")
                        .label(
                            self.catalog
                                .message("customization-more")
                                .expect("localized overflow")
                                .to_owned(),
                        )
                        .dropdown_menu(move |mut menu, _, _| {
                            for (id, label, icon, enabled) in &overflow_items {
                                let host = host.clone();
                                let id = id.clone();
                                menu = menu.item(
                                    PopupMenuItem::new(label.clone())
                                        .when_some(*icon, |item, icon| item.icon(icon))
                                        .disabled(!enabled)
                                        .on_click(move |_, window, cx| {
                                            host.update(cx, |this, cx| {
                                                this.remember_context_invocation_focus(window, cx);
                                                this.dispatch_command(id.as_str(), cx);
                                            });
                                        }),
                                );
                            }
                            menu
                        }),
                )
            })
    }

    fn route_custom_shortcut(
        &mut self,
        key: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.browser_input_blocked() {
            cx.stop_propagation();
            window.prevent_default();
            self.activate_context_dialog(cx);
            return;
        }
        if key.key == "escape" || self.keyboard_context_popup.is_some() {
            return;
        }
        let mut parts = Vec::new();
        if key.modifiers.control {
            parts.push("ctrl");
        }
        if key.modifiers.alt {
            parts.push("alt");
        }
        if key.modifiers.shift {
            parts.push("shift");
        }
        if key.modifiers.platform {
            parts.push("super");
        }
        parts.push(&key.key);
        let chord = parts.join("-");
        let map = cx
            .try_global::<crate::settings::RuntimeSettings>()
            .map(|settings| crate::settings::shortcuts::shortcuts_from_document(&settings.0))
            .unwrap_or_default();
        let scope = if self
            .omnibar_input
            .as_ref()
            .is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
        {
            musheen_core::ShortcutScope::Global
        } else {
            musheen_core::ShortcutScope::Browser
        };
        if scope == musheen_core::ShortcutScope::Global
            && cx
                .all_bindings_for_input(std::slice::from_ref(key))
                .iter()
                .any(|binding| binding.action().name().starts_with("input::"))
        {
            // The focused native input owns its editing actions. Consult its
            // installed bindings, rather than duplicating a list of editing keys.
            return;
        }
        if let Some(id) = map.resolve(&chord, scope, self.shell.commands()) {
            cx.stop_propagation();
            window.prevent_default();
            if self
                .shell
                .commands()
                .get(id.as_str())
                .is_some_and(|command| is_contextual_command(command.action()))
            {
                self.remember_context_invocation_focus(window, cx);
            } else {
                self.remember_browser_focus(window, cx);
            }
            self.dispatch_command(id.as_str(), cx);
        } else if musheen_core::ShortcutMap::default()
            .resolve(
                &chord,
                musheen_core::ShortcutScope::Browser,
                self.shell.commands(),
            )
            .is_some_and(|default| {
                map.resolve(
                    &chord,
                    musheen_core::ShortcutScope::Browser,
                    self.shell.commands(),
                ) != Some(default)
            })
        {
            // Static bindings have no browser scope. Suppress changed defaults even
            // while the omnibar owns focus, but preserve unchanged text-editing keys.
            cx.stop_propagation();
            window.prevent_default();
        }
    }

    fn render_view_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.toolbar_button("view.details", cx))
            .child(self.toolbar_button("view.list", cx))
            .child(self.toolbar_button("view.cards", cx))
            .child(self.toolbar_button("view.grid", cx))
            .child(self.toolbar_button("view.columns", cx))
            .child(self.toolbar_button("view.adaptive", cx))
            .child(self.toolbar_button("view.sort", cx))
            .child(self.toolbar_button("view.group", cx))
            .child(self.toolbar_button("view.directories_first", cx))
            .child(self.toolbar_button("view.hidden", cx))
            .into_any_element()
    }

    fn render_view_overflow(&self, cx: &mut Context<Self>) -> AnyElement {
        let view = cx.entity();
        // Every view command uses the same selection context.
        let context = self.active_command_context(CommandAction::ViewDetails);
        let items = [
            "view.details",
            "view.list",
            "view.cards",
            "view.grid",
            "view.columns",
            "view.adaptive",
            "view.sort",
            "view.group",
            "view.directories_first",
            "view.hidden",
        ]
        .map(|id| {
            let command = self
                .shell
                .commands()
                .get(id)
                .expect("overflow command is registered");
            let label = self
                .catalog
                .message(command.label_key())
                .expect("overflow label is localized")
                .to_owned();
            (
                id,
                label,
                menu_icon(Some(command.icon_key())),
                command.state(&context),
            )
        });
        let label = self
            .catalog
            .message("view-options")
            .expect("view options label is localized")
            .to_owned();
        Button::new("view.overflow")
            .icon(IconName::ListChecks)
            .accessibility_label(label.clone())
            .tooltip(label)
            .ghost()
            .small()
            .compact()
            .dropdown_menu(move |mut menu, _, _| {
                for (command_id, label, icon, state) in items.iter().cloned() {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .icon(icon)
                            .disabled(!state.is_enabled())
                            .checked(state.is_checked())
                            .on_click(move |_, window, cx| {
                                view.update(cx, |this, cx| {
                                    this.remember_context_invocation_focus(window, cx);
                                    this.dispatch_command(command_id, cx);
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let transparent = cx.theme().transparent;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.sidebar_border
        };
        let current = self.focused_directory().location().cloned();
        let context_menu_host = cx.entity().downgrade();
        let sidebar_tab = self.navigation.focused_tab().id();
        let sections =
            self.sidebars
                .get(&self.navigation.focused_tab().id())
                .map(|sidebar| {
                    sidebar
                        .sections()
                        .into_iter()
                        .enumerate()
                        .map(|(section_index, section)| {
                            let kind = section.kind();
                            let collapsed = sidebar.is_section_collapsed(kind);
                            let entries = section
                                .items()
                                .iter()
                                .enumerate()
                                .map(|(entry_index, entry)| {
                                    let location = entry.location().clone();
                                    let navigation_location = location.clone();
                                    let can_drop_location = location.clone();
                                    let drop_location = location.clone();
                                    let context_location = location.clone();
                                    let operation_hub = self.operation_hub.clone();
                                    let selected = current.as_ref() == Some(&location);
                                    let label = entry.label().to_owned();
                                    let icon = match (kind, label.as_str()) {
                                        (SidebarSectionKind::Home, _) => IconName::House,
                                        (_, "Downloads") => IconName::Download,
                                        (_, "Trash") => IconName::Trash,
                                        (SidebarSectionKind::Mounts, _) => IconName::HardDrive,
                                        (SidebarSectionKind::Network, _)
                                        | (SidebarSectionKind::Remote, _) => IconName::Network,
                                        _ => IconName::Folder,
                                    };
                                    div()
                                        .border_l_2()
                                        .border_color(if selected {
                                            colors.primary
                                        } else {
                                            transparent
                                        })
                                        .can_drop(move |value, _, _| {
                                            value.downcast_ref::<FileDragPayload>().is_some_and(
                                                |payload| {
                                                    operation_hub.can_accept_drop(
                                                        payload,
                                                        &can_drop_location,
                                                    )
                                                },
                                            )
                                        })
                                        .on_drop(cx.listener(
                                            move |this, payload: &FileDragPayload, _, cx| {
                                                this.submit_file_drop(
                                                    payload.clone(),
                                                    drop_location.clone(),
                                                    cx,
                                                );
                                            },
                                        ))
                                        .on_mouse_down(
                                            MouseButton::Right,
                                            cx.listener(move |this, _, _, _| {
                                                if !this.context_dialog_windows.is_empty() {
                                                    return;
                                                }
                                                this.pending_context_menu =
                                                    Some(this.sidebar_location_context_menu(
                                                        sidebar_tab,
                                                        if kind == SidebarSectionKind::Mounts {
                                                            MenuTarget::Mount
                                                        } else {
                                                            MenuTarget::SidebarLocation
                                                        },
                                                        context_location.clone(),
                                                    ));
                                            }),
                                        )
                                        .child(
                                            Button::new(SharedString::from(format!(
                                                "sidebar-{section_index}-{entry_index}"
                                            )))
                                            .label(label.clone())
                                            .icon(icon)
                                            .accessibility_label(label)
                                            .ghost()
                                            .small()
                                            .selected(selected)
                                            .w_full()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.navigate(
                                                    navigation_location.clone(),
                                                    true,
                                                    cx,
                                                );
                                            })),
                                        )
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>();
                            div()
                                .w_full()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "sidebar-section-{section_index}"
                                    )))
                                    .label(section.label())
                                    .accessibility_label(format!(
                                        "{} section, {}",
                                        section.label(),
                                        if collapsed { "collapsed" } else { "expanded" }
                                    ))
                                    .ghost()
                                    .small()
                                    .w_full()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.toggle_sidebar_section(kind, cx);
                                    })),
                                )
                                .when(!collapsed, |section| section.children(entries))
                                .into_any_element()
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();

        div()
            .id("sidebar")
            .test_support()
            .role(Role::Navigation)
            .aria_label("Places")
            .w(px(220.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .bg(colors.sidebar)
            .border_r_1()
            .border_color(boundary)
            .children(sections)
            .context_menu(move |popup, window, popup_cx| {
                let Some(menu) = context_menu_host
                    .update(popup_cx, |this, cx| {
                        if this.browser_input_blocked() {
                            this.pending_context_menu = None;
                            return None;
                        }
                        this.remember_context_invocation_focus(window, cx);
                        Some(
                            this.pending_context_menu
                                .take()
                                .unwrap_or_else(|| this.sidebar_context_menu(sidebar_tab)),
                        )
                    })
                    .ok()
                    .flatten()
                else {
                    return popup;
                };
                Self::populate_context_popup(
                    popup,
                    menu,
                    context_menu_host.clone(),
                    format!("sidebar-{sidebar_tab:?}"),
                    window,
                    popup_cx,
                )
            })
    }

    fn toggle_sidebar_section(&mut self, kind: SidebarSectionKind, cx: &mut Context<Self>) {
        let tab_id = self.navigation.focused_tab().id();
        if let Some(sidebar) = self.sidebars.get_mut(&tab_id) {
            let collapsed = !sidebar.is_section_collapsed(kind);
            sidebar.set_section_collapsed(kind, collapsed);
            cx.notify();
        }
    }

    fn render_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let focused = self.navigation.focused_pane_id();
        let wide = window.viewport_size().width.as_f32() >= 960.0;
        let panes = self
            .navigation
            .panes()
            .iter()
            .filter(|pane| wide || pane.id() == focused)
            .map(|pane| (pane.id(), pane.active_tab().id()))
            .collect::<Vec<_>>();
        panes
            .into_iter()
            .enumerate()
            .map(|(index, (pane_id, tab_id))| {
                let directory = self.render_directory(
                    PaneRenderSpec {
                        tab_id,
                        pane_index: index,
                        focused: pane_id == focused,
                    },
                    window,
                    cx,
                );
                div()
                    .id(SharedString::from(format!("pane-{index}")))
                    .test_support()
                    .h_full()
                    .min_w(px(0.))
                    .flex_grow(1.0)
                    .overflow_hidden()
                    .when(index > 0, |pane| pane.border_l_1())
                    .border_color(cx.theme().border)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.focus_pane(pane_id, cx);
                    }))
                    .child(directory)
                    .into_any_element()
            })
            .collect()
    }

    fn render_directory(
        &mut self,
        spec: PaneRenderSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors;
        let transparent = cx.theme().transparent;
        let state = self
            .directories
            .get(&spec.tab_id)
            .map(|directory| directory.state().clone())
            .unwrap_or(DirectoryState::Loading);
        let trash = self
            .navigation
            .tab(spec.tab_id)
            .is_some_and(|tab| is_trash_location(tab.location()));
        let body = if trash {
            self.render_trash_surface(spec.tab_id, cx)
        } else if self.searches.contains_key(&spec.tab_id) {
            self.render_search_results(spec.tab_id, spec.pane_index, cx)
        } else {
            match state {
                DirectoryState::Loading => self.render_loading(colors.skeleton),
                DirectoryState::Empty => self.render_empty(colors.muted_foreground),
                DirectoryState::Error(message) => self.render_error(message, cx),
                DirectoryState::Ready => {
                    self.render_items(spec.tab_id, spec.pane_index, window, cx)
                }
            }
        };
        let content_id = if spec.pane_index == 0 {
            SharedString::from("directory-content")
        } else {
            SharedString::from(format!("directory-content-{}", spec.pane_index))
        };
        let context_menu_host = cx.entity().downgrade();
        let tab_id = spec.tab_id;
        div()
            .id(content_id)
            .test_support()
            .key_context("DirectoryContent")
            .role(Role::Main)
            .aria_label("Folder contents")
            .tab_index(0)
            .track_focus(&self.content_focus)
            .h_full()
            .flex_grow(1.0)
            .overflow_hidden()
            .border_1()
            .border_color(if spec.focused {
                colors.ring
            } else {
                transparent
            })
            .focus(|style| style.border_color(colors.ring))
            .bg(colors.background)
            .child(body)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    // Item context events bubble through the content surface.
                    // Their item handler has already captured a menu; only an
                    // unclaimed event is a genuine background invocation.
                    if !this.context_dialog_windows.is_empty()
                        || this.pending_context_menu.is_some()
                    {
                        return;
                    }
                    this.focus_directory_item(tab_id, None, cx);
                    this.pending_context_menu =
                        Some(this.compose_context_menu(tab_id, MenuTarget::Background, Vec::new()));
                }),
            )
            .context_menu(move |popup, window, popup_cx| {
                let Some(menu) = context_menu_host
                    .update(popup_cx, |this, cx| {
                        if this.browser_input_blocked() {
                            this.pending_context_menu = None;
                            return None;
                        }
                        this.remember_context_invocation_focus(window, cx);
                        Some(this.pending_context_menu.take().unwrap_or_else(|| {
                            this.compose_context_menu(tab_id, MenuTarget::Background, Vec::new())
                        }))
                    })
                    .ok()
                    .flatten()
                else {
                    return popup;
                };
                Self::populate_context_popup(
                    popup,
                    menu,
                    context_menu_host.clone(),
                    format!("pane-{tab_id:?}"),
                    window,
                    popup_cx,
                )
            })
            .into_any_element()
    }

    fn render_trash_surface(&self, tab_id: TabId, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let state = self
            .trash_states
            .get(&tab_id)
            .cloned()
            .unwrap_or(TrashState::Loading);
        match state {
            TrashState::Loading => self.render_loading(colors.skeleton),
            TrashState::Error(message) => self.render_error(message, cx),
            TrashState::Ready(surface) => {
                let items = surface.items().to_vec();
                let rows = items.iter().cloned().enumerate().map(|(index, item)| {
                    let receipt = item.receipt().clone();
                    let target = trash_command_target(&item);
                    let restore_target = target.clone();
                    let pointer_target = target.clone();
                    let original = DisplayPath::from_store_path(receipt.original_path())
                        .as_str()
                        .to_owned();
                    div()
                        .id(SharedString::from(format!("trash-item-{index}")))
                        .test_support()
                        .role(Role::ListItem)
                        .w_full()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_4()
                        .py_3()
                        .border_b_1()
                        .border_color(colors.border)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                if this.browser_input_blocked() {
                                    cx.stop_propagation();
                                    return;
                                }
                                this.trash_focus.insert(tab_id, target.clone());
                                if let Some(tab) = this.navigation.tab_mut(tab_id) {
                                    tab.set_selection(vec![target.id().clone()]);
                                }
                                this.content_focus.focus(window, cx);
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(move |this, _, _, cx| {
                                if this.browser_input_blocked() {
                                    cx.stop_propagation();
                                    return;
                                }
                                this.trash_focus.insert(tab_id, pointer_target.clone());
                                if let Some(tab) = this.navigation.tab_mut(tab_id)
                                    && !tab.selection().contains(pointer_target.id())
                                {
                                    tab.set_selection(vec![pointer_target.id().clone()]);
                                }
                                let selection = this
                                    .trash_states
                                    .get(&tab_id)
                                    .and_then(|state| match state {
                                        TrashState::Ready(surface) => Some(
                                            surface
                                                .items()
                                                .iter()
                                                .map(trash_command_target)
                                                .filter(|target| {
                                                    this.navigation.tab(tab_id).is_some_and(|tab| {
                                                        tab.selection().contains(target.id())
                                                    })
                                                })
                                                .collect(),
                                        ),
                                        _ => None,
                                    })
                                    .unwrap_or_else(|| vec![pointer_target.clone()]);
                                this.pending_context_menu = Some(this.compose_context_menu(
                                    tab_id,
                                    MenuTarget::TrashItem,
                                    selection,
                                ));
                                cx.notify();
                            }),
                        )
                        .child(
                            div()
                                .flex_grow(1.0)
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .child(original)
                                .child(div().text_xs().text_color(colors.muted_foreground).child(
                                    format!(
                                        "Deleted at Unix time {}",
                                        item.deleted_at_unix_seconds()
                                    ),
                                )),
                        )
                        .child(
                            Button::new(SharedString::from(format!("trash-restore-{index}")))
                                .label("Restore")
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let menu = this.compose_context_menu(
                                        tab_id,
                                        MenuTarget::TrashItem,
                                        vec![restore_target.clone()],
                                    );
                                    if let Some(entry) =
                                        Self::menu_entry_by_id(&menu, "trash.restore")
                                    {
                                        this.dispatch_context_entry(entry.clone(), cx);
                                    }
                                })),
                        )
                });
                let count = items.len();
                let confirmation = self.pending_empty_trash.as_ref().map(|pending| {
                    let pending_count = match pending {
                        MenuInvocation::NeedsConfirmation(pending) => pending.selection().len(),
                        _ => 0,
                    };
                    let label = if pending_count == 1 {
                        "Permanently delete 1 item from Trash? This cannot be undone.".to_owned()
                    } else {
                        format!(
                            "Permanently delete {pending_count} items from Trash? This cannot be undone."
                        )
                    };
                    div()
                        .id("trash-empty-confirmation")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(label.clone())
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(colors.border)
                        .child(div().flex_grow(1.0).child(label))
                        .child(
                            Button::new("trash-empty-cancel")
                                .label("Cancel")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pending_empty_trash = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("trash-empty-confirm")
                                .label("Empty Trash")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_empty_trash(cx);
                                })),
                        )
                });
                div()
                    .id("trash-surface")
                    .test_support()
                    .role(Role::Region)
                    .aria_label("Trash contents")
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .px_4()
                            .py_3()
                            .border_b_1()
                            .border_color(colors.border)
                            .child(
                                div()
                                    .flex_grow(1.0)
                                    .child(format!("{count} items in Trash")),
                            )
                            .when(count > 0, |header| {
                                header.child(
                                    Button::new("trash-empty")
                                        .label("Empty Trash")
                                        .small()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            let menu = this.compose_context_menu(
                                                tab_id,
                                                MenuTarget::TrashBackground,
                                                Vec::new(),
                                            );
                                            if let Some(entry) =
                                                Self::menu_entry_by_id(&menu, "trash.empty")
                                            {
                                                this.dispatch_context_entry(entry.clone(), cx);
                                            }
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .id("trash-items")
                            .test_support()
                            .role(Role::List)
                            .aria_label("Deleted items")
                            .flex_grow(1.0)
                            .min_h(px(0.))
                            .overflow_y_scrollbar()
                            .children(rows),
                    )
                    .children(confirmation)
                    .into_any_element()
            }
        }
    }

    fn trash_items_for_targets(
        &self,
        tab_id: TabId,
        targets: &[CommandTargetRef],
    ) -> Option<Vec<TrashItem>> {
        if targets.is_empty()
            || !self
                .navigation
                .tab(tab_id)
                .is_some_and(|tab| is_trash_location(tab.location()))
        {
            return None;
        }
        let TrashState::Ready(surface) = self.trash_states.get(&tab_id)? else {
            return None;
        };
        let available: HashMap<_, _> = surface
            .items()
            .iter()
            .map(|item| (trash_command_target(item).id().clone(), item))
            .collect();
        targets
            .iter()
            .map(|target| {
                available
                    .get(target.id())
                    .filter(|item| trash_command_target(item) == *target)
                    .map(|item| (*item).clone())
            })
            .collect()
    }

    fn restore_trash_item(
        &mut self,
        tab_id: TabId,
        receipt: musheen_ops::TrashReceipt,
        source_kind: ConflictItemKind,
        cx: &mut Context<Self>,
    ) {
        let work = cx.background_spawn(async move {
            let mut store = LocalStore::new();
            match musheen_ops::execute_restore(&mut store, &receipt) {
                Ok(()) => TrashRestoreResult::Restored,
                Err(MutationError::Conflict) => {
                    let destination_identity =
                        match MutationProvider::identity(&mut store, receipt.original_path()) {
                            Ok(Some(identity)) => identity,
                            Ok(None) => return TrashRestoreResult::Failed(MutationError::Missing),
                            Err(error) => return TrashRestoreResult::Failed(error),
                        };
                    let destination_kind = match store.conflict_item_kind(receipt.original_path()) {
                        Ok(kind) => kind,
                        Err(error) => return TrashRestoreResult::Failed(error),
                    };
                    let source = match StorePath::from_provider_key(
                        ProviderId::new("local.trash").expect("the Trash provider ID is valid"),
                        receipt.provider_reference().to_vec(),
                    ) {
                        Ok(source) => source,
                        Err(error) => {
                            return TrashRestoreResult::Failed(MutationError::Provider(
                                error.to_string().into(),
                            ));
                        }
                    };
                    match ConflictRecord::new(
                        OperationKind::Restore,
                        source,
                        receipt.provider_reference().to_vec(),
                        source_kind,
                        receipt.original_path().clone(),
                        destination_identity.to_vec(),
                        destination_kind,
                    ) {
                        Ok(conflict) => TrashRestoreResult::Conflict { receipt, conflict },
                        Err(error) => TrashRestoreResult::Failed(MutationError::Provider(
                            error.to_string().into(),
                        )),
                    }
                }
                Err(error) => TrashRestoreResult::Failed(error),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| match result {
                TrashRestoreResult::Restored => state.start_trash_load(tab_id, cx),
                TrashRestoreResult::Conflict { receipt, conflict } => {
                    state.open_restore_conflict(tab_id, receipt, conflict, cx);
                }
                TrashRestoreResult::Failed(error) => {
                    state.operation_error = Some(error.to_string().into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn open_restore_conflict(
        &mut self,
        tab_id: TabId,
        receipt: musheen_ops::TrashReceipt,
        conflict: ConflictRecord,
        cx: &mut Context<Self>,
    ) {
        let options = conflict_window_options(cx);
        let model = ConflictDialogModel::new(conflict.clone());
        let mut dialog = None;
        let handle = cx
            .open_window(options, |window, cx| {
                let view = cx.new(|cx| ConflictDialog::new(model, cx));
                dialog = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            })
            .expect("Musheen could not open a conflict dialog");
        let dialog = dialog.expect("the conflict window constructs its view");
        let window_id = handle.window_id();
        self.pending_restores.insert(
            window_id,
            PendingRestore {
                tab_id,
                receipt,
                conflict,
                _dialog: dialog.clone(),
            },
        );
        self.track_context_dialog_window(window_id, Some(tab_id), cx);
        let subscription = cx.subscribe(&dialog, move |this, _, event, cx| {
            this.handle_restore_conflict_event(window_id, event, cx);
        });
        self.conflict_subscriptions.push(subscription);
    }

    fn handle_restore_conflict_event(
        &mut self,
        window_id: WindowId,
        event: &ConflictDialogEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_restores.remove(&window_id) else {
            return;
        };
        match event {
            ConflictDialogEvent::Resolved(choice, scope) => self.apply_restore_decision(
                pending.tab_id,
                pending.receipt,
                pending.conflict,
                *choice,
                *scope,
                cx,
            ),
            ConflictDialogEvent::Cancelled => self.start_trash_load(pending.tab_id, cx),
        }
    }

    fn apply_restore_decision(
        &mut self,
        tab_id: TabId,
        receipt: musheen_ops::TrashReceipt,
        conflict: ConflictRecord,
        choice: ConflictChoice,
        scope: ApplyScope,
        cx: &mut Context<Self>,
    ) {
        let decision = ConflictDecisionStore::for_current_user()
            .map_err(|error| MutationError::Provider(error.to_string().into()))
            .and_then(|mut journal| {
                ConflictPolicies::default()
                    .decide(&conflict, choice, scope, &mut journal)
                    .map_err(|error| MutationError::Provider(error.to_string().into()))
            });
        let decision = match decision {
            Ok(decision) => decision,
            Err(error) => {
                self.operation_error = Some(error.to_string().into());
                cx.notify();
                return;
            }
        };
        let work = cx.background_spawn(async move {
            let mut store = LocalStore::new();
            store.resolve_restore_conflict(&receipt, &decision)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| match result {
                Ok(()) => state.start_trash_load(tab_id, cx),
                Err(error) => {
                    state.operation_error = Some(error.to_string().into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn confirm_empty_trash(&mut self, cx: &mut Context<Self>) {
        if self.browser_input_blocked() {
            return;
        }
        let Some(invocation) = self.pending_empty_trash.take() else {
            return;
        };
        self.confirm_context_review(invocation, cx);
    }

    fn empty_trash(
        &mut self,
        tab_id: TabId,
        receipts: Vec<musheen_ops::TrashReceipt>,
        cx: &mut Context<Self>,
    ) {
        let confirmed = self
            .trash_states
            .get(&tab_id)
            .and_then(|state| match state {
                TrashState::Ready(surface) => Some(surface.empty_challenge()),
                TrashState::Loading | TrashState::Error(_) => None,
            })
            .is_some_and(|challenge| challenge.confirm(receipts.len(), true).is_ok());
        if !confirmed {
            self.operation_error = Some("Trash changed before it could be emptied".into());
            cx.notify();
            return;
        }
        let work = cx.background_spawn(async move {
            let mut store = LocalStore::new();
            store.purge_trash(&receipts)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |state, cx| match result {
                Ok(()) => state.start_trash_load(tab_id, cx),
                Err(error) => {
                    state.operation_error = Some(error.to_string().into());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn render_search_results(
        &mut self,
        tab_id: TabId,
        pane_index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(search) = self.searches.get(&tab_id) else {
            return div().into_any_element();
        };
        let scope = DisplayPath::from_store_path(search.model.scope())
            .as_str()
            .to_owned();
        let hidden_policy = if search.model.query().include_hidden() {
            "Hidden items included"
        } else {
            "Hidden items excluded"
        };
        let link_policy = if search.model.query().follow_links() {
            "Symbolic links followed"
        } else {
            "Symbolic links not followed"
        };
        let scope_label = format!(
            "Search '{}' in {scope} · {hidden_policy} · {link_policy}",
            search.expression
        );
        let state = search.model.state();
        let state_label = search.status_text();
        let error_panel = self.render_search_errors(search, state, cx);
        let items = search
            .model
            .retained_results()
            .iter()
            .enumerate()
            .map(|(index, result)| SearchItemRenderSpec {
                index,
                name: result.item().display_name().as_str().to_owned(),
                path: DisplayPath::from_store_path(result.item().path())
                    .as_str()
                    .to_owned(),
                kind: result.item().kind(),
                mime: result.mime().map(Into::into),
            })
            .collect::<Vec<_>>();
        let items = Arc::new(items);
        let list_items = Arc::clone(&items);
        let rows = uniform_list(
            SharedString::from(format!("search-list-{pane_index}")),
            items.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|index| list_items.get(index).cloned())
                    .map(|item| this.render_search_item(pane_index, item, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .w_full()
        .flex_grow(1.0)
        .min_h(px(0.));
        div()
            .id("search-results")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("search-scope")
                    .test_support()
                    .aria_label(scope_label.clone())
                    .h(px(40.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px_4()
                    .border_b_1()
                    .border_color(cx.theme().colors.border)
                    .child(scope_label)
                    .child(state_label),
            )
            .children(error_panel)
            .child(rows)
            .into_any_element()
    }

    fn render_search_errors(
        &self,
        search: &ActiveSearch,
        state: SearchState,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let error_count = search.model.errors().len() + search.model.dropped_error_count();
        if error_count == 0 && state != SearchState::Error {
            return None;
        }
        let error_rows = search
            .model
            .errors()
            .iter()
            .take(3)
            .enumerate()
            .map(|(index, error)| {
                let path = DisplayPath::from_store_path(error.path());
                let message = format!("{}: {}", path.as_str(), error.message());
                div()
                    .id(SharedString::from(format!("search-error-{index}")))
                    .test_support()
                    .aria_label(message.clone())
                    .text_xs()
                    .child(message)
            })
            .collect::<Vec<_>>();
        let hidden_error_count = error_count.saturating_sub(error_rows.len());
        let can_retry = search.retryable
            || search
                .model
                .errors()
                .iter()
                .any(SearchScopeError::retryable);
        let retry_expression = search.expression.clone();
        Some(
            div()
                .id("search-errors")
                .test_support()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(cx.theme().colors.border)
                .children(error_rows)
                .when(hidden_error_count > 0, |panel| {
                    panel.child(format!("and {hidden_error_count} more"))
                })
                .when(can_retry, |panel| {
                    panel.child(
                        Button::new("search-retry")
                            .label("Retry")
                            .accessibility_label("Retry search")
                            .secondary()
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.start_search(retry_expression.clone(), cx);
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_search_item(
        &mut self,
        pane_index: usize,
        item: SearchItemRenderSpec,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let identity = match item.kind {
            ItemKind::Directory => ContentIdentity::directory(),
            ItemKind::SymbolicLink => ContentIdentity::symbolic_link(),
            ItemKind::RegularFile | ItemKind::Other => item
                .mime
                .clone()
                .map_or(ContentIdentity::GenericFile, ContentIdentity::Mime),
        };
        let icon = self
            .content_icon(freedesktop_icon_name(&identity))
            .map_or_else(
                || {
                    Icon::new(if item.kind == ItemKind::Directory {
                        IconName::Folder
                    } else {
                        IconName::File
                    })
                    .into_any_element()
                },
                |source| img(source).size(px(28.)).into_any_element(),
            );
        div()
            .id(SharedString::from(format!(
                "search-item-{pane_index}-{}",
                item.index
            )))
            .role(Role::ListItem)
            .h(px(44.))
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .hover(|style| style.bg(cx.theme().colors.list_hover))
            .child(icon)
            .child(
                div()
                    .flex_grow(1.0)
                    .min_w(px(0.))
                    .child(div().child(item.name))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().colors.muted_foreground)
                            .child(item.path),
                    ),
            )
            .into_any_element()
    }

    fn render_info_pane(&self, wide: bool, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.sidebar_border
        };
        let tab_id = self.navigation.focused_tab().id();
        let state = self
            .info_panes
            .get(&tab_id)
            .map(InfoPaneModel::state)
            .unwrap_or(&InfoPaneState::Empty);
        let content = self.render_info_state(tab_id, state, cx);
        let pane = div()
            .id("info-pane")
            .test_support()
            .role(Role::Region)
            .aria_label("Information pane")
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p_4()
            .bg(colors.sidebar)
            .border_l_1()
            .border_color(boundary)
            .child(content);
        if wide {
            pane.w(px(280.)).into_any_element()
        } else {
            pane.flex_grow(1.0).into_any_element()
        }
    }

    fn render_info_state(
        &self,
        tab_id: TabId,
        state: &InfoPaneState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match state {
            InfoPaneState::Empty => self.render_info_empty(cx),
            InfoPaneState::Multiple { count } => self.render_info_multiple(*count, cx),
            InfoPaneState::Loading { details, path } => self.render_info_loading(details, path, cx),
            InfoPaneState::Error {
                details,
                path,
                message,
            } => self.render_info_error(tab_id, details, path, message, cx),
            InfoPaneState::Ready {
                details,
                path,
                mime_type,
                preview,
            } => self.render_info_ready(tab_id, details, path, mime_type, preview, cx),
        }
    }

    fn render_info_empty(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .text_color(cx.theme().colors.muted_foreground)
            .child(Icon::new(IconName::Info).large())
            .child(div().text_sm().child("No item selected"))
            .child(
                div()
                    .text_xs()
                    .text_center()
                    .child("Select an item to see its details."),
            )
            .into_any_element()
    }

    fn render_info_multiple(&self, count: usize, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .text_color(cx.theme().colors.muted_foreground)
            .child(Icon::new(IconName::ListChecks).large())
            .child(div().text_sm().child(format!("{count} items selected")))
            .child(
                div()
                    .text_xs()
                    .child("Properties are available for one item at a time."),
            )
            .into_any_element()
    }

    fn render_info_loading(
        &self,
        details: &InfoPaneDetails,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().child(details.name().to_owned()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child("Loading preview…"),
            )
            .child(self.info_open_button("info-open-loading", path, cx))
            .into_any_element()
    }

    fn render_info_error(
        &self,
        tab_id: TabId,
        details: &InfoPaneDetails,
        path: &Path,
        message: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().child(details.name().to_owned()))
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(message.to_owned()),
            )
            .child(
                Button::new("info-retry")
                    .label("Retry preview")
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.retry_info_pane(tab_id, cx);
                    })),
            )
            .child(self.info_open_button("info-open-error", path, cx))
            .into_any_element()
    }

    fn render_info_ready(
        &self,
        tab_id: TabId,
        details: &InfoPaneDetails,
        path: &Path,
        mime_type: &str,
        preview: &PreviewPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let can_load_more = matches!(
            preview,
            PreviewPresentation::Text(document) if document.has_more()
        );
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_sm().child(details.name().to_owned()))
            .child(self.render_info_preview(preview, cx))
            .child(self.render_info_details(details, mime_type, cx))
            .when(can_load_more, |pane| {
                pane.child(
                    Button::new("info-load-more")
                        .label("Load 16 MiB more")
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.load_more_info_pane(tab_id, cx);
                        })),
                )
            })
            .child(self.info_open_button("info-open-ready", path, cx))
            .into_any_element()
    }

    fn render_info_preview(
        &self,
        preview: &PreviewPresentation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match preview {
            PreviewPresentation::Text(document) => {
                let text = document
                    .text()
                    .unwrap_or_default()
                    .chars()
                    .take(65_536)
                    .collect::<String>();
                div()
                    .w_full()
                    .max_h(px(260.))
                    .overflow_hidden()
                    .p_3()
                    .rounded_md()
                    .bg(cx.theme().colors.background)
                    .text_xs()
                    .child(text)
                    .into_any_element()
            }
            PreviewPresentation::Binary { bytes_read } => div()
                .w_full()
                .p_3()
                .rounded_md()
                .bg(cx.theme().colors.background)
                .text_xs()
                .child(format!(
                    "Binary or undecodable content ({})",
                    format_size(*bytes_read as u64)
                ))
                .into_any_element(),
            PreviewPresentation::Thumbnail(path) => img(ImageSource::from(path.clone()))
                .size(px(220.))
                .into_any_element(),
            PreviewPresentation::DetailsOnly => div()
                .text_xs()
                .text_color(cx.theme().colors.muted_foreground)
                .child("No inline preview is available.")
                .into_any_element(),
        }
    }

    fn render_info_details(
        &self,
        details: &InfoPaneDetails,
        mime_type: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_1()
            .text_xs()
            .text_color(cx.theme().colors.muted_foreground)
            .child(format!("Type: {}", item_kind_label(details.kind())))
            .child(format!("MIME: {mime_type}"))
            .child(
                details
                    .size()
                    .map(|size| format!("Size: {}", format_size(size)))
                    .unwrap_or_else(|| "Size: Unknown".to_owned()),
            )
            .child(
                details
                    .modified_unix_seconds()
                    .map(|value| format!("Modified: {value}"))
                    .unwrap_or_else(|| "Modified: Unknown".to_owned()),
            )
            .into_any_element()
    }

    fn info_open_button(
        &self,
        id: &'static str,
        path: &Path,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let open_path = path.to_path_buf();
        Button::new(id)
            .label("Open With…")
            .small()
            .on_click(cx.listener(move |_, _, _, _| {
                let _ = open::that(&open_path);
            }))
            .into_any_element()
    }

    fn render_loading(&self, skeleton: gpui_kit::Hsla) -> AnyElement {
        let rows = (0..8).map(|index| {
            div()
                .id(SharedString::from(format!("loading-item-{index}")))
                .h(px(32.))
                .rounded_sm()
                .bg(skeleton)
        });
        div()
            .id("directory-loading")
            .test_support()
            .role(Role::Status)
            .aria_label("Loading folder")
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .children(rows)
            .into_any_element()
    }

    fn render_empty(&self, muted: gpui_kit::Hsla) -> AnyElement {
        div()
            .id("directory-empty")
            .test_support()
            .role(Role::Status)
            .aria_label("This folder is empty")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .text_color(muted)
            .child(Icon::new(IconName::Folder).large())
            .child("This folder is empty")
            .into_any_element()
    }

    fn render_error(&self, message: Box<str>, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("directory-error")
            .test_support()
            .role(Role::Alert)
            .aria_label("Folder could not be opened")
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Icon::new(IconName::Info).large())
            .child(div().text_lg().child("Folder could not be opened"))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message.into_string()),
            )
            .child(
                Button::new("retry-directory")
                    .label("Try again")
                    .primary()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.dispatch_command("navigation.refresh", cx);
                    })),
            )
            .into_any_element()
    }

    fn render_items(
        &mut self,
        tab_id: TabId,
        pane_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let configured = self
            .directories
            .get(&tab_id)
            .map(|directory| directory.view().preferences().layout)
            .unwrap_or_default();
        let layout = if configured == Layout::Adaptive {
            AdaptiveLayout::resolve(window.viewport_size().width.as_f32())
        } else {
            configured
        };
        let item_count = self.filtered_items(tab_id).len();
        let filter_label = self.filters.get(&tab_id).map(|filter| {
            filter.error.as_deref().map_or_else(
                || format!("Filtered view — {item_count} matches"),
                |error| format!("Filter error — {error}"),
            )
        });
        let list =
            match layout {
                Layout::Details | Layout::List | Layout::Columns => uniform_list(
                    SharedString::from(format!("directory-items-list-{pane_index}")),
                    item_count,
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let items = range
                            .filter_map(|index| {
                                this.item_render_spec(tab_id, pane_index, index, layout)
                            })
                            .collect::<Vec<_>>();
                        items
                            .into_iter()
                            .map(|item| div().h(px(40.)).child(this.render_item(item, cx)))
                            .collect::<Vec<_>>()
                    }),
                )
                .w_full()
                .flex_grow(1.0)
                .min_h(px(0.))
                .p_4()
                .into_any_element(),
                Layout::Cards | Layout::Grid | Layout::Adaptive => {
                    let base_columns = grid_column_count(window.viewport_size().width.as_f32());
                    let columns = if layout == Layout::Cards {
                        base_columns.div_ceil(2)
                    } else {
                        base_columns
                    };
                    uniform_list(
                        SharedString::from(format!("directory-items-grid-{pane_index}")),
                        grid_row_count(item_count, columns),
                        cx.processor(move |this, rows: std::ops::Range<usize>, _, cx| {
                            rows.map(|row| {
                                let items = grid_item_range(row, item_count, columns)
                                    .filter_map(|index| {
                                        this.item_render_spec(tab_id, pane_index, index, layout)
                                    })
                                    .collect::<Vec<_>>();
                                div().h(px(116.)).flex().gap_2().children(
                                    items.into_iter().map(|item| this.render_item(item, cx)),
                                )
                            })
                            .collect::<Vec<_>>()
                        }),
                    )
                    .w_full()
                    .flex_grow(1.0)
                    .min_h(px(0.))
                    .p_4()
                    .into_any_element()
                }
            };

        let items_id = if pane_index == 0 {
            SharedString::from("directory-items")
        } else {
            SharedString::from(format!("directory-items-{pane_index}"))
        };
        div()
            .id(items_id)
            .test_support()
            .role(Role::List)
            .aria_label("Items")
            .size_full()
            .flex()
            .flex_col()
            .when_some(filter_label, |items, label| {
                items.child(
                    div()
                        .id("filter-summary")
                        .test_support()
                        .h(px(32.))
                        .flex()
                        .items_center()
                        .px_4()
                        .border_b_1()
                        .border_color(cx.theme().colors.border)
                        .text_sm()
                        .child(label),
                )
            })
            .when(layout == Layout::Details, |items| {
                items.child(self.render_details_header(tab_id, cx))
            })
            .child(list)
            .into_any_element()
    }

    fn render_details_header(&self, tab_id: TabId, cx: &mut Context<Self>) -> AnyElement {
        let column_layout = self
            .directories
            .get(&tab_id)
            .map(|directory| directory.view().preferences().columns.clone())
            .unwrap_or_default();
        let header =
            column_layout
                .visible_columns_with_widths()
                .into_iter()
                .map(|(column, width)| {
                    let label = column_label(column);
                    Button::new(SharedString::from(format!("details-{label}")))
                        .label(label)
                        .accessibility_label(format!("Sort by {label}"))
                        .ghost()
                        .small()
                        .w(px(f32::from(width)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.sort_details(tab_id, column, cx);
                        }))
                });
        let view = cx.entity();
        let visible_columns = column_layout.visible_columns();
        let column_menu = Button::new("details-columns")
            .icon(IconName::Settings)
            .accessibility_label("Configure details columns")
            .tooltip("Configure details columns")
            .ghost()
            .small()
            .compact()
            .dropdown_menu(move |mut menu, _, _| {
                menu = menu.item(PopupMenuItem::label("Visible columns"));
                for column in ColumnKey::ALL {
                    let view = view.clone();
                    menu = menu.item(
                        PopupMenuItem::new(column_label(column))
                            .checked(column_layout.is_visible(column))
                            .disabled(column == ColumnKey::Name)
                            .on_click(move |_, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.adjust_column(tab_id, ColumnAction::Toggle(column), cx);
                                });
                            }),
                    );
                }
                menu = menu.item(PopupMenuItem::separator());
                for column in visible_columns.iter().copied() {
                    for (label, action) in [
                        (
                            format!("Move {} left", column_label(column)),
                            ColumnAction::MoveLeft(column),
                        ),
                        (
                            format!("Move {} right", column_label(column)),
                            ColumnAction::MoveRight(column),
                        ),
                        (
                            format!("Narrow {}", column_label(column)),
                            ColumnAction::Narrower(column),
                        ),
                        (
                            format!("Widen {}", column_label(column)),
                            ColumnAction::Wider(column),
                        ),
                    ] {
                        let view = view.clone();
                        menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.adjust_column(tab_id, action, cx);
                            });
                        }));
                    }
                }
                menu
            });
        div()
            .id("details-header")
            .test_support()
            .role(Role::Toolbar)
            .aria_label("Details columns")
            .h(px(36.))
            .flex()
            .items_center()
            .gap_2()
            .px_4()
            .children(header)
            .child(column_menu)
            .into_any_element()
    }

    fn sort_details(&mut self, tab_id: TabId, column: ColumnKey, cx: &mut Context<Self>) {
        if let Some(directory) = self.directories.get_mut(&tab_id) {
            directory.view_mut().toggle_details_sort(column);
        }
        self.persist_view_preferences(tab_id, cx);
        cx.notify();
    }

    fn adjust_column(&mut self, tab_id: TabId, action: ColumnAction, cx: &mut Context<Self>) {
        let Some(directory) = self.directories.get_mut(&tab_id) else {
            return;
        };
        let columns = &mut directory.view_mut().preferences_mut().columns;
        let changed = match action {
            ColumnAction::Toggle(column) => {
                if columns.is_visible(column) {
                    columns.hide(column).is_ok()
                } else {
                    columns.show(column).is_ok()
                }
            }
            ColumnAction::MoveLeft(column) => columns.move_left(column).is_ok(),
            ColumnAction::MoveRight(column) => columns.move_right(column).is_ok(),
            ColumnAction::Narrower(column) | ColumnAction::Wider(column) => {
                let Some(width) = columns.width(column) else {
                    return;
                };
                let delta = if matches!(action, ColumnAction::Narrower(_)) {
                    -24.0
                } else {
                    24.0
                };
                columns
                    .resize(column, (f32::from(width) + delta).clamp(48.0, 1_024.0))
                    .is_ok()
            }
        };
        if changed {
            self.persist_view_preferences(tab_id, cx);
            cx.notify();
        }
    }

    fn item_render_spec(
        &self,
        tab_id: TabId,
        pane_index: usize,
        index: usize,
        layout: Layout,
    ) -> Option<ItemRenderSpec> {
        let view = self.directories.get(&tab_id)?.view();
        let item = self.filtered_items(tab_id).get(index)?.to_owned();
        Some(ItemRenderSpec {
            tab_id,
            pane_index,
            index,
            id: item.id().clone(),
            path: item.path().clone(),
            name: item.display_name().as_str().to_owned(),
            kind: item.kind(),
            size: item.size(),
            modified_unix_seconds: item.modified_unix_seconds(),
            columns: view.preferences().columns.visible_columns_with_widths(),
            layout,
            selected: view.selected_ids().contains(item.id()),
            focused: view
                .focused_item_id()
                .is_some_and(|focused| focused == item.id()),
        })
    }

    fn render_item(&mut self, spec: ItemRenderSpec, cx: &mut Context<Self>) -> AnyElement {
        let identity = match spec.kind {
            ItemKind::Directory => ContentIdentity::directory(),
            ItemKind::SymbolicLink => ContentIdentity::symbolic_link(),
            ItemKind::RegularFile | ItemKind::Other => ContentIdentity::GenericFile,
        };
        let icon_name = freedesktop_icon_name(&identity);
        let icon = self.content_icon(icon_name).map_or_else(
            || {
                Icon::new(if spec.kind == ItemKind::Directory {
                    IconName::Folder
                } else {
                    IconName::File
                })
                .large()
                .into_any_element()
            },
            |source| img(source).size(px(42.)).into_any_element(),
        );
        let colors = cx.theme().colors;
        let focused_unselected = spec.focused && !spec.selected;
        let item_id =
            SharedString::from(format!("directory-item-{}-{}", spec.pane_index, spec.index));
        let tab_id = spec.tab_id;
        let stable_id = spec.id.clone();
        let focused_id = spec.id.clone();
        let context_menu_id = spec.id.clone();
        let drag_payload = self.drag_payload(&spec);
        let is_drop_target = spec.kind == ItemKind::Directory;
        let drop_target = spec.path.clone();
        let item = match spec.layout {
            Layout::Cards | Layout::Grid | Layout::Adaptive => div()
                .id(item_id)
                .test_support()
                .role(Role::ListItem)
                .aria_label(spec.name.clone())
                .w(if spec.layout == Layout::Cards {
                    px(240.)
                } else {
                    px(128.)
                })
                .h(px(108.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_2()
                .rounded_md()
                .when(spec.selected, |item| {
                    item.bg(colors.list_active)
                        .border_1()
                        .border_color(colors.list_active_border)
                })
                .hover(|style| style.bg(colors.list_hover))
                .child(icon)
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_center()
                        .overflow_hidden()
                        .child(spec.name),
                ),
            Layout::List => div()
                .id(item_id)
                .test_support()
                .role(Role::ListItem)
                .aria_label(spec.name.clone())
                .h(px(38.))
                .w_full()
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .rounded_sm()
                .when(spec.selected, |item| {
                    item.bg(colors.list_active)
                        .border_1()
                        .border_color(colors.list_active_border)
                })
                .hover(|style| style.bg(colors.list_hover))
                .child(div().w(px(28.)).flex().justify_center().child(icon))
                .child(div().flex_grow(1.0).text_sm().child(spec.name))
                .child(
                    div()
                        .w(px(96.))
                        .text_right()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(spec.size.map_or_else(String::new, format_size)),
                ),
            Layout::Details | Layout::Columns => {
                let mut icon = Some(icon);
                let cells = spec
                    .columns
                    .iter()
                    .map(|(column, width)| {
                        let cell = div().w(px(f32::from(*width))).text_xs();
                        match column {
                            ColumnKey::Name => cell
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .w(px(28.))
                                        .flex()
                                        .justify_center()
                                        .child(icon.take().expect("name is a required column")),
                                )
                                .child(spec.name.clone()),
                            ColumnKey::Size => cell
                                .text_right()
                                .text_color(colors.muted_foreground)
                                .child(spec.size.map_or_else(|| "—".to_owned(), format_size)),
                            ColumnKey::Kind => cell
                                .text_color(colors.muted_foreground)
                                .child(item_kind_label(spec.kind)),
                            ColumnKey::Modified => cell.text_color(colors.muted_foreground).child(
                                spec.modified_unix_seconds
                                    .map_or_else(|| "—".to_owned(), |value| value.to_string()),
                            ),
                        }
                        .into_any_element()
                    })
                    .collect::<Vec<_>>();
                div()
                    .id(item_id)
                    .test_support()
                    .role(Role::ListItem)
                    .aria_label(spec.name)
                    .h(px(38.))
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_4()
                    .rounded_sm()
                    .when(spec.selected, |item| {
                        item.bg(colors.list_active)
                            .border_1()
                            .border_color(colors.list_active_border)
                    })
                    .hover(|style| style.bg(colors.list_hover))
                    .children(cells)
            }
        };
        let item = div()
            .id(SharedString::from(format!(
                "directory-item-focus-{}-{}",
                spec.pane_index, spec.index
            )))
            .when(focused_unselected, |item| {
                item.border_1().border_color(colors.ring)
            })
            .child(item);
        item.on_click(cx.listener(move |this, _, _, cx| {
            this.select_item(tab_id, stable_id.clone(), cx);
            this.focus_directory_item(tab_id, Some(focused_id.clone()), cx);
        }))
        .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |this, _, _, cx| {
                if !this.context_dialog_windows.is_empty() {
                    return;
                }
                this.pending_context_menu =
                    Some(this.item_context_menu(tab_id, context_menu_id.clone(), cx));
            }),
        )
        .when_some(drag_payload, |item, payload| {
            item.cursor_move()
                .on_drag(payload, |payload, position, _, cx| {
                    let label = if payload.sources().len() == 1 {
                        "Move 1 item".to_owned()
                    } else {
                        format!("Move {} items", payload.sources().len())
                    };
                    cx.new(|_| FileDragPreview { label, position })
                })
        })
        .when(is_drop_target, |item| {
            let can_target = drop_target.clone();
            let drop_target = drop_target.clone();
            let operation_hub = self.operation_hub.clone();
            item.can_drop(move |value, _, _| {
                value
                    .downcast_ref::<FileDragPayload>()
                    .is_some_and(|payload| operation_hub.can_accept_drop(payload, &can_target))
            })
            .on_drop(cx.listener(move |this, payload: &FileDragPayload, _, cx| {
                this.submit_file_drop(payload.clone(), drop_target.clone(), cx);
            }))
        })
        .into_any_element()
    }

    fn content_icon(&mut self, name: &str) -> Option<ImageSource> {
        self.icon_cache
            .entry(name.into())
            .or_insert_with(|| {
                FreedesktopLoader::new(name)
                    .size(48)
                    .load()
                    .and_then(|data| {
                        native_theme_gpui::icons::into_image_source(data, None, Some(48))
                    })
            })
            .clone()
    }

    fn focused_status_text(&self) -> String {
        let tab_id = self.navigation.focused_tab().id();
        if is_trash_location(self.navigation.focused_tab().location()) {
            return match self.trash_states.get(&tab_id) {
                Some(TrashState::Ready(surface)) => match surface.items().len() {
                    1 => "1 item in Trash".into(),
                    count => format!("{count} items in Trash"),
                },
                Some(TrashState::Error(_)) => "Trash unavailable".into(),
                Some(TrashState::Loading) | None => "Loading Trash".into(),
            };
        }
        if let Some(search) = self.searches.get(&tab_id) {
            return search.status_text();
        }
        let visible_count = self.filtered_items(tab_id).len();
        let view = self.focused_directory().view();
        let selected_bytes = view
            .selected_ids()
            .iter()
            .map(|id| view.item(id).and_then(StoreItem::size))
            .collect::<Option<Vec<_>>>()
            .map(|sizes| sizes.into_iter().sum());
        status_text_with_size(
            visible_count,
            view.selected_ids().len(),
            selected_bytes,
            view.is_complete(),
        )
    }

    fn operation_status_summary(&self) -> String {
        let custom_summary = self.custom_action_status_summary();
        let status = self.operation_hub.status();
        let Ok(status) = status.lock() else {
            return "Operation status unavailable".into();
        };
        let active = status
            .history()
            .into_iter()
            .filter(|entry| {
                matches!(
                    entry.status(),
                    OperationStatus::Pending | OperationStatus::Running | OperationStatus::Paused
                )
            })
            .collect::<Vec<_>>();
        match active.as_slice() {
            [entry] => match entry.total_items() {
                Some(total) => {
                    format!("{:?}: {} of {total}", entry.kind(), entry.completed_items())
                }
                None => format!("{:?}: {} complete", entry.kind(), entry.completed_items()),
            },
            entries @ [_, _, ..] => {
                let completed = entries
                    .iter()
                    .map(|entry| entry.completed_items())
                    .sum::<u64>();
                let total = entries
                    .iter()
                    .map(|entry| entry.total_items())
                    .collect::<Option<Vec<_>>>()
                    .map(|totals| totals.into_iter().sum::<u64>());
                match total {
                    Some(total) => {
                        format!(
                            "{} active operations: {completed} of {total}",
                            entries.len()
                        )
                    }
                    None => format!("{} active operations", entries.len()),
                }
            }
            [] => custom_summary.unwrap_or_else(|| {
                if status.history().is_empty() {
                    "No operations".into()
                } else {
                    format!("{} past operations", status.history().len())
                }
            }),
        }
    }

    fn operation_status_entries(&self) -> Vec<OperationStatusEntry> {
        self.operation_hub
            .status()
            .lock()
            .map(|status| status.visible_entries().into_iter().cloned().collect())
            .unwrap_or_default()
    }

    fn record_operation_control_error(
        &mut self,
        result: Result<(), crate::OperationHubError>,
        cx: &mut Context<Self>,
    ) -> bool {
        match result {
            Ok(()) => {
                self.operation_error = self.operation_hub.persistence_error();
                cx.notify();
                true
            }
            Err(error) => {
                self.operation_error = Some(error.to_string().into());
                cx.notify();
                false
            }
        }
    }

    fn pause_operation(&mut self, id: musheen_ops::JobId, cx: &mut Context<Self>) {
        let result = self.operation_hub.pause(id);
        self.record_operation_control_error(result, cx);
    }

    fn resume_operation(&mut self, id: musheen_ops::JobId, cx: &mut Context<Self>) {
        let result = self.operation_hub.resume(id);
        self.record_operation_control_error(result, cx);
    }

    fn cancel_operation(&mut self, id: musheen_ops::JobId, cx: &mut Context<Self>) {
        let result = self.operation_hub.cancel(id);
        self.record_operation_control_error(result, cx);
    }

    fn retry_operation(&mut self, id: musheen_ops::JobId, cx: &mut Context<Self>) {
        let result = self.operation_hub.retry(id);
        if self.record_operation_control_error(result, cx) {
            self.pump_operation_queue(cx);
        }
    }

    fn resume_recovery_operation(&mut self, id: musheen_ops::JobId, cx: &mut Context<Self>) {
        let result = self.operation_hub.resume_recovery(id);
        if self.record_operation_control_error(result, cx) {
            self.pump_operation_queue(cx);
        }
    }

    fn discard_recovery_operation(&mut self, id: musheen_ops::JobId, cx: &mut Context<Self>) {
        let result = self.operation_hub.discard_recovery(id);
        self.record_operation_control_error(result, cx);
    }

    fn view_operation_location(&mut self, location: StorePath, cx: &mut Context<Self>) {
        self.status_center_open = false;
        self.navigate(operation_browser_location(&location), true, cx);
    }

    fn dismiss_operation(&mut self, id: musheen_ops::JobId, cx: &mut Context<Self>) {
        let result = self.operation_hub.dismiss(id);
        self.record_operation_control_error(result, cx);
    }

    fn render_operation_status_center(&self, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.border
        };
        let entries = self.operation_status_entries();
        let rows = entries.into_iter().map(|entry| {
            let id = entry.id();
            let id_value = id.get();
            let status = entry.status();
            let can_retry = self.operation_hub.can_retry(id);
            let can_resume_recovery = self.operation_hub.can_resume_recovery(id);
            let can_discard_recovery = self.operation_hub.can_discard_recovery(id);
            let has_failures = !entry.failures().is_empty();
            let browser_location = operation_browser_location(entry.location());
            let location = DisplayPath::from_store_path(entry.location())
                .as_str()
                .to_owned();
            let label = format!("{:?} — {:?} — {location}", entry.kind(), status);
            let failures = entry
                .failures()
                .iter()
                .map(|failure| failure.message(entry.kind()))
                .collect::<Vec<_>>()
                .join(" ");
            div()
                .id(SharedString::from(format!("operation-status-{id_value}")))
                .test_support()
                .w_full()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(boundary)
                .child(
                    div()
                        .flex_grow(1.0)
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .text_sm()
                        .child(label)
                        .when(!failures.is_empty(), |row| {
                            row.child(
                                div()
                                    .text_xs()
                                    .text_color(colors.muted_foreground)
                                    .child(failures),
                            )
                        }),
                )
                .when(status == OperationStatus::Running, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("operation-pause-{id_value}")))
                            .label("Pause")
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pause_operation(id, cx);
                            })),
                    )
                })
                .when(status == OperationStatus::Paused, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("operation-resume-{id_value}")))
                            .label("Resume")
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.resume_operation(id, cx);
                            })),
                    )
                })
                .when(
                    matches!(
                        status,
                        OperationStatus::Pending
                            | OperationStatus::Running
                            | OperationStatus::Paused
                    ),
                    |row| {
                        row.child(
                            Button::new(SharedString::from(format!("operation-cancel-{id_value}")))
                                .label("Cancel")
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.cancel_operation(id, cx);
                                })),
                        )
                    },
                )
                .when(
                    can_retry
                        && matches!(
                            status,
                            OperationStatus::Failed
                                | OperationStatus::PartialSuccess
                                | OperationStatus::Interrupted
                        ),
                    |row| {
                        row.child(
                            Button::new(SharedString::from(format!("operation-retry-{id_value}")))
                                .label("Retry failed")
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.retry_operation(id, cx);
                                })),
                        )
                    },
                )
                .when(can_resume_recovery, |row| {
                    row.child(
                        Button::new(SharedString::from(format!(
                            "operation-recovery-resume-{id_value}"
                        )))
                        .label("Resume")
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.resume_recovery_operation(id, cx);
                        })),
                    )
                })
                .when(can_discard_recovery, |row| {
                    row.child(
                        Button::new(SharedString::from(format!(
                            "operation-recovery-discard-{id_value}"
                        )))
                        .label("Discard staging")
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.discard_recovery_operation(id, cx);
                        })),
                    )
                })
                .when(has_failures, |row| {
                    row.child(
                        Button::new(SharedString::from(format!("operation-view-{id_value}")))
                            .label("View location")
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.view_operation_location(browser_location.clone(), cx);
                            })),
                    )
                })
                .when(
                    !matches!(
                        status,
                        OperationStatus::Pending
                            | OperationStatus::Running
                            | OperationStatus::Paused
                    ),
                    |row| {
                        row.child(
                            Button::new(SharedString::from(format!(
                                "operation-dismiss-{id_value}"
                            )))
                            .label("Dismiss")
                            .ghost()
                            .small()
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.dismiss_operation(id, cx);
                                },
                            )),
                        )
                    },
                )
        });
        div()
            .id("operation-status-center")
            .test_support()
            .role(Role::Status)
            .aria_label("Operation history")
            .max_h(px(260.))
            .overflow_y_scroll()
            .bg(colors.background)
            .border_t_1()
            .border_color(boundary)
            .children(rows)
            .children(self.custom_action_status_rows(cx))
            .into_any_element()
    }
}

impl Render for MusheenApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.refresh_custom_actions(cx);
        let request = self.active_command_request(CommandAction::CustomAction);
        self.preflight_custom_actions(request.selection(), request.location().clone(), cx);
        if self.customization_keys.is_none() {
            let owner = cx.entity().downgrade();
            let window_id = window.window_handle().window_id();
            self.customization_keys = Some(cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle().window_id() == window_id {
                    let _ = owner.update(cx, |this, cx| {
                        this.route_custom_shortcut(&event.keystroke, window, cx)
                    });
                }
            }));
        }
        self.ensure_omnibar(window, cx);
        if let Some(mode) = self.requested_omnibar_mode.take() {
            self.activate_omnibar(mode, window, cx);
        } else if let Some(value) = self.pending_omnibar_value.take()
            && let Some(input) = self.omnibar_input.as_ref()
        {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if let Some(focus) = self.pending_restored_focus.take() {
            focus.focus(window, cx);
            self.pending_content_focus = false;
        } else if self.pending_content_focus {
            self.content_focus.focus(window, cx);
            self.pending_content_focus = false;
        }
        if self.keyboard_context_popup.is_none() {
            self.remember_browser_focus(window, cx);
        }
        let colors = cx.theme().colors;
        let status = self.focused_status_text();
        let operation_summary = self.operation_status_summary();
        let info_visible = self.shell.info_visible();
        let wide = window.viewport_size().width.as_f32() >= 960.0;
        let high_contrast = Self::high_contrast(cx);
        self.context_menu_theme = crate::ThemeProfile::from_active_native(
            cx.theme().mode.is_dark(),
            high_contrast,
            cx.try_global::<NativeTheme>()
                .is_some_and(|theme| theme.accessibility().reduce_motion),
        );
        let boundary = if high_contrast {
            colors.foreground
        } else {
            colors.border
        };
        let panes = if !info_visible || wide {
            self.render_panes(window, cx)
        } else {
            Vec::new()
        };
        let info = info_visible.then(|| self.render_info_pane(wide, cx));
        let operation_error = self
            .operation_error
            .as_deref()
            .map(|error| SharedString::from(format!("File operation failed: {error}")));
        div()
            .id("musheen-shell")
            .test_support()
            .capture_any_mouse_down(cx.listener(|this, _, window, cx| {
                if this.browser_input_blocked() {
                    cx.stop_propagation();
                    window.prevent_default();
                    this.activate_context_dialog(cx);
                } else {
                    this.remember_browser_focus(window, cx);
                }
            }))
            .capture_any_mouse_up(cx.listener(|this, _, window, cx| {
                if this.browser_input_blocked() {
                    cx.stop_propagation();
                    window.prevent_default();
                    this.activate_context_dialog(cx);
                }
            }))
            .capture_key_down(cx.listener(|this, _, window, cx| {
                if this.browser_input_blocked() {
                    cx.stop_propagation();
                    window.prevent_default();
                    this.activate_context_dialog(cx);
                } else if this.keyboard_context_popup.is_none() {
                    this.remember_browser_focus(window, cx);
                }
            }))
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.background)
            .text_color(colors.foreground)
            .border_color(boundary)
            .when(high_contrast, |shell| shell.border_2())
            .on_action(cx.listener(|this, _: &GoBack, _, cx| {
                this.dispatch_command("navigation.back", cx);
            }))
            .on_action(cx.listener(|this, _: &GoForward, _, cx| {
                this.dispatch_command("navigation.forward", cx);
            }))
            .on_action(cx.listener(|this, _: &GoParent, _, cx| {
                this.dispatch_command("navigation.parent", cx);
            }))
            .on_action(cx.listener(|this, _: &Reload, _, cx| {
                this.dispatch_command("navigation.refresh", cx);
            }))
            .on_action(cx.listener(|this, _: &EditLocation, _, cx| {
                this.dispatch_command("navigation.location", cx);
            }))
            .on_action(cx.listener(|this, _: &SearchLocation, _, cx| {
                this.dispatch_command("view.search", cx);
            }))
            .on_action(cx.listener(|this, _: &FilterLocation, _, cx| {
                this.dispatch_command("view.filter", cx);
            }))
            .on_action(cx.listener(|this, _: &OpenCommandMode, _, cx| {
                this.dispatch_command("view.command", cx);
            }))
            .on_action(cx.listener(|this, _: &NewTabShortcut, _, cx| {
                this.dispatch_command("tab.new", cx);
            }))
            .on_action(cx.listener(|this, _: &CloseTabShortcut, _, cx| {
                this.dispatch_command("tab.close", cx);
            }))
            .on_action(cx.listener(|this, _: &ReopenClosedTabShortcut, _, cx| {
                this.dispatch_command("tab.reopen_closed", cx);
            }))
            .on_action(cx.listener(|this, _: &SplitPaneShortcut, _, cx| {
                this.dispatch_command("pane.split", cx);
            }))
            .on_action(cx.listener(|this, _: &FocusNextPaneShortcut, _, cx| {
                this.dispatch_command("pane.focus_next", cx);
            }))
            .on_action(cx.listener(|this, _: &SelectAllShortcut, _, cx| {
                this.dispatch_command("selection.select_all", cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleHiddenShortcut, _, cx| {
                this.dispatch_command("view.hidden", cx);
            }))
            .on_action(cx.listener(|this, _: &ViewDetailsShortcut, _, cx| {
                this.dispatch_command("view.details", cx);
            }))
            .on_action(cx.listener(|this, _: &ViewListShortcut, _, cx| {
                this.dispatch_command("view.list", cx);
            }))
            .on_action(cx.listener(|this, _: &ViewCardsShortcut, _, cx| {
                this.dispatch_command("view.cards", cx);
            }))
            .on_action(cx.listener(|this, _: &ViewGridShortcut, _, cx| {
                this.dispatch_command("view.grid", cx);
            }))
            .on_action(cx.listener(|this, _: &ViewColumnsShortcut, _, cx| {
                this.dispatch_command("view.columns", cx);
            }))
            .on_action(cx.listener(|this, _: &ViewAdaptiveShortcut, _, cx| {
                this.dispatch_command("view.adaptive", cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebarShortcut, _, cx| {
                this.dispatch_command("view.sidebar", cx);
            }))
            .on_action(cx.listener(|this, _: &OpenPropertiesShortcut, _, cx| {
                this.dispatch_command("item.properties", cx);
            }))
            .on_action(
                cx.listener(|this, _: &OpenContextMenuShortcut, window, cx| {
                    this.open_keyboard_context_menu(window, cx);
                }),
            )
            .on_action(cx.listener(|this, _: &FocusNextDirectoryItem, _, cx| {
                this.move_directory_focus(1, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusPreviousDirectoryItem, _, cx| {
                this.move_directory_focus(-1, cx);
            }))
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                this.handle_escape(cx);
            }))
            .child(self.render_tab_strip(cx))
            .child(self.render_toolbar(window, cx))
            .child(self.render_custom_toolbar(cx))
            .children(self.custom_action_warning_row())
            .when_some(operation_error, |shell, message| {
                shell.child(
                    div()
                        .id("operation-error")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(message.clone())
                        .px_3()
                        .py_2()
                        .text_sm()
                        .bg(colors.background)
                        .border_b_1()
                        .border_color(boundary)
                        .child(message),
                )
            })
            .child(
                div()
                    .flex_grow(1.0)
                    .min_h(px(0.))
                    .flex()
                    .when(self.sidebar_visible, |content| {
                        content.child(self.render_sidebar(cx))
                    })
                    .children(panes)
                    .children(info),
            )
            .when(self.status_center_open, |shell| {
                shell.child(self.render_operation_status_center(cx))
            })
            .when_some(self.keyboard_context_popup.clone(), |shell, popup| {
                shell.child(
                    div()
                        .id("keyboard-context-menu")
                        .test_support()
                        .absolute()
                        .top(px(8.))
                        .bottom(px(8.))
                        .right(px(16.))
                        .child(popup),
                )
            })
            .child(
                div()
                    .id("status-bar")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(status.clone())
                    .h(px(28.))
                    .flex()
                    .items_center()
                    .px_3()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .bg(colors.background)
                    .border_t_1()
                    .border_color(boundary)
                    .child(status)
                    .child(div().flex_grow(1.0))
                    .child(
                        Button::new("operation-status-summary")
                            .label(operation_summary)
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.status_center_open = !this.status_center_open;
                                cx.notify();
                            })),
                    ),
            )
    }
}

fn place_path(label: &str, home: Option<&Path>) -> Option<StorePath> {
    if label == "Trash" {
        return Some(trash_store_path());
    }
    let home = home?;
    match label {
        "Home" => Some(StorePath::from_unix_path(home.as_os_str())),
        "Desktop" | "Documents" | "Downloads" => {
            Some(StorePath::from_unix_path(home.join(label).into_os_string()))
        }
        _ => None,
    }
}

fn operation_browser_location(path: &StorePath) -> StorePath {
    path.as_unix_path()
        .and_then(Path::parent)
        .map(|parent| StorePath::from_unix_path(parent.as_os_str()))
        .unwrap_or_else(|| path.clone())
}

fn trash_store_path() -> StorePath {
    StorePath::from_provider_key(
        ProviderId::new("musheen.trash").expect("the built-in Trash provider ID is valid"),
        b"root".to_vec(),
    )
    .expect("the built-in Trash path is valid")
}

fn trash_command_target(item: &TrashItem) -> CommandTargetRef {
    let provider = ProviderId::new("musheen.trash").expect("Trash provider ID is valid");
    let key = item.receipt().provider_reference().to_vec();
    CommandTargetRef::new(
        ItemId::new(provider.clone(), key.clone()).expect("Trash receipt identity is valid"),
        StorePath::from_provider_key(provider, key).expect("Trash receipt path is valid"),
    )
    .expect("Trash item and path share their provider")
}

fn is_trash_location(path: &StorePath) -> bool {
    path == &trash_store_path()
}

fn is_contextual_command(action: CommandAction) -> bool {
    matches!(
        action,
        CommandAction::Open
            | CommandAction::OpenWith
            | CommandAction::ChooseApplication
            | CommandAction::SetDefaultApplication
            | CommandAction::OpenInNewTab
            | CommandAction::OpenInNewWindow
            | CommandAction::OpenTerminalHere
            | CommandAction::OpenAsAdministrator
            | CommandAction::Run
            | CommandAction::RunAsAdministrator
            | CommandAction::Copy
            | CommandAction::Cut
            | CommandAction::PasteInto
            | CommandAction::CopyTo
            | CommandAction::MoveTo
            | CommandAction::SendTo
            | CommandAction::Rename
            | CommandAction::Duplicate
            | CommandAction::CreateSymbolicLink
            | CommandAction::CreateHardLink
            | CommandAction::Hide
            | CommandAction::Unhide
            | CommandAction::Compress
            | CommandAction::Extract
            | CommandAction::ExtractHere
            | CommandAction::NewDirectory
            | CommandAction::NewEmptyFile
            | CommandAction::NewFromTemplate
            | CommandAction::MoveToTrash
            | CommandAction::DeletePermanently
            | CommandAction::Restore
            | CommandAction::EmptyTrash
            | CommandAction::OpenProperties
            | CommandAction::DirectoryProperties
            | CommandAction::Permissions
            | CommandAction::CopyLocation
            | CommandAction::Unmount
            | CommandAction::Eject
            | CommandAction::PowerOff
            | CommandAction::Pin
            | CommandAction::Unpin
            | CommandAction::Share
    )
}

fn menu_icon(icon_key: Option<&str>) -> IconName {
    use crate::icons::LucideIcon;
    match crate::icons::lucide_icon_or_fallback(icon_key.unwrap_or("puzzle")) {
        LucideIcon::ArrowLeft => IconName::ArrowLeft,
        LucideIcon::ArrowRight => IconName::ArrowRight,
        LucideIcon::ArrowUp => IconName::ArrowUp,
        LucideIcon::RefreshCw => IconName::RefreshCw,
        LucideIcon::Search => IconName::Search,
        LucideIcon::ListFilter | LucideIcon::TextCursorInput => IconName::TextCursorInput,
        LucideIcon::List => IconName::List,
        LucideIcon::ListChecks => IconName::ListChecks,
        LucideIcon::Grid2x2 => IconName::Grid2x2,
        LucideIcon::Settings => IconName::Settings,
        LucideIcon::X => IconName::X,
        LucideIcon::Home => IconName::House,
        LucideIcon::Folder => IconName::Folder,
        LucideIcon::HardDrive => IconName::HardDrive,
        LucideIcon::Network => IconName::Network,
        LucideIcon::Trash => IconName::Trash,
        LucideIcon::Info => IconName::Info,
        LucideIcon::Plus => IconName::Plus,
        LucideIcon::Copy => IconName::Copy,
        LucideIcon::RotateCcw => IconName::RotateCcw,
        LucideIcon::File => IconName::File,
        LucideIcon::Puzzle => IconName::Puzzle,
        LucideIcon::Columns2 => IconName::Columns2,
        LucideIcon::PanelRight => IconName::PanelRight,
        LucideIcon::Scissors => IconName::Scissors,
        LucideIcon::Terminal => IconName::Terminal,
        LucideIcon::Eye => IconName::Eye,
        LucideIcon::EyeOff => IconName::EyeOff,
        LucideIcon::FileSymlink => IconName::FileSymlink,
        LucideIcon::Link2 => IconName::Link2,
        LucideIcon::Tag => IconName::Tag,
        LucideIcon::Shield => IconName::Shield,
        LucideIcon::ShieldCheck => IconName::ShieldCheck,
        LucideIcon::Pin => IconName::Pin,
        LucideIcon::PinOff => IconName::PinOff,
        LucideIcon::MapPin => IconName::MapPin,
        LucideIcon::Star => IconName::Star,
    }
}

fn is_hidden_path(path: &StorePath) -> bool {
    path.as_unix_path().is_some_and(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.') && name != "." && name != "..")
    })
}

fn is_archive_path(path: &StorePath) -> bool {
    path.as_unix_path().is_some_and(|path| {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        [
            ".zip", ".tar", ".tgz", ".tar.gz", ".tbz", ".tar.bz2", ".txz", ".tar.xz",
        ]
        .iter()
        .any(|extension| name.ends_with(extension))
    })
}

fn default_sidebar_model(pins: PinStore) -> SidebarModel {
    let mut model = SidebarModel::new(pins);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(home) = home.as_deref() {
        model.set_section_items(
            SidebarSectionKind::Home,
            [SidebarEntry::new(
                "Home",
                StorePath::from_unix_path(home.as_os_str()),
            )],
        );
        model.set_section_items(
            SidebarSectionKind::Places,
            ["Desktop", "Documents", "Downloads", "Trash"]
                .into_iter()
                .filter_map(|label| {
                    place_path(label, Some(home)).map(|path| SidebarEntry::new(label, path))
                }),
        );
    }
    model.set_section_items(
        SidebarSectionKind::Mounts,
        [SidebarEntry::new(
            "Computer",
            StorePath::from_unix_path("/"),
        )],
    );
    model
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1_024;
    const MIB: u64 = KIB * 1_024;
    const GIB: u64 = MIB * 1_024;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn item_kind_label(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::Directory => "Folder",
        ItemKind::RegularFile => "File",
        ItemKind::SymbolicLink => "Link",
        ItemKind::Other => "Other",
    }
}

fn load_info_pane(work: InfoPaneWork) -> Result<InfoPaneResult, Box<str>> {
    if work.cancellation().is_cancelled() {
        return Err("Preview cancelled".into());
    }
    let detected = MimeDetector::default()
        .detect(work.path())
        .map_err(|error| error.to_string().into_boxed_str())?;
    let mime_type: Box<str> = detected.mime_type().into();
    if work.details().kind() != ItemKind::RegularFile {
        return Ok(InfoPaneResult::Details { mime_type });
    }
    if mime_type.starts_with("image/") {
        let cache =
            ThumbnailCache::for_user().map_err(|error| error.to_string().into_boxed_str())?;
        let request = ThumbnailRequest::new(
            work.path(),
            work.details()
                .modified_unix_seconds()
                .and_then(|value| u64::try_from(value).ok())
                .unwrap_or(0),
            ThumbnailSize::Large,
        )
        .map_err(|error| error.to_string().into_boxed_str())?;
        let service = ThumbnailService::with_worker(
            cache,
            thumbnail_worker_path(),
            ThumbnailLimits::default(),
        );
        return match service
            .resolve(
                &request,
                ThumbnailMode::Generate,
                work.cancellation().clone(),
            )
            .map_err(|error| error.to_string().into_boxed_str())?
        {
            ThumbnailLookup::Hit(path) => Ok(InfoPaneResult::Thumbnail { mime_type, path }),
            ThumbnailLookup::Failed { reason } => Err(reason),
            ThumbnailLookup::Miss => Err("Thumbnail worker produced no preview".into()),
        };
    }
    let document = PreviewDocument::open(work.path(), work.cancellation().clone())
        .map_err(|error| error.to_string().into_boxed_str())?;
    Ok(InfoPaneResult::Preview {
        mime_type,
        document,
    })
}

fn thumbnail_worker_path() -> PathBuf {
    if let Some(path) =
        std::env::var_os("MUSHEEN_THUMBNAIL_WORKER").filter(|value| !value.is_empty())
    {
        return PathBuf::from(path);
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_default()
        .join("musheen-thumbnail-worker")
}

fn column_label(column: ColumnKey) -> &'static str {
    match column {
        ColumnKey::Name => "Name",
        ColumnKey::Size => "Size",
        ColumnKey::Kind => "Type",
        ColumnKey::Modified => "Modified",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Locale;
    use crate::search::SearchState;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use gpui_kit::{AnyWindowHandle, Modifiers, MouseButton, TestAppContext, VisualTestContext};
    use musheen_core::{
        CapabilityMatrix, CapabilityReason, CapabilityState, MutationRequest, PageRequest,
        ProviderId, SearchCapabilities, SearchResult, SearchScopeError,
    };
    use standard_library::fs as filesystem;
    use std as standard_library;
    use std::time::Duration;

    #[gpui_kit::test]
    async fn customization_keys_and_toolbar_use_live_registry_dispatch(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().unwrap();
        filesystem::write(temporary.path().join("item.txt"), b"test").unwrap();
        let child = temporary.path().join("child");
        filesystem::create_dir(&child).unwrap();
        filesystem::write(child.join("child.txt"), b"test").unwrap();
        let child = StorePath::from_unix_path(child.into_os_string());
        let mut app = None;
        let handle = cx.open_window(size(px(960.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update_window(handle.into(), |_, _, cx| {
            app.update(cx, |state, cx| state.navigate(child.clone(), true, cx));
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let mut document = musheen_desktop::SettingsDocument::default();
            let registry = musheen_core::CommandRegistry::built_in();
            let mut shortcuts = musheen_core::ShortcutMap::default();
            shortcuts
                .assign(
                    "view.sidebar",
                    musheen_core::ShortcutScope::Browser,
                    "ctrl-alt-b",
                    &registry,
                )
                .unwrap();
            shortcuts
                .clear(
                    "navigation.refresh",
                    musheen_core::ShortcutScope::Browser,
                    &registry,
                )
                .unwrap();
            shortcuts
                .assign(
                    "navigation.back",
                    musheen_core::ShortcutScope::Browser,
                    "ctrl-alt-left",
                    &registry,
                )
                .unwrap();
            document
                .set_value("shortcuts.bindings", &shortcuts.export())
                .unwrap();
            let mut toolbar = musheen_core::ToolbarLayout::default();
            toolbar.add("view.sidebar", &registry).unwrap();
            document
                .set_value("layout.toolbar", &toolbar.export())
                .unwrap();
            cx.set_global(crate::settings::RuntimeSettings(document));
            window.render_frame(cx);
            assert!(app.read(cx).sidebar_visible);
            window.press("ctrl-alt-b", cx);
            assert!(!app.read(cx).sidebar_visible);
            window.press("ctrl-b", cx);
            assert!(
                !app.read(cx).sidebar_visible,
                "removed static shortcut must not run"
            );
            window.press("f5", cx);
            assert_eq!(
                app.read(cx).focused_directory().state(),
                &DirectoryState::Ready,
                "removed F5 must not start a reload"
            );
            window.press("alt-left", cx);
            assert_eq!(app.read(cx).navigation.focused_tab().location(), &child);
            window.render_frame(cx);
            window.click("custom-toolbar-view.sidebar", cx);
            assert!(app.read(cx).sidebar_visible);
            window.press("ctrl-l", cx);
            window.render_frame(cx);
            assert!(
                app.read(cx)
                    .omnibar_input
                    .as_ref()
                    .unwrap()
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            window.press("ctrl-b", cx);
            assert!(
                app.read(cx).sidebar_visible,
                "removed browser shortcut must not bubble through omnibar focus"
            );
            window.press("f5", cx);
            assert_eq!(
                app.read(cx).focused_directory().state(),
                &DirectoryState::Ready,
                "removed F5 must not reload through omnibar focus"
            );
            window.press("alt-left", cx);
            assert_eq!(
                app.read(cx).navigation.focused_tab().location(),
                &child,
                "reassigned Alt+Left must not navigate through omnibar focus"
            );
            let input = app.read(cx).omnibar_input.as_ref().unwrap().clone();
            for rebind in [false, true] {
                let mut shortcuts = musheen_core::ShortcutMap::default();
                for (id, key) in [
                    ("selection.select_all", "ctrl-alt-a"),
                    ("clipboard.copy", "ctrl-alt-c"),
                    ("clipboard.cut", "ctrl-alt-x"),
                    ("clipboard.paste_into", "ctrl-alt-v"),
                ] {
                    if rebind {
                        shortcuts
                            .assign(id, musheen_core::ShortcutScope::Browser, key, &registry)
                            .unwrap();
                    } else {
                        shortcuts
                            .clear(id, musheen_core::ShortcutScope::Browser, &registry)
                            .unwrap();
                    }
                }
                let mut document = musheen_desktop::SettingsDocument::default();
                shortcuts
                    .assign(
                        "view.sidebar",
                        musheen_core::ShortcutScope::Global,
                        "ctrl-a",
                        &registry,
                    )
                    .unwrap();
                document
                    .set_value("shortcuts.bindings", &shortcuts.export())
                    .unwrap();
                cx.set_global(crate::settings::RuntimeSettings(document));
                input.update(cx, |input, cx| input.set_value("alpha beta", window, cx));
                window.render_frame(cx);
                window.press("end", cx);
                window.press("ctrl-a", cx);
                assert_eq!(
                    input.read(cx).selected_range(),
                    0..10,
                    "native select all, rebind={rebind}"
                );
                assert!(
                    app.read(cx).sidebar_visible,
                    "native editing wins over a custom global binding"
                );
                window.press("ctrl-c", cx);
                assert_eq!(
                    cx.read_from_clipboard().and_then(|item| item.text()),
                    Some("alpha beta".into())
                );
                window.press("ctrl-x", cx);
                assert_eq!(
                    input.read(cx).value().as_str(),
                    "",
                    "native cut, rebind={rebind}"
                );
                window.press("ctrl-v", cx);
                assert_eq!(
                    input.read(cx).value().as_str(),
                    "alpha beta",
                    "native paste, rebind={rebind}"
                );
                window.press("home", cx);
                assert_eq!(input.read(cx).selected_range(), 0..0);
                window.press("shift-end", cx);
                assert_eq!(input.read(cx).selected_range(), 0..10);
                window.press("backspace", cx);
                assert_eq!(input.read(cx).value().as_str(), "");
            }
            // Reset restores the static default while the omnibar remains focused.
            cx.set_global(crate::settings::RuntimeSettings(
                musheen_desktop::SettingsDocument::default(),
            ));
            window.press("ctrl-b", cx);
            assert!(!app.read(cx).sidebar_visible);
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn customization_tab_toolbar_tracks_live_navigation_state(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().unwrap();
        filesystem::write(temporary.path().join("item.txt"), b"test").unwrap();
        let mut app = None;
        let handle = cx.open_window(size(px(1200.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let mut document = musheen_desktop::SettingsDocument::default();
            document.set_value("layout.toolbar", "v1;navigation.location;tab.close;tab.reopen_closed;tab.move_left;tab.move_right").unwrap();
            cx.set_global(crate::settings::RuntimeSettings(document));
            for (trigger, expected) in [
                (None, [false, false, false, false]),
                (Some("ctrl-w"), [false, false, false, false]),
                (Some("ctrl-shift-t"), [false, false, false, false]),
                (Some("ctrl-t"), [true, false, true, false]),
                (Some("custom-toolbar-tab.move_left"), [true, false, false, true]),
                (Some("custom-toolbar-tab.close"), [false, true, false, false]),
                (Some("custom-toolbar-tab.reopen_closed"), [true, false, true, false]),
                (Some("ctrl-w"), [false, true, false, false]),
                (Some("ctrl-shift-t"), [true, false, true, false]),
            ] {
                if let Some(trigger) = trigger {
                    if trigger.starts_with("custom-toolbar-") {
                        window.click(trigger, cx);
                    } else {
                        window.press(trigger, cx);
                    }
                }
                window.render_frame(cx);
                for (id, enabled) in ["tab.close", "tab.reopen_closed", "tab.move_left", "tab.move_right"].into_iter().zip(expected) {
                    let state = app.read(cx);
                    let command = state.shell.commands().get(id).unwrap();
                    let command_state = command.state(&state.active_command_context(command.action()));
                    assert_eq!(command_state.is_enabled(), enabled, "{id} after {trigger:?}");
                    assert_eq!(command_state.disabled_reason().is_some(), !enabled);
                    assert!(window.find(format!("custom-toolbar-{id}")).visible());
                    if !enabled {
                        let before = state.navigation.clone();
                        window.click(format!("custom-toolbar-{id}"), cx);
                        assert_eq!(app.read(cx).navigation, before, "disabled {id} must be inert");
                    }
                }
            }
            window.remove_window();
        }).unwrap();
    }

    #[gpui_kit::test]
    async fn customization_tab_and_pane_capacity_matches_toolbar_and_shortcuts(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().unwrap();
        filesystem::write(temporary.path().join("item.txt"), b"test").unwrap();
        let location = StorePath::from_unix_path(temporary.path().as_os_str());
        let mut app = None;
        let handle = cx.open_window(size(px(1200.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(
                    temporary.path().to_path_buf(),
                    Some(SessionStore::at(temporary.path().join("session.json"))),
                    cx,
                )
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            let binding = app.read(cx).session_binding.clone().unwrap();
            let commands = [
                "tab.new",
                "tab.duplicate",
                "tab.reopen_closed",
                "tab.move_other_pane",
                "tab.tear_out",
                "pane.split",
                "pane.focus_next",
            ];
            let registry = musheen_core::CommandRegistry::built_in();
            let mut shortcuts = musheen_core::ShortcutMap::default();
            for (index, id) in commands.iter().enumerate() {
                shortcuts
                    .assign(
                        id,
                        musheen_core::ShortcutScope::Browser,
                        &format!("ctrl-shift-{}", index + 1),
                        &registry,
                    )
                    .unwrap();
            }
            let mut document = musheen_desktop::SettingsDocument::default();
            document
                .set_value(
                    "layout.toolbar",
                    &format!("v1;navigation.location;{}", commands.join(";")),
                )
                .unwrap();
            document
                .set_value("shortcuts.bindings", &shortcuts.export())
                .unwrap();
            cx.set_global(crate::settings::RuntimeSettings(document));
            // source tabs, destination tabs (zero = no other pane), windows, expected states
            for (source_tabs, destination_tabs, windows, expected) in [
                (1, 0, 1, [true, true, true, false, false, true, false]),
                (2, 0, 1, [true, true, true, false, true, true, false]),
                (128, 0, 1, [false, false, false, false, true, true, false]),
                (1, 1, 1, [true, true, true, false, false, false, true]),
                (2, 1, 1, [true, true, true, true, true, false, true]),
                (2, 128, 1, [true, true, true, false, true, false, true]),
                (
                    2,
                    0,
                    MAX_WINDOWS,
                    [true, true, true, false, false, true, false],
                ),
                (2, 0, 0, [true, true, true, false, false, true, false]),
            ] {
                let mut navigation = WindowSession::new(location.clone());
                navigation.new_tab(location.clone()).unwrap();
                navigation.close_active_tab().unwrap();
                for _ in 1..source_tabs {
                    navigation.new_tab(location.clone()).unwrap();
                }
                let source = navigation.focused_pane_id();
                if destination_tabs > 0 {
                    navigation.split_focused(location.clone()).unwrap();
                    for _ in 1..destination_tabs {
                        navigation.new_tab(location.clone()).unwrap();
                    }
                    navigation.focus_pane(source).unwrap();
                }
                binding.coordinator.lock().unwrap().windows = (1..=windows)
                    .map(|id| (id as u64, navigation.clone()))
                    .collect();
                app.update(cx, |state, cx| {
                    state.navigation = navigation;
                    state.session_binding = (windows > 0).then(|| binding.clone());
                    state.load_focused_tab(cx);
                });
                window.render_frame(cx);
                for (index, (id, enabled)) in commands.iter().zip(expected).enumerate() {
                    let state = app.read(cx);
                    let command = state.shell.commands().get(id).unwrap();
                    let command_state =
                        command.state(&state.active_command_context(command.action()));
                    assert_eq!(
                        command_state.is_enabled(),
                        enabled,
                        "{id}: tabs={source_tabs}, other={destination_tabs}, windows={windows}"
                    );
                    assert_eq!(command_state.disabled_reason().is_some(), !enabled);
                    assert!(window.find(format!("custom-toolbar-{id}")).visible());
                    if !enabled {
                        let before = state.navigation.clone();
                        window.click(format!("custom-toolbar-{id}"), cx);
                        window.press(&format!("ctrl-shift-{}", index + 1), cx);
                        assert!(
                            app.read(cx).navigation == before,
                            "disabled {id} changed navigation"
                        );
                        assert_eq!(binding.coordinator.lock().unwrap().windows.len(), windows);
                    }
                }
            }
            let navigation = WindowSession::new(location.clone());
            binding.coordinator.lock().unwrap().windows = vec![(1, navigation.clone())];
            app.update(cx, |state, cx| {
                state.navigation = navigation;
                state.session_binding = Some(binding.clone());
                state.load_focused_tab(cx);
            });
            window.render_frame(cx);
            window.click("custom-toolbar-tab.new", cx);
            assert_eq!(app.read(cx).navigation.focused_pane().tabs().len(), 2);
            window.press("ctrl-shift-2", cx);
            assert_eq!(app.read(cx).navigation.focused_pane().tabs().len(), 3);
            window.click("custom-toolbar-pane.split", cx);
            assert_eq!(app.read(cx).navigation.panes().len(), 2);
            window.press("ctrl-shift-7", cx);
            assert_eq!(app.read(cx).navigation.focused_pane().tabs().len(), 3);
            window.render_frame(cx);
            window.click("custom-toolbar-tab.move_other_pane", cx);
            assert_eq!(app.read(cx).navigation.focused_pane().tabs().len(), 2);
            window.press("ctrl-shift-4", cx);
            assert_eq!(app.read(cx).navigation.focused_pane().tabs().len(), 1);
            window.press("ctrl-shift-1", cx);
            window.render_frame(cx);
            window.click("custom-toolbar-tab.tear_out", cx);
            assert_eq!(app.read(cx).navigation.focused_pane().tabs().len(), 1);
            assert_eq!(binding.coordinator.lock().unwrap().windows.len(), 2);
            window.remove_window();
        })
        .unwrap();
    }

    struct ImmediateSearchStream(Option<SearchBatch>);

    impl SearchStream for ImmediateSearchStream {
        fn next_batch<'a>(
            &'a mut self,
            cancellation: CancellationToken,
        ) -> musheen_core::BoxFuture<'a, Result<Option<SearchBatch>, StoreError>> {
            let batch = self.0.take();
            Box::pin(async move {
                cancellation.check()?;
                Ok(batch)
            })
        }
    }

    #[derive(Clone, Copy)]
    enum ImmediateSearchOutcome {
        Complete,
        Partial,
        MissingTerminal,
    }

    struct ImmediateSearchStore {
        provider: ProviderId,
        outcome: ImmediateSearchOutcome,
    }

    impl ImmediateSearchStore {
        fn new() -> Self {
            Self {
                provider: ProviderId::new("search.test").unwrap(),
                outcome: ImmediateSearchOutcome::Complete,
            }
        }

        fn partial() -> Self {
            Self {
                provider: ProviderId::new("search.test.partial").unwrap(),
                outcome: ImmediateSearchOutcome::Partial,
            }
        }

        fn without_terminal_batch() -> Self {
            Self {
                provider: ProviderId::new("search.test.empty").unwrap(),
                outcome: ImmediateSearchOutcome::MissingTerminal,
            }
        }
    }

    impl Store for ImmediateSearchStore {
        fn provider_id(&self) -> &ProviderId {
            &self.provider
        }

        fn capabilities(&self, _location: &StorePath) -> CapabilityMatrix {
            CapabilityMatrix::new(|_| {
                CapabilityState::Unsupported(CapabilityReason::new("test store").unwrap())
            })
        }

        fn search_capabilities(&self, _location: &StorePath) -> SearchCapabilities {
            SearchCapabilities::all()
        }

        fn search<'a>(
            &'a self,
            scope: &'a StorePath,
            _query: SearchQuery,
            cancellation: CancellationToken,
        ) -> musheen_core::BoxFuture<'a, Result<Box<dyn SearchStream>, StoreError>> {
            if matches!(self.outcome, ImmediateSearchOutcome::MissingTerminal) {
                return Box::pin(async {
                    Ok(Box::new(ImmediateSearchStream(None)) as Box<dyn SearchStream>)
                });
            }
            let item = StoreItem::new(
                ItemId::new(self.provider.clone(), b"welcome".to_vec()).unwrap(),
                StorePath::from_unix_path("/fixture/Welcome.md"),
                DisplayPath::new("Welcome.md"),
                ItemKind::RegularFile,
                Some(7),
            );
            let result = cancellation.check().and_then(|()| {
                let errors = matches!(self.outcome, ImmediateSearchOutcome::Partial)
                    .then(|| {
                        vec![SearchScopeError::new(
                            StorePath::from_unix_path("/fixture/denied"),
                            "permission denied",
                            true,
                        )]
                    })
                    .unwrap_or_default();
                SearchBatch::new(
                    vec![SearchResult::new(item, Some("text/plain"))],
                    errors,
                    SearchCompletion::Complete,
                )
            });
            let _ = scope;
            Box::pin(async move {
                Ok(Box::new(ImmediateSearchStream(Some(result?))) as Box<dyn SearchStream>)
            })
        }

        fn read_directory<'a>(
            &'a self,
            _location: &'a StorePath,
            _request: PageRequest,
            _cancellation: CancellationToken,
        ) -> musheen_core::BoxFuture<'a, Result<Page<StoreItem>, StoreError>> {
            Box::pin(async { Err(StoreError::unsupported("read_directory", "test store")) })
        }

        fn watch_directory<'a>(
            &'a self,
            _location: &'a StorePath,
            _cancellation: CancellationToken,
        ) -> musheen_core::BoxFuture<'a, Result<Box<dyn DirectoryWatch>, StoreError>> {
            Box::pin(async { Err(StoreError::unsupported("watch_directory", "test store")) })
        }

        fn validate_mutation(&self, request: &MutationRequest) -> Result<(), StoreError> {
            Err(request.unsupported("test store"))
        }

        fn mutate<'a>(
            &'a self,
            request: MutationRequest,
            _cancellation: CancellationToken,
        ) -> musheen_core::BoxFuture<'a, Result<(), StoreError>> {
            Box::pin(async move { Err(request.unsupported("test store")) })
        }
    }

    #[test]
    fn preview_theme_parser_accepts_only_supported_profiles() {
        assert_eq!(preview_theme("light"), Some((false, false)));
        assert_eq!(preview_theme("dark"), Some((true, false)));
        assert_eq!(preview_theme("high-contrast"), Some((false, true)));
        assert_eq!(preview_theme("sepia"), None);
    }

    #[test]
    fn preview_window_width_is_bounded_for_usable_layouts() {
        assert_eq!(preview_window_width(Some("960")), 960.0);
        assert_eq!(preview_window_width(Some("320")), 720.0);
        assert_eq!(preview_window_width(Some("5000")), 1_920.0);
        assert_eq!(preview_window_width(Some("invalid")), 1_180.0);
        assert_eq!(preview_window_width(None), 1_180.0);
    }

    #[test]
    fn preview_window_height_is_bounded_for_usable_layouts() {
        assert_eq!(preview_window_height(Some("640")), 640.0);
        assert_eq!(preview_window_height(Some("300")), 480.0);
        assert_eq!(preview_window_height(Some("3000")), 1_200.0);
        assert_eq!(preview_window_height(None), 760.0);
    }

    #[test]
    fn grid_rows_cover_items_beyond_the_initial_viewport() {
        assert_eq!(grid_row_count(73, 6), 13);
        assert_eq!(grid_item_range(12, 73, 6), 72..73);
    }

    #[test]
    fn context_transfer_preflight_uses_the_operation_queue_for_copy_move_and_refusal() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let source = temporary.path().join("source.txt");
        let destination = temporary.path().join("destination");
        filesystem::write(&source, b"source").expect("source writes");
        filesystem::create_dir(&destination).expect("destination creates");
        let source = StorePath::from_unix_path(source.into_os_string());
        let destination = StorePath::from_unix_path(destination.into_os_string());
        let hub = OperationHub::new(&ResourceLimits::default());

        for action in [DropAction::Copy, DropAction::Move] {
            let payload = FileDragPayload::new(vec![source.clone()], action)
                .expect("one source is a valid transfer payload");
            let resolver = ContextTransferDestinationResolver {
                operation_hub: &hub,
                payload: &payload,
                catalog: &Catalog::load(Locale::EnUs).unwrap(),
            };
            assert!(
                resolver
                    .resolve_context_menu_destination(&destination)
                    .is_writable
            );
        }

        let payload = FileDragPayload::new(vec![source], DropAction::Copy)
            .expect("one source is a valid transfer payload");
        let resolver = ContextTransferDestinationResolver {
            operation_hub: &hub,
            payload: &payload,
            catalog: &Catalog::load(Locale::EnUs).unwrap(),
        };
        let refusal = resolver.resolve_context_menu_destination(&StorePath::from_unix_path(
            temporary.path().join("missing").into_os_string(),
        ));
        assert!(!refusal.is_writable);
        assert_eq!(
            refusal.refusal_reason.as_deref(),
            Some("the destination provider refused this operation")
        );
    }

    #[test]
    fn unavailable_context_actions_name_the_missing_production_capability() {
        assert!(
            MusheenApp::backend_action_state(CommandAction::Extract)
                .reason()
                .is_some_and(|reason| reason.contains("archive operation provider"))
        );
        assert!(
            MusheenApp::backend_action_state(CommandAction::OpenAsAdministrator)
                .reason()
                .is_some_and(|reason| reason.contains("authorization broker"))
        );
        assert!(
            MusheenApp::backend_action_state(CommandAction::OpenWith)
                .reason()
                .is_some_and(|reason| reason.contains("association backend"))
        );
    }

    #[test]
    fn session_restore_falls_back_to_the_last_valid_document() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let store = SessionStore::at(temporary.path().join("session.json"));
        let expected = StorePath::from_unix_path("/restored");
        let document = WindowSession::new(expected.clone())
            .to_json()
            .expect("session serializes");
        store.save(&document).expect("valid session saves");
        store
            .save(b"not-json")
            .expect("corrupt primary is installed");

        let restored =
            restore_application_session(&store, StorePath::from_unix_path("/fallback"), |_| true)
                .expect("backup session restores");

        assert_eq!(restored.windows()[0].focused_tab().location(), &expected);
    }

    #[test]
    fn saving_one_window_preserves_every_other_window() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let coordinator = Arc::new(Mutex::new(SessionCoordinator::new(
            SessionStore::at(temporary.path().join("session.json")),
            vec![
                WindowSession::new(StorePath::from_unix_path("/first")),
                WindowSession::new(StorePath::from_unix_path("/second")),
            ],
        )));
        let first_id = coordinator
            .lock()
            .expect("coordinator lock is available")
            .entries()[0]
            .0;
        let binding = SessionBinding {
            coordinator,
            window_id: first_id,
            operation_hub: OperationHub::new(&ResourceLimits::default()),
        };

        let prepared = binding
            .prepare_save(WindowSession::new(StorePath::from_unix_path("/changed")))
            .expect("coordinated save serializes");
        let restored = ApplicationSession::restore_json(
            &prepared.document,
            |_| true,
            StorePath::from_unix_path("/fallback"),
        )
        .expect("coordinated document restores");

        assert_eq!(restored.windows().len(), 2);
        assert_eq!(
            restored.windows()[0].focused_tab().location(),
            &StorePath::from_unix_path("/changed")
        );
        assert_eq!(
            restored.windows()[1].focused_tab().location(),
            &StorePath::from_unix_path("/second")
        );
    }

    #[test]
    fn detached_windows_share_one_application_operation_hub() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let coordinator = Arc::new(Mutex::new(SessionCoordinator::new(
            SessionStore::at(temporary.path().join("session.json")),
            vec![WindowSession::new(StorePath::from_unix_path("/first"))],
        )));
        let window_id = coordinator
            .lock()
            .expect("coordinator lock is available")
            .entries()[0]
            .0;
        let binding = SessionBinding {
            coordinator,
            window_id,
            operation_hub: OperationHub::new(&ResourceLimits::default()),
        };

        let detached = binding
            .append_window(WindowSession::new(StorePath::from_unix_path("/second")))
            .expect("detached window is accepted");

        assert!(Arc::ptr_eq(
            &binding.operation_hub.status(),
            &detached.operation_hub.status()
        ));
    }

    #[test]
    fn an_older_window_save_cannot_overwrite_a_newer_session() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let coordinator = Arc::new(Mutex::new(SessionCoordinator::new(
            SessionStore::at(temporary.path().join("session.json")),
            vec![WindowSession::new(StorePath::from_unix_path("/first"))],
        )));
        let window_id = coordinator
            .lock()
            .expect("coordinator lock is available")
            .entries()[0]
            .0;
        let binding = SessionBinding {
            coordinator,
            window_id,
            operation_hub: OperationHub::new(&ResourceLimits::default()),
        };
        let older = binding
            .prepare_save(WindowSession::new(StorePath::from_unix_path("/older")))
            .expect("older save prepares");
        let newer = binding
            .prepare_save(WindowSession::new(StorePath::from_unix_path("/newer")))
            .expect("newer save prepares");

        assert!(newer.save_if_current().expect("newer save succeeds"));
        assert!(!older.save_if_current().expect("stale save is skipped"));

        let document = binding
            .coordinator
            .lock()
            .expect("coordinator lock is available")
            .store
            .load()
            .expect("saved session loads")
            .expect("saved session exists");
        let restored = ApplicationSession::restore_json(
            &document,
            |_| true,
            StorePath::from_unix_path("/fallback"),
        )
        .expect("saved session restores");
        assert_eq!(
            restored.windows()[0].focused_tab().location(),
            &StorePath::from_unix_path("/newer")
        );
    }

    #[test]
    fn session_coordinator_rejects_windows_beyond_the_persisted_limit() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let mut coordinator = SessionCoordinator::new(
            SessionStore::at(temporary.path().join("session.json")),
            vec![WindowSession::new(StorePath::from_unix_path("/first"))],
        );

        for index in 1..MAX_WINDOWS {
            coordinator
                .append(WindowSession::new(StorePath::from_unix_path(format!(
                    "/window-{index}"
                ))))
                .expect("a window within the persistence limit is accepted");
        }

        assert!(!coordinator.can_append());
        assert!(matches!(
            coordinator.append(WindowSession::new(StorePath::from_unix_path("/overflow"))),
            Err(NavigationError::LimitReached("windows"))
        ));
        assert_eq!(coordinator.entries().len(), MAX_WINDOWS);
    }

    #[gpui_kit::test]
    async fn pseudo_localized_controls_keep_semantics_bounds_and_content_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(720.), px(480.)), |window, cx| {
            let view = cx.new(|cx| {
                let mut state = MusheenApp::new_with_session_store(fixture, None, cx);
                state.catalog = Catalog::load(Locale::EnXa).expect("the pseudo catalog is valid");
                state
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;

        cx.update_window(handle.into(), |_, window, cx| {
            window.set_scale_factor(2.0);
            window.render_frame(cx);
            assert_eq!(window.scale_factor(), 2.0);
            assert_eq!(window.find("navigation.back").label(), Some("⟦Ɓȧƈķ··⟧"));
            assert_eq!(window.find("directory-content").focused(), Some(true));

            let shell_bounds = window.find("musheen-shell").bounds();
            let snapshots = gpui_kit::base::test_support::snapshots(window);
            let interactive = snapshots
                .iter()
                .filter(|snapshot| {
                    snapshot.visible()
                        && matches!(snapshot.role(), Some(Role::Button | Role::TextInput))
                })
                .collect::<Vec<_>>();
            assert!(interactive.len() >= 12, "too few observed controls");
            for control in interactive {
                assert!(
                    control.role().is_some(),
                    "missing role: {:?}",
                    control.path()
                );
                assert!(
                    control
                        .label()
                        .is_some_and(|label| !label.trim().is_empty()),
                    "missing name: {:?}",
                    control.path()
                );
                let bounds = control.bounds();
                assert!(
                    bounds.origin.x >= shell_bounds.origin.x,
                    "left clip: {:?}",
                    control.path()
                );
                assert!(
                    bounds.origin.y >= shell_bounds.origin.y,
                    "top clip: {:?}",
                    control.path()
                );
                assert!(
                    bounds.bottom_right().x <= shell_bounds.bottom_right().x,
                    "right clip: {:?}",
                    control.path()
                );
                assert!(
                    bounds.bottom_right().y <= shell_bounds.bottom_right().y,
                    "bottom clip: {:?}",
                    control.path()
                );
            }
            assert_eq!(window.find("view.sidebar").checked(), Some(true));
            assert_eq!(window.find("view.info").checked(), Some(false));
            assert_eq!(window.find("pane.split").checked(), Some(false));
            assert!(
                app.read(cx)
                    .shell
                    .commands()
                    .get("app.settings")
                    .unwrap()
                    .state(
                        &app.read(cx)
                            .active_command_context(CommandAction::OpenSettings)
                    )
                    .is_enabled()
            );

            app.update(cx, |state, cx| {
                state.dispatch_command("view.details", cx);
            });
            window.render_frame(cx);
            assert_eq!(window.find("directory-content").focused(), Some(true));
        })
        .expect("test window remains open");
    }

    #[gpui_kit::test]
    async fn rendered_shell_registers_its_semantic_regions(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");

        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "tab-strip",
                "navigation-toolbar",
                "sidebar",
                "directory-content",
                "directory-items",
                "status-bar",
            ] {
                assert!(window.find(id).visible(), "missing visible region {id}");
            }
            assert_eq!(window.find("status-bar").label(), Some("2 items"));
            assert!(window.find("tab-0").visible());
            assert!(window.find("omnibar").visible());
            assert!(window.find("omnibar-path").visible());
            assert!(window.find("omnibar-search").visible());
            assert!(window.find("omnibar-command").visible());
            assert!(window.find("breadcrumbs").visible());
            assert!(window.find("breadcrumb-current").visible());
            assert!(window.find("sidebar-section-0").visible());
            window.click("view.sidebar", cx);
            assert!(window.try_find("sidebar").is_none());
            window.click("view.sidebar", cx);
            assert!(window.find("sidebar").visible());
            window.click("view.details", cx);
            assert!(window.find("details-header").visible());
            assert!(window.find("details-columns").visible());
            window.click("view.hidden", cx);
            assert_eq!(window.find("status-bar").label(), Some("3 items"));
            window.press("ctrl-shift-f", cx);
            window.render_frame(cx);
            assert_eq!(app.read(cx).omnibar.mode(), OmnibarMode::Filter);
            app.update(cx, |state, cx| {
                state.omnibar.enter(OmnibarMode::Filter, "name:Welcome");
                state.submit_omnibar(cx);
            });
            window.render_frame(cx);
            assert!(window.find("filter-summary").visible());
            assert_eq!(window.find("status-bar").label(), Some("1 item"));
            assert_eq!(app.read(cx).focused_directory().items().len(), 3);
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(window.try_find("filter-summary").is_none());
            assert_eq!(window.find("status-bar").label(), Some("3 items"));
            window.press("ctrl-a", cx);
            assert!(
                window
                    .find("status-bar")
                    .label()
                    .is_some_and(|label| label.starts_with("3 items selected"))
            );
            window.click("view.grid", cx);
            assert!(window.try_find("details-header").is_none());
            for (command, layout) in [
                ("view.list", Layout::List),
                ("view.cards", Layout::Cards),
                ("view.columns", Layout::Columns),
                ("view.adaptive", Layout::Adaptive),
                ("view.grid", Layout::Grid),
            ] {
                window.click(command, cx);
                assert_eq!(
                    app.read(cx).focused_directory().view().preferences().layout,
                    layout
                );
                assert!(window.find("directory-items").visible());
            }
            window.press("ctrl-f", cx);
            assert_eq!(app.read(cx).omnibar.mode(), OmnibarMode::Search);
            window.press("escape", cx);
            assert_eq!(app.read(cx).omnibar.mode(), OmnibarMode::Path);
            window.press("ctrl-t", cx);
            assert!(window.find("tab-1").visible());
            window.press("f3", cx);
            assert!(window.find("pane-1").visible());
            assert!(window.try_find("info-pane").is_none());
            window.click("view.info", cx);
            assert!(window.find("info-pane").visible());
            window.click("view.info", cx);
            assert!(window.try_find("info-pane").is_none());
        })
        .expect("test window remains open");
    }

    #[gpui_kit::test]
    fn destination_path_is_typed_and_invalid_or_cancelled_input_never_dispatches(
        cx: &mut TestAppContext,
    ) {
        use std::cell::RefCell;
        use std::rc::Rc;
        cx.update(gpui_kit::init);
        for cancel in [false, true] {
            let events = Rc::new(RefCell::new(Vec::new()));
            let mut subscription = None;
            let handle = cx.open_window(size(px(520.), px(540.)), |window, cx| {
                let strings =
                    ContextDialogStrings::from_catalog(&Catalog::load(Locale::EnUs).unwrap());
                let dialog =
                    cx.new(|cx| ContextDestinationDialog::new(Vec::new(), strings, window, cx));
                let events = events.clone();
                subscription = Some(cx.subscribe(
                    &dialog,
                    move |_, _, event: &ContextDestinationEvent, _| {
                        events.borrow_mut().push(event.clone())
                    },
                ));
                Root::new(dialog, window, cx)
            });
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.click("context-destination-path", cx);
                window.input("relative/path", cx);
                window.click("context-destination-use-path", cx);
                window.render_frame(cx);
                assert!(window.find("context-destination-error").visible());
                assert!(events.borrow().is_empty());
                window.click("context-destination-path", cx);
                window.press("ctrl-a", cx);
                window.input("/tmp/arbitrary folder", cx);
                window.click(
                    if cancel {
                        "context-destination-cancel"
                    } else {
                        "context-destination-use-path"
                    },
                    cx,
                );
            })
            .unwrap();
            cx.run_until_parked();
            assert_eq!(events.borrow().len(), 1);
            match &events.borrow()[0] {
                ContextDestinationEvent::Chosen(path) => {
                    assert!(!cancel);
                    assert_eq!(path, &StorePath::from_unix_path("/tmp/arbitrary folder"));
                }
                ContextDestinationEvent::Cancelled => assert!(cancel),
            }
            drop(subscription);
        }
    }

    #[gpui_kit::test]
    async fn trash_registry_requires_live_receipts_and_restore_collision_close_cancels_only_its_token(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().unwrap();
        let destination = temporary.path().join("existing.txt");
        filesystem::write(&destination, b"keep this file").unwrap();
        let mut app = None;
        let handle = cx.open_window(size(px(1180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        let prior_focus = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.press("ctrl-l", cx);
                let focus = window.focused(cx).unwrap();
                app.update(cx, |state, cx| {
                    state.remember_context_invocation_focus(window, cx)
                });
                focus
            })
            .unwrap();
        let (first, second) = app.update(cx, |state, cx| {
            let tab = state.navigation.focused_tab().id();
            let receipt = musheen_ops::TrashReceipt::new(StorePath::from_unix_path(&destination), b"captured-trash-receipt".to_vec());
            let item = TrashItem::new(receipt.clone(), 1);
            let target = trash_command_target(&item);
            state.navigation.navigate_focused(trash_store_path());
            for trash_state in [TrashState::Loading, TrashState::Error("unavailable".into()), TrashState::Ready(TrashSurfaceModel::new(Vec::new())), TrashState::Ready(TrashSurfaceModel::new(vec![item]))] {
                let available = matches!(&trash_state, TrashState::Ready(surface) if !surface.items().is_empty());
                state.trash_states.insert(tab, trash_state);
                for (menu_target, id) in [(MenuTarget::TrashItem, "trash.restore"), (MenuTarget::TrashBackground, "trash.empty")] {
                    let menu = state.compose_context_menu(tab, menu_target, vec![target.clone()]);
                    let entry = MusheenApp::menu_entry_by_id(&menu, id).unwrap();
                    assert_eq!(entry.state().is_enabled(), available);
                    if available {
                        assert_eq!(entry.captured_targets(), std::slice::from_ref(&target));
                        let surface = state.shell.context_menus();
                        let mut dispatcher = AppMenuDispatcher::default();
                        let invocation = surface.invoke(entry, &mut dispatcher);
                        let expected_action = if id == "trash.empty" {
                            assert!(matches!(&invocation, MenuInvocation::NeedsConfirmation(pending) if pending.selection() == std::slice::from_ref(&target)));
                            surface.confirm(invocation, &mut dispatcher).unwrap();
                            CommandAction::EmptyTrash
                        } else {
                            assert!(matches!(invocation, MenuInvocation::Dispatched));
                            CommandAction::Restore
                        };
                        assert_eq!(dispatcher.dispatched.as_ref().unwrap().0, expected_action);
                    }
                }
            }
            let wrong = CommandTargetRef::new(target.id().clone(), StorePath::from_provider_key(target.id().provider().clone(), b"different".to_vec()).unwrap()).unwrap();
            let menu = state.compose_context_menu(tab, MenuTarget::TrashItem, vec![wrong]);
            assert!(!MusheenApp::menu_entry_by_id(&menu, "trash.restore").unwrap().state().is_enabled());
            let conflict = ConflictRecord::new(OperationKind::Restore, target.path().clone(), receipt.provider_reference().to_vec(), ConflictItemKind::File, receipt.original_path().clone(), b"destination-identity".to_vec(), ConflictItemKind::File).unwrap();
            state.open_restore_conflict(tab, receipt.clone(), conflict.clone(), cx);
            state.open_restore_conflict(tab, receipt, conflict, cx);
            assert!(state.browser_input_blocked());
            (state.context_dialog_windows[0].id, state.context_dialog_windows[1].id)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("omnibar-search", cx);
            window.right_click("directory-content", cx);
            window.render_frame(cx);
            assert!(window.try_find("popup-menu").is_none());
            assert_eq!(app.read(cx).omnibar.mode(), OmnibarMode::Path);
        })
        .unwrap();
        for id in [first, second] {
            cx.update(|cx| {
                let dialog = cx
                    .windows()
                    .into_iter()
                    .find(|handle| handle.window_id() == id)
                    .unwrap();
                dialog
                    .update(cx, |_, window, _| window.remove_window())
                    .unwrap();
            });
            cx.run_until_parked();
            app.update(cx, |state, cx| {
                assert!(!state.pending_restores.contains_key(&id));
                if id == first {
                    assert!(state.pending_restores.contains_key(&second));
                }
                state.handle_restore_conflict_event(
                    id,
                    &ConflictDialogEvent::Resolved(
                        ConflictChoice::Replace,
                        ApplyScope::ThisConflict,
                    ),
                    cx,
                );
            });
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!app.read(cx).browser_input_blocked());
            assert!(prior_focus.is_focused(window));
        })
        .unwrap();
        assert_eq!(filesystem::read(&destination).unwrap(), b"keep this file");
    }

    #[gpui_kit::test]
    async fn context_move_chooser_review_keeps_origin_focus_and_runs_the_queue(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source.txt");
        let destination = temporary.path().join("destination");
        filesystem::write(&source, b"reviewed transfer").unwrap();
        filesystem::create_dir(&destination).unwrap();
        let mut app = None;
        let handle = cx.open_window(size(px(1180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        let prior_focus = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.press("ctrl-l", cx);
                let focus = window.focused(cx).unwrap();
                app.update(cx, |state, cx| {
                    state.catalog = Catalog::load(Locale::EnXa).unwrap();
                    let tab = state.navigation.focused_tab().id();
                    let item = state
                        .focused_directory()
                        .view()
                        .items()
                        .iter()
                        .find(|item| item.path().as_unix_path() == Some(source.as_path()))
                        .unwrap()
                        .clone();
                    state.select_item(tab, item.id().clone(), cx);
                    let menu = state.compose_context_menu(
                        tab,
                        MenuTarget::Item,
                        vec![
                            CommandTargetRef::new(item.id().clone(), item.path().clone()).unwrap(),
                        ],
                    );
                    let entry = MusheenApp::menu_entry_by_id(&menu, "clipboard.move_to")
                        .unwrap()
                        .clone();
                    assert!(state.context_destination_choices(tab).iter().all(|choice| {
                        choice.location.as_unix_path() != Some(destination.as_path())
                    }));
                    state.remember_context_invocation_focus(window, cx);
                    state.dispatch_context_entry(entry, cx);
                });
                focus
            })
            .unwrap();
        let browser: AnyWindowHandle = handle.into();
        let chooser = cx
            .windows()
            .into_iter()
            .find(|window| *window != browser)
            .unwrap();
        cx.update(|cx| {
            assert_eq!(
                app.read(cx).context_dialog_windows[0].restore_focus,
                prior_focus
            )
        });
        cx.update_window(chooser, |_, window, cx| {
            window.render_frame(cx);
            assert!(
                window
                    .find("context-destination-dialog")
                    .label()
                    .unwrap()
                    .starts_with('⟦')
            );
            assert!(
                window
                    .find("context-destination-use-path")
                    .bounds()
                    .bottom_right()
                    .y
                    <= window.viewport_size().height,
                "arbitrary destination control must fit in chooser"
            );
            window.click("context-destination-path", cx);
            window.input(destination.to_str().unwrap(), cx);
            window.click("context-destination-use-path", cx);
        })
        .unwrap();
        cx.update(|cx| {
            assert!(
                app.read(cx).operation_error.is_none(),
                "{:?}",
                app.read(cx).operation_error
            )
        });
        cx.wait_for(browser, Duration::from_secs(2), |_, cx| {
            cx.windows()
                .iter()
                .any(|window| *window != browser && *window != chooser)
        })
        .await;
        let review = cx
            .windows()
            .into_iter()
            .find(|window| *window != browser)
            .unwrap();
        cx.update(|cx| {
            assert_eq!(
                app.read(cx).context_dialog_windows[0].restore_focus,
                prior_focus
            )
        });
        cx.update_window(review, |_, window, cx| {
            window.render_frame(cx);
            for id in [
                "context-command-review",
                "context-review-command",
                "context-review-targets",
                "context-review-explanation",
            ] {
                assert!(window.find(id).label().unwrap().starts_with('⟦'));
            }
            assert!(
                source.exists(),
                "chooser must not perform the reviewed move"
            );
            window.click("context-review-confirm", cx);
        })
        .unwrap();
        cx.wait_for(browser, Duration::from_secs(2), |_, _| {
            destination.join("source.txt").exists()
        })
        .await;
        cx.wait_for(browser, Duration::from_secs(2), |_, cx| {
            !app.read(cx)
                .operation_hub
                .status()
                .lock()
                .unwrap()
                .history()
                .is_empty()
        })
        .await;
        cx.run_until_parked();
        assert!(!source.exists());
        cx.update_window(browser, |_, window, cx| {
            window.render_frame(cx);
            assert!(app.read(cx).context_dialog_windows.is_empty());
            assert!(prior_focus.is_focused(window));
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn drop_conflicts_block_browser_input_and_clear_only_their_own_pending_drop(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source.txt");
        let destination = temporary.path().join("destination");
        filesystem::write(&source, b"source").unwrap();
        filesystem::create_dir(&destination).unwrap();
        filesystem::write(destination.join("source.txt"), b"existing").unwrap();
        let second_source = temporary.path().join("second.txt");
        filesystem::write(&second_source, b"second source").unwrap();
        filesystem::write(destination.join("second.txt"), b"second existing").unwrap();
        let mut app = None;
        let handle = cx.open_window(size(px(1180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        let browser: AnyWindowHandle = handle.into();
        cx.wait_for(browser, Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        let prior_focus = cx
            .update_window(browser, |_, window, cx| {
                window.render_frame(cx);
                window.press("ctrl-l", cx);
                window.focused(cx).unwrap()
            })
            .unwrap();
        let mut previous_window = None;
        for finish in ["window-manager", "cancel", "confirm"] {
            // Exercise the real menu -> destination -> review -> collision path.
            let choice = cx
                .update_window(browser, |_, window, cx| {
                    app.update(cx, |state, cx| {
                        state.pins.replace([SidebarEntry::new(
                            "Destination",
                            StorePath::from_unix_path(destination.as_os_str()),
                        )]);
                        let tab = state.navigation.focused_tab().id();
                        let item = state
                            .focused_directory()
                            .view()
                            .items()
                            .iter()
                            .find(|item| item.path().as_unix_path() == Some(source.as_path()))
                            .unwrap()
                            .clone();
                        state.select_item(tab, item.id().clone(), cx);
                        let targets = state
                            .focused_directory()
                            .view()
                            .items()
                            .iter()
                            .filter(|item| {
                                [Some(source.as_path()), Some(second_source.as_path())]
                                    .contains(&item.path().as_unix_path())
                            })
                            .map(|item| {
                                CommandTargetRef::new(item.id().clone(), item.path().clone())
                                    .unwrap()
                            })
                            .collect();
                        let menu = state.compose_context_menu(tab, MenuTarget::Item, targets);
                        let entry = MusheenApp::menu_entry_by_id(&menu, "clipboard.move_to")
                            .unwrap()
                            .clone();
                        let choice = state
                            .context_destination_choices(tab)
                            .iter()
                            .position(|choice| {
                                choice.location.as_unix_path() == Some(destination.as_path())
                            })
                            .unwrap();
                        state.remember_context_invocation_focus(window, cx);
                        state.dispatch_context_entry(entry, cx);
                        choice
                    })
                })
                .unwrap();
            let chooser = cx
                .windows()
                .into_iter()
                .find(|window| *window != browser)
                .unwrap();
            cx.update_window(chooser, |_, window, cx| {
                window.render_frame(cx);
                window.click(format!("context-destination-{choice}"), cx);
            })
            .unwrap();
            let review = cx
                .windows()
                .into_iter()
                .find(|window| *window != browser)
                .unwrap();
            cx.update_window(review, |_, window, cx| {
                window.render_frame(cx);
                window.click("context-review-confirm", cx);
            })
            .unwrap();
            let conflict = cx
                .windows()
                .into_iter()
                .find(|window| *window != browser)
                .unwrap();
            cx.update_window(conflict, |_, window, cx| {
                window.render_frame(cx);
                assert!(window.find("conflict-dialog").visible());
            })
            .unwrap();
            cx.update_window(browser, |_, window, cx| {
                window.render_frame(cx);
                assert!(app.read(cx).browser_input_blocked());
                assert_eq!(
                    app.read(cx).context_dialog_windows[0].restore_focus,
                    prior_focus
                );
                window.click("omnibar-search", cx);
                assert_eq!(app.read(cx).omnibar.mode(), OmnibarMode::Path);
                window.right_click("directory-content", cx);
                window.render_frame(cx);
                assert!(window.try_find("popup-menu").is_none());
                app.update(cx, |state, cx| {
                    let location = state.navigation.focused_tab().location().clone();
                    state.navigate(StorePath::from_unix_path("/tmp"), true, cx);
                    assert_eq!(state.navigation.focused_tab().location(), &location);
                    let layout = state.focused_directory().view().preferences().layout;
                    state.dispatch_command("view.list", cx);
                    assert_eq!(
                        state.focused_directory().view().preferences().layout,
                        layout
                    );
                    let payload = FileDragPayload::new(
                        vec![StorePath::from_unix_path(source.as_os_str())],
                        DropAction::Copy,
                    )
                    .unwrap();
                    state.submit_file_drop(payload.clone(), StorePath::from_unix_path("/tmp"), cx);
                    state.submit_reviewed_transfer(payload, StorePath::from_unix_path("/tmp"), cx);
                    if let Some(previous) = previous_window {
                        state.handle_drop_conflict_event(
                            previous,
                            &ConflictDialogEvent::Cancelled,
                            cx,
                        );
                        state.handle_drop_conflict_event(
                            previous,
                            &ConflictDialogEvent::Resolved(
                                ConflictChoice::Skip,
                                ApplyScope::ThisConflict,
                            ),
                            cx,
                        );
                    }
                    let pending = state.pending_drop.as_ref().unwrap();
                    assert_eq!(pending.conflicts.len(), 2);
                    assert_eq!(pending.conflict_window, Some(conflict.window_id()));
                    assert_eq!(pending.target.as_unix_path(), Some(destination.as_path()));
                    assert_eq!(pending.next_conflict, 0);
                });
            })
            .unwrap();
            cx.update_window(conflict, |_, window, cx| {
                window.render_frame(cx);
                window.click("conflict-choice-Skip", cx);
                window.click("conflict-confirm", cx);
            })
            .unwrap();
            let next_conflict = cx
                .windows()
                .into_iter()
                .find(|window| *window != browser)
                .unwrap_or_else(|| {
                    cx.update(|cx| {
                        let state = app.read(cx);
                        panic!(
                            "missing next conflict: pending {}, error {:?}",
                            state.pending_drop.is_some(),
                            state.operation_error
                        );
                    })
                });
            assert_ne!(next_conflict, conflict);
            cx.update(|cx| {
                let state = app.read(cx);
                assert!(state.browser_input_blocked());
                let pending = state.pending_drop.as_ref().unwrap();
                assert_eq!(pending.next_conflict, 1);
                assert_eq!(pending.conflict_window, Some(next_conflict.window_id()));
                assert_eq!(state.context_dialog_windows[0].restore_focus, prior_focus);
            });
            cx.update_window(next_conflict, |_, window, cx| {
                window.render_frame(cx);
                match finish {
                    "window-manager" => window.remove_window(),
                    "cancel" => window.click("conflict-cancel", cx),
                    _ => {
                        window.click("conflict-choice-Skip", cx);
                        window.click("conflict-confirm", cx);
                    }
                }
            })
            .unwrap();
            cx.run_until_parked();
            cx.update_window(browser, |_, window, cx| {
                window.render_frame(cx);
                assert!(app.read(cx).context_dialog_windows.is_empty());
                assert!(app.read(cx).pending_drop.is_none());
                assert!(!app.read(cx).browser_input_blocked());
                assert!(prior_focus.is_focused(window));
            })
            .unwrap();
            assert_eq!(filesystem::read(&source).unwrap(), b"source");
            assert_eq!(filesystem::read(&second_source).unwrap(), b"second source");
            assert_eq!(
                filesystem::read(destination.join("second.txt")).unwrap(),
                b"second existing"
            );
            assert_eq!(
                filesystem::read(destination.join("source.txt")).unwrap(),
                b"existing"
            );
            previous_window = Some(next_conflict.window_id());
        }
    }

    #[gpui_kit::test]
    async fn sidebar_identity_is_resolved_outside_the_directory_cache_and_revalidated(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temporary = tempfile::tempdir().unwrap();
        let current = temporary.path().join("current");
        let external = temporary.path().join("external");
        filesystem::create_dir(&current).unwrap();
        filesystem::create_dir(&external).unwrap();
        let mut app = None;
        let handle = cx.open_window(size(px(1180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(current, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Empty
        })
        .await;
        app.update(cx, |state, _| {
            let tab = state.navigation.focused_tab().id();
            for target in [MenuTarget::SidebarLocation, MenuTarget::Mount] {
                let menu = state.sidebar_location_context_menu(
                    tab,
                    target,
                    StorePath::from_unix_path(external.as_os_str()),
                );
                let entry = MusheenApp::menu_entry_by_id(&menu, "directory.properties").unwrap();
                assert_eq!(entry.captured_targets().len(), 1);
                assert!(
                    state
                        .revalidate_context_targets(Some(tab), entry.captured_targets())
                        .is_ok()
                );
                filesystem::rename(&external, temporary.path().join("old")).unwrap();
                filesystem::create_dir(&external).unwrap();
                assert!(
                    state
                        .revalidate_context_targets(Some(tab), entry.captured_targets())
                        .is_err()
                );
                filesystem::remove_dir(temporary.path().join("old")).unwrap();
            }
            state.store = Arc::new(ImmediateSearchStore::new());
            let menu = state.sidebar_location_context_menu(
                tab,
                MenuTarget::SidebarLocation,
                StorePath::from_unix_path(external.as_os_str()),
            );
            assert!(
                menu.entries()
                    .iter()
                    .all(|entry| entry.captured_targets().is_empty()),
                "an unresolved provider target must never acquire a fabricated local identity"
            );
        });
    }

    #[gpui_kit::test]
    async fn keyboard_context_menu_uses_the_focused_pane_target(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press("down", cx);
            assert!(
                app.read(cx)
                    .focused_directory()
                    .view()
                    .selected_ids()
                    .is_empty()
            );
            assert!(
                app.read(cx)
                    .focused_directory()
                    .view()
                    .focused_item_id()
                    .is_some()
            );
            window.press("shift-f10", cx);
            window.render_frame(cx);
            assert!(window.find("keyboard-context-menu").visible());
            assert!(window.find("popup-menu").visible());
            let row_id = format!(
                "context-menu-row-keyboard-{:?}-0",
                app.read(cx).navigation.focused_tab().id()
            );
            assert!(window.find(row_id).visible());
            window.press("escape", cx);
            window.render_frame(cx);
            assert!(window.try_find("keyboard-context-menu").is_none());
            assert_eq!(window.find("directory-content").focused(), Some(true));
        })
        .expect("test window remains open");
    }

    #[gpui_kit::test]
    async fn closing_a_context_modal_restores_content_focus_without_dispatching(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        let mut dialog_handle: Option<AnyWindowHandle> = None;
        let prior_focus = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.press("ctrl-l", cx);
                window.focused(cx).expect("location editor is focused")
            })
            .unwrap();
        cx.update(|cx| {
            let strings = ContextDialogStrings::from_catalog(
                &Catalog::load(Locale::EnUs).expect("English catalog loads"),
            );
            let dialog = cx
                .open_window(WindowOptions::default(), |window, cx| {
                    let view =
                        cx.new(|cx| ContextDestinationDialog::new(Vec::new(), strings, window, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("context modal opens");
            dialog_handle = Some(dialog.into());
            app.update(cx, |state, cx| {
                state.track_context_dialog_window(dialog.window_id(), None, cx);
                assert_eq!(state.context_dialog_windows.len(), 1);
            });
        });
        let dialog_handle = dialog_handle.expect("dialog handle is retained");
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("omnibar-search", cx);
            assert_eq!(app.read(cx).omnibar.mode(), OmnibarMode::Path);
            assert!(prior_focus.is_focused(window));
            window.right_click("directory-content", cx);
            window.render_frame(cx);
            assert!(window.try_find("popup-menu").is_none());
            assert!(app.read(cx).pending_context_menu.is_none());
        })
        .unwrap();
        cx.update(|cx| {
            app.update(cx, |state, cx| {
                let before = state.focused_directory().view().preferences().layout;
                state.dispatch_command("view.list", cx);
                assert_eq!(
                    state.focused_directory().view().preferences().layout,
                    before
                );
                let location = state.navigation.focused_tab().location().clone();
                state.navigate(StorePath::from_unix_path("/tmp"), true, cx);
                assert_eq!(state.navigation.focused_tab().location(), &location);
            });
        });
        cx.update(|cx| {
            dialog_handle
                .update(cx, |_, window, _| window.remove_window())
                .expect("context modal is open");
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(app.read(cx).context_dialog_windows.is_empty());
            assert!(prior_focus.is_focused(window));
        })
        .expect("main window remains open");
    }

    #[gpui_kit::test]
    async fn status_bar_opens_shared_operation_history(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let recovery_directory = tempfile::tempdir().expect("recovery directory is available");
        let recovery_destination = StorePath::from_unix_path(
            recovery_directory
                .path()
                .join("restored.txt")
                .into_os_string(),
        );
        let recovery_id = musheen_ops::JobId::new(43).expect("non-zero job ID");
        let recovery_staging = musheen_ops::StagingPath::for_destination(
            &recovery_destination,
            recovery_id,
            musheen_ops::EventGeneration::new(0),
        )
        .expect("staging path derives")
        .path()
        .clone();
        filesystem::write(
            recovery_staging.as_unix_path().expect("staging is local"),
            b"partial",
        )
        .expect("staging writes");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update(|cx| {
            app.update(cx, |state, cx| {
                let id = musheen_ops::JobId::new(41).expect("non-zero job ID");
                state
                    .operation_hub
                    .status()
                    .lock()
                    .expect("status center lock is available")
                    .register(
                        id,
                        musheen_ops::EventGeneration::new(0),
                        musheen_ops::OperationKind::Copy,
                        StorePath::from_unix_path("/fixture/Welcome.md"),
                        Some(1),
                    )
                    .expect("operation registers");
                let completed = musheen_ops::JobId::new(42).expect("non-zero job ID");
                let status = state.operation_hub.status();
                let mut status = status.lock().expect("status center lock is available");
                status
                    .register(
                        completed,
                        musheen_ops::EventGeneration::new(0),
                        musheen_ops::OperationKind::Move,
                        StorePath::from_unix_path("/fixture/Archive"),
                        Some(1),
                    )
                    .expect("completed operation registers");
                status.mark_running(completed).unwrap();
                status.record_item_success(completed).unwrap();
                status.complete(completed).unwrap();
                status
                    .register(
                        recovery_id,
                        musheen_ops::EventGeneration::new(0),
                        musheen_ops::OperationKind::Copy,
                        recovery_destination.clone(),
                        Some(1),
                    )
                    .unwrap();
                status.mark_running(recovery_id).unwrap();
                status
                    .record_recoverable_failure(
                        recovery_id,
                        recovery_destination.clone(),
                        recovery_staging.clone(),
                        "recovery staging remains",
                    )
                    .unwrap();
                status.mark_recoverable(recovery_id).unwrap();
                cx.notify();
            });
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                window.find("operation-status-summary").label(),
                Some("Copy: 0 of 1")
            );
            window.click("operation-status-summary", cx);
            window.render_frame(cx);
            assert!(window.find("operation-status-center").visible());
            assert!(window.find("operation-status-41").visible());
            assert!(window.find("operation-status-42").visible());
            assert!(window.find("operation-recovery-discard-43").visible());
            assert!(window.try_find("operation-recovery-resume-43").is_none());
            assert!(window.find("operation-view-43").visible());
            window.click("operation-recovery-discard-43", cx);
            window.render_frame(cx);
            assert!(
                !recovery_staging
                    .as_unix_path()
                    .expect("staging is local")
                    .exists()
            );
            assert!(window.try_find("operation-recovery-discard-43").is_none());
            window.click("operation-dismiss-42", cx);
            window.render_frame(cx);
            assert!(window.try_find("operation-status-42").is_none());
        })
        .expect("test window remains open");
    }

    #[gpui_kit::test]
    async fn trash_toolbar_and_keyboard_commands_match_context_menu_receipts_and_state(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let temporary = tempfile::tempdir().unwrap();
        filesystem::write(temporary.path().join("ordinary.txt"), b"ordinary file").unwrap();
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        app.update(cx, |state, cx| {
            let tab_id = state.navigation.focused_tab().id();
            let ordinary = state.focused_directory().view().items()[0].id().clone();
            state.select_item(tab_id, ordinary, cx);
            state.navigation.navigate_focused(trash_store_path());
            let items: Vec<_> = ["first", "second"]
                .into_iter()
                .map(|name| {
                    TrashItem::new(
                        musheen_ops::TrashReceipt::new(
                            StorePath::from_unix_path(temporary.path().join(name)),
                            name.as_bytes().to_vec(),
                        ),
                        1,
                    )
                })
                .collect();
            let targets: Vec<_> = items.iter().map(trash_command_target).collect();
            for trash_state in [
                TrashState::Loading,
                TrashState::Error("unavailable".into()),
                TrashState::Ready(TrashSurfaceModel::new(Vec::new())),
                TrashState::Ready(TrashSurfaceModel::new(items)),
            ] {
                let available = matches!(
                    &trash_state,
                    TrashState::Ready(surface) if !surface.items().is_empty()
                );
                state.trash_states.insert(tab_id, trash_state);
                for selected_count in 0..=2 {
                    state.navigation.tab_mut(tab_id).unwrap().set_selection(
                        targets[..selected_count]
                            .iter()
                            .map(|target| target.id().clone()),
                    );
                    for (action, id) in [
                        (CommandAction::Restore, "trash.restore"),
                        (CommandAction::EmptyTrash, "trash.empty"),
                    ] {
                        let request = state.active_command_request(action);
                        let menu_target =
                            if action == CommandAction::EmptyTrash || selected_count == 0 {
                                MenuTarget::TrashBackground
                            } else {
                                MenuTarget::TrashItem
                            };
                        let selected = if available {
                            targets[..selected_count].to_vec()
                        } else {
                            Vec::new()
                        };
                        let menu = state.compose_context_menu(tab_id, menu_target, selected);
                        let command_state = state
                            .shell
                            .commands()
                            .get(id)
                            .unwrap()
                            .state(&state.active_command_context(action))
                            .map_disabled_reason(|reason| state.catalog.localize_reason(reason));
                        assert_eq!(
                            command_state.is_enabled(),
                            available
                                && (action == CommandAction::EmptyTrash || selected_count == 1)
                        );
                        assert_eq!(request.target(), menu_target);
                        if action == CommandAction::EmptyTrash {
                            assert_eq!(request.context().target, CommandTarget::TrashBackground);
                            assert_eq!(request.context().selection_count, 0);
                        }
                        if let Some(entry) = MusheenApp::menu_entry_by_id(&menu, id) {
                            assert_eq!(
                                &command_state,
                                entry.state(),
                                "{id}: {selected_count} selected"
                            );
                            assert_eq!(request.captured_targets(), entry.captured_targets());
                            if command_state.is_enabled() {
                                let shared_menu = state.compose_context_request(request);
                                let shared_entry =
                                    MusheenApp::menu_entry_by_id(&shared_menu, id).unwrap();
                                let mut dispatcher = AppMenuDispatcher::default();
                                let invocation = state
                                    .shell
                                    .context_menus()
                                    .invoke(shared_entry, &mut dispatcher);
                                if action == CommandAction::Restore {
                                    assert!(matches!(invocation, MenuInvocation::Dispatched));
                                    assert_eq!(
                                        dispatcher.dispatched,
                                        Some((
                                            action,
                                            CommandParameters::Targets(vec![targets[0].clone()])
                                        ))
                                    );
                                } else {
                                    assert!(matches!(
                                        invocation,
                                        MenuInvocation::NeedsConfirmation(pending)
                                            if pending.selection() == targets
                                    ));
                                }
                            }
                        } else {
                            assert!(!command_state.is_enabled());
                        }
                    }
                }
            }
            state
                .navigation
                .tab_mut(tab_id)
                .unwrap()
                .set_selection(vec![targets[0].id().clone()]);
            state.dispatch_command("trash.empty", cx);
            assert!(matches!(
                &state.pending_empty_trash,
                Some(MenuInvocation::NeedsConfirmation(pending)) if pending.selection() == targets
            ));
            state.pending_empty_trash = None;
            // The synthetic receipt cannot restore a real file. A backend error
            // proves shared dispatch reached Restore rather than silently refusing it.
            state.dispatch_command("trash.restore", cx);
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).operation_error.is_some()
        })
        .await;
        app.update(cx, |state, _| {
            assert_ne!(
                state.operation_error.as_deref(),
                Some(state.catalog.message("context.target-changed").unwrap())
            );
        });
    }

    #[gpui_kit::test]
    async fn trash_view_commands_are_disabled_by_the_live_registry(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update(|cx| {
            app.update(cx, |state, cx| {
                let tab_id = state.navigation.focused_tab().id();
                state.navigation.navigate_focused(trash_store_path());
                state.trash_states.insert(
                    tab_id,
                    TrashState::Ready(TrashSurfaceModel::new(Vec::new())),
                );
                let before = state.focused_directory().view().preferences().clone();
                let menu = state.compose_context_menu(tab_id, MenuTarget::Background, Vec::new());
                for id in ["view.hidden", "view.sort", "view.group"] {
                    let entry = menu.entry(id).expect("Trash background view command");
                    assert!(!entry.state().is_enabled(), "{id}");
                    assert_eq!(
                        entry.state().disabled_reason(),
                        Some(
                            state
                                .catalog
                                .message("context.backend-unavailable")
                                .unwrap()
                        )
                    );
                    state.dispatch_context_entry(entry.clone(), cx);
                }
                for id in [
                    "view.details",
                    "view.list",
                    "view.cards",
                    "view.grid",
                    "view.columns",
                    "view.adaptive",
                    "view.sort",
                    "view.group",
                    "view.directories_first",
                    "view.hidden",
                ] {
                    let command = state.shell.commands().get(id).unwrap();
                    assert!(
                        !command
                            .state(&state.active_command_context(command.action()))
                            .is_enabled(),
                        "{id}"
                    );
                    state.dispatch_command(id, cx);
                }
                assert_eq!(state.focused_directory().view().preferences(), &before);
                cx.notify();
            });
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("trash-surface").visible());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn trash_sidebar_uses_receipts_and_requires_a_second_empty_confirmation(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update(|cx| {
            app.update(cx, |state, cx| {
                let tab_id = state.navigation.focused_tab().id();
                state.navigation.navigate_focused(trash_store_path());
                state.trash_states.insert(
                    tab_id,
                    TrashState::Ready(TrashSurfaceModel::new(vec![TrashItem::new(
                        musheen_ops::TrashReceipt::new(
                            StorePath::from_unix_path("/home/user/Documents/old.txt"),
                            b"trash-id".to_vec(),
                        ),
                        1_726_742_400,
                    )])),
                );
                cx.notify();
            });
        });

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("trash-surface").visible());
            assert!(window.find("trash-item-0").visible());
            assert!(window.find("trash-restore-0").visible());
            window.press("down", cx);
            window.press("shift-f10", cx);
            window.render_frame(cx);
            assert!(
                gpui_kit::base::test_support::snapshots(window)
                    .iter()
                    .any(|node| {
                        node.label()
                            .is_some_and(|label| label.starts_with("Restore"))
                            && node.role() == Some(Role::MenuItem)
                    }),
                "focused Trash rows must offer Restore rather than Empty Trash: {:?}",
                gpui_kit::base::test_support::snapshots(window)
                    .iter()
                    .map(|node| (node.role(), node.label().map(str::to_owned)))
                    .collect::<Vec<_>>()
            );
            window.press("escape", cx);
            assert!(app.read(cx).navigation.focused_tab().selection().is_empty());
            window.right_click("trash-item-0", cx);
        })
        .expect("test window remains open");
        cx.wait_for(handle.into(), Duration::from_secs(2), |window, _| {
            gpui_kit::base::test_support::snapshots(window)
                .iter()
                .any(|node| node.label() == Some("Restore") && node.role() == Some(Role::MenuItem))
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let selection = app.read(cx).navigation.focused_tab().selection();
            assert_eq!(selection.len(), 1);
            assert_eq!(selection[0].provider().as_str(), "musheen.trash");
            assert!(
                gpui_kit::base::test_support::snapshots(window)
                    .iter()
                    .any(|node| {
                        node.label()
                            .is_some_and(|label| label.starts_with("Restore"))
                            && node.role() == Some(Role::MenuItem)
                    })
            );
            window.press("escape", cx);
            window.click("trash-empty", cx);
            window.render_frame(cx);
            assert!(window.find("trash-empty-confirmation").visible());
            assert_eq!(
                window.find("trash-empty-confirmation").label(),
                Some("Permanently delete 1 item from Trash? This cannot be undone.")
            );
        })
        .expect("test window remains open");
        cx.run_until_parked();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("popup-menu").is_none());
        })
        .expect("dismissed Trash popup is removed before teardown");
    }

    #[gpui_kit::test]
    async fn alt_enter_opens_properties_for_the_current_selection(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        app.update(cx, |state, cx| {
            state.dispatch_command("selection.select_all", cx);
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.press("alt-enter", cx);
        })
        .expect("the browser window remains open");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            cx.windows().len() == 2
        })
        .await;

        let browser: gpui_kit::AnyWindowHandle = handle.into();
        let properties = cx
            .windows()
            .into_iter()
            .find(|candidate| *candidate != browser)
            .expect("Alt+Enter opens a second window");
        cx.update_window(properties, |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("properties-dialog").visible());
            assert!(window.find("properties-identity").visible());
        })
        .expect("the Properties window remains open");
    }

    #[gpui_kit::test]
    async fn narrow_shell_uses_view_overflow_and_one_reachable_pane(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(720.), px(600.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");

        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("view.overflow").visible());
            assert!(window.try_find("view.details").is_none());
            window.press("f3", cx);
            assert_eq!(app.read(cx).navigation.panes().len(), 2);
            assert!(window.find("pane-0").visible());
            assert!(window.try_find("pane-1").is_none());
            let focused = app.read(cx).navigation.focused_pane_id();
            window.press("f6", cx);
            assert_ne!(app.read(cx).navigation.focused_pane_id(), focused);
            assert!(window.find("pane-0").visible());
            assert!(window.try_find("pane-1").is_none());
        })
        .expect("test window remains open");
    }

    #[gpui_kit::test]
    async fn search_submission_streams_scoped_results_into_the_content_area(
        cx: &mut TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;

        cx.update(|cx| {
            app.update(cx, |state, cx| {
                state.store = Arc::new(ImmediateSearchStore::new());
                state
                    .focused_directory_mut()
                    .view_mut()
                    .preferences_mut()
                    .show_hidden = true;
                state.omnibar.enter(OmnibarMode::Search, "name:Welcome");
                state.submit_omnibar(cx);
            });
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx)
                .active_search()
                .is_some_and(|search| search.state() == SearchState::Complete)
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("search-results").visible());
            assert!(window.find("search-scope").visible());
            let scope_label = window
                .find("search-scope")
                .label()
                .expect("search scope has an accessible label")
                .to_owned();
            assert!(scope_label.contains("name:Welcome"));
            assert!(scope_label.contains("Hidden items included"));
            assert!(scope_label.contains("Symbolic links not followed"));
            assert_eq!(window.find("status-bar").label(), Some("1 result"));
            assert_eq!(
                app.read(cx)
                    .active_search()
                    .expect("search remains active")
                    .total_results(),
                1
            );
        })
        .expect("test window remains open");

        cx.update(|cx| {
            app.update(cx, |state, cx| {
                state.store = Arc::new(ImmediateSearchStore::partial());
                state.start_search("name:Welcome".to_owned(), cx);
            });
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx)
                .active_search()
                .is_some_and(|search| search.state() == SearchState::Partial)
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.find("search-errors").visible());
            assert!(window.find("search-error-0").visible());
            assert!(window.find("search-retry").visible());
        })
        .expect("test window remains open");

        cx.update(|cx| {
            app.update(cx, |state, cx| {
                state.store = Arc::new(ImmediateSearchStore::without_terminal_batch());
                state.start_search("name:Welcome hidden:false".to_owned(), cx);
            });
        });
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx)
                .active_search()
                .is_some_and(|search| search.state() == SearchState::Error)
        })
        .await;
        cx.update(|cx| {
            let state = app.read(cx);
            let tab_id = state.navigation.focused_tab().id();
            assert!(
                !state
                    .searches
                    .get(&tab_id)
                    .expect("search remains active")
                    .model
                    .query()
                    .include_hidden()
            );
            assert_eq!(
                state
                    .searches
                    .get(&tab_id)
                    .and_then(|search| search.error.as_deref()),
                Some("search provider ended before reporting completion")
            );
        });
    }

    #[gpui_kit::test]
    async fn dragging_an_item_onto_a_folder_runs_the_shared_move_queue(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let source = temporary.path().join("source.txt");
        let destination = temporary.path().join("destination");
        filesystem::write(&source, b"move through the UI").unwrap();
        filesystem::create_dir(&destination).unwrap();

        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;

        let (source_index, destination_index) = cx.read(|cx| {
            let state = app.read(cx);
            let tab_id = state.navigation.focused_tab().id();
            let items = state.filtered_items(tab_id);
            let source_index = items
                .iter()
                .position(|item| item.display_name().as_str() == "source.txt")
                .expect("source item is visible");
            let destination_index = items
                .iter()
                .position(|item| item.display_name().as_str() == "destination")
                .expect("destination item is visible");
            (source_index, destination_index)
        });
        let (source_bounds, destination_bounds) = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                (
                    window
                        .find(format!("directory-item-0-{source_index}"))
                        .bounds(),
                    window
                        .find(format!("directory-item-0-{destination_index}"))
                        .bounds(),
                )
            })
            .expect("the application window remains open");
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.simulate_mouse_down(source_bounds.center(), MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_move(
            destination_bounds.center(),
            Some(MouseButton::Left),
            Modifiers::none(),
        );
        visual.simulate_mouse_up(
            destination_bounds.center(),
            MouseButton::Left,
            Modifiers::none(),
        );
        visual.run_until_parked();

        assert_eq!(
            filesystem::read(destination.join("source.txt")).unwrap(),
            b"move through the UI"
        );
        assert!(!source.exists());
    }

    #[gpui_kit::test]
    async fn dragging_an_item_onto_a_sidebar_folder_uses_the_same_queue(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let source = temporary.path().join("source.txt");
        let destination = temporary.path().join("destination");
        filesystem::write(&source, b"move through the sidebar").unwrap();
        filesystem::create_dir(&destination).unwrap();

        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| {
                MusheenApp::new_with_session_store(temporary.path().to_path_buf(), None, cx)
            });
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        app.update(cx, |state, cx| {
            state.pins.replace([SidebarEntry::new(
                "Drop destination",
                StorePath::from_unix_path(destination.as_os_str()),
            )]);
            cx.notify();
        });

        let (source_index, pinned_section_index) = cx.read(|cx| {
            let state = app.read(cx);
            let tab_id = state.navigation.focused_tab().id();
            let source_index = state
                .filtered_items(tab_id)
                .iter()
                .position(|item| item.display_name().as_str() == "source.txt")
                .expect("source item is visible");
            let section_index = state
                .sidebars
                .get(&tab_id)
                .unwrap()
                .sections()
                .iter()
                .position(|section| section.kind() == SidebarSectionKind::Pinned)
                .expect("pinned section is visible");
            (source_index, section_index)
        });
        let (source_bounds, destination_bounds) = cx
            .update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                (
                    window
                        .find(format!("directory-item-0-{source_index}"))
                        .bounds(),
                    window
                        .find(format!("sidebar-{pinned_section_index}-0"))
                        .bounds(),
                )
            })
            .expect("the application window remains open");
        let mut visual = VisualTestContext::from_window(handle.into(), cx);
        visual.simulate_mouse_down(source_bounds.center(), MouseButton::Left, Modifiers::none());
        visual.simulate_mouse_move(
            destination_bounds.center(),
            Some(MouseButton::Left),
            Modifiers::none(),
        );
        visual.simulate_mouse_up(
            destination_bounds.center(),
            MouseButton::Left,
            Modifiers::none(),
        );
        visual.run_until_parked();

        assert_eq!(
            filesystem::read(destination.join("source.txt")).unwrap(),
            b"move through the sidebar"
        );
        assert!(!source.exists());
    }

    #[gpui_kit::test]
    async fn operation_failures_are_exposed_in_the_shell(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
        });
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new_with_session_store(fixture, None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");
        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;

        app.update(cx, |state, cx| {
            state.operation_error = Some("destination became read-only".into());
            cx.notify();
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            let alert = window.find("operation-error");
            assert!(alert.visible());
            assert_eq!(
                alert.label(),
                Some("File operation failed: destination became read-only")
            );
        })
        .expect("the application window remains open");
    }
}
