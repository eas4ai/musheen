mod settings {
    use musheen_core::ResourceLimitConfig;
    use musheen_desktop::{SettingsDocument, SettingsStore};
    use std::fs;

    #[test]
    fn uses_the_musheen_xdg_config_location() {
        let config_home = std::path::Path::new("/tmp/musheen-config-location-fixture");
        let store = SettingsStore::from_config_home(config_home);

        assert_eq!(store.path(), config_home.join("musheen/settings.conf"));
    }

    #[test]
    fn atomic_writes_preserve_and_recover_the_last_known_good_document() {
        let root = tempfile::tempdir().expect("the temporary root is created");
        let store = SettingsStore::from_config_home(root.path());
        let mut first = SettingsDocument::default();
        first.resource_limits_mut().directory_page_items = 256;
        first.resource_limits_mut().operation_metadata_jobs = 3;
        store.save(&first).expect("the first document is saved");
        let mut second = first.clone();
        second.resource_limits_mut().directory_page_items = 128;
        store.save(&second).expect("the second document is saved");

        fs::write(store.path(), "not a settings document\n")
            .expect("the primary document is corrupted");
        let recovered = store.load().expect("the valid backup is recovered");

        assert_eq!(recovered, first);
        assert_eq!(store.load().expect("the restored primary reloads"), first);
        assert_no_temporary_files(&store);
    }

    #[test]
    fn invalid_values_default_without_discarding_valid_neighbors() {
        let root = tempfile::tempdir().expect("the temporary root is created");
        let store = SettingsStore::from_config_home(root.path());
        fs::create_dir_all(store.path().parent().unwrap())
            .expect("the settings directory is created");
        fs::write(
            store.path(),
            "schema_version=1\ndirectory_page_items=invalid\ndirectory_prefetch_pages=2\ndirectory_retained_items=1234\ndirectory_rendered_viewports=3\n",
        )
        .expect("the fixture is written");

        let loaded = store.load().expect("the recoverable document loads");

        assert_eq!(
            loaded.resource_limits().directory_page_items,
            ResourceLimitConfig::default().directory_page_items
        );
        assert_eq!(loaded.resource_limits().directory_retained_items, 1234);
        assert_eq!(
            loaded.resource_limits().operation_data_mutations,
            ResourceLimitConfig::default().operation_data_mutations
        );
    }

    #[test]
    fn operations_capture_an_immutable_resource_limit_snapshot() {
        let mut document = SettingsDocument::default();
        let snapshot = document
            .resource_limits_snapshot()
            .expect("the default limits are valid");
        document.resource_limits_mut().directory_page_items = 64;
        document.resource_limits_mut().operation_data_mutations = 1;

        assert_eq!(snapshot.directory_page_items(), 512);
        assert_eq!(snapshot.operation_data_mutations(), 2);
        assert_eq!(
            document
                .resource_limits_snapshot()
                .expect("the edited limits are valid")
                .directory_page_items(),
            64
        );
        assert_eq!(
            document
                .resource_limits_snapshot()
                .expect("the edited limits are valid")
                .operation_data_mutations(),
            1
        );
    }

    fn assert_no_temporary_files(store: &SettingsStore) {
        let entries = fs::read_dir(store.path().parent().unwrap())
            .expect("the settings directory is readable")
            .collect::<Result<Vec<_>, _>>()
            .expect("the settings entries are readable");
        assert!(
            entries
                .iter()
                .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp."))
        );
    }
}
