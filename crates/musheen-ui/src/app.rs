use crate::directory::{DirectoryLoad, DirectoryModel, DirectoryState, enumerate_directory};
use crate::icons::{ContentIdentity, freedesktop_icon_name};
use crate::navigation::{
    ApplicationSession, BreadcrumbTrail, MAX_WINDOWS, NavigationError, OmnibarMode, OmnibarState,
    OmnibarSubmission, PaneId, TabId, WindowSession, resolve_path_input,
};
use crate::search::{DirectoryFilter, SearchGeneration, SearchResultModel, SearchState};
use crate::sidebar::{PinStore, SidebarEntry, SidebarModel, SidebarSectionKind};
use crate::status_bar::status_text_with_size;
use crate::toolbar::COMMAND_IDS;
use crate::views::{
    AdaptiveLayout, ColumnKey, GroupKey, Layout, SelectionMode, SortDirection, SortKey, SortSpec,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Root, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, ImageSource, IntoElement,
    KeyBinding, Render, Role, SharedString, Subscription, TestSupportExt, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, img, px, size, uniform_list,
};
use musheen_core::{
    CancellationToken, CommandAction, DirectoryWatch, DisplayPath, ItemId, ItemKind, Page,
    ResourceLimits, SEARCH_RESULT_LIMIT, SEARCH_RETAINED_RESULTS, SearchBatch, SearchCompletion,
    SearchQuery, SearchScopeError, SearchStream, Store, StoreError, StoreItem, StorePath,
    WatchEvent,
};
use musheen_desktop::SessionStore;
use musheen_local::LocalStore;
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
        Columns2,
        Download,
        File,
        Folder,
        Grid2x2,
        HardDrive,
        House,
        Info,
        List,
        ListChecks,
        Network,
        PanelRight,
        Plus,
        RefreshCw,
        Search,
        Settings,
        TextCursorInput,
        Trash,
        X,
    ]
);

const APP_ICON: &[u8] = include_bytes!("../../../assets/icons/musheen.svg");
const SIDEBAR_WIDTH: f32 = 220.0;
const CONTENT_PADDING: f32 = 32.0;
const GRID_ITEM_WIDTH: f32 = 128.0;
const GRID_GAP: f32 = 8.0;
const SESSION_SAVE_DELAY: Duration = Duration::from_millis(250);

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
    ]
);

fn install_navigation_key_bindings(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("alt-left", GoBack, None),
        KeyBinding::new("alt-right", GoForward, None),
        KeyBinding::new("alt-up", GoParent, None),
        KeyBinding::new("f5", Reload, None),
        KeyBinding::new("ctrl-l", EditLocation, None),
        KeyBinding::new("ctrl-f", SearchLocation, None),
        KeyBinding::new("ctrl-shift-f", FilterLocation, None),
        KeyBinding::new("ctrl-shift-p", OpenCommandMode, None),
        KeyBinding::new("ctrl-t", NewTabShortcut, None),
        KeyBinding::new("ctrl-w", CloseTabShortcut, None),
        KeyBinding::new("ctrl-shift-t", ReopenClosedTabShortcut, None),
        KeyBinding::new("f3", SplitPaneShortcut, None),
        KeyBinding::new("f6", FocusNextPaneShortcut, None),
        KeyBinding::new("ctrl-a", SelectAllShortcut, None),
        KeyBinding::new("escape", Escape, None),
        KeyBinding::new("ctrl-h", ToggleHiddenShortcut, None),
        KeyBinding::new("ctrl-1", ViewDetailsShortcut, None),
        KeyBinding::new("ctrl-2", ViewListShortcut, None),
        KeyBinding::new("ctrl-3", ViewCardsShortcut, None),
        KeyBinding::new("ctrl-4", ViewGridShortcut, None),
        KeyBinding::new("ctrl-5", ViewColumnsShortcut, None),
        KeyBinding::new("ctrl-6", ViewAdaptiveShortcut, None),
        KeyBinding::new("ctrl-b", ToggleSidebarShortcut, None),
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
    name: String,
    kind: ItemKind,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
    columns: Vec<(ColumnKey, u16)>,
    layout: Layout,
    selected: bool,
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

pub fn run(initial_path: PathBuf) {
    gpui_kit::application()
        .with_assets(MusheenAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
            install_navigation_key_bindings(cx);
            install_native_theme(cx);
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
            let application = restore_application_session(
                &store,
                fallback.clone(),
                LocalStore::session_location_exists,
            )
            .unwrap_or_else(|| {
                ApplicationSession::new(vec![WindowSession::new(fallback)])
                    .expect("a single fallback window is a valid application session")
            });
            let coordinator = Arc::new(Mutex::new(SessionCoordinator::new(
                store,
                application.windows().to_vec(),
            )));
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
                        },
                    )
                })
                .collect::<Vec<_>>();
            cx.spawn(async move |cx| {
                for (window_options, navigation, binding) in windows {
                    cx.open_window(window_options, move |window, cx| {
                        let view = cx.new(|cx| {
                            MusheenApp::new_with_navigation(
                                navigation,
                                Some(binding),
                                ResourceLimits::default(),
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
        app_id: Some("io.musheen.Musheen".into()),
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
        app_id: Some("io.musheen.Musheen".into()),
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
                native_theme_gpui::from_preset("adwaita", false, &preferences)
            {
                native_theme_gpui::apply(theme, &resolved, &preferences, cx);
            }
        }
    }
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
    session_binding: Option<SessionBinding>,
    session_save_generation: u64,
    watch_directories: bool,
    sidebar_visible: bool,
    icon_cache: HashMap<Box<str>, Option<ImageSource>>,
}

impl Drop for MusheenApp {
    fn drop(&mut self) {
        for directory in self.directories.values() {
            directory.cancel();
        }
        for search in self.searches.values() {
            search.cancellation.cancel();
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
                }),
            )
        } else {
            (WindowSession::new(initial), None)
        };
        Self::new_with_navigation(navigation, session_binding, limits, false, cx)
    }

    fn new_with_navigation(
        navigation: WindowSession,
        session_binding: Option<SessionBinding>,
        limits: ResourceLimits,
        watch_directories: bool,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let mut this = Self {
            directories,
            searches: HashMap::new(),
            filters: HashMap::new(),
            sidebars,
            pins,
            limits,
            store: Arc::new(LocalStore::new()),
            shell: crate::ShellModel::default(),
            navigation,
            omnibar: OmnibarState::default(),
            omnibar_input: None,
            omnibar_subscription: None,
            requested_omnibar_mode: None,
            pending_omnibar_value: None,
            content_focus: cx.focus_handle(),
            pending_content_focus: true,
            session_binding,
            session_save_generation: 0,
            watch_directories,
            sidebar_visible: true,
            icon_cache: HashMap::new(),
        };
        this.start_load(location, cx);
        this
    }

    fn start_load(&mut self, location: StorePath, cx: &mut Context<Self>) {
        let tab_id = self.navigation.focused_tab().id();
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
        let tab_id = self.navigation.focused_tab().id();
        self.cancel_search(tab_id);
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
        let loaded = self
            .directories
            .get(&tab_id)
            .and_then(DirectoryModel::location)
            == Some(&location);
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
        if self.navigation.focused_pane_mut().activate_tab(id).is_ok() {
            self.schedule_session_save(cx);
            self.load_focused_tab(cx);
        }
    }

    fn focus_pane(&mut self, id: PaneId, cx: &mut Context<Self>) {
        if self.navigation.focus_pane(id).is_ok() {
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
        let Some(action) = self
            .shell
            .commands()
            .get(command_id)
            .map(|command| command.action())
        else {
            return;
        };
        self.dispatch_action(action, cx);
    }

    fn dispatch_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        match action {
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
            CommandAction::OpenSettings => {}
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
        self.schedule_session_save(cx);
        cx.notify();
    }

    fn select_item(&mut self, tab_id: TabId, id: ItemId, cx: &mut Context<Self>) {
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
        self.schedule_session_save(cx);
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
        if self.omnibar.mode() != OmnibarMode::Path {
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

    fn toolbar_button(
        &self,
        id: &'static str,
        label: &'static str,
        icon: IconName,
        disabled: bool,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .icon(icon)
            .accessibility_label(label)
            .tooltip(label)
            .ghost()
            .small()
            .compact()
            .disabled(disabled)
            .selected(selected)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.dispatch_command(id, cx);
            }))
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
            .child(Icon::default().data(APP_ICON).small())
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
        let no_parent = self
            .focused_directory()
            .location()
            .and_then(StorePath::as_unix_path)
            .and_then(Path::parent)
            .is_none();

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
            .child(self.toolbar_button(
                "navigation.back",
                "Back",
                IconName::ArrowLeft,
                !self.navigation.focused_tab().history().can_go_back(),
                false,
                cx,
            ))
            .child(self.toolbar_button(
                "navigation.forward",
                "Forward",
                IconName::ArrowRight,
                !self.navigation.focused_tab().history().can_go_forward(),
                false,
                cx,
            ))
            .child(self.toolbar_button(
                "navigation.parent",
                "Parent folder",
                IconName::ArrowUp,
                no_parent,
                false,
                cx,
            ))
            .child(self.toolbar_button(
                "navigation.refresh",
                "Refresh",
                IconName::RefreshCw,
                false,
                false,
                cx,
            ))
            .child(self.render_omnibar(cx))
            .child(self.toolbar_button("view.search", "Search", IconName::Search, false, false, cx))
            .when(compact, |toolbar| {
                toolbar.child(self.render_view_overflow(cx))
            })
            .when(!compact, |toolbar| {
                toolbar.child(self.render_view_controls(cx))
            })
            .child(self.toolbar_button(
                "view.sidebar",
                "Sidebar",
                IconName::PanelRight,
                false,
                self.sidebar_visible,
                cx,
            ))
            .child(self.toolbar_button(
                "view.info",
                "Information pane",
                IconName::Info,
                false,
                self.shell.info_visible(),
                cx,
            ))
            .child(self.toolbar_button(
                "pane.split",
                "Split pane",
                IconName::Columns2,
                self.navigation.panes().len() >= 2,
                self.navigation.panes().len() == 2,
                cx,
            ))
            .child(self.toolbar_button(
                "pane.focus_next",
                "Switch pane",
                IconName::PanelRight,
                self.navigation.panes().len() < 2,
                false,
                cx,
            ))
            .child(self.toolbar_button(
                "app.settings",
                "Settings",
                IconName::Settings,
                false,
                false,
                cx,
            ))
    }

    fn render_view_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let preferences = self.focused_directory().view().preferences();
        div()
            .flex()
            .items_center()
            .gap_1()
            .child(self.toolbar_button(
                "view.details",
                "Details view",
                IconName::ListChecks,
                false,
                preferences.layout == Layout::Details,
                cx,
            ))
            .child(self.toolbar_button(
                "view.list",
                "List view",
                IconName::List,
                false,
                preferences.layout == Layout::List,
                cx,
            ))
            .child(self.toolbar_button(
                "view.cards",
                "Cards view",
                IconName::Grid2x2,
                false,
                preferences.layout == Layout::Cards,
                cx,
            ))
            .child(self.toolbar_button(
                "view.grid",
                "Grid view",
                IconName::Grid2x2,
                false,
                preferences.layout == Layout::Grid,
                cx,
            ))
            .child(self.toolbar_button(
                "view.columns",
                "Columns view",
                IconName::Columns2,
                false,
                preferences.layout == Layout::Columns,
                cx,
            ))
            .child(self.toolbar_button(
                "view.adaptive",
                "Adaptive view",
                IconName::PanelRight,
                false,
                preferences.layout == Layout::Adaptive,
                cx,
            ))
            .child(self.toolbar_button(
                "view.sort",
                "Change sort",
                IconName::List,
                false,
                false,
                cx,
            ))
            .child(self.toolbar_button(
                "view.group",
                "Change grouping",
                IconName::ListChecks,
                false,
                preferences.group != GroupKey::None,
                cx,
            ))
            .child(self.toolbar_button(
                "view.directories_first",
                "Show folders first",
                IconName::Folder,
                false,
                preferences.directories_first,
                cx,
            ))
            .child(self.toolbar_button(
                "view.hidden",
                "Show hidden items",
                IconName::TextCursorInput,
                false,
                preferences.show_hidden,
                cx,
            ))
            .into_any_element()
    }

    fn render_view_overflow(&self, cx: &mut Context<Self>) -> AnyElement {
        let preferences = self.focused_directory().view().preferences().clone();
        let view = cx.entity();
        let items = [
            (
                "view.details",
                "Details",
                preferences.layout == Layout::Details,
            ),
            ("view.list", "List", preferences.layout == Layout::List),
            ("view.cards", "Cards", preferences.layout == Layout::Cards),
            ("view.grid", "Grid", preferences.layout == Layout::Grid),
            (
                "view.columns",
                "Columns",
                preferences.layout == Layout::Columns,
            ),
            (
                "view.adaptive",
                "Adaptive",
                preferences.layout == Layout::Adaptive,
            ),
            ("view.sort", "Change sort", false),
            (
                "view.group",
                "Change grouping",
                preferences.group != GroupKey::None,
            ),
            (
                "view.directories_first",
                "Folders first",
                preferences.directories_first,
            ),
            ("view.hidden", "Show hidden", preferences.show_hidden),
        ];
        Button::new("view.overflow")
            .icon(IconName::ListChecks)
            .accessibility_label("View options")
            .tooltip("View options")
            .ghost()
            .small()
            .compact()
            .dropdown_menu(move |mut menu, _, _| {
                for (command_id, label, checked) in items {
                    let view = view.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            view.update(cx, |this, cx| {
                                this.dispatch_command(command_id, cx);
                            });
                        },
                    ));
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
                                                this.navigate(location.clone(), true, cx);
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
        let body = if self.searches.contains_key(&spec.tab_id) {
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

        div()
            .id(content_id)
            .test_support()
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
            .into_any_element()
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
            .gap_3()
            .p_4()
            .bg(colors.sidebar)
            .border_l_1()
            .border_color(boundary)
            .text_color(colors.muted_foreground)
            .child(Icon::new(IconName::Info).large())
            .child(div().text_sm().child("No item selected"))
            .child(
                div()
                    .text_xs()
                    .text_center()
                    .child("Select an item to see its details."),
            );
        if wide {
            pane.w(px(280.)).into_any_element()
        } else {
            pane.flex_grow(1.0).into_any_element()
        }
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
            name: item.display_name().as_str().to_owned(),
            kind: item.kind(),
            size: item.size(),
            modified_unix_seconds: item.modified_unix_seconds(),
            columns: view.preferences().columns.visible_columns_with_widths(),
            layout,
            selected: view.selected_ids().contains(item.id()),
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
        let item_id =
            SharedString::from(format!("directory-item-{}-{}", spec.pane_index, spec.index));
        let tab_id = spec.tab_id;
        let stable_id = spec.id.clone();
        match spec.layout {
            Layout::Cards | Layout::Grid | Layout::Adaptive => div()
                .id(item_id)
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_item(tab_id, stable_id.clone(), cx);
                }))
                .child(icon)
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_center()
                        .overflow_hidden()
                        .child(spec.name),
                )
                .into_any_element(),
            Layout::List => div()
                .id(item_id)
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
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_item(tab_id, stable_id.clone(), cx);
                }))
                .child(div().w(px(28.)).flex().justify_center().child(icon))
                .child(div().flex_grow(1.0).text_sm().child(spec.name))
                .child(
                    div()
                        .w(px(96.))
                        .text_right()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(spec.size.map_or_else(String::new, format_size)),
                )
                .into_any_element(),
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
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_item(tab_id, stable_id.clone(), cx);
                    }))
                    .children(cells)
                    .into_any_element()
            }
        }
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
}

impl Render for MusheenApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_omnibar(window, cx);
        if let Some(mode) = self.requested_omnibar_mode.take() {
            self.activate_omnibar(mode, window, cx);
        } else if let Some(value) = self.pending_omnibar_value.take()
            && let Some(input) = self.omnibar_input.as_ref()
        {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        if self.pending_content_focus {
            self.content_focus.focus(window, cx);
            self.pending_content_focus = false;
        }
        let colors = cx.theme().colors;
        let status = self.focused_status_text();
        let info_visible = self.shell.info_visible();
        let wide = window.viewport_size().width.as_f32() >= 960.0;
        let high_contrast = Self::high_contrast(cx);
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
        div()
            .id("musheen-shell")
            .test_support()
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
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                this.handle_escape(cx);
            }))
            .child(self.render_tab_strip(cx))
            .child(self.render_toolbar(window, cx))
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
                    .child(status),
            )
    }
}

fn place_path(label: &str, home: Option<&Path>) -> Option<StorePath> {
    let home = home?;
    match label {
        "Home" => Some(StorePath::from_unix_path(home.as_os_str())),
        "Desktop" | "Documents" | "Downloads" => {
            Some(StorePath::from_unix_path(home.join(label).into_os_string()))
        }
        "Trash" => Some(StorePath::from_unix_path(
            home.join(".local/share/Trash/files").into_os_string(),
        )),
        _ => None,
    }
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
    use crate::search::SearchState;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use musheen_core::{
        CapabilityMatrix, CapabilityReason, CapabilityState, MutationRequest, PageRequest,
        ProviderId, SearchCapabilities, SearchResult, SearchScopeError,
    };
    use std::time::Duration;

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
}
