use super::window::{SettingsWindow, native_button, observed_label};
use gpui_kit::prelude::*;
use gpui_kit::{App, ClipboardItem, Context, IntoElement, Role, SharedString, TestSupportExt, div};
use musheen_core::{CommandRegistry, ToolbarLayout};

impl SettingsWindow {
    pub(super) fn render_toolbar_editor(&self, cx: &Context<Self>) -> impl IntoElement {
        let registry = CommandRegistry::built_in();
        let layout = self.state.toolbar();
        let mut panel = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.transfer_buttons("layout.toolbar", cx));
        for (index, id) in layout.ids().iter().enumerate() {
            let command = registry.get(id.as_str());
            let label = command
                .map(|command| self.label(command.label_key()))
                .unwrap_or_else(|| {
                    format!(
                        "{}: {}",
                        self.label("customization-unavailable"),
                        id.as_str()
                    )
                });
            let row = div()
                .flex()
                .flex_col()
                .flex_shrink_0()
                .gap_1()
                .child(observed_label(format!("toolbar-label-{index}"), label));
            let mut actions = div().flex().flex_wrap().gap_1();
            for (action, destination) in [
                ("up", index.checked_sub(1)),
                (
                    "down",
                    (index + 1 < layout.ids().len()).then_some(index + 1),
                ),
                ("remove", None),
            ] {
                let id = id.as_str().to_owned();
                let disabled = self.blocked()
                    || if action == "remove" {
                        id == "navigation.location"
                    } else {
                        destination.is_none()
                    };
                actions = actions.child(
                    native_button(
                        format!("toolbar-{action}-{id}"),
                        self.label(&format!("customization-{action}")),
                        cx,
                    )
                    .when(action == "down" && id == "navigation.location", |button| {
                        button.track_focus(&self.toolbar_move_focus)
                    })
                    .disabled(disabled)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let mut layout = this.state.toolbar();
                        let result = if action == "remove" {
                            layout.remove(&id)
                        } else {
                            layout.move_to(&id, destination.expect("enabled move"))
                        };
                        if result.is_ok() {
                            this.state.set_toolbar(layout).expect("valid toolbar edit");
                            this.preview_customization(cx);
                        }
                        cx.notify();
                    })),
                );
            }
            panel = panel.child(row.child(actions));
        }
        panel = panel.child(self.label("customization-add"));
        for command in registry
            .commands()
            .iter()
            .filter(|command| !layout.ids().contains(command.id()))
        {
            let id = command.id().clone();
            panel = panel.child(
                native_button(
                    SharedString::from(format!("toolbar-add-{}", id.as_str())),
                    self.label(command.label_key()),
                    cx,
                )
                .disabled(self.blocked())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let mut layout = this.state.toolbar();
                    if layout
                        .add(id.as_str(), &CommandRegistry::built_in())
                        .is_ok()
                    {
                        this.state.set_toolbar(layout).expect("valid toolbar edit");
                        this.preview_customization(cx);
                    }
                    cx.notify();
                })),
            );
        }
        panel
            .id("layout.toolbar")
            .test_support()
            .role(Role::Group)
            .aria_label(self.label("setting-layout-toolbar"))
            .h(gpui_kit::px(240.))
            .overflow_y_scroll()
    }

    pub(super) fn transfer_buttons(
        &self,
        key: &'static str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(
                native_button(
                    format!("customization-import-{key}"),
                    self.label("customization-import"),
                    cx,
                )
                .disabled(self.blocked())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let text = cx
                        .read_from_clipboard()
                        .and_then(|item| item.text())
                        .unwrap_or_default();
                    if this.state.edit(key, &text).is_ok() {
                        this.failure = None;
                        this.preview_customization(cx);
                    } else {
                        this.failure = Some("customization-invalid");
                    }
                    cx.notify();
                })),
            )
            .child(
                native_button(
                    format!("customization-export-{key}"),
                    self.label("customization-export"),
                    cx,
                )
                .disabled(self.blocked())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let value = if key == "layout.toolbar" {
                        this.state.toolbar().export()
                    } else {
                        this.state.shortcuts().export()
                    };
                    cx.write_to_clipboard(ClipboardItem::new_string(value));
                })),
            )
    }

    pub(super) fn preview_customization(&self, cx: &mut App) {
        let mut document = cx
            .try_global::<super::RuntimeSettings>()
            .map(|settings| settings.0.clone())
            .unwrap_or_default();
        for key in ["layout.toolbar", "shortcuts.bindings"] {
            document
                .set_value(key, &self.state.draft().value(key).expect("schema key"))
                .expect("valid draft");
        }
        cx.set_global(super::RuntimeSettings(document));
        cx.refresh_windows();
    }
}

pub fn toolbar_from_document(document: &musheen_desktop::SettingsDocument) -> ToolbarLayout {
    ToolbarLayout::import(&document.value("layout.toolbar").expect("schema key"))
        .expect("validated toolbar")
}
