use super::window::{SettingsWindow, native_button, observed_label};
use crate::theme::document::ThemeDocument;
use crate::{AppearanceMode, MotionPolicy, ThemeProfile};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::*;
use gpui_kit::{ClipboardItem, Context, IntoElement, div};
use musheen_desktop::{SettingSpec, SettingsDocument, SettingsPage};

pub(super) fn controls() -> Vec<&'static SettingSpec> {
    super::controls_for(SettingsPage::Appearance)
}

impl SettingsWindow {
    pub(super) fn render_theme_editor(&self, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(observed_label(
                "theme-editor-hint",
                self.label("theme-editor-hint"),
            ))
            .child(
                Input::new(&self.theme_input)
                    .id("appearance.theme")
                    .disabled(self.blocked())
                    .accessibility_id("appearance.theme")
                    .aria_label(self.label("setting-appearance-theme")),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        native_button("theme-starter", self.label("theme-starter"), cx)
                            .disabled(self.blocked())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.theme_input.update(cx, |input, cx| {
                                    input.set_value(ThemeDocument::starter().export(), window, cx)
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        native_button("theme-preview", self.label("theme-preview"), cx)
                            .disabled(self.blocked())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this.theme_input.read(cx).value().to_string();
                                this.theme_error = crate::theme::validate::validate_setting(&text)
                                    .err()
                                    .map(|error| error.message_key());
                                if this.state.edit("appearance.theme", &text).is_ok() {
                                    this.preview_appearance(cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        native_button("theme-export", self.label("theme-export"), cx)
                            .disabled(self.blocked())
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this
                                    .state
                                    .draft()
                                    .value("appearance.theme")
                                    .expect("theme schema key");
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                            })),
                    )
                    .child(
                        native_button("theme-native", self.label("theme-native"), cx)
                            .disabled(self.blocked())
                            .on_click(cx.listener(|this, _, window, cx| {
                                if this.state.edit("appearance.theme", "native").is_ok() {
                                    this.theme_error = None;
                                    this.theme_input.update(cx, |input, cx| {
                                        input.set_value("native", window, cx)
                                    });
                                    this.preview_appearance(cx);
                                }
                                cx.notify();
                            })),
                    ),
            )
            .children(
                self.theme_error
                    .map(|key| observed_label("theme-error", self.label(key))),
            )
    }
}

pub fn appearance_profile(document: &SettingsDocument, native: ThemeProfile) -> ThemeProfile {
    let mode = match document.value("appearance.mode").as_deref() {
        Some("light") => AppearanceMode::Light,
        Some("dark") => AppearanceMode::Dark,
        Some("high-contrast") => AppearanceMode::HighContrast,
        _ => native.mode(),
    };
    ThemeProfile::new(
        mode,
        native.motion() == MotionPolicy::Reduced
            || document.value("appearance.reduce_motion").as_deref() == Some("true"),
    )
}
