use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, Role, SharedString, Styled, TestSupportExt, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use musheen_core::DisplayPath;
use musheen_ops::{ApplyScope, ConflictChoice, ConflictItemKind, ConflictRecord};
use std::error::Error;
use std::fmt;

#[derive(Clone, Debug)]
pub struct ConflictDialogModel {
    conflict: ConflictRecord,
    choices: Vec<ConflictChoice>,
    choice: ConflictChoice,
    apply_to_remaining: bool,
}

impl ConflictDialogModel {
    #[must_use]
    pub fn new(conflict: ConflictRecord) -> Self {
        let directory = conflict.source_kind() == ConflictItemKind::Directory
            && conflict.destination_kind() == ConflictItemKind::Directory;
        let choices = if directory {
            vec![
                ConflictChoice::Skip,
                ConflictChoice::KeepBoth,
                ConflictChoice::MergeDirectory,
                ConflictChoice::ReplaceTree,
            ]
        } else {
            vec![
                ConflictChoice::Skip,
                ConflictChoice::KeepBoth,
                ConflictChoice::Replace,
            ]
        };
        Self {
            conflict,
            choices,
            choice: ConflictChoice::Skip,
            apply_to_remaining: false,
        }
    }

    #[must_use]
    pub const fn conflict(&self) -> &ConflictRecord {
        &self.conflict
    }

    #[must_use]
    pub fn choices(&self) -> &[ConflictChoice] {
        &self.choices
    }

    #[must_use]
    pub const fn choice(&self) -> ConflictChoice {
        self.choice
    }

    pub fn select(&mut self, choice: ConflictChoice) -> Result<(), ConflictDialogError> {
        if !self.choices.contains(&choice) {
            return Err(ConflictDialogError::InvalidChoice);
        }
        self.choice = choice;
        Ok(())
    }

    pub fn set_apply_to_remaining(&mut self, apply: bool) {
        self.apply_to_remaining = apply;
    }

    #[must_use]
    pub const fn decision(&self) -> (ConflictChoice, ApplyScope) {
        (
            self.choice,
            if self.apply_to_remaining {
                ApplyScope::CompatibleRemaining
            } else {
                ApplyScope::ThisConflict
            },
        )
    }

    #[must_use]
    pub fn destructive_warning(&self) -> Option<String> {
        (self.choice == ConflictChoice::ReplaceTree).then(|| {
            format!(
                "Replace existing folder tree at {}. The existing destination tree will be \
                 removed before publication; this cannot be undone.",
                DisplayPath::from_store_path(self.conflict.destination()).as_str()
            )
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictDialogError {
    InvalidChoice,
}

impl fmt::Display for ConflictDialogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the selected action does not apply to this conflict")
    }
}

impl Error for ConflictDialogError {}

pub struct ConflictDialog {
    model: ConflictDialogModel,
    focus: FocusHandle,
    pending_focus: bool,
    resolved: Option<(ConflictChoice, ApplyScope)>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictDialogEvent {
    Resolved(ConflictChoice, ApplyScope),
    Cancelled,
}

impl EventEmitter<ConflictDialogEvent> for ConflictDialog {}

pub(crate) fn conflict_window_options(cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(620.), px(420.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from("Resolve file conflict")),
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(520.), px(360.))),
        ..WindowOptions::default()
    }
}

impl ConflictDialog {
    pub fn new(model: ConflictDialogModel, cx: &mut Context<Self>) -> Self {
        Self {
            model,
            focus: cx.focus_handle(),
            pending_focus: true,
            resolved: None,
        }
    }

    #[must_use]
    pub const fn resolved(&self) -> Option<(ConflictChoice, ApplyScope)> {
        self.resolved
    }
}

impl Render for ConflictDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.pending_focus = false;
            self.focus.focus(window, cx);
        }
        let source = DisplayPath::from_store_path(self.model.conflict().source());
        let destination = DisplayPath::from_store_path(self.model.conflict().destination());
        let apply_to_remaining = self.model.apply_to_remaining;
        let destructive_warning = self.model.destructive_warning();
        let confirm_label = if self.model.choice() == ConflictChoice::ReplaceTree {
            "Replace folder tree"
        } else {
            "Continue"
        };
        let mut choices = div().flex().flex_wrap().gap_2();
        for choice in self.model.choices().iter().copied() {
            let selected = self.model.choice() == choice;
            choices = choices.child(
                Button::new(format!("conflict-choice-{choice:?}"))
                    .label(choice_label(choice))
                    .when(selected, Button::primary)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.model.select(choice).is_ok() {
                            cx.notify();
                        }
                    })),
            );
        }
        div()
            .id("conflict-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label("Resolve file conflict")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(div().text_lg().child("Resolve file conflict"))
            .child(format!("Source: {}", source.as_str()))
            .child(format!("Destination: {}", destination.as_str()))
            .child(choices)
            .when_some(destructive_warning, |dialog, warning| {
                dialog.child(div().child(warning))
            })
            .child(
                Button::new("conflict-apply-remaining")
                    .label(if apply_to_remaining {
                        "Apply to compatible remaining conflicts: on"
                    } else {
                        "Apply to compatible remaining conflicts: off"
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.model
                            .set_apply_to_remaining(!this.model.apply_to_remaining);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("conflict-cancel")
                            .label("Cancel operation")
                            .on_click(cx.listener(|_, _, window, cx| {
                                cx.emit(ConflictDialogEvent::Cancelled);
                                window.remove_window();
                            })),
                    )
                    .child(
                        Button::new("conflict-confirm")
                            .label(confirm_label)
                            .primary()
                            .on_click(cx.listener(|this, _, window, cx| {
                                let decision = this.model.decision();
                                this.resolved = Some(decision);
                                cx.emit(ConflictDialogEvent::Resolved(decision.0, decision.1));
                                window.remove_window();
                            })),
                    ),
            )
    }
}

const fn choice_label(choice: ConflictChoice) -> &'static str {
    match choice {
        ConflictChoice::Replace => "Replace",
        ConflictChoice::Skip => "Skip",
        ConflictChoice::KeepBoth => "Keep both",
        ConflictChoice::MergeDirectory => "Merge folders",
        ConflictChoice::ReplaceTree => "Replace existing folder tree",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use musheen_core::StorePath;
    use musheen_ops::OperationKind;

    #[gpui_kit::test]
    async fn conflict_dialog_keeps_focus_safe_and_returns_the_visible_choice(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let conflict = ConflictRecord::new(
            OperationKind::Restore,
            StorePath::from_provider_key(
                musheen_core::ProviderId::new("local.trash").unwrap(),
                b"receipt".to_vec(),
            )
            .unwrap(),
            b"receipt".to_vec(),
            ConflictItemKind::Directory,
            StorePath::from_unix_path("/home/user/folder"),
            b"destination".to_vec(),
            ConflictItemKind::Directory,
        )
        .unwrap();
        let mut dialog = None;
        let handle = cx.open_window(size(px(620.), px(420.)), |window, cx| {
            let view = cx.new(|cx| ConflictDialog::new(ConflictDialogModel::new(conflict), cx));
            dialog = Some(view.clone());
            Root::new(view, window, cx)
        });
        let dialog = dialog.expect("the conflict dialog is constructed");

        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            assert_eq!(window.find("conflict-dialog").focused(), Some(true));
            assert_ne!(window.find("conflict-confirm").focused(), Some(true));
            assert!(window.find("conflict-choice-MergeDirectory").visible());
            assert!(window.find("conflict-choice-ReplaceTree").visible());
            window.click("conflict-choice-MergeDirectory", cx);
            window.click("conflict-apply-remaining", cx);
            window.click("conflict-confirm", cx);
        })
        .expect("the conflict dialog accepts its decision");

        assert_eq!(
            cx.update(|cx| dialog.read(cx).resolved()),
            Some((
                ConflictChoice::MergeDirectory,
                ApplyScope::CompatibleRemaining
            ))
        );
    }
}
