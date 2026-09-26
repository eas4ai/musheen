use super::presentation::{choices, display_number, display_value, input_value, stored_number};
use super::{SettingsBackends, SettingsState};
use crate::theme::preview::AppearanceSnapshot;
use crate::{ApplicationIdentity, Catalog, Locale};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Root, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable, Global, IntoElement,
    Render, Role, ScrollHandle, SharedString, Subscription, TestSupportExt, TitlebarOptions,
    Window, WindowBounds, WindowHandle, WindowOptions, div, px, size,
};
use musheen_desktop::{
    CatalogStore, CredentialReference, ProxyKind, RemoteProtocol, SaveRequirement, SecurityPolicy,
    SettingKind, SettingSpec, SettingsDocument, SettingsFeature, SettingsPage, SettingsStore,
    TestReport, settings_schema,
};
use std::collections::BTreeMap;
use std::sync::Arc;

pub(crate) type RecentHistoryClearer =
    Arc<dyn Fn(&mut App) -> Result<(), Box<str>> + Send + Sync + 'static>;

fn publish_saved_settings(document: &SettingsDocument, cx: &mut App) {
    let changed_remote_connections =
        cx.try_global::<super::RuntimeSettings>()
            .is_none_or(|current| {
                current.0.value("remote.connections") != document.value("remote.connections")
            });
    cx.set_global(super::RuntimeSettings(document.clone()));
    if changed_remote_connections {
        let revision = cx
            .try_global::<super::RemoteConnectionsRevision>()
            .map_or(0, |revision| revision.0)
            .wrapping_add(1);
        cx.set_global(super::RemoteConnectionsRevision(revision));
    }
    cx.refresh_windows();
}

#[derive(Default)]
struct SettingsWindowOwner {
    handle: Option<WindowHandle<Root>>,
    view: Option<gpui_kit::WeakEntity<SettingsWindow>>,
}
impl Global for SettingsWindowOwner {}

/// Application-global ownership ensures Settings commands from separate
/// browsers activate the same non-modal window and retain its draft and query.
pub fn open_settings_window(cx: &mut App) {
    open_settings_at(SettingsStore::for_current_user(), None, cx);
}

pub(crate) fn open_settings_window_with_recent_clearer(
    clearer: RecentHistoryClearer,
    cx: &mut App,
) {
    open_settings_at(SettingsStore::for_current_user(), Some(clearer), cx);
}

fn open_settings_at(
    store: SettingsStore,
    recent_history_clearer: Option<RecentHistoryClearer>,
    cx: &mut App,
) {
    if !cx.has_global::<SettingsWindowOwner>() {
        cx.set_global(SettingsWindowOwner::default());
    }
    if let Some(handle) = cx.global::<SettingsWindowOwner>().handle
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let catalog = Catalog::load(Locale::from_environment()).expect("settings catalog is valid");
    let title = catalog
        .message("settings-title")
        .expect("settings title is localized")
        .to_owned();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(840.), px(680.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..Default::default()
        }),
        window_min_size: Some(size(px(720.), px(480.))),
        app_id: Some(ApplicationIdentity::ID.into()),
        ..Default::default()
    };
    match cx.open_window(options, move |window, cx| {
        let view = cx.new(|cx| {
            let mut settings = SettingsWindow::new(
                store,
                SettingsBackends::default().with(SettingsFeature::Catalog),
                catalog,
                window,
                cx,
            );
            settings.recent_history_clearer = recent_history_clearer;
            settings
        });
        cx.global_mut::<SettingsWindowOwner>().view = Some(view.downgrade());
        cx.new(|cx| Root::new(view, window, cx))
    }) {
        Ok(handle) => cx.global_mut::<SettingsWindowOwner>().handle = Some(handle),
        Err(error) => {
            cx.global_mut::<SettingsWindowOwner>().view = None;
            eprintln!("could not open Settings: {error}");
        }
    }
}

pub struct SettingsWindow {
    pub(super) state: SettingsState,
    pub(super) store: SettingsStore,
    pub(super) catalog: Catalog,
    search: Entity<InputState>,
    inputs: BTreeMap<&'static str, Entity<InputState>>,
    subscriptions: Vec<Subscription>,
    pub(super) failure: Option<&'static str>,
    load_failed: bool,
    sync_inputs: bool,
    focus_pending: bool,
    scroll: ScrollHandle,
    pub(super) saving: bool,
    choices_focus: BTreeMap<&'static str, FocusHandle>,
    reset_trigger: FocusHandle,
    appearance_base: AppearanceSnapshot,
    pub(super) theme_input: Entity<InputState>,
    pub(super) theme_error: Option<&'static str>,
    pub(super) shortcut_input: Entity<InputState>,
    pub(super) shortcut_scope: musheen_core::ShortcutScope,
    pub(super) toolbar_move_focus: FocusHandle,
    pub(super) action_inputs: BTreeMap<&'static str, Entity<InputState>>,
    pub(super) action_shell: bool,
    pub(super) action_provider_uris: bool,
    pub(super) action_confirmation: musheen_desktop::ActionConfirmation,
    pub(super) remote_inputs: BTreeMap<&'static str, Entity<InputState>>,
    pub(super) remote_protocol: RemoteProtocol,
    pub(super) remote_security: SecurityPolicy,
    pub(super) remote_proxy: Option<ProxyKind>,
    pub(super) remote_credential: Option<CredentialReference>,
    pub(super) remote_tester: Arc<dyn super::remote::ConnectionTestService>,
    pub(super) remote_testing: bool,
    pub(super) remote_test_generation: u64,
    pub(super) remote_test_cancellation: Option<musheen_core::CancellationToken>,
    pub(super) remote_test_report: Option<TestReport>,
    pub(super) remote_save_requirement: Option<SaveRequirement>,
    pub(super) remote_validation_failed: bool,
    pub(super) remote_editor_open: bool,
    recent_history_clearer: Option<RecentHistoryClearer>,
}

impl SettingsWindow {
    pub fn new(
        store: SettingsStore,
        backends: SettingsBackends,
        catalog: Catalog,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_with_connection_tester(
            store,
            backends,
            catalog,
            super::default_connection_tester(),
            window,
            cx,
        )
    }

    pub fn new_with_connection_tester(
        store: SettingsStore,
        backends: SettingsBackends,
        catalog: Catalog,
        remote_tester: Arc<dyn super::remote::ConnectionTestService>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (document, failure) = match store.load() {
            Ok(document) => (document, None),
            Err(_) => (SettingsDocument::default(), Some("settings-load-error")),
        };
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                catalog
                    .message("settings-search")
                    .expect("localized search")
                    .to_owned(),
            )
        });
        let shortcut_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                catalog
                    .message("customization-chord")
                    .expect("localized shortcut prompt")
                    .to_owned(),
            )
        });
        let theme_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(
                document
                    .value("appearance.theme")
                    .expect("theme schema key"),
            )
        });
        let remote_credential = document
            .value("remote.credential")
            .filter(|value| !value.is_empty())
            .and_then(|value| CredentialReference::from_setting_value(&value).ok());
        let mut this = Self {
            action_inputs: super::custom_actions::inputs(window, cx),
            action_shell: false,
            action_provider_uris: false,
            action_confirmation: musheen_desktop::ActionConfirmation::Always,
            remote_inputs: super::remote::inputs(window, cx),
            remote_protocol: RemoteProtocol::Sftp,
            remote_security: super::remote::default_security(RemoteProtocol::Sftp),
            remote_proxy: None,
            remote_credential,
            remote_tester,
            remote_testing: false,
            remote_test_generation: 0,
            remote_test_cancellation: None,
            remote_test_report: None,
            remote_save_requirement: None,
            remote_validation_failed: false,
            remote_editor_open: false,
            recent_history_clearer: None,
            theme_input: theme_input.clone(),
            theme_error: None,
            toolbar_move_focus: cx.focus_handle(),
            shortcut_input,
            shortcut_scope: musheen_core::ShortcutScope::Browser,
            state: SettingsState::new(document, backends),
            store,
            catalog,
            search,
            inputs: BTreeMap::from([("appearance.theme", theme_input)]),
            subscriptions: Vec::new(),
            failure,
            load_failed: failure.is_some(),
            sync_inputs: false,
            focus_pending: false,
            scroll: ScrollHandle::new(),
            saving: false,
            choices_focus: BTreeMap::new(),
            reset_trigger: cx.focus_handle(),
            appearance_base: AppearanceSnapshot::capture(cx),
        };
        for spec in settings_schema()
            .iter()
            .filter(|spec| this.state.available(spec))
        {
            if matches!(
                spec.kind,
                SettingKind::Toolbar
                    | SettingKind::Shortcuts
                    | SettingKind::Theme
                    | SettingKind::CustomActions
                    | SettingKind::ConnectionProfiles
                    | SettingKind::CredentialReference
            ) {
                this.choices_focus.insert(spec.key, cx.focus_handle());
                continue;
            }
            if !choices(spec.kind).is_empty() {
                this.choices_focus.insert(spec.key, cx.focus_handle());
                continue;
            }
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(this.label("settings-value-none"))
                    .default_value(input_value(
                        spec,
                        &this.state.draft().value(spec.key).expect("schema key"),
                        &this.catalog,
                    ))
            });
            this.subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        if this.blocked() {
                            return;
                        }
                        let value = if matches!(spec.kind, SettingKind::Integer { .. }) {
                            stored_number(&input.read(cx).value())
                        } else {
                            input.read(cx).value().to_string()
                        };
                        let _validation = this.state.edit(spec.key, &value);
                        if spec.page == SettingsPage::Appearance {
                            this.preview_appearance(cx);
                        }
                        cx.notify();
                    }
                },
            ));
            this.inputs.insert(spec.key, input);
        }
        this.subscriptions
            .push(cx.subscribe(&this.search, |this, input, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.state.set_query(input.read(cx).value().to_string());
                    cx.notify();
                }
            }));
        for input in this.remote_inputs.values() {
            this.subscriptions
                .push(cx.subscribe(input, |this, _, event, cx| {
                    if matches!(event, InputEvent::Change) {
                        if this.remote_testing
                            || this.remote_test_report.as_ref().is_some_and(|report| {
                                this.remote_profile(cx)
                                    .is_ok_and(|profile| report.matches(&profile))
                            })
                        {
                            return;
                        }
                        this.invalidate_remote_test();
                        cx.notify();
                    }
                }));
        }
        this.subscriptions.push(cx.on_release(|this, cx| {
            if let Some(cancellation) = this.remote_test_cancellation.take() {
                cancellation.cancel();
            }
            this.restore_committed_appearance(cx);
        }));
        this
    }

    pub fn state(&self) -> &SettingsState {
        &self.state
    }

    pub(super) fn blocked(&self) -> bool {
        self.saving || self.load_failed || self.state.reset_confirmation_pending()
    }

    pub(super) fn preview_appearance(&self, cx: &mut App) {
        self.appearance_base.restore(cx);
        apply_appearance(self.state.draft(), cx);
    }

    fn restore_committed_appearance(&mut self, cx: &mut App) {
        self.state.cancel();
        self.preview_customization(cx);
        self.appearance_base.restore(cx);
        apply_appearance(self.state.draft(), cx);
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.blocked() || !self.state.errors().is_empty() || !self.state.is_dirty() {
            return;
        }
        self.saving = true;
        let document = self.state.draft().clone();
        let store = self.store.clone();
        let work = cx.background_spawn(async move { store.save(&document).map(|()| document) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            cx.update(|cx| {
                if let Ok(document) = &result {
                    publish_saved_settings(document, cx);
                }
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.saving = false;
                        match result {
                            Ok(document) => {
                                this.state.committed = document;
                                this.failure = None;
                            }
                            Err(_) => this.failure = Some("settings-save-error"),
                        }
                        cx.notify();
                    });
                } else {
                    match result {
                        Ok(document) => apply_appearance(&document, cx),
                        Err(error) => eprintln!("Musheen could not save settings: {error}"),
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn label(&self, key: &str) -> String {
        self.catalog
            .message(key)
            .expect("settings key is localized")
            .to_owned()
    }

    fn synchronize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sync_inputs {
            self.sync_inputs = false;
            self.theme_error = None;
            let theme_value = self
                .state
                .draft()
                .value("appearance.theme")
                .expect("theme schema key");
            self.theme_input
                .update(cx, |input, cx| input.set_value(theme_value, window, cx));
            for (key, input) in &self.inputs {
                let value = self.state.draft().value(key).expect("schema key");
                let spec = settings_schema()
                    .iter()
                    .find(|spec| spec.key == *key)
                    .expect("schema key");
                input.update(cx, |input, cx| {
                    input.set_value(input_value(spec, &value, &self.catalog), window, cx)
                });
            }
            self.preview_appearance(cx);
            self.preview_customization(cx);
        }
        if self.focus_pending {
            self.focus_pending = false;
            if let Some(input) = self
                .state
                .focused_key()
                .and_then(|key| self.inputs.get(key))
            {
                input.read(cx).focus_handle(cx).focus(window, cx);
            } else if let Some(focus) = self
                .state
                .focused_key()
                .and_then(|key| self.choices_focus.get(key))
            {
                focus.focus(window, cx);
            }
        }
    }

    fn render_sidebar(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut sidebar = div()
            .w(px(205.))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap_2()
            .p_3();
        if self.state.query().is_empty() {
            for page in SettingsPage::ALL {
                sidebar = sidebar.child(
                    native_button(page.label(), self.label(page.label()), cx)
                        .w_full()
                        .disabled(self.blocked())
                        .selected(self.state.page() == page)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.select_page(page);
                            cx.notify();
                        })),
                );
            }
        } else {
            for hit in self.state.search(self.state.query(), &self.catalog) {
                let key = hit.key;
                sidebar = sidebar.child(
                    native_button(
                        format!("result-{key}"),
                        format!("{} · {}", self.label(hit.page.label()), hit.label),
                        cx,
                    )
                    .w_full()
                    .disabled(self.blocked())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.state.navigate_to(key).is_ok() {
                            this.focus_pending = true;
                        }
                        cx.notify();
                    })),
                );
            }
        }
        sidebar
            .id("settings-sidebar")
            .test_support()
            .overflow_y_scroll()
    }

    fn render_remote_editor(&self, cx: &Context<Self>) -> AnyElement {
        super::remote::render_editor(self, cx)
    }

    fn render_controls(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut panel = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(div().text_xl().child(self.label(self.state.page().label())));
        let mut group = "";
        let mut child_index = 1;
        for spec in self.state.page_controls() {
            if group != spec.group {
                group = spec.group;
                panel = panel.child(div().text_lg().child(self.label(group)));
                child_index += 1;
            }
            if self.state.focused_key() == Some(spec.key) {
                self.scroll.scroll_to_item(child_index);
            }
            child_index += 1;
            let mut row = div()
                .id(SharedString::from(format!("row-{}", spec.key)))
                .test_support()
                .role(Role::Group)
                .aria_label(self.label(spec.label))
                .flex()
                .flex_col()
                .gap_1()
                .flex_shrink_0()
                .child(observed_label(
                    format!("label-{}", spec.key),
                    self.label(spec.label),
                ));
            if matches!(
                spec.kind,
                SettingKind::Toolbar
                    | SettingKind::Shortcuts
                    | SettingKind::Theme
                    | SettingKind::CustomActions
                    | SettingKind::ConnectionProfiles
            ) {
                let editor = match spec.kind {
                    SettingKind::Toolbar => self.render_toolbar_editor(cx).into_any_element(),
                    SettingKind::Shortcuts => self.render_shortcut_editor(cx).into_any_element(),
                    SettingKind::Theme => self.render_theme_editor(cx).into_any_element(),
                    SettingKind::CustomActions => {
                        self.render_custom_actions_editor(cx).into_any_element()
                    }
                    SettingKind::ConnectionProfiles => {
                        self.render_remote_editor(cx).into_any_element()
                    }
                    _ => unreachable!("editor kind"),
                };
                row = row.track_focus(&self.choices_focus[spec.key]).child(editor);
            }
            if !choices(spec.kind).is_empty() {
                row = row.child(self.render_choices(spec, cx));
            }
            if let Some(input) = self.inputs.get(spec.key) {
                row = row.child(
                    Input::new(input)
                        .id(spec.key)
                        .disabled(self.blocked())
                        .accessibility_id(spec.key)
                        .aria_label(self.label(spec.label)),
                );
            }
            if matches!(spec.kind, SettingKind::CredentialReference) {
                let status = display_value(
                    spec,
                    &self.state.draft().value(spec.key).expect("schema key"),
                    &self.catalog,
                );
                row = row.child(
                    div()
                        .id(spec.key)
                        .test_support()
                        .role(Role::Status)
                        .aria_label(status.clone())
                        .child(status),
                );
            }
            let values = match spec.kind {
                SettingKind::Boolean
                | SettingKind::Choice(_)
                | SettingKind::Toolbar
                | SettingKind::Shortcuts
                | SettingKind::Theme
                | SettingKind::CustomActions
                | SettingKind::ConnectionProfiles => String::new(),
                SettingKind::Integer { maximum, units } => format!(
                    "{}: {} {}",
                    self.label("settings-maximum"),
                    display_number(&maximum.to_string(), self.catalog.locale()),
                    self.label(units)
                ),
                SettingKind::CredentialReference => self.label("settings-credential-hint"),
            };
            row = row.child(
                observed_label(
                    format!("help-{}", spec.key),
                    format!(
                        "{}: {} {}",
                        self.label("settings-default"),
                        display_value(spec, spec.default, &self.catalog),
                        values
                    ),
                )
                .text_sm()
                .text_color(cx.theme().colors.muted_foreground),
            );
            if spec.restart_required {
                row = row.child(
                    observed_label(
                        format!("restart-{}", spec.key),
                        self.label("settings-restart"),
                    )
                    .text_sm(),
                );
            }
            if self.state.errors().contains(spec.key) {
                row = row.child(div().child(self.label("settings-invalid")));
            }
            panel = panel.child(row);
        }
        if let Some(clear_history) = self.clear_history_button(cx) {
            panel = panel.child(clear_history);
        }
        panel
            .child(
                Button::new("settings-reset-page")
                    .disabled(self.blocked())
                    .label(self.label("settings-reset-page"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.reset_page(this.state.page());
                        this.sync_inputs = true;
                        cx.notify();
                    })),
            )
            .id("settings-controls")
            .test_support()
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
    }

    fn clear_history_button(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let catalog_available = self
            .state
            .page_controls()
            .iter()
            .any(|spec| spec.key == "general.record_history");
        (self.state.page() == SettingsPage::General && catalog_available).then(|| {
            Button::new("settings-clear-recent-locations")
                .disabled(self.blocked())
                .label(self.label("settings-clear-recent-locations"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.clear_recent_locations(cx);
                    cx.notify();
                }))
                .into_any_element()
        })
    }

    fn clear_recent_locations(&mut self, cx: &mut Context<Self>) {
        let result = match self.recent_history_clearer.as_ref() {
            Some(clearer) => clearer(cx),
            None => super::clear_recent_locations(&CatalogStore::for_current_user())
                .map_err(|error| Box::<str>::from(error.to_string())),
        };
        self.failure = result.err().map(|_| "settings-save-error");
    }

    fn render_choices(&self, spec: &'static SettingSpec, cx: &Context<Self>) -> impl IntoElement {
        let selected = self.state.draft().value(spec.key).expect("schema key");
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(choices(spec.kind).iter().map(|value| {
                let value = *value;
                let active = value == selected;
                native_button(
                    if active {
                        spec.key.to_owned()
                    } else {
                        format!("{}:{value}", spec.key)
                    },
                    display_value(spec, value, &self.catalog),
                    cx,
                )
                .selected(active)
                .aria_toggled(if active {
                    gpui_kit::accesskit::Toggled::True
                } else {
                    gpui_kit::accesskit::Toggled::False
                })
                .disabled(self.blocked())
                .when(active, |button| {
                    button.track_focus(&self.choices_focus[spec.key])
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    if this.blocked() {
                        return;
                    }
                    if this.state.edit(spec.key, value).is_ok()
                        && spec.page == SettingsPage::Appearance
                    {
                        this.preview_appearance(cx);
                    }
                    this.choices_focus[spec.key].focus(window, cx);
                    cx.notify();
                }))
            }))
    }

    fn confirm_reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.blocked() {
            return;
        }
        self.state.request_reset_all();
        self.reset_trigger.focus(window, cx);
        let owner = cx.entity().downgrade();
        let cancel_owner = owner.clone();
        let title = self.label("settings-reset-all");
        let summary = self.label("settings-reset-summary");
        let footer = cx.new(|cx| ResetConfirmation {
            owner,
            cancel: self.label("settings-cancel"),
            confirm: self.label("settings-confirm"),
            cancel_focus: cx.focus_handle(),
            pending_focus: true,
        });
        window.open_dialog(cx, move |dialog, _, _| {
            let owner = cancel_owner.clone();
            dialog
                .title(title.clone())
                .close_button(false)
                .overlay_closable(false)
                .child(observed_label("settings-reset-summary", summary.clone()))
                .footer(footer.clone())
                .on_ok(|_, _, _| false)
                .on_cancel(move |_, _, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        this.state.confirm_reset_all(false);
                        cx.notify();
                    });
                    true
                })
        });
        cx.notify();
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.synchronize(window, cx);
        let mut body = div()
            .id("settings-window")
            .test_support()
            .role(Role::Dialog)
            .aria_label(self.label("settings-title"))
            .size_full()
            .flex()
            .flex_col()
            .bg(cx.theme().colors.background)
            .text_color(cx.theme().colors.foreground)
            .p_3()
            .gap_3()
            .child(
                Input::new(&self.search)
                    .disabled(self.blocked())
                    .id("settings-search")
                    .accessibility_id("settings-search")
                    .aria_label(self.label("settings-search")),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.catalog.locale() == Locale::Ar, |div| {
                        div.flex_row_reverse()
                    })
                    .child(self.render_sidebar(cx))
                    .child(self.render_controls(cx)),
            );
        if let Some(failure) = self.failure {
            body = body.child(self.label(failure));
        }
        if self.saving {
            body = body.child(self.label("settings-saving"));
        }
        body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    native_button("settings-reset-all", self.label("settings-reset-all"), cx)
                        .track_focus(&self.reset_trigger)
                        .disabled(self.blocked())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.confirm_reset(window, cx)),
                        ),
                )
                .child(
                    Button::new("settings-cancel")
                        .disabled(self.blocked())
                        .label(self.label("settings-cancel"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.restore_committed_appearance(cx);
                            window.remove_window();
                        })),
                )
                .child(
                    Button::new("settings-apply")
                        .label(self.label("settings-apply"))
                        .primary()
                        .disabled(
                            self.blocked()
                                || !self.state.is_dirty()
                                || !self.state.errors().is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save(cx);
                        })),
                ),
        )
        .children(Root::render_dialog_layer(window, cx))
    }
}

pub(crate) fn apply_appearance(document: &SettingsDocument, cx: &mut App) {
    // Capture the bridge's resolved desktop appearance before the first user
    // override. Reset must also work after reopening Settings with a saved override.
    if !cx.has_global::<DesktopAppearance>() {
        cx.set_global(DesktopAppearance(AppearanceSnapshot::capture(cx)));
    }
    // Startup resolves both desktop variants through the native bridge. Always
    // select from that lossless snapshot; previews must not replace one side
    // with a generic preset or inherit a previous preview.
    let desktop = cx.global::<DesktopAppearance>().0.clone();
    let mut preferences = desktop.preferences.clone();
    let mode = document.value("appearance.mode");
    let is_dark = match mode.as_deref() {
        Some("light" | "high-contrast") => false,
        Some("dark") => true,
        _ => desktop.is_dark(),
    };
    preferences.high_contrast |= mode.as_deref() == Some("high-contrast");
    preferences.reduce_motion |=
        document.value("appearance.reduce_motion").as_deref() == Some("true");
    preferences.reduce_transparency |= preferences.high_contrast;
    desktop.restore_mode(is_dark, &preferences, cx);
    crate::theme::preview::apply_tokens(document, cx);
}

/// Accept a freshly installed native theme, replace the immutable preview
/// base, and then replay the saved user policy over it. Called on the GPUI
/// thread by the runtime theme watcher.
pub(crate) fn accept_native_theme_change(cx: &mut App) {
    let appearance = AppearanceSnapshot::capture(cx);
    cx.set_global(DesktopAppearance(appearance.clone()));
    let settings = cx
        .try_global::<SettingsWindowOwner>()
        .and_then(|owner| owner.view.as_ref())
        .and_then(gpui_kit::WeakEntity::upgrade);
    if let Some(settings) = settings {
        settings.update(cx, |settings, cx| {
            settings.appearance_base = appearance;
            settings.preview_appearance(cx);
            cx.notify();
        });
        return;
    }
    if let Some(document) = cx
        .try_global::<super::RuntimeSettings>()
        .map(|settings| settings.0.clone())
    {
        apply_appearance(&document, cx);
    }
}

struct DesktopAppearance(AppearanceSnapshot);
impl Global for DesktopAppearance {}

pub(super) fn observed_label(
    id: impl Into<SharedString>,
    label: String,
) -> impl IntoElement + Styled {
    div()
        .id(id.into())
        .test_support()
        .role(Role::Label)
        .aria_label(label.clone())
        .min_w_0()
        .max_w_full()
        .whitespace_normal()
        .child(label)
}

pub(super) fn status_label(
    id: impl Into<SharedString>,
    label: String,
    role: Role,
) -> impl IntoElement + Styled {
    div()
        .id(id.into())
        .test_support()
        .role(role)
        .aria_label(label.clone())
        .min_w_0()
        .max_w_full()
        .whitespace_normal()
        .child(label)
}

pub(super) fn native_button(
    id: impl Into<SharedString>,
    label: String,
    cx: &App,
) -> gpui_kit::base::Button {
    let id = id.into();
    let colors = cx.theme().colors;
    gpui_kit::base::Button::new(id.clone())
        .accessibility_label(label.clone())
        .min_w_0()
        .max_w_full()
        .flex_shrink_0()
        .px_3()
        .py_2()
        .border_1()
        .rounded(cx.theme().radius)
        .bg(colors.secondary)
        .text_color(colors.secondary_foreground)
        .border_color(colors.border)
        .focus_visible(move |style| style.border_color(colors.ring).border_2())
        .styles(move |styles| {
            styles
                .selected(move |style| {
                    style
                        .bg(colors.primary)
                        .text_color(colors.primary_foreground)
                })
                .disabled(move |style| style.text_color(colors.muted_foreground))
        })
        .child(observed_label(format!("text-{id}"), label))
}

struct ResetConfirmation {
    owner: gpui_kit::WeakEntity<SettingsWindow>,
    cancel: String,
    confirm: String,
    cancel_focus: FocusHandle,
    pending_focus: bool,
}

impl Render for ResetConfirmation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.pending_focus = false;
            self.cancel_focus.focus(window, cx);
        }
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                native_button("settings-reset-cancel", self.cancel.clone(), cx)
                    .track_focus(&self.cancel_focus)
                    .on_click(cx.listener(|this, _, window, cx| {
                        let _ = this.owner.update(cx, |owner, cx| {
                            owner.state.confirm_reset_all(false);
                            cx.notify();
                        });
                        window.close_dialog(cx);
                    })),
            )
            .child(
                native_button("settings-confirm", self.confirm.clone(), cx).on_click(cx.listener(
                    |this, _, window, cx| {
                        let _ = this.owner.update(cx, |owner, cx| {
                            owner.state.confirm_reset_all(true);
                            owner.sync_inputs = true;
                            cx.notify();
                        });
                        window.close_dialog(cx);
                    },
                )),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};

    fn init_settings_pointer_test(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            cx.set_reduce_motion(true);
        });
    }

    #[gpui_kit::test]
    async fn settings_pointer_tests_use_stable_dialog_geometry(cx: &mut TestAppContext) {
        init_settings_pointer_test(cx);

        assert!(
            cx.update(|cx| cx.reduce_motion()),
            "pointer tests require settled dialog geometry"
        );
    }

    fn install_native_pair(
        preset: &str,
        active_dark: bool,
        cx: &mut App,
    ) -> (
        native_theme::theme::ResolvedTheme,
        native_theme::theme::ResolvedTheme,
    ) {
        let preferences = native_theme::AccessibilityPreferences::default();
        let (light_theme, light) =
            native_theme_gpui::from_preset(preset, false, &preferences).unwrap();
        let (dark_theme, dark) =
            native_theme_gpui::from_preset(preset, true, &preferences).unwrap();
        if active_dark {
            native_theme_gpui::apply(light_theme, &light, &preferences, cx);
            native_theme_gpui::apply(dark_theme, &dark, &preferences, cx);
        } else {
            native_theme_gpui::apply(dark_theme, &dark, &preferences, cx);
            native_theme_gpui::apply(light_theme, &light, &preferences, cx);
        }
        (light, dark)
    }

    #[gpui_kit::test]
    async fn toolbar_keyboard_move_previews_and_cancel_restores_runtime(cx: &mut TestAppContext) {
        init_settings_pointer_test(cx);
        let root = tempfile::tempdir().unwrap();
        let mut view = None;
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    SettingsStore::from_config_home(root.path()),
                    SettingsBackends::default(),
                    Catalog::load(Locale::EnUs).unwrap(),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, _| {
                this.state.navigate_to("layout.toolbar").unwrap()
            });
            window.render_frame(cx);
            window.render_frame(cx);
            let focus = view.read(cx).toolbar_move_focus.clone();
            focus.focus(window, cx);
            window.render_frame(cx);
            window.press("enter", cx);
            window.dispatch_event(
                gpui_kit::PlatformInput::KeyUp(gpui_kit::KeyUpEvent {
                    keystroke: gpui_kit::Keystroke::parse("enter").unwrap(),
                }),
                cx,
            );
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).state.toolbar().ids()[1].as_str(),
                "navigation.location"
            );
            assert_eq!(
                super::super::toolbar::toolbar_from_document(
                    &cx.global::<super::super::RuntimeSettings>().0
                ),
                view.read(cx).state.toolbar()
            );
            window.click("settings-cancel", cx);
        })
        .unwrap();
        cx.update(|cx| {
            assert_eq!(
                super::super::toolbar::toolbar_from_document(
                    &cx.global::<super::super::RuntimeSettings>().0
                ),
                musheen_core::ToolbarLayout::default()
            )
        });
    }

    #[gpui_kit::test]
    async fn localized_choices_are_controls_not_serialized_input_values(cx: &mut TestAppContext) {
        init_settings_pointer_test(cx);
        let root = tempfile::tempdir().unwrap();
        for locale in [Locale::Ar, Locale::EnXa] {
            let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
                let entity = cx.new(|cx| {
                    SettingsWindow::new(
                        SettingsStore::from_config_home(root.path()),
                        SettingsBackends::all(),
                        Catalog::load(locale).unwrap(),
                        window,
                        cx,
                    )
                });
                Root::new(entity, window, cx)
            });
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                assert_eq!(window.find("general.startup").role(), Some(Role::Button));
                assert_ne!(window.find("general.startup").label(), Some("last-session"));
                window.click("settings-page-appearance", cx);
                window.render_frame(cx);
                let label = window.find("appearance.mode").label().unwrap().to_owned();
                assert_eq!(
                    label,
                    if locale == Locale::Ar {
                        "اتّباع النظام"
                    } else {
                        "⟦Follow system ···⟧"
                    }
                );
                window.remove_window();
            })
            .unwrap();
        }
    }

    #[gpui_kit::test]
    async fn localized_numeric_edits_round_trip_and_empty_credentials_remain_empty(
        cx: &mut TestAppContext,
    ) {
        init_settings_pointer_test(cx);
        let root = tempfile::tempdir().unwrap();
        let mut view = None;
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    SettingsStore::from_config_home(root.path()),
                    SettingsBackends::all(),
                    Catalog::load(Locale::Ar).unwrap(),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            assert!(!view.read(cx).inputs.contains_key("remote.credential"));
            view.update(cx, |this, cx| {
                this.state.navigate_to("directory_page_items").unwrap();
                this.focus_pending = true;
                cx.notify();
            });
            window.render_frame(cx);
            window.press("ctrl-a", cx);
            window.input("١٢٨", cx);
        })
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                view.read(cx)
                    .state
                    .draft()
                    .value("directory_page_items")
                    .as_deref(),
                Some("128")
            );
            assert!(view.read(cx).state.errors().is_empty());
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn appearance_override_keeps_captured_native_accessibility(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            let preferences = native_theme::AccessibilityPreferences {
                reduce_motion: true,
                reduce_transparency: true,
                ..Default::default()
            };
            let (theme, resolved) =
                native_theme_gpui::from_preset("kde-breeze", false, &preferences).unwrap();
            native_theme_gpui::apply(theme, &resolved, &preferences, cx);
            let mut document = SettingsDocument::default();
            for mode in ["dark", "light", "system"] {
                document.set_value("appearance.mode", mode).unwrap();
                apply_appearance(&document, cx);
                let actual = cx
                    .global::<native_theme_gpui::NativeTheme>()
                    .accessibility();
                assert!(
                    actual.reduce_motion,
                    "desktop motion preference lost in {mode}"
                );
                assert!(
                    actual.reduce_transparency,
                    "desktop transparency preference lost in {mode}"
                );
            }
        });
    }

    #[gpui_kit::test]
    async fn native_refresh_reapplies_user_overrides_and_replaces_both_base_variants(
        cx: &mut TestAppContext,
    ) {
        use gpui_kit::component::{Theme, ThemeMode};

        cx.update(|cx| {
            gpui_kit::init(cx);
            let preferences = native_theme::AccessibilityPreferences::default();
            let (old_dark_theme, old_dark) =
                native_theme_gpui::from_preset("kde-breeze", true, &preferences).unwrap();
            let (old_light_theme, old_light) =
                native_theme_gpui::from_preset("kde-breeze", false, &preferences).unwrap();
            native_theme_gpui::apply(old_dark_theme, &old_dark, &preferences, cx);
            native_theme_gpui::apply(old_light_theme, &old_light, &preferences, cx);

            let mut saved = SettingsDocument::default();
            saved.set_value("appearance.mode", "dark").unwrap();
            saved
                .set_value(
                    "appearance.theme",
                    &crate::theme::document::ThemeDocument::starter().export(),
                )
                .unwrap();
            cx.set_global(crate::settings::RuntimeSettings(saved.clone()));
            apply_appearance(&saved, cx);
            assert_eq!(
                cx.theme().colors.background,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff))
            );

            // Simulate the watcher installing a newly resolved native theme.
            let (new_light_theme, new_light) =
                native_theme_gpui::from_preset("adwaita", false, &preferences).unwrap();
            let (new_dark_theme, new_dark) =
                native_theme_gpui::from_preset("adwaita", true, &preferences).unwrap();
            native_theme_gpui::apply(new_light_theme, &new_light, &preferences, cx);
            native_theme_gpui::apply(new_dark_theme, &new_dark, &preferences, cx);
            accept_native_theme_change(cx);

            assert!(
                cx.theme().mode.is_dark(),
                "saved explicit mode is reapplied"
            );
            assert_eq!(
                cx.theme().colors.background,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff)),
                "saved semantic overrides survive the native refresh",
            );

            apply_appearance(&SettingsDocument::default(), cx);
            Theme::change(ThemeMode::Light, None, cx);
            assert_eq!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .resolved(cx)
                    .unwrap(),
                &new_light,
            );
            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .resolved(cx)
                    .unwrap(),
                &new_dark,
            );
        });
    }

    #[gpui_kit::test]
    async fn cancel_after_live_native_refresh_rebases_unsaved_preview(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};

        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(root.path());
        let handle = cx.update(|cx| {
            gpui_kit::init(cx);
            install_native_pair("kde-breeze", false, cx);
            open_settings_at(store, None, cx);
            cx.global::<SettingsWindowOwner>().handle.unwrap()
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("settings-page-appearance", cx);
            window.render_frame(cx);
            window.click("theme-starter", cx);
            window.click("theme-preview", cx);
            window.render_frame(cx);
            assert_eq!(
                cx.theme().colors.background,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff))
            );
        })
        .unwrap();

        let (new_light, new_dark) = cx.update(|cx| {
            let variants = install_native_pair("adwaita", true, cx);
            accept_native_theme_change(cx);
            variants
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(
                cx.theme().colors.background,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff)),
                "native refresh must retain the unsaved custom preview",
            );
            window.click("settings-cancel", cx);
        })
        .unwrap();

        cx.update(|cx| {
            assert!(cx.theme().mode.is_dark());
            assert_eq!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .resolved(cx)
                    .unwrap(),
                &new_dark,
            );
            Theme::change(ThemeMode::Light, None, cx);
            assert_eq!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .resolved(cx)
                    .unwrap(),
                &new_light,
            );
            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .resolved(cx)
                    .unwrap(),
                &new_dark,
            );
        });
    }

    #[gpui_kit::test]
    async fn apply_after_live_native_refresh_persists_cross_mode_preview(cx: &mut TestAppContext) {
        use gpui_kit::component::{Theme, ThemeMode};

        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(root.path());
        let handle = cx.update(|cx| {
            gpui_kit::init(cx);
            install_native_pair("kde-breeze", false, cx);
            open_settings_at(store.clone(), None, cx);
            cx.global::<SettingsWindowOwner>().handle.unwrap()
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("settings-page-appearance", cx);
            window.render_frame(cx);
            window.click("appearance.mode:light", cx);
            window.click("theme-starter", cx);
            window.click("theme-preview", cx);
            window.render_frame(cx);
            assert!(!cx.theme().mode.is_dark());
        })
        .unwrap();

        let (new_light, new_dark) = cx.update(|cx| {
            let variants = install_native_pair("adwaita", true, cx);
            accept_native_theme_change(cx);
            variants
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(!cx.theme().mode.is_dark());
            assert_eq!(
                cx.theme().colors.background,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff)),
                "native refresh must retain the unsaved cross-mode preview",
            );
            window.click("settings-apply", cx);
        })
        .unwrap();
        let expected_theme = crate::theme::document::ThemeDocument::starter().export();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, _| {
            store.load().is_ok_and(|document| {
                document.value("appearance.mode").as_deref() == Some("light")
                    && document.value("appearance.theme").as_deref()
                        == Some(expected_theme.as_str())
            })
        })
        .await;
        cx.update_window(handle.into(), |_, window, _| window.remove_window())
            .unwrap();

        cx.update(|cx| {
            assert!(!cx.theme().mode.is_dark());
            assert_eq!(
                cx.theme().colors.background,
                gpui_kit::Hsla::from(gpui_kit::rgb(0xffffff))
            );
            assert_eq!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .resolved(cx)
                    .unwrap()
                    .button
                    .primary_background,
                new_light.button.primary_background,
            );
            Theme::change(ThemeMode::Dark, None, cx);
            assert_eq!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .resolved(cx)
                    .unwrap()
                    .button
                    .primary_background,
                new_dark.button.primary_background,
            );
        });
    }

    #[gpui_kit::test]
    async fn motion_preview_and_cancel_keep_desktop_resolved_colors(cx: &mut TestAppContext) {
        let colors = cx.update(|cx| {
            gpui_kit::init(cx);
            let prefs = native_theme::AccessibilityPreferences::default();
            let (_, mut resolved) =
                native_theme_gpui::from_preset("kde-breeze", false, &prefs).unwrap();
            resolved.button.primary_background = "#805533".parse().unwrap();
            let theme = native_theme_gpui::to_theme(&resolved, "Custom desktop", false, &prefs);
            native_theme_gpui::apply(theme, &resolved, &prefs, cx);
            cx.theme().colors
        });
        let root = tempfile::tempdir().unwrap();
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    SettingsStore::from_config_home(root.path()),
                    SettingsBackends::all(),
                    Catalog::load(Locale::EnUs).unwrap(),
                    window,
                    cx,
                )
            });
            Root::new(entity, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("settings-page-appearance", cx);
            window.render_frame(cx);
            window.click("appearance.reduce_motion:true", cx);
            window.render_frame(cx);
            assert_eq!(cx.theme().colors.background, colors.background);
            assert_eq!(cx.theme().colors.primary, colors.primary);
            assert!(
                cx.global::<native_theme_gpui::NativeTheme>()
                    .accessibility()
                    .reduce_motion
            );
            window.click("settings-cancel", cx);
        })
        .unwrap();
        cx.update(|cx| {
            assert_eq!(cx.theme().colors.background, colors.background);
            assert_eq!(cx.theme().colors.primary, colors.primary);
            assert!(
                !cx.global::<native_theme_gpui::NativeTheme>()
                    .accessibility()
                    .reduce_motion
            );
            let mut saved_override = SettingsDocument::default();
            saved_override.set_value("appearance.mode", "dark").unwrap();
            apply_appearance(&saved_override, cx);
            apply_appearance(&SettingsDocument::default(), cx);
            assert_eq!(
                cx.theme().colors.primary,
                colors.primary,
                "resetting a saved override restores the desktop theme"
            );
        });
    }

    #[gpui_kit::test]
    async fn reset_confirmation_traps_focus_and_escape_restores_trigger(cx: &mut TestAppContext) {
        init_settings_pointer_test(cx);
        let root = tempfile::tempdir().unwrap();
        let mut view = None;
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    SettingsStore::from_config_home(root.path()),
                    SettingsBackends::default(),
                    Catalog::load(Locale::EnUs).unwrap(),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        let prior = cx
            .update_window(handle.into(), |_, window, cx| {
                view.update(cx, |this, _| {
                    this.state.edit("files.hidden", "true").unwrap()
                });
                window.render_frame(cx);
                window.click("settings-reset-all", cx);
                window.render_frame(cx);
                assert!(window.find("settings-reset-cancel").focused().unwrap());
                let prior = view.read(cx).reset_trigger.clone();
                for _ in 0..6 {
                    window.press("tab", cx);
                    window.render_frame(cx);
                    assert!(
                        window.find("settings-reset-cancel").focused() == Some(true)
                            || window.find("settings-confirm").focused() == Some(true)
                    );
                }
                window.click("settings-page-files", cx);
                assert_eq!(view.read(cx).state.page(), SettingsPage::General);
                window.press("escape", cx);
                prior
            })
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(prior.is_focused(window));
            assert!(!view.read(cx).state.reset_confirmation_pending());
            assert_eq!(
                view.read(cx).state.draft().value("files.hidden").as_deref(),
                Some("true")
            );
            window.click("settings-reset-all", cx);
            window.render_frame(cx);
            window.click("settings-confirm", cx);
            window.render_frame(cx);
            assert_eq!(
                view.read(cx).state.draft().value("files.hidden").as_deref(),
                Some("false")
            );
            assert!(prior.is_focused(window));
            window.remove_window();
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn apply_persists_the_ui_draft_before_marking_it_clean(cx: &mut TestAppContext) {
        init_settings_pointer_test(cx);
        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(root.path());
        let mut view = None;
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    store.clone(),
                    SettingsBackends::default(),
                    Catalog::load(Locale::EnUs).unwrap(),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, _| {
                this.state.edit("files.hidden", "true").unwrap();
            });
            window.render_frame(cx);
            window.click("settings-apply", cx);
            assert!(view.read(cx).saving);
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).saving
        })
        .await;
        assert_eq!(store.load().unwrap().value("files.hidden").unwrap(), "true");
        cx.update(|cx| assert!(!view.read(cx).state.is_dirty()));
        cx.update_window(handle.into(), |_, window, _| window.remove_window())
            .unwrap();
    }

    #[gpui_kit::test]
    async fn remote_revision_advances_only_for_saved_connection_changes(cx: &mut TestAppContext) {
        init_settings_pointer_test(cx);
        cx.update(|cx| {
            cx.set_global(crate::settings::RuntimeSettings(SettingsDocument::default()));
            cx.set_global(crate::settings::RemoteConnectionsRevision(0));
        });
        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(root.path());
        let mut view = None;
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    store.clone(),
                    SettingsBackends::all(),
                    Catalog::load(Locale::EnUs).unwrap(),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, _| {
                this.state.edit("files.hidden", "true").unwrap()
            });
            window.render_frame(cx);
            window.click("settings-apply", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).saving
        })
        .await;
        cx.update(|cx| {
            assert_eq!(
                cx.global::<crate::settings::RemoteConnectionsRevision>().0,
                0
            )
        });

        let profile = musheen_desktop::ConnectionProfile::new(
            musheen_desktop::ConnectionId::new("revision-test").unwrap(),
            "Revision test",
            musheen_desktop::RemoteProtocol::Ftp,
            musheen_desktop::RemoteHost::new(
                musheen_desktop::RemoteProtocol::Ftp,
                "files.example.test",
            )
            .unwrap(),
            None,
            "/",
            None::<&str>,
            None,
            musheen_desktop::SecurityPolicy::PlaintextConfirmed,
            None,
        )
        .unwrap();
        let encoded = musheen_desktop::ConnectionProfiles::new(vec![profile])
            .export()
            .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, _| {
                this.state.edit("remote.connections", &encoded).unwrap();
            });
            window.render_frame(cx);
            window.click("settings-apply", cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), std::time::Duration::from_secs(3), |_, cx| {
            !view.read(cx).saving
        })
        .await;
        cx.update(|cx| {
            assert_eq!(
                cx.global::<crate::settings::RemoteConnectionsRevision>().0,
                1
            )
        });
        cx.update_window(handle.into(), |_, window, _| window.remove_window())
            .unwrap();
    }

    #[gpui_kit::test]
    async fn repeated_settings_commands_reuse_the_same_window(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(root.path());
        cx.update(|cx| {
            gpui_kit::init(cx);
            open_settings_at(store.clone(), None, cx);
            let first = cx.global::<SettingsWindowOwner>().handle.unwrap();
            let count = cx.windows().len();
            open_settings_at(store.clone(), None, cx);
            assert_eq!(
                first.window_id(),
                cx.global::<SettingsWindowOwner>()
                    .handle
                    .unwrap()
                    .window_id()
            );
            assert_eq!(cx.windows().len(), count);
            first
                .update(cx, |_, window, _| window.remove_window())
                .unwrap();
        });
    }

    #[gpui_kit::test]
    async fn search_result_focuses_the_owning_control_and_retains_the_query(
        cx: &mut TestAppContext,
    ) {
        init_settings_pointer_test(cx);
        let root = tempfile::tempdir().unwrap();
        let mut view = None;
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    SettingsStore::from_config_home(root.path()),
                    SettingsBackends::all(),
                    Catalog::load(Locale::EnUs).unwrap(),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("settings-search", cx);
            window.input("concurrency", cx);
        })
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("result-operation_data_mutations", cx);
            window.render_frame(cx);
            assert_eq!(view.read(cx).state.page(), SettingsPage::Operations);
            assert_eq!(view.read(cx).state.query(), "concurrency");
            assert!(
                view.read(cx).inputs["operation_data_mutations"]
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window)
            );
            assert!(window.find("operation_data_mutations").visible());
            window.remove_window();
        })
        .unwrap();
    }
}
