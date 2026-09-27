use musheen_core::{ProviderId, StorePath};
use musheen_desktop::{
    ActionArgument, ActionConfirmation, ActionExecution, ActionSelection, CustomAction,
    CustomActionDocument, CustomActionError, CustomActionRunner, WorkingDirectory,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

fn action() -> CustomAction {
    CustomAction {
        id: "inspect".into(),
        label: "Inspect".into(),
        execution: ActionExecution::Direct {
            executable: "/bin/true".into(),
        },
        arguments: vec![ActionArgument::Files],
        working_directory: WorkingDirectory::CurrentLocation,
        mime_patterns: vec!["*/*".into()],
        location_prefix: None,
        supports_provider_uris: false,
        confirmation: ActionConfirmation::Always,
        environment: vec!["LANG".into()],
        timeout_ms: 1000,
    }
}

fn selection(paths: Vec<StorePath>) -> ActionSelection {
    ActionSelection {
        mime_types: vec!["text/plain".into(); paths.len()],
        paths,
        location: StorePath::from_unix_path("/tmp"),
    }
}

#[test]
fn argv_preserves_hostile_and_non_utf8_names_and_filters_environment() {
    let names = [
        b"/tmp/';$(touch injected)\n--x".to_vec(),
        b"/tmp/-option".to_vec(),
        b"/tmp/non\xffutf8".to_vec(),
    ];
    let selected = selection(
        names
            .iter()
            .cloned()
            .map(StorePath::from_unix_bytes)
            .collect(),
    );
    let env = BTreeMap::from([
        ("LANG".into(), OsString::from("C")),
        ("LD_PRELOAD".into(), OsString::from("evil")),
    ]);
    let prepared = action().prepare(&selected, &env).unwrap();
    assert_eq!(prepared.arguments(), names.map(OsString::from_vec));
    assert_eq!(prepared.working_directory(), std::path::Path::new("/tmp"));
    assert_eq!(prepared.environment().len(), 1);
    assert!(!prepared.environment().contains_key("LD_PRELOAD"));
}

#[test]
fn documents_validate_ids_duplicates_shell_opt_in_and_environment() {
    let valid = CustomActionDocument::new(vec![action()]).unwrap();
    assert_eq!(
        CustomActionDocument::import(&valid.export()).unwrap(),
        valid
    );
    assert!(CustomActionDocument::new(vec![action(), action()]).is_err());
    let mut bad = action();
    bad.id = "../oops".into();
    assert!(bad.validate().is_err());
    let mut bad = action();
    bad.environment.push("LD_PRELOAD".into());
    assert!(bad.validate().is_err());
    let mut bad = action();
    bad.execution = ActionExecution::Shell {
        script: "printf '%s' \"$@\"".into(),
        opted_in: false,
    };
    assert!(bad.validate().is_err());
    bad.execution = ActionExecution::Shell {
        script: "printf '%s' \"$@\"".into(),
        opted_in: true,
    };
    assert!(bad.validate().is_ok());
    bad.confirmation = ActionConfirmation::Never;
    assert!(bad.validate().is_err());
}

#[test]
fn selection_mime_location_cardinality_and_remote_policies_are_enforced() {
    let mut action = action();
    let selected = selection(vec![StorePath::from_unix_path("/tmp/file.txt")]);
    action.mime_patterns = vec!["image/*".into()];
    assert!(action.prepare(&selected, &BTreeMap::new()).is_err());
    action.mime_patterns = vec!["text/*".into()];
    action.location_prefix = Some("/other".into());
    assert!(action.prepare(&selected, &BTreeMap::new()).is_err());
    action.location_prefix = None;
    action.arguments = vec![ActionArgument::File];
    let multi = selection(vec![
        StorePath::from_unix_path("/tmp/a"),
        StorePath::from_unix_path("/tmp/b"),
    ]);
    assert!(action.prepare(&multi, &BTreeMap::new()).is_err());
    let remote = selection(vec![
        StorePath::from_provider_key(
            ProviderId::new("sftp").unwrap(),
            b"sftp://server/file".to_vec(),
        )
        .unwrap(),
    ]);
    assert!(action.prepare(&remote, &BTreeMap::new()).is_err());
    action.supports_provider_uris = true;
    assert!(
        action.prepare(&remote, &BTreeMap::new()).is_err(),
        "local placeholder must never receive remote URI"
    );
    action.arguments = vec![ActionArgument::Uris];
    assert!(action.prepare(&remote, &BTreeMap::new()).is_ok());
}

#[test]
fn runner_reports_confirmation_missing_executable_exit_and_timeout() {
    let selected = selection(vec![StorePath::from_unix_path("/tmp/file")]);
    let mut action = action();
    assert!(matches!(
        CustomActionRunner::run(action.prepare(&selected, &BTreeMap::new()).unwrap(), false),
        Err(CustomActionError::ConfirmationRequired)
    ));
    assert!(
        CustomActionRunner::run(action.prepare(&selected, &BTreeMap::new()).unwrap(), true).is_ok()
    );
    action.execution = ActionExecution::Direct {
        executable: "/definitely/missing/musheen-test".into(),
    };
    assert!(matches!(
        CustomActionRunner::run(action.prepare(&selected, &BTreeMap::new()).unwrap(), true),
        Err(CustomActionError::MissingExecutable)
    ));
    action.execution = ActionExecution::Direct {
        executable: "/bin/false".into(),
    };
    assert!(matches!(
        CustomActionRunner::run(action.prepare(&selected, &BTreeMap::new()).unwrap(), true),
        Err(CustomActionError::ExitStatus(Some(1)))
    ));
    action.execution = ActionExecution::Direct {
        executable: "/bin/sleep".into(),
    };
    action.arguments = vec![ActionArgument::Literal("2".into())];
    action.timeout_ms = 20;
    assert!(matches!(
        CustomActionRunner::run(action.prepare(&selected, &BTreeMap::new()).unwrap(), true),
        Err(CustomActionError::Timeout)
    ));
}

#[test]
fn settings_persists_actions_atomically_and_rejects_invalid_edits() {
    let root = tempfile::tempdir().unwrap();
    let store = musheen_desktop::SettingsStore::from_config_home(root.path());
    let mut settings = musheen_desktop::SettingsDocument::default();
    let doc = CustomActionDocument::new(vec![action()]).unwrap();
    settings
        .set_value("advanced.custom_actions", &doc.export())
        .unwrap();
    store.save(&settings).unwrap();
    assert_eq!(
        store.load().unwrap().value("advanced.custom_actions"),
        Some(doc.export())
    );
    assert!(
        settings
            .set_value("advanced.custom_actions", "bad")
            .is_err()
    );
    assert_eq!(
        store.load().unwrap().value("advanced.custom_actions"),
        Some(doc.export())
    );
}

#[test]
fn configured_non_utf8_paths_round_trip_and_old_boolean_settings_migrate() {
    let root = tempfile::tempdir().unwrap();
    let mut action = action();
    action.execution = ActionExecution::Direct {
        executable: std::path::PathBuf::from(OsString::from_vec(b"/tmp/non\xffutf8".to_vec())),
    };
    let actions = CustomActionDocument::new(vec![action]).unwrap();
    assert_eq!(
        CustomActionDocument::import(&actions.export()).unwrap(),
        actions
    );
    for value in ["true", "false"] {
        let path = root.path().join(value);
        std::fs::write(&path, format!("schema_version=2\nadvanced.custom_actions={value}\ngeneral.startup=home\nfuture.key=preserved\n")).unwrap();
        let store = musheen_desktop::SettingsStore::at(&path);
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.schema_version(),
            musheen_desktop::SETTINGS_SCHEMA_VERSION
        );
        assert_eq!(loaded.value("general.startup").as_deref(), Some("home"));
        assert_eq!(
            loaded.value("advanced.custom_actions"),
            Some(CustomActionDocument::default().export())
        );
        store.save(&loaded).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("future.key=preserved")
        );
    }
    assert!(
        musheen_desktop::SettingsDocument::default()
            .set_value("advanced.custom_actions", "true")
            .is_err()
    );
}

#[test]
fn shell_receives_hostile_values_only_as_positional_arguments() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("recorded");
    let hostile = root
        .path()
        .join(OsString::from_vec(b"'; touch INJECTED;\n\xff".to_vec()));
    let mut action = action();
    action.execution = ActionExecution::Shell {
        script: "out=$1; shift; printf '%s\\0' \"$@\" > \"$out\"".into(),
        opted_in: true,
    };
    action.arguments = vec![
        ActionArgument::Literal(output.to_str().unwrap().into()),
        ActionArgument::Files,
    ];
    action.working_directory = WorkingDirectory::Fixed(root.path().into());
    let selected = selection(vec![StorePath::from_unix_path(hostile.clone())]);
    CustomActionRunner::run(action.prepare(&selected, &BTreeMap::new()).unwrap(), true).unwrap();
    use std::os::unix::ffi::OsStrExt;
    let mut expected = hostile.as_os_str().as_bytes().to_vec();
    expected.push(0);
    assert_eq!(std::fs::read(output).unwrap(), expected);
    assert!(!root.path().join("INJECTED").exists());
}

#[test]
fn multiline_action_document_is_canonicalized_before_atomic_write() {
    let root = tempfile::tempdir().unwrap();
    let store = musheen_desktop::SettingsStore::from_config_home(root.path());
    let doc = CustomActionDocument::new(vec![action()]).unwrap();
    let pretty = serde_json::to_string_pretty(
        &serde_json::from_str::<serde_json::Value>(&doc.export()).unwrap(),
    )
    .unwrap();
    let mut settings = musheen_desktop::SettingsDocument::default();
    settings
        .set_value("advanced.custom_actions", &pretty)
        .unwrap();
    store.save(&settings).unwrap();
    assert_eq!(store.load().unwrap(), settings);
}

#[test]
fn timeout_stops_descendants_in_the_action_process_group() {
    let root = tempfile::tempdir().unwrap();
    let pidfile = root.path().join("pid");
    let mut action = action();
    action.execution = ActionExecution::Shell {
        script: "/bin/sleep 60 & printf '%s' \"$!\" > \"$1\"; wait".into(),
        opted_in: true,
    };
    action.arguments = vec![ActionArgument::Literal(pidfile.to_str().unwrap().into())];
    action.timeout_ms = 100;
    assert!(matches!(
        CustomActionRunner::run(
            action
                .prepare(
                    &selection(vec![StorePath::from_unix_path("/tmp/a")]),
                    &BTreeMap::new()
                )
                .unwrap(),
            true
        ),
        Err(CustomActionError::Timeout)
    ));
    let pid = std::fs::read_to_string(pidfile).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        let status = std::fs::read_to_string(format!("/proc/{pid}/stat"));
        if status.is_err()
            || status
                .unwrap()
                .split(')')
                .nth(1)
                .unwrap()
                .trim_start()
                .starts_with('Z')
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "descendant must be dead or awaiting reaping"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
