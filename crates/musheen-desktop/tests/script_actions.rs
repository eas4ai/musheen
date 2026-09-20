#![cfg(unix)]

use musheen_desktop::{CustomActionError, ScriptActionLoadError, ScriptActionLoader};
use serde_json::{Value, json};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;

#[test]
fn script_failures_keep_their_source_and_expose_safe_actionable_message_keys() {
    use std::error::Error;
    let hostile = Path::new("/tmp/unsafe\n\x1b[31m");
    let errors = [
        (
            ScriptActionLoadError::InvalidManifest {
                path: hostile.into(),
            },
            "custom-action-script-manifest",
        ),
        (
            ScriptActionLoadError::UnsafeScript {
                path: hostile.into(),
            },
            "custom-action-script-unsafe",
        ),
        (
            ScriptActionLoadError::TooManyEntries,
            "custom-action-script-limit",
        ),
        (
            ScriptActionLoadError::Io {
                path: hostile.into(),
                source: std::io::ErrorKind::PermissionDenied.into(),
            },
            "custom-action-script-permission",
        ),
        (
            ScriptActionLoadError::Io {
                path: hostile.into(),
                source: std::io::ErrorKind::NotFound.into(),
            },
            "custom-action-script-missing",
        ),
    ];
    for (error, key) in errors {
        let wrapped = CustomActionError::ScriptSource(Box::new(error));
        assert_eq!(wrapped.message_key(), key);
        assert!(wrapped.source().is_some());
        assert!(!wrapped.message_key().chars().any(char::is_control));
        assert!(!wrapped.message_key().contains("/tmp"));
    }
}

#[test]
fn absent_optional_directory_is_empty_and_explicit_creation_is_private() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("actions");
    assert!(
        ScriptActionLoader::load_optional(&directory)
            .unwrap()
            .actions()
            .is_empty()
    );
    assert!(!directory.exists());
    ScriptActionLoader::create_directory(&directory).unwrap();
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(
        ScriptActionLoader::load(&directory)
            .unwrap()
            .actions()
            .is_empty()
    );
}

fn write_executable(directory: &Path, name: std::ffi::OsString, contents: &[u8]) {
    let path = directory.join(name);
    fs::write(&path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn manifest(id: &str, script: Value) -> Value {
    json!({
        "version": 1,
        "id": id,
        "label": format!("Run {id}"),
        "script": script,
        "arguments": [{ "kind": "files" }],
        "working_directory": { "policy": "current_location" },
        "mime_patterns": ["*/*"],
        "location_prefix": null,
        "supports_provider_uris": false,
        "confirmation": "always",
        "environment": ["LANG"],
        "timeout_ms": 1000
    })
}

fn write_manifest(directory: &Path, name: &str, value: &Value) {
    fs::write(
        directory.join(format!("{name}.musheen-action.json")),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

#[test]
fn loads_direct_actions_in_stable_id_order_with_lossless_unix_script_names() {
    let root = tempfile::tempdir().unwrap();
    let hostile_name = std::ffi::OsString::from_vec(b"--quote-'\nnon-\xff".to_vec());
    write_executable(root.path(), hostile_name.clone(), b"#!/bin/sh\nexit 0\n");
    write_executable(root.path(), "plain.sh".into(), b"#!/bin/sh\nexit 0\n");

    write_manifest(root.path(), "z", &manifest("zeta", json!("plain.sh")));
    write_manifest(
        root.path(),
        "a",
        &manifest("alpha", json!({ "unix_bytes": hostile_name.into_vec() })),
    );

    let document = ScriptActionLoader::load(root.path()).unwrap();
    let ids = document
        .actions()
        .iter()
        .map(|action| action.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, ["alpha", "zeta"]);
    for action in document.actions() {
        assert!(matches!(
            action.execution,
            musheen_desktop::ActionExecution::Direct { .. }
        ));
        action.validate().unwrap();
    }
}

#[test]
fn rejects_symlink_non_executable_traversal_and_unknown_manifest_fields() {
    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "target.sh".into(), b"#!/bin/sh\nexit 0\n");
    symlink(root.path().join("target.sh"), root.path().join("linked.sh")).unwrap();
    write_manifest(
        root.path(),
        "linked",
        &manifest("linked", json!("linked.sh")),
    );
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::UnsafeScript { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("plain.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    write_manifest(root.path(), "plain", &manifest("plain", json!("plain.sh")));
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::UnsafeScript { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    let outside = root.path().parent().unwrap().join("outside-script");
    write_manifest(
        root.path(),
        "escape",
        &manifest("escape", json!("../outside-script")),
    );
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::UnsafeScript { .. })
    ));
    assert!(!outside.exists());

    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "plain.sh".into(), b"#!/bin/sh\nexit 0\n");
    let mut invalid = manifest("plain", json!("plain.sh"));
    invalid["shell"] = json!("$(touch injected)");
    write_manifest(root.path(), "plain", &invalid);
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::InvalidManifest { .. })
    ));
}

#[test]
fn rejects_duplicate_ids_and_bounded_directory_or_file_overflow() {
    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "plain.sh".into(), b"#!/bin/sh\nexit 0\n");
    write_manifest(
        root.path(),
        "one",
        &manifest("duplicate", json!("plain.sh")),
    );
    write_manifest(
        root.path(),
        "two",
        &manifest("duplicate", json!("plain.sh")),
    );
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::InvalidAction(
            CustomActionError::InvalidDocument
        ))
    ));

    let root = tempfile::tempdir().unwrap();
    for index in 0..=ScriptActionLoader::MAX_DIRECTORY_ENTRIES {
        fs::write(root.path().join(format!("ignored-{index}")), b"x").unwrap();
    }
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::TooManyEntries)
    ));

    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "plain.sh".into(), b"#!/bin/sh\nexit 0\n");
    fs::write(
        root.path().join("huge.musheen-action.json"),
        vec![b' '; ScriptActionLoader::MAX_MANIFEST_BYTES + 1],
    )
    .unwrap();
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::ManifestTooLarge { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("huge.sh");
    let file = fs::File::create(&script).unwrap();
    file.set_len(ScriptActionLoader::MAX_SCRIPT_BYTES + 1)
        .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    write_manifest(root.path(), "huge", &manifest("huge", json!("huge.sh")));
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::ScriptTooLarge { .. })
    ));

    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "shared.sh".into(), b"#!/bin/sh\nexit 0\n");
    for index in 0..65 {
        let id = format!("action-{index:02}");
        write_manifest(root.path(), &id, &manifest(&id, json!("shared.sh")));
    }
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::InvalidAction(
            CustomActionError::InvalidDocument
        ))
    ));

    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "shared.sh".into(), b"#!/bin/sh\nexit 0\n");
    for index in 0..9 {
        let id = format!("large-manifest-{index}");
        let mut bytes = serde_json::to_vec(&manifest(&id, json!("shared.sh"))).unwrap();
        bytes.resize(31 * 1024, b' ');
        fs::write(root.path().join(format!("{id}.musheen-action.json")), bytes).unwrap();
    }
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::TotalManifestBytesExceeded)
    ));

    let root = tempfile::tempdir().unwrap();
    for index in 0..5 {
        let script_name = format!("large-{index}.sh");
        let script = root.path().join(&script_name);
        let file = fs::File::create(&script).unwrap();
        file.set_len(ScriptActionLoader::MAX_SCRIPT_BYTES).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        let id = format!("large-script-{index}");
        write_manifest(root.path(), &id, &manifest(&id, json!(script_name)));
    }
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::TotalScriptBytesExceeded)
    ));
}

#[test]
fn manifest_cannot_enable_shell_execution_or_bypass_action_policy() {
    let root = tempfile::tempdir().unwrap();
    write_executable(root.path(), "plain.sh".into(), b"#!/bin/sh\nexit 0\n");
    let mut invalid = manifest("plain", json!("plain.sh"));
    invalid["environment"] = json!(["LD_PRELOAD"]);
    write_manifest(root.path(), "plain", &invalid);
    assert!(matches!(
        ScriptActionLoader::load(root.path()),
        Err(ScriptActionLoadError::InvalidAction(
            CustomActionError::InvalidDocument
        ))
    ));
}
