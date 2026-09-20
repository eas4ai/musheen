use super::{SettingsBackends, SettingsState};
use crate::{AppearanceMode, Catalog, Locale, ThemeProfile};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Disableable, Root, Selectable};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, AppContext, Context, Entity, Focusable, Global, IntoElement, Render, Role, ScrollHandle,
    SharedString, Subscription, TestSupportExt, TitlebarOptions, Window, WindowBounds,
    WindowHandle, WindowOptions, div, px, size,
};
use musheen_desktop::{
    SettingKind, SettingsDocument, SettingsPage, SettingsStore, settings_schema,
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
        };
        for spec in settings_schema()
            .iter()
            .filter(|spec| this.state.available(spec))
        {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(this.state.draft().value(spec.key).expect("schema key"))
            });
            this.subscriptions.push(cx.subscribe_in(
                &input,
                window,
                move |this, input, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let value = input.read(cx).value().to_string();
                        let _validation = this.state.edit(spec.key, &value);
                        if spec.page == SettingsPage::Appearance {
                            apply_appearance(this.state.draft(), cx);
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
            apply_appearance(this.state.draft(), cx);
        }));
        this
    }

    pub fn state(&self) -> &SettingsState {
        &self.state
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.saving
            || self.load_failed
            || !self.state.errors().is_empty()
            || !self.state.is_dirty()
        {
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
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
            apply_appearance(self.state.draft(), cx);
        }
        if self.focus_pending {
            self.focus_pending = false;
            if let Some(input) = self
                .state
                .focused_key()
                .and_then(|key| self.inputs.get(key))
            {
                input.read(cx).focus_handle(cx).focus(window, cx);
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
                    Button::new(SharedString::from(page.label()))
                        .label(self.label(page.label()))
                        .ghost()
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
                    Button::new(SharedString::from(format!("result-{key}")))
                        .label(format!("{} · {}", self.label(hit.page.label()), hit.label))
                        .ghost()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.state.navigate_to(key).is_ok() {
                                this.focus_pending = true;
                            }
                            cx.notify();
                        })),
                );
            }
        }
        sidebar.overflow_y_scrollbar()
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
            if !self.state.available(spec) {
                panel = panel.child(div().text_color(cx.theme().colors.muted_foreground).child(
                    format!(
                        "{} — {}",
                        self.label(spec.label),
                        self.label("settings-unavailable")
                    ),
                ));
                continue;
            }
            let mut row = div()
                .flex()
                .flex_col()
                .gap_1()
                .child(self.label(spec.label));
            if let Some(input) = self.inputs.get(spec.key) {
                row = row.child(
                    Input::new(input)
                        .id(spec.key)
                        .disabled(self.saving || self.load_failed)
                        .accessibility_id(spec.key)
                        .aria_label(self.label(spec.label)),
                );
            }
            let values = match spec.kind {
                SettingKind::Boolean => "true / false".to_owned(),
                SettingKind::Choice(choices) => choices.join(" / "),
                SettingKind::Integer { maximum, units } => format!(
                    "{}: {maximum} {}",
                    self.label("settings-maximum"),
                    self.label(units)
                ),
                SettingKind::CredentialReference => "secret-service:<reference>".to_owned(),
            };
            row = row.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().colors.muted_foreground)
                    .child(format!(
                        "{}: {} · {values}",
                        self.label("settings-default"),
                        spec.default
                    )),
            );
            if spec.restart_required {
                row = row.child(div().text_sm().child(self.label("settings-restart")));
            }
            if self.state.errors().contains(spec.key) {
                row = row.child(div().child(self.label("settings-invalid")));
            }
            panel = panel.child(row);
        }
        panel
            .child(
                Button::new("settings-reset-page")
                    .disabled(self.saving || self.load_failed)
                    .label(self.label("settings-reset-page"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.reset_page(this.state.page());
                        this.sync_inputs = true;
                        cx.notify();
                    })),
            )
            .id("settings-controls")
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
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
        if self.state.reset_confirmation_pending() {
            body = body.child(
                div()
                    .child(self.label("settings-reset-summary"))
                    .child(
                        Button::new("settings-confirm")
                            .disabled(self.saving || self.load_failed)
                            .label(self.label("settings-confirm"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.confirm_reset_all(true);
                                this.sync_inputs = true;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("settings-reset-cancel")
                            .label(self.label("settings-cancel"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.confirm_reset_all(false);
                                cx.notify();
                            })),
                    ),
            );
        }
        body.child(
            div()
                .flex()
                .gap_2()
                .child(
                    Button::new("settings-reset-all")
                        .disabled(self.saving || self.load_failed)
                        .label(self.label("settings-reset-all"))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.request_reset_all();
                            cx.notify();
                        })),
                )
                .child(
                    Button::new("settings-cancel")
                        .disabled(self.saving)
                        .label(self.label("settings-cancel"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.state.cancel();
                            apply_appearance(this.state.draft(), cx);
                            window.remove_window();
                        })),
                )
                .child(
                    Button::new("settings-apply")
                        .label(self.label("settings-apply"))
                        .primary()
                        .disabled(
                            self.load_failed
                                || self.saving
                                || !self.state.is_dirty()
                                || !self.state.errors().is_empty(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save(cx);
                        })),
                ),
        )
    }
}

pub(crate) fn apply_appearance(document: &SettingsDocument, cx: &mut App) {
    let preferences = native_theme::AccessibilityPreferences::from_system();
    if let Ok(system) = native_theme::SystemTheme::from_system() {
        native_theme_gpui::apply_system_theme(&system, cx);
    }
    if document.value("appearance.mode").as_deref() == Some("system")
        && document.value("appearance.reduce_motion").as_deref() != Some("true")
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};

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
