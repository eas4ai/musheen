use musheen_core::{ResourceLimitConfig, ResourceLimits};

#[test]
fn default_directory_limits_match_the_production_budget() {
    let limits = ResourceLimits::default();

    assert_eq!(limits.directory_page_items(), 512);
    assert_eq!(limits.directory_prefetch_pages(), 2);
    assert_eq!(limits.directory_retained_items(), 4_096);
    assert_eq!(limits.directory_rendered_viewports(), 3);
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
    ] {
        assert!(ResourceLimits::try_from(invalid).is_err());
    }
}
