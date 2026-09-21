use gpui_kit::assets::IconName;
use gpui_kit::component::Disableable;
use gpui_kit::component::button::Button;
use gpui_kit::component::radio::Radio;
use gpui_kit::component::{Icon, Sizable};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, Context, EventEmitter, FocusHandle, ImageSource, IntoElement, Render, Role,
    SharedString, TestSupportExt, TitlebarOptions, Window, WindowBounds, WindowOptions, div, img,
    px, size, uniform_list,
};
use musheen_desktop::{
    ApplicationIconProvider, DesktopApplication, DesktopEntryCatalog, DesktopEntryLauncher,
    LaunchError, LaunchTarget, MimeAppsError, MimeAppsResolver, ProcessRunner, TerminalCommand,
};
use std::path::{Path, PathBuf};

use crate::Catalog;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationChoice {
    desktop_id: Box<str>,
    name: Box<str>,
    icon: Option<PathBuf>,
    compatible: bool,
}

impl ApplicationChoice {
    pub fn new(
        desktop_id: impl Into<Box<str>>,
        name: impl Into<Box<str>>,
        compatible: bool,
    ) -> Self {
        Self {
            desktop_id: desktop_id.into(),
            name: name.into(),
            icon: None,
            compatible,
        }
    }

    #[must_use]
    pub fn from_desktop_application(
        application: &DesktopApplication,
        icons: &dyn ApplicationIconProvider,
    ) -> Self {
        Self {
            desktop_id: application.desktop_id().into(),
            name: application.name().into(),
            icon: application.resolve_icon(icons),
            compatible: true,
        }
    }

    pub fn desktop_id(&self) -> &str {
        &self.desktop_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn icon(&self) -> Option<&Path> {
        self.icon.as_deref()
    }

    #[must_use]
    pub fn with_icon_path(mut self, icon: impl Into<PathBuf>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn compatible(&self) -> bool {
        self.compatible
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenWithIntent {
    OpenOnce,
    SetAsDefault,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenWithPlan {
    mime_type: Box<str>,
    desktop_id: Box<str>,
    set_as_default: bool,
}

impl OpenWithPlan {
    pub fn desktop_id(&self) -> &str {
        &self.desktop_id
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn set_as_default(&self) -> bool {
        self.set_as_default
    }

    pub fn execute(
        &self,
        resolver: &MimeAppsResolver,
        catalog: &DesktopEntryCatalog,
        launcher: &DesktopEntryLauncher,
        runner: &(impl ProcessRunner + ?Sized),
        targets: &[LaunchTarget],
        terminal: Option<&TerminalCommand>,
    ) -> Result<(), OpenWithExecutionError> {
        let application = catalog
            .load(&self.desktop_id)?
            .ok_or(OpenWithExecutionError::ApplicationUnavailable)?;
        let prepared = launcher.prepare(&application, targets, terminal)?;
        if self.set_as_default {
            resolver.set_default(&self.mime_type, &self.desktop_id, catalog)?;
        }
        launcher.launch(&prepared, runner)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenWithModel {
    mime_type: Box<str>,
    applications: Vec<ApplicationChoice>,
    selected: Option<Box<str>>,
}

impl OpenWithModel {
    pub fn new(mime_type: impl Into<Box<str>>, applications: Vec<ApplicationChoice>) -> Self {
        Self {
            mime_type: mime_type.into(),
            applications,
            selected: None,
        }
    }

    pub fn from_resolver(
        mime_type: impl Into<Box<str>>,
        resolver: &MimeAppsResolver,
        catalog: &DesktopEntryCatalog,
        icons: &dyn ApplicationIconProvider,
    ) -> Result<Self, MimeAppsError> {
        let mime_type = mime_type.into();
        let applications = resolver
            .visible_applications_for(&mime_type, catalog)?
            .iter()
            .map(|application| ApplicationChoice::from_desktop_application(application, icons))
            .collect();
        Ok(Self::new(mime_type, applications))
    }

    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn compatible_applications(&self) -> Vec<&ApplicationChoice> {
        self.applications
            .iter()
            .filter(|application| application.compatible)
            .collect()
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    pub fn select(&mut self, desktop_id: &str) -> Result<(), OpenWithError> {
        let application = self
            .applications
            .iter()
            .find(|application| application.desktop_id() == desktop_id)
            .ok_or(OpenWithError::UnknownApplication)?;
        if !application.compatible() {
            return Err(OpenWithError::IncompatibleApplication);
        }
        self.selected = Some(desktop_id.into());
        Ok(())
    }

    pub fn plan(&self, intent: OpenWithIntent) -> Result<OpenWithPlan, OpenWithError> {
        let desktop_id = self.selected.clone().ok_or(OpenWithError::NoSelection)?;
        Ok(OpenWithPlan {
            mime_type: self.mime_type.clone(),
            desktop_id,
            set_as_default: intent == OpenWithIntent::SetAsDefault,
        })
    }
}

pub struct OpenWithDialog {
    model: OpenWithModel,
    catalog: Catalog,
    focus: FocusHandle,
    pending_focus: bool,
    decision: Option<(Box<str>, OpenWithIntent)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OpenWithDialogEvent {
    Chosen {
        desktop_id: Box<str>,
        intent: OpenWithIntent,
    },
    Cancelled,
}

impl EventEmitter<OpenWithDialogEvent> for OpenWithDialog {}

pub(crate) fn open_with_window_options(catalog: &Catalog, cx: &App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(560.), px(480.)), cx)),
        titlebar: Some(TitlebarOptions {
            title: Some(SharedString::from(
                catalog
                    .message("open-with-title")
                    .expect("the Open With title is localized"),
            )),
            ..TitlebarOptions::default()
        }),
        window_min_size: Some(size(px(420.), px(360.))),
        ..WindowOptions::default()
    }
}

impl OpenWithDialog {
    pub fn new(model: OpenWithModel, catalog: Catalog, cx: &mut Context<Self>) -> Self {
        Self {
            model,
            catalog,
            focus: cx.focus_handle(),
            pending_focus: true,
            decision: None,
        }
    }

    pub fn decision(&self) -> Option<(&str, OpenWithIntent)> {
        self.decision
            .as_ref()
            .map(|(desktop_id, intent)| (desktop_id.as_ref(), *intent))
    }

    fn choose(&mut self, intent: OpenWithIntent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(desktop_id) = self.model.selected().map(Box::<str>::from) else {
            return;
        };
        self.decision = Some((desktop_id.clone(), intent));
        cx.emit(OpenWithDialogEvent::Chosen { desktop_id, intent });
        window.remove_window();
    }

    fn render_application(
        &mut self,
        application: ApplicationChoice,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let desktop_id = application.desktop_id.clone();
        let view = cx.entity().downgrade();
        let icon_id = format!("open-with-icon-{}", application.desktop_id());
        let icon = application.icon().map_or_else(
            || {
                div()
                    .id(SharedString::from(format!(
                        "open-with-icon-fallback-{}",
                        application.desktop_id()
                    )))
                    .test_support()
                    .child(Icon::new(IconName::File).small())
                    .into_any_element()
            },
            |path| {
                div()
                    .id(SharedString::from(icon_id))
                    .test_support()
                    .child(img(ImageSource::from(path.to_path_buf())).size(px(20.)))
                    .into_any_element()
            },
        );
        div()
            .id(SharedString::from(format!(
                "open-with-row-{}",
                application.desktop_id()
            )))
            .test_support()
            .role(Role::ListItem)
            .aria_label(application.name())
            .h(px(40.))
            .flex()
            .items_center()
            .gap_2()
            .child(icon)
            .child(
                Radio::new(SharedString::from(format!(
                    "open-with-application-{}",
                    application.desktop_id()
                )))
                .label(application.name())
                .checked(is_selected)
                .on_click(move |checked, _, cx| {
                    if *checked {
                        let _ = view.update(cx, |this, cx| {
                            if this.model.select(&desktop_id).is_ok() {
                                cx.notify();
                            }
                        });
                    }
                }),
            )
            .into_any_element()
    }
}

impl Render for OpenWithDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_focus {
            self.pending_focus = false;
            self.focus.focus(window, cx);
        }
        let selected = self.model.selected().map(Box::<str>::from);
        let title = SharedString::from(
            self.catalog
                .message("open-with-title")
                .expect("the Open With title is localized")
                .to_owned(),
        );
        let cancel = SharedString::from(
            self.catalog
                .message("dialog-cancel")
                .expect("the cancel action is localized")
                .to_owned(),
        );
        let open_once = SharedString::from(
            self.catalog
                .message("open-with-open-once")
                .expect("the Open Once action is localized")
                .to_owned(),
        );
        let set_default = SharedString::from(
            self.catalog
                .message("open-with-set-default-and-open")
                .expect("the Set Default action is localized")
                .to_owned(),
        );
        let applications = self
            .model
            .compatible_applications()
            .into_iter()
            .cloned()
            .collect::<Vec<_>>();
        let item_count = applications.len();
        let applications = std::sync::Arc::new(applications);
        let list_items = std::sync::Arc::clone(&applications);
        let applications = uniform_list(
            "open-with-applications",
            item_count,
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                let selected = this.model.selected().map(str::to_owned);
                range
                    .filter_map(|index| list_items.get(index).cloned())
                    .map(|application| {
                        let is_selected = selected.as_deref() == Some(application.desktop_id());
                        this.render_application(application, is_selected, cx)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .h(px(320.))
        .w_full();
        let applications = div()
            .id("open-with-application-list")
            .test_support()
            .role(Role::List)
            .aria_label(title.clone())
            .h(px(320.))
            .w_full()
            .child(applications);
        div()
            .id("open-with-dialog")
            .test_support()
            .role(Role::Dialog)
            .aria_label(title.clone())
            .tab_index(0)
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .child(div().text_lg().child(title))
            .child(applications)
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("open-with-cancel")
                            .label(cancel)
                            .on_click(cx.listener(|_, _, window, cx| {
                                cx.emit(OpenWithDialogEvent::Cancelled);
                                window.remove_window();
                            })),
                    )
                    .child(
                        Button::new("open-with-open-once")
                            .label(open_once)
                            .disabled(selected.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose(OpenWithIntent::OpenOnce, window, cx);
                            })),
                    )
                    .child(
                        Button::new("open-with-set-default")
                            .label(set_default)
                            .disabled(selected.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose(OpenWithIntent::SetAsDefault, window, cx);
                            })),
                    ),
            )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenWithError {
    UnknownApplication,
    IncompatibleApplication,
    NoSelection,
}

#[derive(Debug)]
pub enum OpenWithExecutionError {
    ApplicationUnavailable,
    DesktopEntry(musheen_desktop::DesktopEntryError),
    MimeApps(MimeAppsError),
    Launch(LaunchError),
}

impl From<musheen_desktop::DesktopEntryError> for OpenWithExecutionError {
    fn from(value: musheen_desktop::DesktopEntryError) -> Self {
        Self::DesktopEntry(value)
    }
}

impl From<MimeAppsError> for OpenWithExecutionError {
    fn from(value: MimeAppsError) -> Self {
        Self::MimeApps(value)
    }
}

impl From<LaunchError> for OpenWithExecutionError {
    fn from(value: LaunchError) -> Self {
        Self::Launch(value)
    }
}

impl std::fmt::Display for OpenWithExecutionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ApplicationUnavailable => {
                formatter.write_str("selected application is unavailable")
            }
            Self::DesktopEntry(error) => write!(formatter, "{error}"),
            Self::MimeApps(error) => write!(formatter, "{error}"),
            Self::Launch(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for OpenWithExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ApplicationUnavailable => None,
            Self::DesktopEntry(error) => Some(error),
            Self::MimeApps(error) => Some(error),
            Self::Launch(error) => Some(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;

    #[gpui_kit::test]
    async fn many_applications_are_virtualized_accessible_and_focused_at_200_percent(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let choices = (0..500)
            .map(|index| {
                ApplicationChoice::new(
                    format!("example-{index}.desktop"),
                    format!("Example {index}"),
                    true,
                )
            })
            .collect();
        let handle = cx.open_window(size(px(560.), px(480.)), |window, cx| {
            let view = cx.new(|cx| {
                OpenWithDialog::new(
                    OpenWithModel::new("text/plain", choices),
                    Catalog::load(crate::Locale::EnUs).unwrap(),
                    cx,
                )
            });
            Root::new(view, window, cx)
        });
        cx.update_window(handle.into(), |_, window, cx| {
            window.set_scale_factor(2.0);
            window.render_frame(cx);
            assert_eq!(window.scale_factor(), 2.0);
            assert_eq!(window.find("open-with-dialog").focused(), Some(true));
            assert!(window.find("open-with-application-list").visible());
            window.click("open-with-application-example-0.desktop", cx);
            window.render_frame(cx);
            assert_eq!(
                window
                    .find("open-with-application-example-0.desktop")
                    .checked(),
                Some(true)
            );
            assert!(
                window
                    .try_find("open-with-row-example-499.desktop")
                    .is_none()
            );
        })
        .unwrap();
    }
}
