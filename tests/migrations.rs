use musheen_desktop::{
    CatalogDocument, CatalogError, CatalogStore, SettingsDocument, SettingsStore,
};
use std::fs;

#[test]
fn settings_v1_source_survives_multiple_candidate_writes_for_downgrade() {
    let temporary = tempfile::tempdir().unwrap();
    let store = SettingsStore::from_config_home(temporary.path());
    fs::create_dir_all(store.path().parent().unwrap()).unwrap();
    let original = b"schema_version=1\ndirectory_page_items=256\nfuture.keep=opaque\n";
    fs::write(store.path(), original).unwrap();

    let mut migrated = store.load().unwrap();
    store.save(&migrated).unwrap();
    migrated.resource_limits_mut().directory_page_items = 128;
    store.save(&migrated).unwrap();

    let migration_backup = store.path().with_file_name("settings.conf.pre-migration");
    assert_eq!(fs::read(migration_backup).unwrap(), original);
    assert!(
        fs::read_to_string(store.path())
            .unwrap()
            .starts_with("schema_version=3\ndirectory_page_items=128\n")
    );
}

#[test]
fn older_catalog_writer_preserves_a_newer_schema_backup() {
    let temporary = tempfile::tempdir().unwrap();
    let store = CatalogStore::at(temporary.path().join("catalog.json"));
    store.save(&CatalogDocument::default()).unwrap();
    let primary = fs::read(store.path()).unwrap();
    let mut newer_backup: serde_json::Value = serde_json::from_slice(&primary).unwrap();
    newer_backup["schema_version"] = serde_json::json!(2);
    newer_backup["future"] = serde_json::json!("keep");
    let newer_backup = serde_json::to_vec(&newer_backup).unwrap();
    fs::write(store.backup_path(), &newer_backup).unwrap();

    assert!(matches!(
        store.save(&CatalogDocument::default()),
        Err(CatalogError::UnsupportedVersion { version: 2, .. })
    ));
    assert_eq!(fs::read(store.path()).unwrap(), primary);
    assert_eq!(fs::read(store.backup_path()).unwrap(), newer_backup);
}

#[test]
fn newer_catalog_shape_is_not_treated_as_corrupt_old_data() {
    let temporary = tempfile::tempdir().unwrap();
    let store = CatalogStore::at(temporary.path().join("catalog.json"));
    store.save(&CatalogDocument::default()).unwrap();
    store.save(&CatalogDocument::default()).unwrap();
    let backup = fs::read(store.backup_path()).unwrap();
    let newer = br#"{"schema_version":2,"new_layout":{"future":"keep"}}"#;
    fs::write(store.path(), newer).unwrap();

    assert!(matches!(
        store.load(),
        Err(CatalogError::UnsupportedVersion { version: 2, .. })
    ));
    assert_eq!(fs::read(store.path()).unwrap(), newer);
    assert_eq!(fs::read(store.backup_path()).unwrap(), backup);
}

#[test]
fn oversized_future_catalog_version_cannot_trigger_backup_recovery() {
    let temporary = tempfile::tempdir().unwrap();
    let store = CatalogStore::at(temporary.path().join("catalog.json"));
    store.save(&CatalogDocument::default()).unwrap();
    store.save(&CatalogDocument::default()).unwrap();
    let backup = fs::read(store.backup_path()).unwrap();
    let newer = br#"{"schema_version":4294967296,"new_layout":{}}"#;
    fs::write(store.path(), newer).unwrap();

    assert!(matches!(
        store.load(),
        Err(CatalogError::UnsupportedVersion { version, .. })
            if version.to_string() == "4294967296"
    ));
    assert_eq!(fs::read(store.path()).unwrap(), newer);
    assert_eq!(fs::read(store.backup_path()).unwrap(), backup);
}

#[test]
fn unrecognized_catalog_version_does_not_destroy_the_only_newer_copy() {
    let temporary = tempfile::tempdir().unwrap();
    let store = CatalogStore::at(temporary.path().join("catalog.json"));
    store.save(&CatalogDocument::default()).unwrap();
    store.save(&CatalogDocument::default()).unwrap();
    let backup = fs::read(store.backup_path()).unwrap();
    let newer = br#"{"schema_version":"next","new_layout":{}}"#;
    fs::write(store.path(), newer).unwrap();

    assert!(matches!(
        store.load(),
        Err(CatalogError::UnrecognizedVersion { .. })
    ));
    assert_eq!(fs::read(store.path()).unwrap(), newer);
    assert_eq!(fs::read(store.backup_path()).unwrap(), backup);
}

#[test]
fn unrecognized_settings_version_does_not_trigger_backup_recovery() {
    for version in ["next", "4294967296"] {
        let temporary = tempfile::tempdir().unwrap();
        let store = SettingsStore::from_config_home(temporary.path());
        store.save(&SettingsDocument::default()).unwrap();
        store.save(&SettingsDocument::default()).unwrap();
        let backup = fs::read(store.backup_path()).unwrap();
        let newer = format!("schema_version={version}\nnew_layout=keep\n");
        fs::write(store.path(), &newer).unwrap();

        assert!(matches!(
            store.load(),
            Err(musheen_desktop::SettingsError::UnrecognizedVersion { .. })
        ));
        assert_eq!(fs::read(store.path()).unwrap(), newer.as_bytes());
        assert_eq!(fs::read(store.backup_path()).unwrap(), backup);
    }
}
