use super::presentation::{choices, display_number, display_value, input_value, stored_number};
use super::{SettingsBackends, SettingsState};
use crate::{AppearanceMode, Catalog, Locale, ThemeProfile};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Disableable, Root, Theme, WindowExt};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, Focusable, Global, IntoElement, Render, Role,
    ScrollHandle, SharedString, Subscription, TestSupportExt, TitlebarOptions, Window,
    WindowBounds, WindowHandle, WindowOptions, div, px, size,
};
use musheen_desktop::{
    SettingKind, SettingSpec, SettingsDocument, SettingsPage, SettingsStore, settings_schema,
};
use std::collections::BTreeMap;

#[derive(Default)]
struct SettingsWindowOwner {
    handle: Option<WindowHandle<Root>>,
}
impl Global for SettingsWindowOwner {}

/// Application-global ownership ensures Settings commands from separate
/// browsers activate the same non-modal window and retain its draft and query.
pub fn open_settings_window(cx: &mut App) {
    open_settings_at(SettingsStore::for_current_user(), cx);
}

fn open_settings_at(store: SettingsStore, cx: &mut App) {
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
        app_id: Some("io.github.musheen.Settings".into()),
        ..Default::default()
    };
    match cx.open_window(options, move |window, cx| {
        let view = cx
            .new(|cx| SettingsWindow::new(store, SettingsBackends::default(), catalog, window, cx));
        cx.new(|cx| Root::new(view, window, cx))
    }) {
        Ok(handle) => cx.global_mut::<SettingsWindowOwner>().handle = Some(handle),
        Err(error) => eprintln!("could not open Settings: {error}"),
    }
}

pub struct SettingsWindow {
    state: SettingsState,
    store: SettingsStore,
    catalog: Catalog,
    search: Entity<InputState>,
    inputs: BTreeMap<&'static str, Entity<InputState>>,
    subscriptions: Vec<Subscription>,
    failure: Option<&'static str>,
    load_failed: bool,
    sync_inputs: bool,
    focus_pending: bool,
    scroll: ScrollHandle,
    saving: bool,
    choices_focus: BTreeMap<&'static str, FocusHandle>,
    reset_trigger: FocusHandle,
    appearance_base: AppearanceSnapshot,
}

impl SettingsWindow {
    pub fn new(
        store: SettingsStore,
        backends: SettingsBackends,
        catalog: Catalog,
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
        let mut this = Self {
            state: SettingsState::new(document, backends),
            store,
            catalog,
            search,
            inputs: BTreeMap::new(),
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
        this.subscriptions.push(cx.on_release(|this, cx| {
            this.state.cancel();
            this.appearance_base.restore(cx);
        }));
        this
    }

    pub fn state(&self) -> &SettingsState {
        &self.state
    }

    fn blocked(&self) -> bool {
        self.saving || self.load_failed || self.state.reset_confirmation_pending()
    }

    fn preview_appearance(&self, cx: &mut App) {
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
                    cx.set_global(super::RuntimeSettings(document.clone()));
                }
                if let Some(this) = this.upgrade() {
                    this.update(cx, |this, cx| {
                        this.saving = false;
                        match result {
                            Ok(document) => {
                                this.state.committed = document;
                                this.appearance_base = AppearanceSnapshot::capture(cx);
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

    fn label(&self, key: &str) -> String {
        self.catalog
            .message(key)
            .expect("settings key is localized")
            .to_owned()
    }

    fn synchronize(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sync_inputs {
            self.sync_inputs = false;
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
            let values = match spec.kind {
                SettingKind::Boolean | SettingKind::Choice(_) => String::new(),
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
                            this.state.cancel();
                            this.appearance_base.restore(cx);
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
    let preferences = native_theme::AccessibilityPreferences::from_system();
    if document.value("appearance.mode").as_deref() == Some("system") {
        let desktop = cx.global::<DesktopAppearance>().0.clone();
        desktop.restore(cx);
        let mut preferences = cx
            .try_global::<native_theme_gpui::NativeTheme>()
            .map(|theme| theme.accessibility().clone())
            .unwrap_or(preferences.clone());
        preferences.reduce_motion |=
            document.value("appearance.reduce_motion").as_deref() == Some("true");
        native_theme_gpui::apply_accessibility(&preferences, cx);
        return;
    }
    let native = ThemeProfile::from_active_native(
        cx.theme().mode.is_dark(),
        preferences.high_contrast,
        preferences.reduce_motion,
    );
    let profile = super::appearance_profile(document, native);
    let mut preferences = preferences;
    preferences.high_contrast = profile.mode() == AppearanceMode::HighContrast;
    preferences.reduce_motion = profile.motion() == crate::MotionPolicy::Reduced;
    preferences.reduce_transparency |= preferences.high_contrast;
    if let Ok((theme, resolved)) = native_theme_gpui::from_preset(
        "adwaita",
        profile.mode() == AppearanceMode::Dark,
        &preferences,
    ) {
        native_theme_gpui::apply(theme, &resolved, &preferences, cx);
    }
}

struct DesktopAppearance(AppearanceSnapshot);
impl Global for DesktopAppearance {}

#[derive(Clone)]
struct AppearanceSnapshot {
    theme: Theme,
    resolved: Option<native_theme::theme::ResolvedTheme>,
    preferences: native_theme::AccessibilityPreferences,
}

impl AppearanceSnapshot {
    fn capture(cx: &App) -> Self {
        let native = cx.try_global::<native_theme_gpui::NativeTheme>();
        Self {
            theme: cx.theme().clone(),
            resolved: native.and_then(|theme| theme.resolved(cx)).cloned(),
            preferences: native
                .map(|theme| theme.accessibility().clone())
                .unwrap_or_default(),
        }
    }
    fn restore(&self, cx: &mut App) {
        if let Some(resolved) = &self.resolved {
            native_theme_gpui::apply(self.theme.clone(), resolved, &self.preferences, cx);
        } else {
            *Theme::global_mut(cx) = self.theme.clone();
            native_theme_gpui::apply_accessibility(&self.preferences, cx);
        }
    }
}

fn observed_label(id: impl Into<SharedString>, label: String) -> impl IntoElement + Styled {
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

fn native_button(id: impl Into<SharedString>, label: String, cx: &App) -> gpui_kit::base::Button {
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

    #[gpui_kit::test]
    async fn localized_choices_are_controls_not_serialized_input_values(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
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
        cx.update(gpui_kit::init);
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
            assert_eq!(
                view.read(cx).inputs["remote.credential"]
                    .read(cx)
                    .value()
                    .as_ref(),
                ""
            );
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
        cx.update(gpui_kit::init);
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
        cx.update(gpui_kit::init);
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
    async fn repeated_settings_commands_reuse_the_same_window(cx: &mut TestAppContext) {
        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(root.path());
        cx.update(|cx| {
            gpui_kit::init(cx);
            open_settings_at(store.clone(), cx);
            let first = cx.global::<SettingsWindowOwner>().handle.unwrap();
            let count = cx.windows().len();
            open_settings_at(store.clone(), cx);
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
        cx.update(gpui_kit::init);
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
