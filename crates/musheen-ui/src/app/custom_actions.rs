use super::*;
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
            if enabled {
                self.reload_script_actions(revision, cx);
            }
        }
        let (actions, warning) = if enabled {
            merge_actions(actions, &self.script_actions)
        } else {
            (actions, None)
        };
        if let Some(warning) = warning {
            self.operation_error = Some(
                self.catalog
                    .message(warning)
                    .expect("localized action warning")
                    .into(),
            );
        }
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
                        Ok(actions) => this.script_actions = actions,
                        Err(error) => this.custom_action_error(
                            CustomActionError::ScriptSource(Box::new(error)),
                            cx,
                        ),
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
        if self.running_custom_actions >= 4 {
            return Err(CustomActionError::SelectionMismatch);
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
        let action = match self.reviewed_custom_action(&id, &definition, confirmed) {
            Ok(action) => action,
            Err(error) => {
                self.custom_action_error(error, cx);
                return;
            }
        };
        if let Err(error) = self.revalidate_context_targets(origin_tab, &targets) {
            self.operation_error = Some(error);
            cx.notify();
            return;
        }
        let store = Arc::clone(&self.store);
        let work = CustomActionWork {
            script_action: self.is_script_action(&id, cx),
            action,
            targets,
            location,
            confirmed,
        };
        self.running_custom_actions += 1;
        let work = cx.background_spawn(async move { work.run(store.as_ref()) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let Some(this) = this.upgrade() else {
                return;
            };
            this.update(cx, |this, cx| this.finish_custom_action(result, cx));
        })
        .detach();
    }

    fn finish_custom_action(
        &mut self,
        result: Result<(), CustomActionError>,
        cx: &mut Context<Self>,
    ) {
        self.running_custom_actions = self.running_custom_actions.saturating_sub(1);
        match result {
            Err(error) => self.custom_action_error(error, cx),
            Ok(()) => self.load_focused_tab(cx),
        }
        cx.notify();
    }

    fn custom_action_error(&mut self, error: CustomActionError, cx: &mut Context<Self>) {
        let message = self
            .catalog
            .message(error.message_key())
            .expect("action error is localized");
        self.operation_error = Some(match error {
            CustomActionError::ExitStatus(code) => format!("{message} ({code:?})").into(),
            _ => message.into(),
        });
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
        self.revalidate_script()?;
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
        let prepared = self.action.prepare(&selection, &environment)?;
        CustomActionRunner::run(prepared, self.confirmed)
    }

    fn revalidate_script(&self) -> Result<(), CustomActionError> {
        if !self.script_action {
            return Ok(());
        }
        let scripts = musheen_desktop::ScriptActionLoader::load(&script_directory())
            .map_err(|error| CustomActionError::ScriptSource(Box::new(error)))?;
        if scripts
            .get(&self.action.id)
            .is_none_or(|current| current != &self.action)
        {
            return Err(CustomActionError::InvalidDocument);
        }
        Ok(())
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
            assert!(app.read(cx).operation_error.is_some());
        })
        .unwrap();
        assert!(!output.exists());
    }
}
