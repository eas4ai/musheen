use super::*;
use crate::status_center::custom_actions::CustomActionContext;
use musheen_desktop::{
    ActionSelection, CustomAction, CustomActionDocument, CustomActionError, CustomActionRunner,
    SettingsDocument,
};

pub(super) fn from_settings(settings: Option<&SettingsDocument>) -> CustomActionDocument {
    settings
        .and_then(|settings| settings.value("advanced.custom_actions"))
        .and_then(|value| CustomActionDocument::import(&value).ok())
        .unwrap_or_default()
}

fn merge_actions(
    mut users: CustomActionDocument,
    scripts: &CustomActionDocument,
) -> (CustomActionDocument, Option<&'static str>) {
    let mut warning = None;
    let mut scripts = scripts.actions().iter().collect::<Vec<_>>();
    scripts.sort_by(|left, right| left.id.cmp(&right.id));
    for action in scripts {
        if users.get(&action.id).is_some() {
            warning.get_or_insert("custom-action-script-collision");
        } else if users.upsert(action.clone()).is_err() {
            // Upsert is transactional and bounds both count and serialized bytes.
            // Keep every accepted contribution even when later entries exceed a limit.
            warning = Some("custom-action-script-overflow");
        }
    }
    (users, warning)
}

pub(super) struct ActionPreflight {
    selection: Vec<CommandTargetRef>,
    location: StorePath,
    definitions: CustomActionDocument,
    results: Option<Vec<Result<(), &'static str>>>,
}

impl ActionPreflight {
    fn matches(
        &self,
        selection: &[CommandTargetRef],
        location: &StorePath,
        definitions: &CustomActionDocument,
    ) -> bool {
        self.selection == selection
            && &self.location == location
            && &self.definitions == definitions
    }

    fn availability(&self, action: &CustomAction) -> Result<(), &'static str> {
        let index = self
            .definitions
            .actions()
            .iter()
            .position(|current| current == action)
            .ok_or("custom-action-invalid")?;
        self.results
            .as_ref()
            .and_then(|results| results.get(index))
            .copied()
            .unwrap_or(Err("custom-action-checking"))
    }
}

pub(super) struct LiveActionPopup {
    pub popup: gpui_kit::WeakEntity<PopupMenu>,
    pub window: gpui_kit::AnyWindowHandle,
    pub menu: ContextMenu,
    pub path: String,
}

impl LiveActionPopup {
    fn rebuild(&self, menu: ContextMenu, cx: &mut Context<MusheenApp>) {
        let Some(popup) = self.popup.upgrade() else {
            return;
        };
        let path = self.path.clone();
        let owner = cx.entity().downgrade();
        let dispatch = move |entry: MenuEntry, _: &mut Window, cx: &mut App| {
            let _ = owner.update(cx, |this, cx| this.dispatch_context_entry(entry, cx));
        };
        let _ = self.window.update(cx, move |_, window, cx| {
            popup.update(cx, |popup, cx| {
                popup.rebuild(window, cx, |popup, window, cx| {
                    crate::menus::ContextMenuRenderer::populate(
                        popup, menu, path, window, cx, dispatch,
                    )
                })
            });
        });
    }
}

impl MusheenApp {
    pub(super) fn custom_action_warning_row(&self) -> Option<AnyElement> {
        let message = self.custom_action_warning.map(|key| {
            self.catalog
                .message(key)
                .expect("custom action warning is localized")
        })?;
        Some(
            div()
                .id("custom-action-source-warning")
                .role(Role::Status)
                .px_3()
                .py_2()
                .child(message.to_owned())
                .into_any_element(),
        )
    }

    pub(super) fn custom_action_status_summary(&self) -> Option<String> {
        let status = self.operation_hub.status();
        let status = status.lock().ok()?;
        let entry = status
            .custom_actions()
            .iter()
            .rev()
            .find(|entry| entry.visible())?;
        Some(format!(
            "{} — {}",
            crate::status_center::custom_actions::safe_text(&entry.context.label),
            self.catalog
                .message(match entry.status {
                    OperationStatus::Running => "custom-action-job-running",
                    OperationStatus::Completed => "custom-action-job-completed",
                    _ => "custom-action-job-failed",
                })
                .expect("action status")
        ))
    }

    pub(super) fn custom_action_status_rows(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let status = self.operation_hub.status();
        let Ok(status) = status.lock() else {
            return Vec::new();
        };
        status
            .custom_actions()
            .iter()
            .filter(|entry| entry.visible())
            .map(|entry| {
                let id = entry.id;
                let message = entry.message(&self.catalog);
                let location = entry.context.location.clone();
                div()
                    .id(SharedString::from(format!("custom-action-status-{id}")))
                    .test_support()
                    .role(Role::Status)
                    .aria_label(message.clone())
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(cx.theme().colors.border)
                    .child(div().flex_1().text_sm().child(message))
                    .child(
                        Button::new(SharedString::from(format!("custom-action-view-{id}")))
                            .label(
                                self.catalog
                                    .message("custom-action-job-view")
                                    .expect("action view"),
                            )
                            .small()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.view_operation_location(location.clone(), cx)
                            })),
                    )
                    .when(entry.status != OperationStatus::Running, |row| {
                        row.child(
                            Button::new(SharedString::from(format!("custom-action-dismiss-{id}")))
                                .label(
                                    self.catalog
                                        .message("custom-action-job-dismiss")
                                        .expect("action dismiss"),
                                )
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.operation_hub.dismiss_custom_action(id);
                                    cx.notify();
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect()
    }

    pub(super) fn track_live_action_popup(
        owner: gpui_kit::WeakEntity<Self>,
        menu: &ContextMenu,
        path: &str,
        window: &Window,
        cx: &mut Context<PopupMenu>,
    ) {
        if !menu.entries().iter().any(|entry| {
            entry
                .invocation
                .as_ref()
                .is_some_and(|data| data.custom_action.is_some())
        }) {
            return;
        }
        let popup = LiveActionPopup {
            popup: cx.entity().downgrade(),
            window: window.window_handle(),
            menu: menu.clone(),
            path: path.to_owned(),
        };
        cx.defer(move |cx| {
            let _ = owner.update(cx, |this, _| {
                this.live_action_popups
                    .retain(|popup| popup.popup.upgrade().is_some());
                this.live_action_popups.push(popup);
            });
        });
    }

    pub(super) fn custom_action_review_label(&self, id: Option<&str>) -> Option<String> {
        let action = self.custom_actions.get(id?)?;
        let kind = if matches!(
            action.execution,
            musheen_desktop::ActionExecution::Shell { .. }
        ) {
            "custom-action-shell"
        } else {
            "custom-action-direct"
        };
        Some(format!(
            "{} — {}",
            action.label,
            self.catalog.message(kind).expect("action execution label")
        ))
    }

    fn refresh_action_popups(&self, cx: &mut Context<Self>) {
        for live in &self.live_action_popups {
            let mut menu = live.menu.clone();
            menu.refresh_custom_actions(
                self.shell.commands(),
                &self.catalog,
                |action, selection, location| {
                    let checked = self
                        .custom_preflight
                        .as_ref()
                        .filter(|checked| {
                            checked.matches(selection, location, &self.custom_actions)
                        })
                        .ok_or("custom-action-checking")?;
                    checked.availability(action)
                },
            );
            live.rebuild(menu, cx);
        }
    }
    pub(super) fn custom_action_contributions(
        &self,
        selection: &[CommandTargetRef],
        location: &StorePath,
    ) -> Vec<crate::MenuContribution> {
        let preflight = self
            .custom_preflight
            .as_ref()
            .filter(|checked| checked.matches(selection, location, &self.custom_actions));
        self.custom_actions
            .actions()
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, action)| {
                let contribution = crate::MenuContribution::custom_action(action);
                match preflight
                    .and_then(|checked| checked.results.as_ref())
                    .and_then(|results| results.get(index))
                {
                    Some(result) => contribution.with_availability(*result),
                    None => contribution,
                }
            })
            .collect()
    }

    pub(super) fn preflight_custom_actions(
        &mut self,
        selection: &[CommandTargetRef],
        location: StorePath,
        cx: &mut Context<Self>,
    ) {
        if self.custom_actions.actions().is_empty() || selection.is_empty() {
            return;
        }
        if self.custom_preflight.as_ref().is_some_and(|checked| {
            checked.results.is_none()
                || (checked.selection == selection
                    && checked.location == location
                    && checked.definitions == self.custom_actions)
        }) {
            return;
        }
        let targets = selection.to_vec();
        let paths = targets.iter().map(|target| target.path().clone()).collect();
        let definitions = self.custom_actions.clone();
        self.custom_preflight = Some(ActionPreflight {
            selection: targets.clone(),
            location: location.clone(),
            definitions: definitions.clone(),
            results: None,
        });
        let work = cx.background_spawn(async move {
            let inspected = ActionSelection::inspect(paths, location.clone());
            let results = definitions
                .actions()
                .iter()
                .map(|action| match &inspected {
                    Ok(selection) => action
                        .prepare(selection, &Default::default())
                        .map(|_| ())
                        .map_err(|error| error.message_key()),
                    Err(error) => Err(error.message_key()),
                })
                .collect();
            ActionPreflight {
                selection: targets,
                location,
                definitions,
                results: Some(results),
            }
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    this.custom_preflight = Some(result);
                    this.refresh_action_popups(cx);
                    cx.notify();
                });
            }
        })
        .detach();
    }
    pub(super) fn refresh_custom_actions(&mut self, cx: &mut Context<Self>) {
        let settings = cx
            .try_global::<crate::settings::RuntimeSettings>()
            .map(|runtime| &runtime.0);
        let enabled = settings
            .and_then(|settings| settings.value("advanced.script_directory"))
            .as_deref()
            == Some("true");
        let actions = from_settings(settings);
        let revision = cx
            .try_global::<crate::settings::custom_actions::ScriptActionReload>()
            .map_or(0, |reload| reload.0);
        if enabled != self.scripts_enabled || revision != self.script_reload_revision {
            self.scripts_enabled = enabled;
            self.script_reload_revision = revision;
            self.script_actions = CustomActionDocument::default();
            self.script_load_warning = None;
            if enabled {
                self.reload_script_actions(revision, cx);
            }
        }
        let (actions, warning) = if enabled {
            merge_actions(actions, &self.script_actions)
        } else {
            (actions, None)
        };
        self.custom_action_warning = if enabled {
            warning.or(self.script_load_warning)
        } else {
            None
        };
        self.custom_actions = actions;
    }

    fn reload_script_actions(&self, revision: u64, cx: &mut Context<Self>) {
        let work = cx.background_spawn(async {
            musheen_desktop::ScriptActionLoader::load_optional(&script_directory())
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.scripts_enabled && this.script_reload_revision == revision {
                    match result {
                        Ok(actions) => {
                            this.script_actions = actions;
                            this.script_load_warning = None;
                        }
                        Err(error) => this.script_load_warning = Some(error.message_key()),
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn reviewed_custom_action(
        &self,
        id: &str,
        definition: &str,
        confirmed: bool,
    ) -> Result<CustomAction, CustomActionError> {
        let action = self
            .custom_actions
            .get(id)
            .filter(|action| action.fingerprint() == definition)
            .ok_or(CustomActionError::InvalidDocument)?;
        if action.requires_confirmation() && !confirmed {
            return Err(CustomActionError::ConfirmationRequired);
        }
        Ok(action.clone())
    }

    fn is_script_action(&self, id: &str, cx: &App) -> bool {
        self.scripts_enabled
            && self.script_actions.get(id).is_some()
            && from_settings(
                cx.try_global::<crate::settings::RuntimeSettings>()
                    .map(|runtime| &runtime.0),
            )
            .get(id)
            .is_none()
    }

    pub(super) fn run_custom_action(
        &mut self,
        parameters: CommandParameters,
        origin_tab: Option<TabId>,
        confirmed: bool,
        cx: &mut Context<Self>,
    ) {
        let CommandParameters::CustomAction {
            targets,
            action_id: Some(id),
            definition: Some(definition),
            location,
            ..
        } = parameters
        else {
            return;
        };
        self.refresh_custom_actions(cx);
        let context = self.capture_custom_action_context(&id, &targets, &location);
        let action = match self.reviewed_custom_action(&id, &definition, confirmed) {
            Ok(action) => action,
            Err(error) => {
                self.reject_custom_action(context, error, cx);
                return;
            }
        };
        if self
            .revalidate_context_targets(origin_tab, &targets)
            .is_err()
        {
            self.reject_custom_action(context, CustomActionError::SelectionMismatch, cx);
            return;
        }
        let id = match self.operation_hub.submit_custom_action(context.clone()) {
            Ok(id) => id,
            Err(error) => {
                self.reject_custom_action(context, error, cx);
                return;
            }
        };
        let work = CustomActionWork {
            script_action: self.is_script_action(&action.id, cx),
            action,
            targets,
            location,
            confirmed,
        };
        self.spawn_custom_action(id, work, (origin_tab, context.location), cx);
    }

    fn capture_custom_action_context(
        &self,
        id: &str,
        targets: &[CommandTargetRef],
        location: &StorePath,
    ) -> CustomActionContext {
        CustomActionContext {
            action_id: id.into(),
            label: self
                .custom_actions
                .get(id)
                .map_or_else(|| id.into(), |action| action.label.clone()),
            targets: targets
                .iter()
                .take(16)
                .map(|target| target.path().clone())
                .collect(),
            target_count: targets.len(),
            location: location.clone(),
        }
    }

    fn spawn_custom_action(
        &mut self,
        id: u64,
        work: CustomActionWork,
        origin: (Option<TabId>, StorePath),
        cx: &mut Context<Self>,
    ) {
        let store = Arc::clone(&self.store);
        let hub = self.operation_hub.clone();
        self.running_custom_actions += 1;
        let work = cx.background_spawn(async move {
            let result = work.run(store.as_ref());
            hub.finish_custom_action(id, &result);
            result.is_ok()
        });
        cx.spawn(async move |this, cx| {
            let succeeded = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |this, cx| {
                this.finish_custom_action(succeeded, origin.0, origin.1, cx)
            });
        })
        .detach();
    }

    fn finish_custom_action(
        &mut self,
        succeeded: bool,
        origin_tab: Option<TabId>,
        location: StorePath,
        cx: &mut Context<Self>,
    ) {
        self.running_custom_actions = self.running_custom_actions.saturating_sub(1);
        if succeeded {
            if let Some(tab) = origin_tab.and_then(|id| self.navigation.tab(id))
                && tab.location() == &location
            {
                self.start_load_for_tab(tab.id(), location, cx);
            }
        } else {
            self.status_center_open = true;
        }
        cx.notify();
    }

    fn reject_custom_action(
        &mut self,
        context: CustomActionContext,
        error: CustomActionError,
        cx: &mut Context<Self>,
    ) {
        self.operation_hub.reject_custom_action(context, &error);
        self.status_center_open = true;
        cx.notify();
    }
}

struct CustomActionWork {
    action: CustomAction,
    targets: Vec<CommandTargetRef>,
    location: StorePath,
    script_action: bool,
    confirmed: bool,
}

impl CustomActionWork {
    fn run(self, store: &dyn Store) -> Result<(), CustomActionError> {
        revalidate_action_targets(store, &self.targets)?;
        let paths = self
            .targets
            .into_iter()
            .map(|target| target.path().clone())
            .collect();
        let selection = ActionSelection::inspect(paths, self.location)?;
        let environment = self
            .action
            .environment
            .iter()
            .filter_map(|key| std::env::var_os(key).map(|value| (key.clone(), value)))
            .collect();
        let prepared = if self.script_action {
            musheen_desktop::ScriptActionLoader::prepare(
                &script_directory(),
                &self.action,
                &selection,
                &environment,
            )?
        } else {
            self.action.prepare(&selection, &environment)?
        };
        CustomActionRunner::run(prepared, self.confirmed)
    }
}

fn revalidate_action_targets(
    store: &dyn Store,
    targets: &[CommandTargetRef],
) -> Result<(), CustomActionError> {
    // Check identity immediately before preparing argv. A file manager cannot
    // promise path stability after handing a path to another process.
    for target in targets {
        let current = store
            .resolve_item(target.path())
            .map_err(|_| CustomActionError::SelectionMismatch)?;
        if current.is_none_or(|item| item.id() != target.id() || item.path() != target.path()) {
            return Err(CustomActionError::SelectionMismatch);
        }
    }
    Ok(())
}

fn script_directory() -> PathBuf {
    musheen_desktop::SettingsStore::for_current_user()
        .path()
        .parent()
        .expect("settings has a parent")
        .join("actions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::test::{TestAppContextExt, TestWindowExt};
    use musheen_desktop::{
        ActionArgument, ActionConfirmation, ActionExecution, CustomAction, WorkingDirectory,
    };
    use standard_library::fs as filesystem;
    use std as standard_library;

    #[gpui_kit::test]
    fn custom_action_script_warnings_clear_without_overwriting_operation_errors(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let root = tempfile::tempdir().unwrap();
        let document = CustomActionDocument::new(vec![action(root.path())]).unwrap();
        let mut settings = SettingsDocument::default();
        settings
            .set_value("advanced.custom_actions", &document.export())
            .unwrap();
        settings
            .set_value("advanced.script_directory", "true")
            .unwrap();
        cx.update(|cx| {
            cx.set_global(crate::settings::RuntimeSettings(settings.clone()));
            let app = cx.new(|cx| MusheenApp::new_with_session_store(root.path().into(), None, cx));
            app.update(cx, |app, cx| {
                app.scripts_enabled = true;
                app.script_actions = document;
                app.operation_error = Some("unrelated operation error".into());
                app.refresh_custom_actions(cx);
                assert_eq!(
                    app.operation_error.as_deref(),
                    Some("unrelated operation error")
                );
                assert_eq!(
                    app.custom_action_warning,
                    Some("custom-action-script-collision")
                );
                app.script_actions = CustomActionDocument::default();
                app.refresh_custom_actions(cx);
                assert!(app.custom_action_warning.is_none());
                settings
                    .set_value("advanced.script_directory", "false")
                    .unwrap();
                cx.set_global(crate::settings::RuntimeSettings(settings));
                app.refresh_custom_actions(cx);
                assert!(app.custom_action_warning.is_none());
                assert_eq!(
                    app.operation_error.as_deref(),
                    Some("unrelated operation error")
                );
            });
        });
    }

    #[gpui_kit::test]
    async fn custom_action_completion_refreshes_origin_tab_without_changing_focus(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let source = root.path().join("source.txt");
        let output = root.path().join("created.txt");
        filesystem::write(&source, "source").unwrap();
        let mut action = action(&output);
        action.execution = ActionExecution::Shell {
            script: "/bin/sleep 0.15; /usr/bin/touch -- \"$1\"".into(),
            opted_in: true,
        };
        action.arguments = vec![ActionArgument::Literal(output.to_str().unwrap().into())];
        let mut settings = SettingsDocument::default();
        settings
            .set_value(
                "advanced.custom_actions",
                &CustomActionDocument::new(vec![action.clone()])
                    .unwrap()
                    .export(),
            )
            .unwrap();
        cx.update(|cx| cx.set_global(crate::settings::RuntimeSettings(settings)));
        let mut app = None;
        let window = cx.open_window(size(px(900.), px(700.)), |window, cx| {
            let view =
                cx.new(|cx| MusheenApp::new_with_session_store(root.path().into(), None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(window.into(), Duration::from_secs(3), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        let mut origin = None;
        cx.update_window(window.into(), |_, _, cx| {
            app.update(cx, |app, cx| {
                let tab = app.navigation.focused_tab().id();
                origin = Some(tab);
                let item = app
                    .focused_directory()
                    .items()
                    .iter()
                    .find(|item| item.path().as_unix_path() == Some(source.as_path()))
                    .unwrap();
                let targets =
                    vec![CommandTargetRef::new(item.id().clone(), item.path().clone()).unwrap()];
                app.run_custom_action(
                    CommandParameters::CustomAction {
                        targets,
                        action_id: Some(action.id.clone().into()),
                        definition: Some(action.fingerprint().into()),
                        location: StorePath::from_unix_path(root.path()),
                        supports_provider_uris: false,
                    },
                    Some(tab),
                    true,
                    cx,
                );
                app.dispatch_tab_action(CommandAction::NewTab, cx);
                app.navigate(StorePath::from_unix_path(other.path()), true, cx);
            })
        })
        .unwrap();
        cx.wait_for(window.into(), Duration::from_secs(3), |_, cx| {
            app.read(cx).directories[&origin.unwrap()]
                .items()
                .iter()
                .any(|item| item.path().as_unix_path() == Some(output.as_path()))
        })
        .await;
        cx.update_window(window.into(), |_, _, cx| {
            assert_ne!(app.read(cx).navigation.focused_tab().id(), origin.unwrap());
            assert_eq!(
                app.read(cx)
                    .navigation
                    .focused_tab()
                    .location()
                    .as_unix_path(),
                Some(other.path())
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    async fn custom_action_shared_result_survives_origin_window_close(cx: &mut TestAppContext) {
        cx.update(gpui_kit::init);
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("hostile\n\x1b-name.txt");
        filesystem::write(&source, "target").unwrap();
        let mut action = action(root.path());
        action.label = "Delayed failure".into();
        action.execution = ActionExecution::Shell {
            script: "/bin/sleep 0.15; exit 7".into(),
            opted_in: true,
        };
        action.arguments = vec![ActionArgument::File];
        let mut settings = SettingsDocument::default();
        settings
            .set_value(
                "advanced.custom_actions",
                &CustomActionDocument::new(vec![action.clone()])
                    .unwrap()
                    .export(),
            )
            .unwrap();
        cx.update(|cx| cx.set_global(crate::settings::RuntimeSettings(settings)));
        let hub = OperationHub::new(&ResourceLimits::default());
        let mut views = Vec::new();
        let mut handles = Vec::new();
        for _ in 0..2 {
            handles.push(cx.open_window(size(px(900.), px(700.)), |window, cx| {
                let view = cx.new(|cx| {
                    let mut app = MusheenApp::new_with_session_store(root.path().into(), None, cx);
                    app.operation_hub = hub.clone();
                    app
                });
                views.push(view.clone());
                Root::new(view, window, cx)
            }));
        }
        cx.wait_for(handles[0].into(), Duration::from_secs(3), |_, cx| {
            views[0].read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        cx.update_window(handles[0].into(), |_, window, cx| {
            views[0].update(cx, |app, cx| {
                let item = app
                    .focused_directory()
                    .items()
                    .iter()
                    .find(|item| item.path().as_unix_path() == Some(source.as_path()))
                    .unwrap();
                let targets =
                    vec![CommandTargetRef::new(item.id().clone(), item.path().clone()).unwrap()];
                app.run_custom_action(
                    CommandParameters::CustomAction {
                        targets,
                        action_id: Some(action.id.clone().into()),
                        definition: Some(action.fingerprint().into()),
                        location: StorePath::from_unix_path(root.path()),
                        supports_provider_uris: false,
                    },
                    Some(app.navigation.focused_tab().id()),
                    true,
                    cx,
                );
            });
            window.remove_window();
        })
        .unwrap();
        let origin = views.remove(0);
        let weak = origin.downgrade();
        drop(origin);
        cx.wait_for(handles[1].into(), Duration::from_secs(3), |_, _| {
            hub.status()
                .lock()
                .unwrap()
                .custom_actions()
                .iter()
                .any(|entry| entry.status == OperationStatus::Failed)
        })
        .await;
        cx.update_window(handles[1].into(), |_, window, cx| {
            assert!(
                weak.upgrade().is_none(),
                "origin view is gone before result is inspected"
            );
            views[0].update(cx, |app, cx| {
                app.status_center_open = true;
                cx.notify();
            });
            window.activate_accessibility_for_test();
            window.render_frame(cx);
            let status = hub.status();
            let status = status.lock().unwrap();
            let entry = status.custom_actions().last().unwrap();
            let message = entry.message(&views[0].read(cx).catalog);
            assert!(message.contains("Delayed failure"));
            assert!(message.contains("hostile"));
            assert!(message.contains("7"));
            assert!(!message.chars().any(char::is_control));
            assert!(!message.contains("File operation failed"));
            let tree: serde_json::Value =
                serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap();
            assert!(
                tree["nodes"]
                    .as_object()
                    .unwrap()
                    .values()
                    .any(|node| node["aria"]["label"] == message),
                "the surviving window renders the shared result"
            );
            let roundtrip =
                crate::StatusCenterModel::from_json(&status.to_json().unwrap()).unwrap();
            assert_eq!(roundtrip.custom_actions().len(), 1);
        })
        .unwrap();
    }

    fn action(destination: &Path) -> CustomAction {
        CustomAction {
            id: "copy-test".into(),
            label: "Copy test".into(),
            execution: ActionExecution::Direct {
                executable: "/bin/cp".into(),
            },
            arguments: vec![
                ActionArgument::Literal("--".into()),
                ActionArgument::File,
                ActionArgument::Literal(destination.to_str().unwrap().into()),
            ],
            working_directory: WorkingDirectory::CurrentLocation,
            mime_patterns: vec!["text/*".into()],
            location_prefix: None,
            supports_provider_uris: false,
            confirmation: ActionConfirmation::Always,
            environment: vec![],
            timeout_ms: 1000,
        }
    }

    #[test]
    fn custom_action_merge_keeps_user_actions_and_fills_remaining_script_slots() {
        let users = CustomActionDocument::new(
            (0..63)
                .map(|index| {
                    let mut action = action(Path::new("/tmp/result"));
                    action.id = format!("user-{index}");
                    action
                })
                .collect(),
        )
        .unwrap();
        let scripts = CustomActionDocument::new(
            ["script-z", "script-a", "user-0"]
                .into_iter()
                .map(|id| {
                    let mut action = action(Path::new("/tmp/result"));
                    action.id = id.into();
                    action
                })
                .collect(),
        )
        .unwrap();
        let (merged, warning) = merge_actions(users.clone(), &scripts);
        assert_eq!(merged.actions().len(), 64);
        assert!(
            users
                .actions()
                .iter()
                .all(|action| merged.get(&action.id) == Some(action))
        );
        assert!(merged.get("script-a").is_some());
        assert!(merged.get("script-z").is_none());
        assert_eq!(warning, Some("custom-action-script-overflow"));
    }

    #[gpui_kit::test]
    async fn live_custom_action_preflight_registry_confirmation_and_background_execution(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::init);
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("hostile';\n--name.txt");
        let output = root.path().join("result");
        filesystem::write(&source, "original").unwrap();
        let actions = CustomActionDocument::new(vec![action(&output)]).unwrap();
        let mut settings = SettingsDocument::default();
        settings
            .set_value("advanced.custom_actions", &actions.export())
            .unwrap();
        cx.update(|cx| cx.set_global(crate::settings::RuntimeSettings(settings.clone())));
        let mut app = None;
        let handle = cx.open_window(size(px(960.), px(760.)), |window, cx| {
            let view =
                cx.new(|cx| MusheenApp::new_with_session_store(root.path().into(), None, cx));
            app = Some(view.clone());
            Root::new(view, window, cx)
        });
        let app = app.unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            app.read(cx).focused_directory().state() == &DirectoryState::Ready
        })
        .await;
        let mut targets = Vec::new();
        let mut tab = None;
        let mut first_menu = None;
        cx.update_window(handle.into(), |_, window, cx| {
            app.update(cx, |app, cx| {
                tab = Some(app.navigation.focused_tab().id());
                let item = app
                    .focused_directory()
                    .items()
                    .iter()
                    .find(|item| item.path().as_unix_path() == Some(source.as_path()))
                    .unwrap();
                targets
                    .push(CommandTargetRef::new(item.id().clone(), item.path().clone()).unwrap());
                let clicked = item.id().clone();
                let menu = app.item_context_menu(tab.unwrap(), clicked, cx);
                let child = MusheenApp::menu_entry_by_id(&menu, "actions.custom")
                    .unwrap()
                    .submenu()
                    .unwrap()
                    .entries()
                    .first()
                    .unwrap();
                assert!(
                    !child.state().is_enabled(),
                    "pending preflight cannot execute"
                );
                first_menu = MusheenApp::menu_entry_by_id(&menu, "actions.custom")
                    .unwrap()
                    .submenu()
                    .cloned();
            });
            window.render_frame(cx);
        })
        .unwrap();
        // Mount the first popup projection, while preflight is still pending.
        let popup_window = cx.open_window(size(px(600.), px(400.)), |window, cx| {
            let owner = app.downgrade();
            let popup = PopupMenu::build(window, cx, |popup, window, cx| {
                MusheenApp::populate_context_popup(
                    popup,
                    first_menu.unwrap(),
                    owner,
                    "live-actions".into(),
                    window,
                    cx,
                )
            });
            popup.update(cx, |popup, cx| popup.focus_handle(cx).focus(window, cx));
            Root::new(popup, window, cx)
        });
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            app.read(cx)
                .custom_preflight
                .as_ref()
                .is_some_and(|checked| checked.results.is_some())
        })
        .await;
        cx.update_window(popup_window.into(), |_, window, cx| {
            window.activate_accessibility_for_test();
            window.render_frame(cx);
            let tree: serde_json::Value =
                serde_json::from_str(&window.debug_a11y_tree_json().unwrap()).unwrap();
            let node = tree["nodes"]
                .as_object()
                .unwrap()
                .values()
                .find(|node| node["aria"]["label"] == "Copy test")
                .unwrap();
            assert_ne!(
                node["aria"]["disabled"], true,
                "the first-open popup updates without being reopened"
            );
        })
        .unwrap();
        let mut pending = None;
        cx.update_window(handle.into(), |_, _, cx| {
            app.update(cx, |app, cx| {
                let menu =
                    app.compose_context_menu(tab.unwrap(), MenuTarget::Item, targets.clone());
                let child = MusheenApp::menu_entry_by_id(&menu, "actions.custom")
                    .unwrap()
                    .submenu()
                    .unwrap()
                    .entries()
                    .first()
                    .unwrap();
                assert!(child.state().is_enabled());
                let mut dispatcher = AppMenuDispatcher::default();
                let invocation = app.shell.context_menus().invoke(child, &mut dispatcher);
                assert!(dispatcher.dispatched.is_none());
                let MenuInvocation::NeedsConfirmation(review) = &invocation else {
                    panic!("review required")
                };
                assert_eq!(review.custom_action_id(), Some("copy-test"));
                pending = Some(invocation.clone());
                app.dispatch_typed_context_command(
                    CommandAction::CustomAction,
                    CommandParameters::CustomAction {
                        targets: targets.clone(),
                        supports_provider_uris: false,
                        action_id: Some("copy-test".into()),
                        definition: Some(actions.get("copy-test").unwrap().fingerprint().into()),
                        location: StorePath::from_unix_path(root.path()),
                    },
                    tab,
                    Some(&targets),
                    false,
                    cx,
                );
                assert_eq!(
                    app.running_custom_actions, 0,
                    "typed dispatch cannot bypass confirmation"
                );
                app.confirm_context_review(invocation, cx);
                assert_eq!(app.running_custom_actions, 1);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(3), |_, cx| {
            app.read(cx).running_custom_actions == 0
        })
        .await;
        assert_eq!(filesystem::read(&output).unwrap(), b"original");
        filesystem::remove_file(&output).unwrap();
        // An edited or removed action cannot reuse a review captured earlier.
        settings
            .set_value(
                "advanced.custom_actions",
                &CustomActionDocument::default().export(),
            )
            .unwrap();
        cx.update_window(handle.into(), |_, _, cx| {
            cx.set_global(crate::settings::RuntimeSettings(settings));
            app.update(cx, |app, cx| {
                app.confirm_context_review(pending.unwrap(), cx)
            });
            assert_eq!(app.read(cx).running_custom_actions, 0);
            assert!(
                app.read(cx)
                    .operation_hub
                    .status()
                    .lock()
                    .unwrap()
                    .custom_actions()
                    .iter()
                    .any(|entry| entry.status == OperationStatus::Failed)
            );
        })
        .unwrap();
        assert!(!output.exists());
    }
}
