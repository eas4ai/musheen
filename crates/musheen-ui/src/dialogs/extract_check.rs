//! The window a collision check shows once it has run for a moment, so the
//! user can see it and stop it (OPS-034).

use gpui_kit::component::button::Button;
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, EventEmitter, FocusHandle, IntoElement, Render, Role, TestSupportExt, Window, div,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtractCheckEvent {
    Cancelled,
}

/// The dialog's text, localized by the caller.
#[derive(Clone, Debug)]
pub struct ExtractCheckStrings {
    pub title: String,
    pub cancel: String,
}

/// Names the folder a collision check is looking at. Cancel emits once and
/// closes the window; closing it any other way cancels the check too.
pub struct ExtractCheckDialog {
    message: String,
    strings: ExtractCheckStrings,
    focus: FocusHandle,
}

impl EventEmitter<ExtractCheckEvent> for ExtractCheckDialog {}

impl ExtractCheckDialog {
    pub fn new(message: String, strings: ExtractCheckStrings, cx: &mut Context<Self>) -> Self {
        Self {
            message,
            strings,
            focus: cx.focus_handle(),
        }
    }
}

impl Render for ExtractCheckDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("extract-check-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label(self.strings.title.clone())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(
                div()
                    .id("extract-check-folder")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(self.message.clone())
                    .whitespace_normal()
                    .child(self.message.clone()),
            )
            .child(
                div().flex().child(
                    Button::new("extract-check-cancel")
                        .label(self.strings.cancel.clone())
                        .on_click(cx.listener(|_, _, window, cx| {
                            cx.emit(ExtractCheckEvent::Cancelled);
                            window.defer(cx, |window, _| window.remove_window());
                        })),
                ),
            )
    }
}
