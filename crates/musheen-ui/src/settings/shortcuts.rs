use super::window::{SettingsWindow, native_button, observed_label};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::*;
use gpui_kit::{Context, IntoElement, Role, TestSupportExt, div};
use musheen_core::{CommandRegistry, ShortcutMap, ShortcutScope, scope_name};

impl SettingsWindow {
    pub(super) fn render_shortcut_editor(&self, cx: &Context<Self>) -> impl IntoElement {
        let registry = CommandRegistry::built_in();
        let bindings = self.state.shortcuts().bindings(&registry);
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.transfer_buttons("shortcuts.bindings", cx))
            .child(
                Input::new(&self.shortcut_input)
                    .id("shortcut-chord")
                    .accessibility_id("shortcut-chord")
                    .aria_label(self.label("customization-chord"))
                    .disabled(self.blocked()),
            );
        let mut scopes = div().flex().flex_wrap().gap_2();
        for scope in [
            ShortcutScope::Global,
            ShortcutScope::Browser,
            ShortcutScope::Dialog,
        ] {
            scopes = scopes.child(
                native_button(
                    format!("shortcut-scope-{}", scope_name(scope)),
                    self.label(&format!("customization-scope-{}", scope_name(scope))),
                    cx,
                )
                .selected(self.shortcut_scope == scope)
                .disabled(self.blocked())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.shortcut_scope = scope;
                    cx.notify();
                })),
            );
        }
        panel = panel.child(scopes);
        if self.shortcut_scope == ShortcutScope::Dialog {
            panel = panel.child(observed_label(
                "shortcut-dialog-policy",
                self.label("customization-dialog-policy"),
            ));
        }
        for command in registry.commands() {
            let id = command.id().clone();
            let clear_id = id.clone();
            let chord = bindings
                .iter()
                .filter(|binding| binding.command == id && binding.scope == self.shortcut_scope)
                .map(|binding| binding.chord.as_str())
                .collect::<Vec<_>>()
                .join(" / ");
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(observed_label(
                        format!("shortcut-label-{}", id.as_str()),
                        format!("{} · {}", self.label(command.label_key()), chord),
                    ))
                    .child(
                        native_button(
                            format!("shortcut-assign-{}", id.as_str()),
                            self.label("customization-assign"),
                            cx,
                        )
                        .disabled(self.blocked() || self.shortcut_scope == ShortcutScope::Dialog)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut bindings = this.state.shortcuts();
                            match bindings.assign(
                                id.as_str(),
                                this.shortcut_scope,
                                &this.shortcut_input.read(cx).value(),
                                &CommandRegistry::built_in(),
                            ) {
                                Ok(()) => {
                                    this.state.set_shortcuts(bindings).expect("valid binding");
                                    this.failure = None;
                                    this.preview_customization(cx);
                                }
                                Err(_) => this.failure = Some("customization-conflict"),
                            }
                            cx.notify();
                        })),
                    )
                    .child(
                        native_button(
                            format!("shortcut-remove-{}", clear_id.as_str()),
                            self.label("customization-remove"),
                            cx,
                        )
                        .disabled(self.blocked())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut bindings = this.state.shortcuts();
                            if bindings
                                .clear(
                                    clear_id.as_str(),
                                    this.shortcut_scope,
                                    &CommandRegistry::built_in(),
                                )
                                .is_ok()
                            {
                                this.state.set_shortcuts(bindings).expect("valid binding");
                                this.preview_customization(cx);
                            } else {
                                this.failure = Some("customization-invalid");
                            }
                            cx.notify();
                        })),
                    ),
            );
        }
        for binding in self
            .state
            .shortcuts()
            .overrides()
            .iter()
            .filter(|binding| registry.get(binding.command.as_str()).is_none())
        {
            panel = panel.child(observed_label(
                format!("shortcut-orphan-{}", binding.command.as_str()),
                format!(
                    "{}: {} · {}",
                    self.label("customization-unavailable"),
                    binding.command.as_str(),
                    binding.chord
                ),
            ));
        }
        panel
            .id("shortcuts.bindings")
            .test_support()
            .role(Role::Group)
            .aria_label(self.label("setting-shortcuts-bindings"))
            .h(gpui_kit::px(240.))
            .overflow_y_scroll()
    }
}

pub fn shortcuts_from_document(document: &musheen_desktop::SettingsDocument) -> ShortcutMap {
    ShortcutMap::import(&document.value("shortcuts.bindings").expect("schema key"))
        .expect("validated shortcuts")
}
