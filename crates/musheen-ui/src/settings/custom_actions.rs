use super::window::{SettingsWindow, native_button, observed_label};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, IntoElement, Role, TestSupportExt, Window, div};
use musheen_desktop::{
    ActionArgument, ActionConfirmation, ActionExecution, CustomAction, CustomActionDocument,
    CustomActionError, WorkingDirectory,
};
use std::collections::BTreeMap;

pub(super) const FIELDS: &[(&str, &str)] = &[
    ("id", ""),
    ("label", ""),
    ("command", ""),
    ("arguments", "[\"{files}\"]"),
    ("mime", "*/*"),
    ("directory", ""),
    ("location", ""),
    ("environment", "LANG"),
    ("timeout", "30000"),
];

#[derive(Default)]
pub(crate) struct ScriptActionReload(pub u64);
impl gpui_kit::Global for ScriptActionReload {}

pub(super) fn inputs(
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> BTreeMap<&'static str, Entity<InputState>> {
    FIELDS
        .iter()
        .map(|(field, default)| {
            (
                *field,
                cx.new(|cx| InputState::new(window, cx).default_value(*default)),
            )
        })
        .collect()
}

fn argument(value: String) -> Result<ActionArgument, CustomActionError> {
    Ok(match value.as_str() {
        "{file}" => ActionArgument::File,
        "{files}" => ActionArgument::Files,
        "{directory}" => ActionArgument::Directory,
        "{uris}" => ActionArgument::Uris,
        _ if value.contains('{') || value.contains('}') => {
            return Err(CustomActionError::InvalidDocument);
        }
        _ => ActionArgument::Literal(value),
    })
}
fn argument_text(value: &ActionArgument) -> String {
    match value {
        ActionArgument::File => "{file}".into(),
        ActionArgument::Files => "{files}".into(),
        ActionArgument::Directory => "{directory}".into(),
        ActionArgument::Uris => "{uris}".into(),
        ActionArgument::Literal(value) => value.clone(),
    }
}

impl SettingsWindow {
    fn actions(&self) -> CustomActionDocument {
        CustomActionDocument::import(
            &self
                .state
                .draft()
                .value("advanced.custom_actions")
                .expect("actions schema key"),
        )
        .expect("validated actions")
    }
    fn action_from_fields(&self, cx: &Context<Self>) -> Result<CustomAction, CustomActionError> {
        let field = |name| self.action_inputs[name].read(cx).value().to_string();
        let list = |name| {
            field(name)
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
                .collect()
        };
        let arguments: Vec<String> = serde_json::from_str(&field("arguments"))
            .map_err(|_| CustomActionError::InvalidDocument)?;
        let command = field("command");
        let directory = field("directory");
        let location = field("location");
        let action = CustomAction {
            id: field("id"),
            label: field("label"),
            execution: if self.action_shell {
                ActionExecution::Shell {
                    script: command,
                    opted_in: true,
                }
            } else {
                ActionExecution::Direct {
                    executable: command.into(),
                }
            },
            arguments: arguments
                .into_iter()
                .map(argument)
                .collect::<Result<_, _>>()?,
            working_directory: if directory.is_empty() {
                WorkingDirectory::CurrentLocation
            } else {
                WorkingDirectory::Fixed(directory.into())
            },
            mime_patterns: list("mime"),
            location_prefix: (!location.is_empty()).then(|| location.into()),
            environment: list("environment"),
            supports_provider_uris: self.action_provider_uris,
            confirmation: self.action_confirmation,
            timeout_ms: field("timeout")
                .parse()
                .map_err(|_| CustomActionError::InvalidDocument)?,
        };
        action.validate()?;
        Ok(action)
    }
    fn edit_action(&mut self, action: &CustomAction, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(&action.execution, ActionExecution::Direct { executable } if executable.to_str().is_none())
            || matches!(&action.working_directory, WorkingDirectory::Fixed(path) if path.to_str().is_none())
            || action
                .location_prefix
                .as_ref()
                .is_some_and(|path| path.to_str().is_none())
        {
            self.failure = Some("custom-action-non-utf8-config");
            cx.notify();
            return;
        }
        let command = match &action.execution {
            ActionExecution::Direct { executable } => executable.to_string_lossy().into_owned(),
            ActionExecution::Shell { script, .. } => script.clone(),
        };
        let directory = match &action.working_directory {
            WorkingDirectory::CurrentLocation => String::new(),
            WorkingDirectory::Fixed(path) => path.to_string_lossy().into_owned(),
        };
        let arguments = serde_json::to_string(
            &action
                .arguments
                .iter()
                .map(argument_text)
                .collect::<Vec<_>>(),
        )
        .expect("arguments encode");
        for (key, value) in [
            ("id", action.id.clone()),
            ("label", action.label.clone()),
            ("command", command),
            ("arguments", arguments),
            ("mime", action.mime_patterns.join(",")),
            ("directory", directory),
            (
                "location",
                action
                    .location_prefix
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
            ),
            ("environment", action.environment.join(",")),
            ("timeout", action.timeout_ms.to_string()),
        ] {
            self.action_inputs[key].update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.action_shell = matches!(action.execution, ActionExecution::Shell { .. });
        self.action_provider_uris = action.supports_provider_uris;
        self.action_confirmation = action.confirmation;
        cx.notify();
    }
    pub(super) fn render_custom_actions_editor(&self, cx: &Context<Self>) -> impl IntoElement {
        let mut panel = div()
            .id("advanced.custom_actions")
            .test_support()
            .role(Role::Group)
            .aria_label(self.label("setting-advanced-custom-actions"))
            .flex()
            .flex_col()
            .h(gpui_kit::px(240.))
            .overflow_y_scroll()
            .gap_2();
        for action in self.actions().actions() {
            let edit = action.clone();
            let remove_id = action.id.clone();
            panel = panel.child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .child(observed_label(
                        format!("custom-action-label-{}", action.id),
                        action.label.clone(),
                    ))
                    .child(
                        native_button(
                            format!("custom-action-edit-{}", action.id),
                            self.label("custom-action-edit"),
                            cx,
                        )
                        .disabled(self.blocked())
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.edit_action(&edit, window, cx),
                        )),
                    )
                    .child(
                        native_button(
                            format!("custom-action-remove-{}", action.id),
                            self.label("customization-remove"),
                            cx,
                        )
                        .disabled(self.blocked())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut actions = this.actions();
                            actions.remove(&remove_id);
                            this.state
                                .edit("advanced.custom_actions", &actions.export())
                                .expect("valid actions");
                            cx.notify();
                        })),
                    ),
            );
        }
        for (field, _) in FIELDS {
            let label = self.label(&format!("custom-action-field-{field}"));
            panel = panel
                .child(observed_label(
                    format!("custom-action-field-{field}"),
                    label.clone(),
                ))
                .child(
                    Input::new(&self.action_inputs[field])
                        .id(format!("custom-action-{field}"))
                        .aria_label(label)
                        .disabled(self.blocked()),
                );
        }
        panel = panel.child(observed_label(
            "custom-action-confirm-policy",
            self.label("custom-action-confirm-policy"),
        ));
        for (policy, key) in [
            (ActionConfirmation::Never, "custom-action-never"),
            (ActionConfirmation::Always, "custom-action-always"),
            (ActionConfirmation::Destructive, "custom-action-destructive"),
        ] {
            panel = panel.child(
                native_button(key, self.label(key), cx)
                    .selected(self.action_confirmation == policy)
                    .disabled(
                        self.blocked()
                            || (self.action_shell && policy == ActionConfirmation::Never),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.action_confirmation = policy;
                        cx.notify();
                    })),
            );
        }
        panel
            .child(observed_label(
                "custom-action-hint",
                self.label("custom-action-hint"),
            ))
            .child(
                native_button(
                    "custom-action-shell-opt-in",
                    self.label("custom-action-shell-opt-in"),
                    cx,
                )
                .selected(self.action_shell)
                .disabled(self.blocked())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.action_shell = !this.action_shell;
                    if this.action_shell && this.action_confirmation == ActionConfirmation::Never {
                        this.action_confirmation = ActionConfirmation::Always;
                    }
                    cx.notify();
                })),
            )
            .child(observed_label(
                "custom-action-risk",
                self.label(if self.action_shell {
                    "custom-action-shell-risk"
                } else {
                    "custom-action-direct-risk"
                }),
            ))
            .child(
                native_button(
                    "custom-action-provider-opt-in",
                    self.label("custom-action-provider-opt-in"),
                    cx,
                )
                .selected(self.action_provider_uris)
                .disabled(self.blocked())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.action_provider_uris = !this.action_provider_uris;
                    cx.notify();
                })),
            )
            .child(
                native_button(
                    "custom-action-reload-scripts",
                    self.label("custom-action-reload-scripts"),
                    cx,
                )
                .disabled(self.blocked())
                .on_click(cx.listener(|_, _, _, cx| {
                    let next = cx
                        .try_global::<ScriptActionReload>()
                        .map_or(1, |reload| reload.0.wrapping_add(1));
                    cx.set_global(ScriptActionReload(next));
                    cx.refresh_windows();
                })),
            )
            .child(observed_label(
                "custom-action-script-directory",
                format!(
                    "{}: {}",
                    self.label("custom-action-script-directory"),
                    self.store
                        .path()
                        .parent()
                        .expect("settings parent")
                        .join("actions")
                        .display()
                ),
            ))
            .child(
                native_button(
                    "custom-action-create-directory",
                    self.label("custom-action-create-directory"),
                    cx,
                )
                .disabled(self.blocked())
                .on_click(cx.listener(|this, _, _, cx| {
                    let path = this
                        .store
                        .path()
                        .parent()
                        .expect("settings parent")
                        .join("actions");
                    let work = cx.background_spawn(async move {
                        musheen_desktop::ScriptActionLoader::create_directory(&path)
                    });
                    cx.spawn(async move |this, cx| {
                        let result = work.await;
                        if let Some(this) = this.upgrade() {
                            this.update(cx, |this, cx| {
                                this.failure = result.err().map(|error| error.message_key());
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                })),
            )
            .child(
                native_button("custom-action-save", self.label("custom-action-save"), cx)
                    .disabled(self.blocked())
                    .on_click(cx.listener(|this, _, _, cx| {
                        let result = this.action_from_fields(cx).and_then(|action| {
                            let mut actions = this.actions();
                            actions.upsert(action)?;
                            Ok(actions)
                        });
                        match result {
                            Ok(actions) => {
                                this.state
                                    .edit("advanced.custom_actions", &actions.export())
                                    .expect("valid action editor");
                                this.failure = None;
                            }
                            Err(error) => this.failure = Some(error.message_key()),
                        }
                        cx.notify();
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::Root;
    use gpui_kit::test::TestWindowExt;
    use gpui_kit::{AppContext, TestAppContext, px, size};
    use musheen_desktop::{SettingsPage, SettingsStore};

    #[gpui_kit::test]
    fn custom_action_editor_add_edit_remove_shell_opt_in_and_cancel(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let root = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(root.path());
        let mut view = None;
        let handle = cx.open_window(size(px(840.), px(680.)), |window, cx| {
            let entity = cx.new(|cx| {
                SettingsWindow::new(
                    store.clone(),
                    super::super::SettingsBackends::default(),
                    crate::Catalog::load(crate::Locale::EnUs).unwrap(),
                    window,
                    cx,
                )
            });
            view = Some(entity.clone());
            Root::new(entity, window, cx)
        });
        let view = view.unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |view, cx| {
                view.state.select_page(SettingsPage::Advanced);
                for (key, value) in [
                    ("id", "test"),
                    ("label", "Test action"),
                    ("command", "/bin/true"),
                ] {
                    view.action_inputs[key]
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                }
                let action = view.action_from_fields(cx).unwrap();
                let mut actions = view.actions();
                actions.upsert(action).unwrap();
                view.state
                    .edit("advanced.custom_actions", &actions.export())
                    .unwrap();
            });
            window.render_frame(cx);
            assert!(view.read(cx).state.is_dirty());
            assert_eq!(view.read(cx).actions().actions().len(), 1);
            view.update(cx, |view, cx| {
                view.action_shell = true;
                view.action_confirmation = ActionConfirmation::Never;
                assert!(
                    view.action_from_fields(cx).is_err(),
                    "shell always requires confirmation"
                );
                view.action_confirmation = ActionConfirmation::Always;
                assert!(view.action_from_fields(cx).is_ok());
                let mut action = view.actions().get("test").unwrap().clone();
                use std::os::unix::ffi::OsStringExt;
                action.execution = ActionExecution::Direct {
                    executable: std::ffi::OsString::from_vec(b"/tmp/non\xffutf8".to_vec()).into(),
                };
                let before = view.state.draft().clone();
                view.edit_action(&action, window, cx);
                assert_eq!(view.state.draft(), &before);
                assert_eq!(view.failure, Some("custom-action-non-utf8-config"));
                let mut actions = view.actions();
                actions.remove("test");
                view.state
                    .edit("advanced.custom_actions", &actions.export())
                    .unwrap();
                assert!(view.actions().actions().is_empty());
                view.state.cancel();
                assert!(!view.state.is_dirty());
            });
        })
        .unwrap();
        assert!(
            !store.path().exists(),
            "draft edits never write until Apply"
        );
    }
}
