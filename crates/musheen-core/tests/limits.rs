use musheen_core::{ResourceLimitConfig, ResourceLimits};

#[test]
fn default_directory_limits_match_the_production_budget() {
    let limits = ResourceLimits::default();

    assert_eq!(limits.directory_page_items(), 512);
    assert_eq!(limits.directory_prefetch_pages(), 2);
    assert_eq!(limits.directory_retained_items(), 4_096);
    assert_eq!(limits.directory_rendered_viewports(), 3);
    assert_eq!(limits.operation_data_mutations(), 2);
    assert_eq!(limits.operation_metadata_jobs(), 4);
    assert_eq!(limits.operation_hash_preview_jobs(), 4);
}

#[test]
fn captured_limits_do_not_change_with_later_settings() {
    let original = ResourceLimits::default();
    let captured = original.snapshot();
    let lowered = ResourceLimits::try_from(ResourceLimitConfig {
        directory_page_items: 128,
        ..ResourceLimitConfig::default()
    })
    .expect("a lower page size is valid");

    assert_eq!(captured.directory_page_items(), 512);
    assert_eq!(lowered.directory_page_items(), 128);
}

#[test]
fn zero_and_above_maximum_limits_are_rejected() {
    let zero = ResourceLimits::try_from(ResourceLimitConfig {
        directory_page_items: 0,
        ..ResourceLimitConfig::default()
    });
    let too_many = ResourceLimits::try_from(ResourceLimitConfig {
        directory_page_items: ResourceLimitConfig::MAX_DIRECTORY_PAGE_ITEMS + 1,
        ..ResourceLimitConfig::default()
    });

    assert!(zero.is_err());
    assert!(too_many.is_err());

    for invalid in [
        ResourceLimitConfig {
            directory_prefetch_pages: 0,
            ..ResourceLimitConfig::default()
        },
        ResourceLimitConfig {
            directory_retained_items: 0,
            ..ResourceLimitConfig::default()
        },
        ResourceLimitConfig {
            directory_rendered_viewports: 0,
            ..ResourceLimitConfig::default()
        },
        ResourceLimitConfig {
            operation_data_mutations: 0,
            ..ResourceLimitConfig::default()
        },
        ResourceLimitConfig {
            operation_metadata_jobs: 0,
            ..ResourceLimitConfig::default()
        },
        ResourceLimitConfig {
            operation_hash_preview_jobs: 0,
            ..ResourceLimitConfig::default()
        },
    ] {
        assert!(ResourceLimits::try_from(invalid).is_err());
    }
}

#[test]
fn directory_retention_hard_max_matches_the_resident_model_cap() {
    assert_eq!(ResourceLimitConfig::MAX_DIRECTORY_RETAINED_ITEMS, 4_096);

    let above_cap = ResourceLimits::try_from(ResourceLimitConfig {
        directory_retained_items: 4_097,
        ..ResourceLimitConfig::default()
    });
    assert!(above_cap.is_err());
}
