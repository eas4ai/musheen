use crate::directory::{DirectoryLoad, DirectoryModel, DirectoryState, enumerate_directory};
use crate::icons::{ContentIdentity, freedesktop_icon_name};
use crate::navigation::{
    ApplicationSession, BreadcrumbTrail, MAX_WINDOWS, NavigationError, OmnibarMode, OmnibarState,
    OmnibarSubmission, PaneId, TabId, WindowSession, resolve_path_input,
};
use crate::sidebar::PLACES;
use crate::status_bar::status_text;
use crate::toolbar::COMMAND_IDS;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Root, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, ImageSource, IntoElement,
    KeyBinding, Render, Role, SharedString, Subscription, TestSupportExt, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, img, px, size, uniform_list,
};
use musheen_core::{
    CommandAction, DisplayPath, ItemKind, Page, ResourceLimits, Store, StoreError, StoreItem,
    StorePath,
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
        OpenCommandMode,
        NewTabShortcut,
        CloseTabShortcut,
        ReopenClosedTabShortcut,
        SplitPaneShortcut,
        FocusNextPaneShortcut,
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
        KeyBinding::new("ctrl-shift-p", OpenCommandMode, None),
        KeyBinding::new("ctrl-t", NewTabShortcut, None),
        KeyBinding::new("ctrl-w", CloseTabShortcut, None),
        KeyBinding::new("ctrl-shift-t", ReopenClosedTabShortcut, None),
        KeyBinding::new("f3", SplitPaneShortcut, None),
        KeyBinding::new("f6", FocusNextPaneShortcut, None),
    ]);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewMode {
    Grid,
    List,
}

#[derive(Clone, Copy)]
struct PaneRenderSpec {
    tab_id: TabId,
    pane_index: usize,
    focused: bool,
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
    view_mode: ViewMode,
    icon_cache: HashMap<Box<str>, Option<ImageSource>>,
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
        Self::new_with_navigation(navigation, session_binding, limits, cx)
    }

    fn new_with_navigation(
        navigation: WindowSession,
        session_binding: Option<SessionBinding>,
        limits: ResourceLimits,
        cx: &mut Context<Self>,
    ) -> Self {
        let location = navigation.focused_tab().location().clone();
        let focused_tab = navigation.focused_tab().id();
        let mut directories = HashMap::new();
        directories.insert(focused_tab, DirectoryModel::new(limits.snapshot()));
        let mut this = Self {
            directories,
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
            view_mode: ViewMode::Grid,
            icon_cache: HashMap::new(),
        };
        this.start_load(location, cx);
        this
    }

    fn start_load(&mut self, location: StorePath, cx: &mut Context<Self>) {
        let tab_id = self.navigation.focused_tab().id();
        let load = self
            .directories
            .entry(tab_id)
            .or_insert_with(|| DirectoryModel::new(self.limits.snapshot()))
            .begin_navigation(location);
        let worker_load = load.clone();
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
                state.apply_directory_result(tab_id, &load, result);
                cx.notify();
            });
        })
        .detach();
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
        if remember {
            self.navigation.navigate_focused(location.clone());
            self.schedule_session_save(cx);
        }
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
        self.omnibar.enter(OmnibarMode::Path, display.clone());
        self.pending_omnibar_value = Some(display);
        self.pending_content_focus = true;
        let tab_id = self.navigation.focused_tab().id();
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
            CommandAction::FocusLocation | CommandAction::Search | CommandAction::FocusCommand => {
                self.dispatch_omnibar_action(action, cx);
            }
            CommandAction::ViewList | CommandAction::ViewGrid | CommandAction::ToggleInfo => {
                self.dispatch_view_action(action, cx);
            }
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
            CommandAction::SelectAll
            | CommandAction::ClearSelection
            | CommandAction::OpenSettings => {}
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
            CommandAction::FocusCommand => Some(OmnibarMode::Command),
            _ => None,
        };
        cx.notify();
    }

    fn dispatch_view_action(&mut self, action: CommandAction, cx: &mut Context<Self>) {
        match action {
            CommandAction::ViewList => {
                self.view_mode = ViewMode::List;
                cx.notify();
            }
            CommandAction::ViewGrid => {
                self.view_mode = ViewMode::Grid;
                cx.notify();
            }
            CommandAction::ToggleInfo => {
                self.shell.toggle_info();
                cx.notify();
            }
            _ => {}
        }
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
            OmnibarMode::Search | OmnibarMode::Command => String::new(),
        };
        let placeholder = match mode {
            OmnibarMode::Path => "Enter a path",
            OmnibarMode::Search => "Search this location",
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
            OmnibarSubmission::Search(_) => {
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

    fn cancel_omnibar(&mut self, cx: &mut Context<Self>) {
        let value = self.current_location_text().to_string();
        self.omnibar.cancel();
        self.omnibar.enter(OmnibarMode::Path, value.clone());
        self.pending_omnibar_value = Some(value);
        self.pending_content_focus = true;
        cx.notify();
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

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.border
        };
        debug_assert_eq!(COMMAND_IDS.len(), 11);
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
            .child(self.toolbar_button(
                "view.list",
                "List view",
                IconName::List,
                false,
                self.view_mode == ViewMode::List,
                cx,
            ))
            .child(self.toolbar_button(
                "view.grid",
                "Grid view",
                IconName::Grid2x2,
                false,
                self.view_mode == ViewMode::Grid,
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
                "app.settings",
                "Settings",
                IconName::Settings,
                false,
                false,
                cx,
            ))
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
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let places = PLACES.into_iter().map(|place| {
            let location = place_path(place.label, home.as_deref());
            let selected = location
                .as_ref()
                .zip(current.as_ref())
                .is_some_and(|(place, current)| place == current);
            let icon = match place.label {
                "Home" => IconName::House,
                "Downloads" => IconName::Download,
                "Trash" => IconName::Trash,
                _ => IconName::Folder,
            };
            let mut button = Button::new(SharedString::from(format!(
                "sidebar-{}",
                place.label.to_ascii_lowercase()
            )))
            .label(place.label)
            .icon(icon)
            .accessibility_label(place.label)
            .ghost()
            .small()
            .selected(selected)
            .w_full();
            if let Some(location) = location {
                button = button.on_click(cx.listener(move |this, _, _, cx| {
                    this.navigate(location.clone(), true, cx);
                }));
            } else {
                button = button.disabled(true);
            }
            div()
                .border_l_2()
                .border_color(if selected {
                    colors.primary
                } else {
                    transparent
                })
                .child(button)
        });

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
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .px_2()
                    .pb_1()
                    .child("Places"),
            )
            .children(places)
            .child(div().h(px(12.)))
            .child(
                div()
                    .text_xs()
                    .text_color(colors.muted_foreground)
                    .px_2()
                    .pb_1()
                    .child("Storage"),
            )
            .child(
                Button::new("sidebar-computer")
                    .label("Computer")
                    .icon(IconName::HardDrive)
                    .ghost()
                    .small()
                    .w_full()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.navigate(StorePath::from_unix_path("/"), true, cx);
                    })),
            )
            .child(
                Button::new("sidebar-network")
                    .label("Network")
                    .icon(IconName::Network)
                    .ghost()
                    .small()
                    .disabled(true)
                    .w_full(),
            )
    }

    fn render_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let focused = self.navigation.focused_pane_id();
        let panes = self
            .navigation
            .panes()
            .iter()
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
        let body = match state {
            DirectoryState::Loading => self.render_loading(colors.skeleton),
            DirectoryState::Empty => self.render_empty(colors.muted_foreground),
            DirectoryState::Error(message) => self.render_error(message, cx),
            DirectoryState::Ready => self.render_items(spec.tab_id, spec.pane_index, window, cx),
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
        let item_count = self
            .directories
            .get(&tab_id)
            .map(|directory| directory.items().len())
            .unwrap_or_default();
        let list = match self.view_mode {
            ViewMode::List => uniform_list(
                SharedString::from(format!("directory-items-list-{pane_index}")),
                item_count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    let items = range
                        .filter_map(|index| {
                            let item = this.directories.get(&tab_id)?.items().get(index)?;
                            Some((
                                index,
                                item.display_name().as_str().to_owned(),
                                item.kind(),
                                item.size(),
                            ))
                        })
                        .collect::<Vec<_>>();
                    items
                        .into_iter()
                        .map(|(index, name, kind, size)| {
                            div()
                                .h(px(40.))
                                .child(this.render_item(pane_index, index, name, kind, size, cx))
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .p_4()
            .into_any_element(),
            ViewMode::Grid => {
                let columns = grid_column_count(window.viewport_size().width.as_f32());
                uniform_list(
                    SharedString::from(format!("directory-items-grid-{pane_index}")),
                    grid_row_count(item_count, columns),
                    cx.processor(move |this, rows: std::ops::Range<usize>, _, cx| {
                        rows.map(|row| {
                            let items = grid_item_range(row, item_count, columns)
                                .filter_map(|index| {
                                    let item = this.directories.get(&tab_id)?.items().get(index)?;
                                    Some((
                                        index,
                                        item.display_name().as_str().to_owned(),
                                        item.kind(),
                                        item.size(),
                                    ))
                                })
                                .collect::<Vec<_>>();
                            div()
                                .h(px(116.))
                                .flex()
                                .gap_2()
                                .children(items.into_iter().map(|(index, name, kind, size)| {
                                    this.render_item(pane_index, index, name, kind, size, cx)
                                }))
                        })
                        .collect::<Vec<_>>()
                    }),
                )
                .size_full()
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
            .child(list)
            .into_any_element()
    }

    fn render_item(
        &mut self,
        pane_index: usize,
        index: usize,
        name: String,
        kind: ItemKind,
        size: Option<u64>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let identity = match kind {
            ItemKind::Directory => ContentIdentity::directory(),
            ItemKind::SymbolicLink => ContentIdentity::symbolic_link(),
            ItemKind::RegularFile | ItemKind::Other => ContentIdentity::GenericFile,
        };
        let icon_name = freedesktop_icon_name(&identity);
        let icon = self.content_icon(icon_name).map_or_else(
            || {
                Icon::new(if kind == ItemKind::Directory {
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
        let item_id = SharedString::from(format!("directory-item-{pane_index}-{index}"));
        match self.view_mode {
            ViewMode::Grid => div()
                .id(item_id)
                .role(Role::ListItem)
                .aria_label(name.clone())
                .w(px(128.))
                .h(px(108.))
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px_2()
                .rounded_md()
                .hover(|style| style.bg(colors.list_hover))
                .child(icon)
                .child(
                    div()
                        .w_full()
                        .text_sm()
                        .text_center()
                        .overflow_hidden()
                        .child(name),
                )
                .into_any_element(),
            ViewMode::List => div()
                .id(item_id)
                .role(Role::ListItem)
                .aria_label(name.clone())
                .h(px(38.))
                .w_full()
                .flex()
                .items_center()
                .gap_3()
                .px_2()
                .rounded_sm()
                .hover(|style| style.bg(colors.list_hover))
                .child(div().w(px(28.)).flex().justify_center().child(icon))
                .child(div().flex_grow(1.0).text_sm().child(name))
                .child(
                    div()
                        .w(px(96.))
                        .text_right()
                        .text_xs()
                        .text_color(colors.muted_foreground)
                        .child(size.map_or_else(String::new, format_size)),
                )
                .into_any_element(),
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
        let status = status_text(self.focused_directory().items().len(), 0);
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
            .on_action(cx.listener(|this, _: &Escape, _, cx| {
                this.cancel_omnibar(cx);
            }))
            .child(self.render_tab_strip(cx))
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_grow(1.0)
                    .min_h(px(0.))
                    .flex()
                    .child(self.render_sidebar(cx))
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use std::time::Duration;

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
            assert_eq!(window.find("status-bar").label(), Some("3 items"));
            assert!(window.find("tab-0").visible());
            assert!(window.find("omnibar").visible());
            assert!(window.find("omnibar-path").visible());
            assert!(window.find("omnibar-search").visible());
            assert!(window.find("omnibar-command").visible());
            assert!(window.find("breadcrumbs").visible());
            assert!(window.find("breadcrumb-current").visible());
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
}
