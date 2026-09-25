use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::prelude::*;
use gpui_kit::{
    App, Context, EventEmitter, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, Role, SharedString, Styled, TestSupportExt, TitlebarOptions, Window, WindowBounds,
    WindowOptions, div, px, size,
};
use musheen_core::{DisplayPath, StorePath};
use musheen_ops::{MetadataKind, MetadataReport};

use crate::{Catalog, Locale};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataReviewChoice {
    KeepSource,
    RemoveSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MetadataReviewDialogModel {
    source: StorePath,
    destination: StorePath,
    metadata: MetadataReport,
    choice: MetadataReviewChoice,
    strings: MetadataReviewStrings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MetadataReviewStrings {
    rtl: bool,
    list_separator: &'static str,
    title: SharedString,
    copied: SharedString,
    to: SharedString,
    not_preserved: SharedString,
    question: SharedString,
    non_atomic_source_removal: SharedString,
    keep_both: SharedString,
    remove_anyway: SharedString,
    keep_source: SharedString,
    remove_source: SharedString,
    metadata: [SharedString; 7],
}

impl MetadataReviewStrings {
    fn from_catalog(catalog: &Catalog) -> Self {
        let message = |key| {
            catalog
                .message(key)
                .expect("the metadata review catalog is complete")
                .to_owned()
                .into()
        };
        Self {
            rtl: catalog.locale() == Locale::Ar,
            list_separator: if catalog.locale() == Locale::Ar {
                "، "
            } else {
                ", "
            },
            title: message("metadata-review-title"),
            copied: message("metadata-review-copied"),
            to: message("metadata-review-to"),
            not_preserved: message("metadata-review-not-preserved"),
            question: message("metadata-review-question"),
            non_atomic_source_removal: message("metadata-review-non-atomic-source-removal"),
            keep_both: message("metadata-review-keep-both"),
            remove_anyway: message("metadata-review-remove-anyway"),
            keep_source: message("metadata-review-keep-source"),
            remove_source: message("metadata-review-remove-source"),
            metadata: [
                message("metadata-kind-timestamps"),
                message("metadata-kind-permissions"),
                message("metadata-kind-ownership"),
                message("metadata-kind-extended-attributes"),
                message("metadata-kind-access-control-lists"),
                message("metadata-kind-sparse-layout"),
                message("metadata-kind-hard-link-relationships"),
            ],
        }
    }
}

impl MetadataReviewDialogModel {
    #[must_use]
    pub fn new(
        source: StorePath,
        destination: StorePath,
        metadata: MetadataReport,
        catalog: &Catalog,
    ) -> Self {
        Self {
            source,
            destination,
            metadata,
            choice: MetadataReviewChoice::KeepSource,
            strings: MetadataReviewStrings::from_catalog(catalog),
        }
    }

    #[must_use]
    pub fn title(&self) -> &str {
        self.strings.title.as_ref()
    }

    #[must_use]
    pub const fn choice(&self) -> MetadataReviewChoice {
        self.choice
    }

    pub fn select(&mut self, choice: MetadataReviewChoice) {
        self.choice = choice;
    }

    #[must_use]
    pub fn warning(&self) -> String {
        let skipped = self
            .metadata
            .skipped()
            .iter()
            .map(|kind| match kind {
                MetadataKind::Timestamps => self.strings.metadata[0].as_ref(),
                MetadataKind::Mode => self.strings.metadata[1].as_ref(),
                MetadataKind::Ownership => self.strings.metadata[2].as_ref(),
                MetadataKind::ExtendedAttributes => self.strings.metadata[3].as_ref(),
                MetadataKind::AccessControlList => self.strings.metadata[4].as_ref(),
                MetadataKind::SparseLayout => self.strings.metadata[5].as_ref(),
                MetadataKind::HardLinkRelationship => self.strings.metadata[6].as_ref(),
            })
            .collect::<Vec<_>>()
            .join(self.strings.list_separator);
        let mut warning = format!(
            "{} {} {} {}, {}: {skipped}. {}",
            self.strings.copied.as_ref(),
            DisplayPath::from_store_path(&self.source).as_str(),
            self.strings.to.as_ref(),
            DisplayPath::from_store_path(&self.destination).as_str(),
            self.strings.not_preserved.as_ref(),
            self.strings.question.as_ref(),
        );
        if self.source.provider_key().is_some() {
            warning.push(' ');
            warning.push_str(self.strings.non_atomic_source_removal.as_ref());
        }
        warning
    }
}

pub struct MetadataReviewDialog {
    model: MetadataReviewDialogModel,
    focus: FocusHandle,
    pending_focus: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataReviewDialogEvent {
    Resolved(MetadataReviewChoice),
}

impl EventEmitter<MetadataReviewDialogEvent> for MetadataReviewDialog {}

pub(crate) fn metadata_review_window_options(
    title: impl Into<SharedString>,
    cx: &App,
) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(660.), px(360.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(title.into()),
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(560.), px(320.))),
        ..WindowOptions::default()
    }
}

impl MetadataReviewDialog {
    #[must_use]
    pub fn new(model: MetadataReviewDialogModel, cx: &mut Context<Self>) -> Self {
        Self {
            model,
            focus: cx.focus_handle(),
            pending_focus: true,
        }
    }
}

impl Render for MetadataReviewDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.pending_focus = false;
            self.focus.focus(window, cx);
        }
        let choice = self.model.choice();
        let strings = self.model.strings.clone();
        div()
            .id("metadata-review-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label(strings.title.clone())
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(div().text_lg().child(strings.title))
            .child(self.model.warning())
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(strings.rtl, |row| row.flex_row_reverse())
                    .child(
                        Button::new("metadata-review-keep-source")
                            .label(strings.keep_both)
                            .when(choice == MetadataReviewChoice::KeepSource, Button::primary)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.select(MetadataReviewChoice::KeepSource);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("metadata-review-remove-source")
                            .label(strings.remove_anyway)
                            .when(
                                choice == MetadataReviewChoice::RemoveSource,
                                Button::primary,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.model.select(MetadataReviewChoice::RemoveSource);
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div().flex().gap_2().child(
                    Button::new("metadata-review-confirm")
                        .label(if choice == MetadataReviewChoice::RemoveSource {
                            strings.remove_source
                        } else {
                            strings.keep_source
                        })
                        .primary()
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.emit(MetadataReviewDialogEvent::Resolved(this.model.choice()));
                            window.remove_window();
                        })),
                ),
            )
    }
}
