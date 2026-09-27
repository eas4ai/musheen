//! The chooser window Musheen's FileChooser portal backend opens for each
//! request (SYS-027). The user browses places and folders and picks what the
//! request asks for: one file, several files or a folder to open, a folder
//! and a name to save, or a folder for the files Save Many names. Only a
//! selection the user confirms leaves the window; Cancel, Escape and closing
//! the window answer cancelled, and a Close from the portal closes it.

use crate::i18n::Catalog;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Global, KeyBinding, Role,
    SharedString, Subscription, Task, TestSupportExt, TitlebarOptions, UniformListScrollHandle,
    Window, WindowBounds, WindowOptions, div, px, size, uniform_list,
};
use musheen_core::{
    CancellationToken, ItemKind, PageRequest, ResourceLimits, Store, StoreItem, StorePath,
};
use musheen_desktop::{
    BackendChooserDecision, BackendChooserKind, BackendChooserRequest, ChooserFilter,
    ChooserSelection, NameMimeTypes,
};
use musheen_local::LocalStore;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// One item of the listed folder.
#[derive(Clone, Debug)]
struct ChooserEntry {
    name: OsString,
    path: PathBuf,
    folder: bool,
    hidden: bool,
    mime_type: Option<Box<str>>,
}

/// A place the chooser offers beside the list.
#[derive(Clone, Debug)]
struct ChooserPlace {
    label_key: &'static str,
    path: PathBuf,
}

/// What reading a folder gave.
enum Listing {
    Loading,
    Ready(Vec<ChooserEntry>),
    Unreadable,
}

pub struct PortalChooser {
    request: BackendChooserRequest,
    catalog: Catalog,
    folder: PathBuf,
    listing: Listing,
    /// The entries the list shows, kept until the listing, the filter or
    /// the hidden toggle changes, so a frame does not filter the folder.
    shown: Arc<Vec<ChooserEntry>>,
    places: Vec<ChooserPlace>,
    /// Selected paths, always among the shown entries.
    selected: Vec<PathBuf>,
    filter: Option<usize>,
    show_hidden: bool,
    name: Option<Entity<InputState>>,
    /// The name behind the name field's text, when that name is not valid
    /// UTF-8 and the field shows it with replacement characters (CORE-010).
    raw_name: Option<(String, OsString)>,
    /// The chosen value of each of the request's choices, in order.
    choices: Vec<Box<str>>,
    /// The paths Accept would return, waiting for the user to allow
    /// replacing the files that exist there.
    replacing: Option<Vec<PathBuf>>,
    /// Where the decision goes; taken once the user decides, so a closed
    /// window answers cancelled only when nothing was decided.
    response: Option<async_channel::Sender<BackendChooserDecision>>,
    load: Option<Task<()>>,
    /// The existence check a Save started; dropped, and so stopped, when
    /// the name or the folder changes.
    check: Option<Task<()>>,
    _watch: Option<Task<()>>,
    scroll: UniformListScrollHandle,
    focus: FocusHandle,
    pending_focus: bool,
    _subscriptions: Vec<Subscription>,
}

/// Marks the chooser's key bindings as installed.
struct PortalChooserKeys;

impl Global for PortalChooserKeys {}

/// Binds Escape to cancel in chooser windows, once per app, as a start for
/// the portal alone has no other bindings.
pub(crate) fn bind_portal_chooser_keys(cx: &mut App) {
    if cx.has_global::<PortalChooserKeys>() {
        return;
    }
    cx.set_global(PortalChooserKeys);
    cx.bind_keys([KeyBinding::new("escape", Escape, Some("PortalChooser"))]);
}

/// Opens a chooser window for `request`, answering on `response`. When
/// `cancellation` fires, as a Close from the portal makes it, the window
/// closes. When no window can open, the request is cancelled.
pub(crate) fn open_portal_chooser(
    request: BackendChooserRequest,
    response: async_channel::Sender<BackendChooserDecision>,
    cancellation: CancellationToken,
    cx: &mut App,
) {
    bind_portal_chooser_keys(cx);
    let catalog = Catalog::system().expect("the built-in locale catalogs are valid");
    let title = SharedString::from(request.title().to_owned());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(760.), px(600.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title),
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(520.), px(380.))),
        ..WindowOptions::default()
    };
    let cancel = response.clone();
    let opened = cx.open_window(options, |window, cx| {
        let view =
            cx.new(|cx| PortalChooser::new(request, catalog, response, cancellation, window, cx));
        cx.new(|cx| gpui_kit::component::Root::new(view, window, cx))
    });
    if opened.is_err() {
        let _ = cancel.try_send(BackendChooserDecision::Cancelled);
    }
}

impl PortalChooser {
    fn new(
        request: BackendChooserRequest,
        catalog: Catalog,
        response: async_channel::Sender<BackendChooserDecision>,
        cancellation: CancellationToken,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let folder = request
            .current_folder()
            .filter(|folder| folder.is_absolute())
            .map(Path::to_path_buf)
            .or_else(|| home.clone())
            .unwrap_or_else(|| PathBuf::from("/"));
        let mut subscriptions = Vec::new();
        let mut raw_name = None;
        let name = (request.kind() == BackendChooserKind::Save).then(|| {
            let suggested = request.current_name().unwrap_or_default();
            let shown = suggested.to_string_lossy().into_owned();
            if suggested.to_str().is_none() {
                raw_name = Some((shown.clone(), suggested.to_os_string()));
            }
            let input = cx.new(|cx| InputState::new(window, cx).default_value(shown));
            subscriptions.push(cx.subscribe(&input, |this: &mut Self, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.stop_saving();
                    cx.notify();
                }
            }));
            input
        });
        let mut places = Vec::new();
        if let Some(home) = &home {
            places.push(ChooserPlace {
                label_key: "portal-chooser-place-home",
                path: home.clone(),
            });
            for (label_key, folder) in [
                ("portal-chooser-place-desktop", "Desktop"),
                ("portal-chooser-place-documents", "Documents"),
                ("portal-chooser-place-downloads", "Downloads"),
            ] {
                places.push(ChooserPlace {
                    label_key,
                    path: home.join(folder),
                });
            }
        }
        places.push(ChooserPlace {
            label_key: "portal-chooser-place-root",
            path: PathBuf::from("/"),
        });
        let filter = request
            .current_filter()
            .or_else(|| (!request.filters().is_empty()).then_some(0));
        let choices = request
            .choices()
            .iter()
            .map(|choice| choice.initial().into())
            .collect();
        // A Close from the portal ends the request; the window goes with it.
        let watch = cx.spawn_in(window, async move |this, cx| {
            std::future::poll_fn(|context| {
                cancellation.register_waker(context.waker());
                if cancellation.is_cancelled() {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await;
            let _ = this.update_in(cx, |state, window, cx| {
                state.response = None;
                window.defer(cx, |window, _| window.remove_window());
            });
        });
        let mut chooser = Self {
            request,
            catalog,
            folder: folder.clone(),
            listing: Listing::Loading,
            shown: Arc::new(Vec::new()),
            places,
            selected: Vec::new(),
            filter,
            show_hidden: false,
            name,
            raw_name,
            choices,
            replacing: None,
            response: Some(response),
            load: None,
            check: None,
            _watch: Some(watch),
            scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            pending_focus: true,
            _subscriptions: subscriptions,
        };
        chooser.open_folder(folder, cx);
        chooser
    }

    fn message(&self, key: &str) -> String {
        self.catalog
            .message(key)
            .map_or_else(|_| key.to_owned(), str::to_owned)
    }

    /// Forgets a pending replace question and stops an existence check, as
    /// the name or folder it was about has changed.
    fn stop_saving(&mut self) {
        self.replacing = None;
        self.check = None;
    }

    /// Lists `folder` off the UI thread and shows it when read.
    fn open_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.folder = folder.clone();
        self.listing = Listing::Loading;
        self.selected.clear();
        self.stop_saving();
        self.refresh_shown();
        let mime_types = self
            .request
            .filters()
            .iter()
            .any(ChooserFilter::needs_mime_types);
        let work = cx.background_spawn(async move { read_folder(&folder, mime_types).await });
        self.load = Some(cx.spawn(async move |this, cx| {
            let listing = work.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |state, cx| {
                    state.listing = listing;
                    state.refresh_shown();
                    cx.notify();
                });
            }
        }));
        cx.notify();
    }

    /// Whether the chooser picks folders instead of files.
    fn picks_folders(&self) -> bool {
        self.request.kind() == BackendChooserKind::Open && self.request.directory()
    }

    /// Works out the entries the list shows: folders, and the files the
    /// chosen filter matches; hidden items only when asked for. The
    /// selection keeps only shown entries.
    fn refresh_shown(&mut self) {
        let Listing::Ready(entries) = &self.listing else {
            self.shown = Arc::new(Vec::new());
            return;
        };
        let filter = self
            .filter
            .and_then(|index| self.request.filters().get(index));
        let types = filter
            .filter(|filter| filter.needs_mime_types())
            .map(|_| NameMimeTypes::new());
        let shown: Vec<ChooserEntry> = entries
            .iter()
            .filter(|entry| self.show_hidden || !entry.hidden)
            .filter(|entry| {
                entry.folder
                    || (!self.picks_folders()
                        && filter.is_none_or(|filter| {
                            filter.matches(&entry.name, entry.mime_type.as_deref(), types.as_ref())
                        }))
            })
            .cloned()
            .collect();
        self.selected
            .retain(|path| shown.iter().any(|entry| entry.path == *path));
        self.shown = Arc::new(shown);
    }

    fn click_entry(
        &mut self,
        entry: &ChooserEntry,
        double: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.stop_saving();
        if entry.folder && (!self.picks_folders() || double) {
            self.open_folder(entry.path.clone(), cx);
            return;
        }
        match self.request.kind() {
            BackendChooserKind::Open if self.request.multiple() => {
                if let Some(index) = self.selected.iter().position(|path| *path == entry.path) {
                    self.selected.remove(index);
                } else {
                    self.selected.push(entry.path.clone());
                }
            }
            BackendChooserKind::Open => self.selected = vec![entry.path.clone()],
            // Clicking a file in Save takes its name, exactly.
            BackendChooserKind::Save => {
                self.selected = vec![entry.path.clone()];
                let shown = entry.name.to_string_lossy().into_owned();
                self.raw_name = entry
                    .name
                    .to_str()
                    .is_none()
                    .then(|| (shown.clone(), entry.name.clone()));
                if let Some(input) = &self.name {
                    input.update(cx, |input, cx| input.set_value(shown, window, cx));
                }
                // Setting the field emits a change, which stops saving.
                self.check = None;
            }
            BackendChooserKind::SaveMany => {}
        }
        cx.notify();
    }

    /// The name the Save field names: the exact name behind it when the
    /// field shows a name that is not valid UTF-8.
    fn save_name(&self, cx: &App) -> Option<OsString> {
        let text = self.name.as_ref()?.read(cx).value().to_string();
        if let Some((shown, raw)) = &self.raw_name
            && *shown == text
        {
            return Some(raw.clone());
        }
        valid_name(&text).then(|| OsString::from(text))
    }

    /// Whether Accept may be pressed: the folder was read, and the request
    /// has what it needs.
    fn can_accept(&self, cx: &App) -> bool {
        if !matches!(self.listing, Listing::Ready(_)) || self.replacing.is_some() {
            return false;
        }
        match self.request.kind() {
            BackendChooserKind::Open if self.picks_folders() => true,
            BackendChooserKind::Open => !self.selected.is_empty(),
            BackendChooserKind::Save => self.save_name(cx).is_some(),
            BackendChooserKind::SaveMany => true,
        }
    }

    /// The paths Accept returns now.
    fn accepted_paths(&self, cx: &App) -> Vec<PathBuf> {
        match self.request.kind() {
            BackendChooserKind::Open if self.picks_folders() => {
                if self.selected.is_empty() {
                    vec![self.folder.clone()]
                } else {
                    self.selected.clone()
                }
            }
            // In the order the list shows them.
            BackendChooserKind::Open => self
                .shown
                .iter()
                .filter(|entry| self.selected.contains(&entry.path))
                .map(|entry| entry.path.clone())
                .collect(),
            BackendChooserKind::Save => self
                .save_name(cx)
                .map(|name| vec![self.folder.join(name)])
                .unwrap_or_default(),
            BackendChooserKind::SaveMany => self
                .request
                .files()
                .iter()
                .map(|name| self.folder.join(name))
                .collect(),
        }
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_accept(cx) {
            return;
        }
        let paths = self.accepted_paths(cx);
        if paths.is_empty() {
            return;
        }
        if self.request.kind() == BackendChooserKind::Open {
            self.confirm(paths, window, cx);
            return;
        }
        // Saving asks before it replaces a file, and when it cannot tell
        // whether one is there. The check runs off the UI thread.
        let targets = paths.clone();
        let work = cx.background_spawn(async move {
            let store = LocalStore::new();
            targets.iter().any(|path| {
                !matches!(
                    store.resolve_item(&StorePath::from_unix_path(path.as_os_str())),
                    Ok(None)
                )
            })
        });
        self.check = Some(cx.spawn_in(window, async move |this, cx| {
            let ask = work.await;
            let _ = this.update_in(cx, |state, window, cx| {
                state.check = None;
                if ask {
                    state.replacing = Some(paths);
                    cx.notify();
                } else {
                    state.confirm(paths, window, cx);
                }
            });
        }));
    }

    fn confirm(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let selection = ChooserSelection {
            paths,
            filter: self.filter,
            choices: self
                .request
                .choices()
                .iter()
                .zip(&self.choices)
                .map(|(choice, value)| (choice.id().into(), value.clone()))
                .collect(),
        };
        self.decide(BackendChooserDecision::Selected(selection), window, cx);
    }

    fn decide(
        &mut self,
        decision: BackendChooserDecision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(response) = self.response.take() {
            let _ = response.try_send(decision);
        }
        window.defer(cx, |window, _| window.remove_window());
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.decide(BackendChooserDecision::Cancelled, window, cx);
    }

    fn render_entry(
        &self,
        index: usize,
        entry: &ChooserEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown_name = entry.name.to_string_lossy().into_owned();
        // A name that is not valid UTF-8 can share its shown form with
        // another; its position keeps its ID apart.
        let id = if entry.name.to_str().is_some() {
            format!("portal-chooser-entry-{shown_name}")
        } else {
            format!("portal-chooser-entry-{shown_name}-{index}")
        };
        let label = if entry.folder {
            format!("{shown_name}/")
        } else {
            shown_name
        };
        let saving_many = self.request.kind() == BackendChooserKind::SaveMany;
        let entry = entry.clone();
        Button::new(SharedString::from(id))
            .label(label)
            .small()
            .w_full()
            .selected(self.selected.contains(&entry.path))
            .disabled(saving_many && !entry.folder)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_entry(&entry, event.click_count() >= 2, window, cx);
            }))
            .into_any_element()
    }

    fn render_choices(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.request
            .choices()
            .iter()
            .enumerate()
            .map(|(index, choice)| {
                let value = self.choices.get(index).cloned().unwrap_or_default();
                let id = choice.id().to_owned();
                if choice.options().is_empty() {
                    let on = value.as_ref() == "true";
                    Button::new(SharedString::from(format!("portal-chooser-choice-{id}")))
                        .label(choice.label().to_owned())
                        .small()
                        .selected(on)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(value) = this.choices.get_mut(index) {
                                *value = if on { "false" } else { "true" }.into();
                            }
                            cx.notify();
                        }))
                        .into_any_element()
                } else {
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_1()
                        .child(choice.label().to_owned())
                        .children(choice.options().iter().map(|(option, label)| {
                            let option = option.clone();
                            Button::new(SharedString::from(format!(
                                "portal-chooser-choice-{id}-{option}"
                            )))
                            .label(label.to_string())
                            .small()
                            .selected(value == option)
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(value) = this.choices.get_mut(index) {
                                        *value = option.clone();
                                    }
                                    cx.notify();
                                },
                            ))
                        }))
                        .into_any_element()
                }
            })
            .collect()
    }
}

impl Drop for PortalChooser {
    /// A window closed without a decision answers cancelled.
    fn drop(&mut self) {
        if let Some(response) = self.response.take() {
            let _ = response.try_send(BackendChooserDecision::Cancelled);
        }
    }
}

/// Whether `name` names one file in a folder.
fn valid_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\0'])
}

/// Reads every entry of `folder`, sorted with folders first and then by
/// name. A link counts as the kind of item it leads to. With `mime_types`,
/// each file's MIME type is judged from its name.
async fn read_folder(folder: &Path, mime_types: bool) -> Listing {
    let store = LocalStore::new();
    let location = StorePath::from_unix_path(folder.as_os_str());
    let limits = ResourceLimits::default();
    let mut request = Some(PageRequest::first(&limits));
    let mut items = Vec::new();
    while let Some(page_request) = request.take() {
        let Ok(page) = store
            .read_directory(&location, page_request, CancellationToken::new())
            .await
        else {
            return Listing::Unreadable;
        };
        request = page.next_request();
        items.extend(page.into_items());
    }
    let names = mime_types.then(NameMimeTypes::new);
    let mut entries: Vec<ChooserEntry> = items
        .iter()
        .filter_map(|item| chooser_entry(&store, item, names.as_ref()))
        .collect();
    entries.sort_by(|first, second| {
        second.folder.cmp(&first.folder).then_with(|| {
            first
                .name
                .to_string_lossy()
                .to_lowercase()
                .cmp(&second.name.to_string_lossy().to_lowercase())
        })
    });
    Listing::Ready(entries)
}

/// The entry for `item`, or `None` for a socket, pipe or device, and for a
/// link that leads to one, leads nowhere, or cannot be followed.
fn chooser_entry(
    store: &LocalStore,
    item: &StoreItem,
    names: Option<&NameMimeTypes>,
) -> Option<ChooserEntry> {
    let path = item.path().as_unix_path()?.to_path_buf();
    let name = path.file_name()?.to_os_string();
    let kind = match item.kind() {
        ItemKind::SymbolicLink => store.resolve_link_target(item.path()).ok()??.kind(),
        kind => kind,
    };
    let folder = match kind {
        ItemKind::Directory => true,
        ItemKind::RegularFile => false,
        ItemKind::SymbolicLink | ItemKind::Other => return None,
    };
    let mime_type = (!folder)
        .then(|| names.and_then(|names| names.mime_type(&name)))
        .flatten();
    Some(ChooserEntry {
        hidden: name.as_encoded_bytes().starts_with(b"."),
        name,
        path,
        folder,
        mime_type,
    })
}

impl Render for PortalChooser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.focus.focus(window, cx);
            self.pending_focus = false;
        }
        let can_accept = self.can_accept(cx);
        let choices = self.render_choices(cx);
        let colors = cx.theme().colors;
        let location = self.folder.to_string_lossy().into_owned();
        let places = self.places.iter().enumerate().map(|(index, place)| {
            let path = place.path.clone();
            Button::new(SharedString::from(format!("portal-chooser-place-{index}")))
                .label(self.message(place.label_key))
                .small()
                .w_full()
                .selected(place.path == self.folder)
                .on_click(cx.listener(move |this, _, _, cx| this.open_folder(path.clone(), cx)))
        });
        let list: AnyElement = match &self.listing {
            Listing::Loading => status_note(
                "portal-chooser-loading",
                self.message("portal-chooser-loading"),
                Role::Status,
            ),
            Listing::Unreadable => status_note(
                "portal-chooser-unreadable",
                self.message("portal-chooser-unreadable"),
                Role::Alert,
            ),
            Listing::Ready(_) if self.shown.is_empty() => status_note(
                "portal-chooser-empty",
                self.message("portal-chooser-empty"),
                Role::Status,
            ),
            // Only the rows in view are built.
            Listing::Ready(_) => {
                let shown = Arc::clone(&self.shown);
                uniform_list(
                    "portal-chooser-entries",
                    shown.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .filter_map(|index| shown.get(index).map(|entry| (index, entry)))
                            .map(|(index, entry)| this.render_entry(index, entry, cx))
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full()
                .into_any_element()
            }
        };
        let filters = self
            .request
            .filters()
            .iter()
            .enumerate()
            .map(|(index, filter)| {
                Button::new(SharedString::from(format!("portal-chooser-filter-{index}")))
                    .label(filter.label().to_owned())
                    .small()
                    .selected(self.filter == Some(index))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter = Some(index);
                        this.refresh_shown();
                        cx.notify();
                    }))
            });
        let accept_label = self.request.accept_label().map_or_else(
            || {
                self.message(match self.request.kind() {
                    BackendChooserKind::Open if self.picks_folders() => {
                        "portal-chooser-select-folder"
                    }
                    BackendChooserKind::Open => "portal-chooser-open",
                    BackendChooserKind::Save | BackendChooserKind::SaveMany => {
                        "portal-chooser-save"
                    }
                })
            },
            str::to_owned,
        );
        div()
            .id("portal-chooser")
            .test_support()
            .key_context("PortalChooser")
            .role(Role::Dialog)
            .aria_label(self.request.title().to_owned())
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(colors.background)
            .text_color(colors.foreground)
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.cancel(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("portal-chooser-up")
                            .label(self.message("portal-chooser-up"))
                            .small()
                            .disabled(self.folder.parent().is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(parent) = this.folder.parent() {
                                    let parent = parent.to_path_buf();
                                    this.open_folder(parent, cx);
                                }
                            })),
                    )
                    .child(
                        div()
                            .id("portal-chooser-location")
                            .test_support()
                            .role(Role::Label)
                            .aria_label(location.clone())
                            .flex_1()
                            .min_w(px(0.))
                            .child(location.clone()),
                    )
                    .child(
                        Button::new("portal-chooser-hidden")
                            .label(self.message("portal-chooser-hidden"))
                            .small()
                            .selected(self.show_hidden)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_hidden = !this.show_hidden;
                                this.refresh_shown();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .gap_3()
                    .child(
                        div()
                            .id("portal-chooser-places")
                            .test_support()
                            .role(Role::Navigation)
                            .aria_label(self.message("portal-chooser-places"))
                            .w(px(160.))
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(places),
                    )
                    .child(
                        div()
                            .id("portal-chooser-list")
                            .test_support()
                            .role(Role::List)
                            .aria_label(location)
                            .flex_1()
                            .min_w(px(0.))
                            .border_1()
                            .border_color(colors.border)
                            .rounded_md()
                            .p_1()
                            .child(list),
                    ),
            )
            .when(!self.request.filters().is_empty(), |chooser| {
                chooser.child(
                    div()
                        .id("portal-chooser-filters")
                        .test_support()
                        .role(Role::Group)
                        .aria_label(self.message("portal-chooser-filters"))
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .children(filters),
                )
            })
            .when(!choices.is_empty(), |chooser| {
                chooser.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .children(choices),
                )
            })
            .when_some(self.name.clone(), |chooser, name| {
                chooser.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(self.message("portal-chooser-name"))
                        .child(Input::new(&name).id("portal-chooser-name")),
                )
            })
            .when(
                self.request.kind() == BackendChooserKind::SaveMany,
                |chooser| {
                    let names = self
                        .request
                        .files()
                        .iter()
                        .map(|name| name.to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join(self.catalog.list_separator());
                    let text = format!("{} {names}", self.message("portal-chooser-save-many"));
                    chooser.child(status_note("portal-chooser-save-many", text, Role::Label))
                },
            )
            .when_some(self.replacing.clone(), |chooser, paths| {
                let names = paths
                    .iter()
                    .filter_map(|path| path.file_name().map(OsStr::to_string_lossy))
                    .collect::<Vec<_>>()
                    .join(self.catalog.list_separator());
                let question = format!(
                    "{} ({names})",
                    self.message(if paths.len() == 1 {
                        "portal-chooser-replace-one"
                    } else {
                        "portal-chooser-replace-many"
                    })
                );
                chooser.child(
                    div()
                        .id("portal-chooser-replace-question")
                        .test_support()
                        .role(Role::Alert)
                        .aria_label(question.clone())
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(question)
                        .child(
                            Button::new("portal-chooser-replace")
                                .label(self.message("portal-chooser-replace"))
                                .small()
                                .danger()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm(paths.clone(), window, cx);
                                })),
                        )
                        .child(
                            Button::new("portal-chooser-keep")
                                .label(self.message("portal-chooser-keep"))
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.replacing = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("portal-chooser-cancel")
                            .label(self.message("portal-chooser-cancel"))
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    )
                    .child(
                        Button::new("portal-chooser-accept")
                            .label(accept_label)
                            .primary()
                            .disabled(!can_accept)
                            .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    ),
            )
    }
}

/// A labelled line of text in the chooser.
fn status_note(id: &'static str, text: String, role: Role) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .role(role)
        .aria_label(text.clone())
        .child(text)
        .into_any_element()
}
