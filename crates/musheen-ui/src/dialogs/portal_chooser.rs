//! The chooser window Musheen's FileChooser portal backend opens for each
//! request (SYS-027). The user browses places and folders and picks what the
//! request asks for: one file, several files or a folder to open, a folder
//! and a name to save, or a folder for the files Save Many names. Only a
//! selection the user confirms leaves the window; Cancel, Escape and closing
//! the window answer cancelled.

use crate::i18n::Catalog;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Escape, Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, Entity, FocusHandle, Role, SharedString, Subscription,
    Task, TestSupportExt, TitlebarOptions, Window, WindowBounds, WindowOptions, div, px, size,
};
use musheen_core::{
    CancellationToken, ItemKind, PageRequest, ResourceLimits, Store, StoreItem, StorePath,
};
use musheen_desktop::{
    BackendChooserDecision, BackendChooserKind, BackendChooserRequest, NameMimeTypes,
};
use musheen_local::LocalStore;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

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
    places: Vec<ChooserPlace>,
    selected: Vec<PathBuf>,
    filter: Option<usize>,
    show_hidden: bool,
    name: Option<Entity<InputState>>,
    /// The paths Accept would return, waiting for the user to allow
    /// replacing the files that exist there.
    replacing: Option<Vec<PathBuf>>,
    /// Where the decision goes; taken once the user decides, so a closed
    /// window answers cancelled only when nothing was decided.
    response: Option<async_channel::Sender<BackendChooserDecision>>,
    load: Option<Task<()>>,
    check: Option<Task<()>>,
    focus: FocusHandle,
    pending_focus: bool,
    _subscriptions: Vec<Subscription>,
}

/// Opens a chooser window for `request`, answering on `response`. When no
/// window can open, the request is cancelled.
pub(crate) fn open_portal_chooser(
    request: BackendChooserRequest,
    response: async_channel::Sender<BackendChooserDecision>,
    cx: &mut App,
) {
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
        let view = cx.new(|cx| PortalChooser::new(request, catalog, response, window, cx));
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
        let name = (request.kind() == BackendChooserKind::Save).then(|| {
            let suggested = request.current_name().unwrap_or_default().to_owned();
            let input = cx.new(|cx| InputState::new(window, cx).default_value(suggested));
            subscriptions.push(cx.subscribe(&input, |this: &mut Self, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.replacing = None;
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
        let mut chooser = Self {
            request,
            catalog,
            folder: folder.clone(),
            listing: Listing::Loading,
            places,
            selected: Vec::new(),
            filter,
            show_hidden: false,
            name,
            replacing: None,
            response: Some(response),
            load: None,
            check: None,
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

    /// Lists `folder` off the UI thread and shows it when read.
    fn open_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        self.folder = folder.clone();
        self.listing = Listing::Loading;
        self.selected.clear();
        self.replacing = None;
        let mime_types = self
            .request
            .filters()
            .iter()
            .any(musheen_desktop::ChooserFilter::needs_mime_types);
        let work = cx.background_spawn(async move { read_folder(&folder, mime_types).await });
        self.load = Some(cx.spawn(async move |this, cx| {
            let listing = work.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |state, cx| {
                    state.listing = listing;
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

    /// The entries the list shows: folders, and the files the chosen filter
    /// matches; hidden items only when asked for.
    fn shown_entries(&self) -> Vec<ChooserEntry> {
        let Listing::Ready(entries) = &self.listing else {
            return Vec::new();
        };
        let filter = self
            .filter
            .and_then(|index| self.request.filters().get(index));
        entries
            .iter()
            .filter(|entry| self.show_hidden || !entry.hidden)
            .filter(|entry| {
                entry.folder
                    || (!self.picks_folders()
                        && filter.is_none_or(|filter| {
                            filter.matches(&entry.name, entry.mime_type.as_deref())
                        }))
            })
            .cloned()
            .collect()
    }

    fn click_entry(
        &mut self,
        entry: &ChooserEntry,
        double: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.replacing = None;
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
            // Clicking a file in Save takes its name.
            BackendChooserKind::Save => {
                self.selected = vec![entry.path.clone()];
                let name = entry.name.to_string_lossy().into_owned();
                if let Some(input) = &self.name {
                    input.update(cx, |input, cx| input.set_value(name, window, cx));
                }
            }
            BackendChooserKind::SaveMany => {}
        }
        cx.notify();
    }

    /// The paths Accept would return now, or `None` when it cannot accept.
    fn accepted_paths(&self, cx: &App) -> Option<Vec<PathBuf>> {
        match self.request.kind() {
            BackendChooserKind::Open if self.picks_folders() => Some(if self.selected.is_empty() {
                vec![self.folder.clone()]
            } else {
                self.selected.clone()
            }),
            BackendChooserKind::Open => {
                // In the order the list shows them.
                let shown = self.shown_entries();
                let paths: Vec<PathBuf> = shown
                    .iter()
                    .filter(|entry| self.selected.contains(&entry.path))
                    .map(|entry| entry.path.clone())
                    .collect();
                (!paths.is_empty()).then_some(paths)
            }
            BackendChooserKind::Save => {
                let name = self.name.as_ref()?.read(cx).value().to_string();
                valid_name(&name).then(|| vec![self.folder.join(name)])
            }
            BackendChooserKind::SaveMany => Some(
                self.request
                    .files()
                    .iter()
                    .map(|name| self.folder.join(name))
                    .collect(),
            ),
        }
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(paths) = self.accepted_paths(cx) else {
            return;
        };
        if matches!(self.request.kind(), BackendChooserKind::Open) {
            self.decide(BackendChooserDecision::Confirmed(paths), window, cx);
            return;
        }
        // Saving asks before it replaces a file; whether one exists is read
        // off the UI thread.
        let targets = paths.clone();
        let work = cx.background_spawn(async move {
            let store = LocalStore::new();
            targets.iter().any(|path| {
                store
                    .resolve_item(&StorePath::from_unix_path(path.as_os_str()))
                    .is_ok_and(|item| item.is_some())
            })
        });
        self.check = Some(cx.spawn_in(window, async move |this, cx| {
            let exists = work.await;
            let _ = this.update_in(cx, |state, window, cx| {
                if exists {
                    state.replacing = Some(paths);
                    cx.notify();
                } else {
                    state.decide(BackendChooserDecision::Confirmed(paths), window, cx);
                }
            });
        }));
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
/// name. A link counts as a folder when it leads to one. With `mime_types`,
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

fn chooser_entry(
    store: &LocalStore,
    item: &StoreItem,
    names: Option<&NameMimeTypes>,
) -> Option<ChooserEntry> {
    let path = item.path().as_unix_path()?.to_path_buf();
    let name = path.file_name()?.to_os_string();
    let folder = match item.kind() {
        ItemKind::Directory => true,
        ItemKind::RegularFile => false,
        ItemKind::SymbolicLink => store
            .resolve_link_target(item.path())
            .ok()
            .flatten()
            .is_some_and(|target| target.kind() == ItemKind::Directory),
        // Sockets, pipes and devices are not offered.
        ItemKind::Other => return None,
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
            Listing::Loading => div()
                .id("portal-chooser-loading")
                .test_support()
                .role(Role::Status)
                .aria_label(self.message("portal-chooser-loading"))
                .child(self.message("portal-chooser-loading"))
                .into_any_element(),
            Listing::Unreadable => div()
                .id("portal-chooser-unreadable")
                .test_support()
                .role(Role::Alert)
                .aria_label(self.message("portal-chooser-unreadable"))
                .child(self.message("portal-chooser-unreadable"))
                .into_any_element(),
            Listing::Ready(_) => {
                let shown = self.shown_entries();
                if shown.is_empty() {
                    div()
                        .id("portal-chooser-empty")
                        .test_support()
                        .role(Role::Status)
                        .aria_label(self.message("portal-chooser-empty"))
                        .child(self.message("portal-chooser-empty"))
                        .into_any_element()
                } else {
                    let saving_many = self.request.kind() == BackendChooserKind::SaveMany;
                    div()
                        .id("portal-chooser-entries")
                        .test_support()
                        .role(Role::List)
                        .aria_label(location.clone())
                        .flex()
                        .flex_col()
                        .gap_1()
                        .children(shown.into_iter().map(|entry| {
                            let name = entry.name.to_string_lossy().into_owned();
                            let label = if entry.folder {
                                format!("{name}/")
                            } else {
                                name.clone()
                            };
                            let selected = self.selected.contains(&entry.path);
                            Button::new(SharedString::from(format!("portal-chooser-entry-{name}")))
                                .label(label)
                                .small()
                                .w_full()
                                .selected(selected)
                                .disabled(saving_many && !entry.folder)
                                .on_click(cx.listener(
                                    move |this, event: &ClickEvent, window, cx| {
                                        this.click_entry(
                                            &entry,
                                            event.click_count() >= 2,
                                            window,
                                            cx,
                                        );
                                    },
                                ))
                        }))
                        .into_any_element()
                }
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
                        this.selected.clear();
                        cx.notify();
                    }))
            });
        let accept_key = match self.request.kind() {
            BackendChooserKind::Open if self.picks_folders() => "portal-chooser-select-folder",
            BackendChooserKind::Open => "portal-chooser-open",
            BackendChooserKind::Save | BackendChooserKind::SaveMany => "portal-chooser-save",
        };
        let can_accept = self.accepted_paths(cx).is_some() && self.replacing.is_none();
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
                            .flex_1()
                            .min_w(px(0.))
                            .border_1()
                            .border_color(colors.border)
                            .rounded_md()
                            .p_1()
                            .child(list)
                            .overflow_y_scrollbar(),
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
                    chooser.child(
                        div()
                            .id("portal-chooser-save-many")
                            .test_support()
                            .role(Role::Label)
                            .aria_label(text.clone())
                            .child(text),
                    )
                },
            )
            .when_some(self.replacing.clone(), |chooser, paths| {
                let question = self.message(if paths.len() == 1 {
                    "portal-chooser-replace-one"
                } else {
                    "portal-chooser-replace-many"
                });
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
                                    let paths = paths.clone();
                                    this.decide(
                                        BackendChooserDecision::Confirmed(paths),
                                        window,
                                        cx,
                                    );
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
                            .label(self.message(accept_key))
                            .primary()
                            .disabled(!can_accept)
                            .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    ),
            )
    }
}
