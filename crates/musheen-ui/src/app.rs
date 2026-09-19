use crate::directory::{DirectoryModel, DirectoryState, enumerate_directory};
use crate::icons::{ContentIdentity, freedesktop_icon_name};
use crate::sidebar::PLACES;
use crate::status_bar::status_text;
use crate::tab_bar::TabLabel;
use crate::toolbar::COMMAND_IDS;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Disableable, Icon, Root, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, ImageSource, IntoElement, Render, Role, SharedString,
    TestSupportExt, TitlebarOptions, Window, WindowBounds, WindowOptions, div, img, px, size,
    uniform_list,
};
use musheen_core::{CommandAction, DisplayPath, ItemKind, ResourceLimits, Store, StorePath};
use musheen_local::LocalStore;
use native_theme::SystemTheme;
use native_theme::icons::FreedesktopLoader;
use native_theme_gpui::NativeTheme;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

gpui_kit::assets::icon_assets!(
    pub MusheenAssets,
    [
        ArrowLeft,
        ArrowRight,
        ArrowUp,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ViewMode {
    Grid,
    List,
}

pub fn run(initial_path: PathBuf) {
    gpui_kit::application()
        .with_assets(MusheenAssets)
        .run(move |cx| {
            gpui_kit::init(cx);
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
            let window_options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(width), px(height)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Musheen".into()),
                    ..TitlebarOptions::default()
                }),
                app_id: Some("io.musheen.Musheen".into()),
                window_min_size: Some(size(px(720.), px(480.))),
                ..WindowOptions::default()
            };
            cx.spawn(async move |cx| {
                cx.open_window(window_options, |window, cx| {
                    let view = cx.new(|cx| MusheenApp::new(initial_path, cx));
                    cx.new(|cx| Root::new(view, window, cx))
                })
                .expect("Musheen could not open its main window");
            })
            .detach();
        });
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

struct MusheenApp {
    directory: DirectoryModel,
    limits: ResourceLimits,
    store: Arc<dyn Store>,
    shell: crate::ShellModel,
    history: Vec<StorePath>,
    history_index: usize,
    view_mode: ViewMode,
    icon_cache: HashMap<Box<str>, Option<ImageSource>>,
}

impl MusheenApp {
    fn high_contrast(cx: &Context<Self>) -> bool {
        cx.try_global::<NativeTheme>()
            .is_some_and(|theme| theme.accessibility().high_contrast)
    }

    fn new(initial_path: PathBuf, cx: &mut Context<Self>) -> Self {
        let limits = ResourceLimits::default();
        let initial = StorePath::from_unix_path(initial_path.into_os_string());
        let mut this = Self {
            directory: DirectoryModel::new(limits.snapshot()),
            limits,
            store: Arc::new(LocalStore::new()),
            shell: crate::ShellModel::default(),
            history: vec![initial.clone()],
            history_index: 0,
            view_mode: ViewMode::Grid,
            icon_cache: HashMap::new(),
        };
        this.start_load(initial, cx);
        this
    }

    fn start_load(&mut self, location: StorePath, cx: &mut Context<Self>) {
        let load = self.directory.begin_navigation(location);
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
                match result {
                    Ok(pages) => {
                        for page in pages {
                            state.directory.apply_page(&load, page);
                        }
                    }
                    Err(error) => {
                        state.directory.apply_error(&load, error.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn navigate(&mut self, location: StorePath, remember: bool, cx: &mut Context<Self>) {
        if remember {
            self.history.truncate(self.history_index.saturating_add(1));
            self.history.push(location.clone());
            self.history_index = self.history.len().saturating_sub(1);
        }
        self.start_load(location, cx);
        cx.notify();
    }

    fn dispatch_command(&mut self, command_id: &str, cx: &mut Context<Self>) {
        let action = self
            .shell
            .commands()
            .get(command_id)
            .map(|command| command.action());
        match action {
            Some(CommandAction::NavigateBack) if self.history_index > 0 => {
                self.history_index -= 1;
                self.navigate(self.history[self.history_index].clone(), false, cx);
            }
            Some(CommandAction::NavigateForward)
                if self.history_index.saturating_add(1) < self.history.len() =>
            {
                self.history_index += 1;
                self.navigate(self.history[self.history_index].clone(), false, cx);
            }
            Some(CommandAction::NavigateParent) => {
                let parent = self
                    .directory
                    .location()
                    .and_then(StorePath::as_unix_path)
                    .and_then(Path::parent)
                    .map(|path| StorePath::from_unix_path(path.as_os_str()));
                if let Some(parent) = parent {
                    self.navigate(parent, true, cx);
                }
            }
            Some(CommandAction::Refresh) => {
                if let Some(location) = self.directory.location().cloned() {
                    self.start_load(location, cx);
                }
            }
            Some(CommandAction::ViewList) => {
                self.view_mode = ViewMode::List;
                cx.notify();
            }
            Some(CommandAction::ViewGrid) => {
                self.view_mode = ViewMode::Grid;
                cx.notify();
            }
            Some(CommandAction::ToggleInfo) => {
                self.shell.toggle_info();
                cx.notify();
            }
            _ => {}
        }
    }

    fn current_location_text(&self) -> SharedString {
        self.directory
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

    fn location_button(&self) -> Button {
        Button::new("navigation.location")
            .label(self.current_location_text())
            .accessibility_label("Current location")
            .tooltip("Current location")
            .secondary()
            .small()
            .min_w(px(120.))
            .flex_grow(1.0)
            .flex_shrink_1()
            .overflow_hidden()
    }

    fn render_tab_strip(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.sidebar_border
        };
        let label = TabLabel(
            self.directory
                .location()
                .and_then(StorePath::as_unix_path)
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Files".into())
                .into(),
        );
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
            .child(
                div()
                    .id("active-tab")
                    .role(Role::Tab)
                    .aria_label(label.0.as_ref())
                    .tab_index(0)
                    .h(px(34.))
                    .min_w(px(180.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_t_md()
                    .bg(colors.background)
                    .border_1()
                    .border_b_0()
                    .border_color(colors.border)
                    .focus(|style| style.border_color(colors.ring))
                    .child(Icon::default().data(APP_ICON).small())
                    .child(SharedString::from(label.0)),
            )
            .child(
                Button::new("new-tab")
                    .icon(IconName::Plus)
                    .accessibility_label("New tab")
                    .tooltip("New tab")
                    .ghost()
                    .small()
                    .compact(),
            )
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors;
        let boundary = if Self::high_contrast(cx) {
            colors.foreground
        } else {
            colors.border
        };
        debug_assert_eq!(COMMAND_IDS.len(), 10);
        let no_parent = self
            .directory
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
                self.history_index == 0,
                false,
                cx,
            ))
            .child(self.toolbar_button(
                "navigation.forward",
                "Forward",
                IconName::ArrowRight,
                self.history_index.saturating_add(1) >= self.history.len(),
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
            .child(self.location_button())
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
        let current = self.directory.location().cloned();
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

    fn render_directory(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let colors = cx.theme().colors;
        let transparent = cx.theme().transparent;
        let body = match self.directory.state().clone() {
            DirectoryState::Loading => self.render_loading(colors.skeleton),
            DirectoryState::Empty => self.render_empty(colors.muted_foreground),
            DirectoryState::Error(message) => self.render_error(message, cx),
            DirectoryState::Ready => self.render_items(window, cx),
        };

        div()
            .id("directory-content")
            .test_support()
            .role(Role::Main)
            .aria_label("Folder contents")
            .tab_index(0)
            .h_full()
            .flex_grow(1.0)
            .overflow_hidden()
            .border_1()
            .border_color(transparent)
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

    fn render_items(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let item_count = self.directory.items().len();
        let list = match self.view_mode {
            ViewMode::List => uniform_list(
                "directory-items-list",
                item_count,
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    let items = range
                        .filter_map(|index| {
                            let item = this.directory.items().get(index)?;
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
                                .child(this.render_item(index, name, kind, size, cx))
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
                    "directory-items-grid",
                    grid_row_count(item_count, columns),
                    cx.processor(move |this, rows: std::ops::Range<usize>, _, cx| {
                        rows.map(|row| {
                            let items = grid_item_range(row, item_count, columns)
                                .filter_map(|index| {
                                    let item = this.directory.items().get(index)?;
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
                                    this.render_item(index, name, kind, size, cx)
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

        div()
            .id("directory-items")
            .test_support()
            .role(Role::List)
            .aria_label("Items")
            .size_full()
            .child(list)
            .into_any_element()
    }

    fn render_item(
        &mut self,
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
        let item_id = SharedString::from(format!("directory-item-{index}"));
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
        let colors = cx.theme().colors;
        let status = status_text(self.directory.items().len(), 0);
        let info_visible = self.shell.info_visible();
        let wide = window.viewport_size().width.as_f32() >= 960.0;
        let high_contrast = Self::high_contrast(cx);
        let boundary = if high_contrast {
            colors.foreground
        } else {
            colors.border
        };
        let directory = (!info_visible || wide).then(|| self.render_directory(window, cx));
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
            .child(self.render_tab_strip(cx))
            .child(self.render_toolbar(cx))
            .child(
                div()
                    .flex_grow(1.0)
                    .min_h(px(0.))
                    .flex()
                    .child(self.render_sidebar(cx))
                    .children(directory)
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

    #[gpui_kit::test]
    async fn rendered_shell_registers_its_semantic_regions(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../musheen-test-support/fixtures/shell-gallery");
        let mut app = None;
        let handle = cx.open_window(size(px(1_180.), px(760.)), |window, cx| {
            let view = cx.new(|cx| MusheenApp::new(fixture, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.expect("test window constructs the application view");

        cx.wait_for(handle.into(), Duration::from_secs(2), |_, cx| {
            app.read(cx).directory.state() == &DirectoryState::Ready
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
            assert!(window.try_find("info-pane").is_none());
            window.click("view.info", cx);
            assert!(window.find("info-pane").visible());
            window.click("view.info", cx);
            assert!(window.try_find("info-pane").is_none());
        })
        .expect("test window remains open");
    }
}
