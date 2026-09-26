//! The question an extraction asks for each existing item an archive entry
//! would replace (OPS-033).

use gpui_kit::component::button::Button;
use gpui_kit::prelude::*;
use gpui_kit::{
    Context, EventEmitter, FocusHandle, IntoElement, Render, Role, TestSupportExt, Window, div,
};

/// The user's answer for one colliding item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtractConflictChoice {
    Replace,
    ReplaceAll,
    Skip,
    SkipAll,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExtractConflictEvent {
    Chosen(ExtractConflictChoice),
}

/// The dialog's text, localized by the caller.
#[derive(Clone, Debug)]
pub struct ExtractConflictStrings {
    pub title: String,
    pub replace: String,
    pub replace_all: String,
    pub skip: String,
    pub skip_all: String,
}

/// Asks whether one existing item gives way to the archive's entry. A choice
/// emits once and closes the window; closing it any other way cancels the
/// extraction.
pub struct ExtractConflictDialog {
    question: String,
    strings: ExtractConflictStrings,
    focus: FocusHandle,
}

impl EventEmitter<ExtractConflictEvent> for ExtractConflictDialog {}

impl ExtractConflictDialog {
    pub fn new(question: String, strings: ExtractConflictStrings, cx: &mut Context<Self>) -> Self {
        Self {
            question,
            strings,
            focus: cx.focus_handle(),
        }
    }

    fn choice_button(
        id: &'static str,
        label: String,
        choice: ExtractConflictChoice,
        cx: &Context<Self>,
    ) -> Button {
        Button::new(id)
            .label(label)
            .on_click(cx.listener(move |_, _, window, cx| {
                cx.emit(ExtractConflictEvent::Chosen(choice));
                window.defer(cx, |window, _| window.remove_window());
            }))
    }
}

impl Render for ExtractConflictDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("extract-conflict-dialog")
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
                    .id("extract-conflict-item")
                    .test_support()
                    .role(Role::Status)
                    .aria_label(self.question.clone())
                    .whitespace_normal()
                    .child(self.question.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(Self::choice_button(
                        "extract-conflict-replace",
                        self.strings.replace.clone(),
                        ExtractConflictChoice::Replace,
                        cx,
                    ))
                    .child(Self::choice_button(
                        "extract-conflict-replace-all",
                        self.strings.replace_all.clone(),
                        ExtractConflictChoice::ReplaceAll,
                        cx,
                    ))
                    .child(Self::choice_button(
                        "extract-conflict-skip",
                        self.strings.skip.clone(),
                        ExtractConflictChoice::Skip,
                        cx,
                    ))
                    .child(Self::choice_button(
                        "extract-conflict-skip-all",
                        self.strings.skip_all.clone(),
                        ExtractConflictChoice::SkipAll,
                        cx,
                    )),
            )
    }
}
