use musheen_core::{
    DisplayPath, ItemId, ItemKind, Page, PageRequest, ProviderId, ResourceLimits, StoreItem,
    StorePath, TotalHint,
};
use musheen_local::LocalStore;
use musheen_ui::{
    AppearanceMode, ApplyPageResult, ContentIdentity, DirectoryModel, DirectoryState, FocusTarget,
    MotionPolicy, SemanticRegion, ShellModel, ThemeProfile, enumerate_directory,
    freedesktop_icon_name, lucide_icon,
};

#[test]
fn shell_exposes_the_files_layout_as_semantic_regions() {
    let shell = ShellModel::new(true);

    assert_eq!(
        shell.semantic_regions(),
        &[
            SemanticRegion::TabStrip,
            SemanticRegion::NavigationToolbar,
            SemanticRegion::Sidebar,
            SemanticRegion::DirectoryContent,
            SemanticRegion::Info,
            SemanticRegion::StatusBar,
        ]
    );
    assert_eq!(
        ShellModel::new(false).semantic_regions(),
        &[
            SemanticRegion::TabStrip,
            SemanticRegion::NavigationToolbar,
            SemanticRegion::Sidebar,
            SemanticRegion::DirectoryContent,
            SemanticRegion::StatusBar,
        ]
    );
}

#[test]
fn keyboard_focus_order_follows_the_visible_shell() {
    let shell = ShellModel::new(true);

    assert_eq!(
        shell.focus_order(),
        &[
            FocusTarget::Tabs,
            FocusTarget::Back,
            FocusTarget::Forward,
            FocusTarget::Parent,
            FocusTarget::Refresh,
            FocusTarget::Location,
            FocusTarget::Search,
            FocusTarget::ViewMode,
            FocusTarget::InfoToggle,
            FocusTarget::Settings,
            FocusTarget::Sidebar,
            FocusTarget::Directory,
            FocusTarget::Info,
        ]
    );
}

#[test]
fn toolbar_uses_stable_command_registry_ids() {
    let shell = ShellModel::new(false);

    assert_eq!(
        shell.toolbar_command_ids(),
        &[
            "navigation.back",
            "navigation.forward",
            "navigation.parent",
            "navigation.refresh",
            "navigation.location",
            "view.search",
            "view.list",
            "view.grid",
            "view.info",
            "app.settings",
        ]
    );
    for command_id in shell.toolbar_command_ids() {
        assert!(shell.commands().get(command_id).is_some());
    }
}

#[test]
fn every_registered_command_icon_has_one_lucide_mapping() {
    let shell = ShellModel::new(false);

    for command in shell.commands().commands() {
        assert!(
            lucide_icon(command.icon_key()).is_some(),
            "missing icon mapping for {}",
            command.icon_key()
        );
    }
}

#[test]
fn theme_profiles_distinguish_system_appearances_and_reduce_motion() {
    let light = ThemeProfile::new(AppearanceMode::Light, false);
    let dark = ThemeProfile::new(AppearanceMode::Dark, false);
    let high_contrast = ThemeProfile::new(AppearanceMode::HighContrast, true);

    assert_ne!(light.surface(), dark.surface());
    assert_ne!(dark.surface(), high_contrast.surface());
    assert!(high_contrast.has_strong_boundaries());
    assert_eq!(light.motion(), MotionPolicy::Standard);
    assert_eq!(high_contrast.motion(), MotionPolicy::Reduced);
}

#[test]
fn content_identity_maps_to_freedesktop_theme_names() {
    assert_eq!(
        freedesktop_icon_name(&ContentIdentity::directory()),
        "folder"
    );
    assert_eq!(
        freedesktop_icon_name(&ContentIdentity::mime("image/png")),
        "image-png"
    );
    assert_eq!(
        freedesktop_icon_name(&ContentIdentity::application("org.gnome.TextEditor")),
        "org.gnome.TextEditor"
    );
    assert_eq!(
        freedesktop_icon_name(&ContentIdentity::symbolic_link()),
        "emblem-symbolic-link"
    );
}

#[test]
fn directory_model_cancels_old_navigation_and_rejects_stale_pages() {
    let mut model = DirectoryModel::new(ResourceLimits::default());
    let first = model.begin_navigation(StorePath::from_unix_path("/first"));
    let second = model.begin_navigation(StorePath::from_unix_path("/second"));

    assert!(first.cancellation().is_cancelled());
    assert!(!second.cancellation().is_cancelled());
    assert_eq!(model.state(), &DirectoryState::Loading);

    let stale = page(vec![item(1, "/first/old")]);
    assert_eq!(model.apply_page(&first, stale), ApplyPageResult::Stale);
    assert_eq!(model.location(), Some(second.location()));
    assert!(model.items().is_empty());
}

#[test]
fn directory_model_has_loading_empty_ready_and_error_states() {
    let mut model = DirectoryModel::new(ResourceLimits::default());
    let empty_load = model.begin_navigation(StorePath::from_unix_path("/empty"));
    assert_eq!(model.state(), &DirectoryState::Loading);

    assert_eq!(
        model.apply_page(&empty_load, page(Vec::new())),
        ApplyPageResult::Applied
    );
    assert_eq!(model.state(), &DirectoryState::Empty);

    let ready_load = model.begin_navigation(StorePath::from_unix_path("/ready"));
    model.apply_page(&ready_load, page(vec![item(2, "/ready/file.txt")]));
    assert_eq!(model.state(), &DirectoryState::Ready);

    let error_load = model.begin_navigation(StorePath::from_unix_path("/denied"));
    assert!(model.apply_error(&error_load, "Permission denied"));
    assert_eq!(
        model.state(),
        &DirectoryState::Error("Permission denied".into())
    );
}

#[test]
fn directory_model_retains_4096_items_and_renders_three_viewports() {
    let limits = ResourceLimits::default();
    let mut model = DirectoryModel::new(limits);
    let load = model.begin_navigation(StorePath::from_unix_path("/many"));

    for batch in 0..16 {
        let items = (0..512)
            .map(|offset| {
                let index = batch * 512 + offset;
                item(index, &format!("/many/{index}"))
            })
            .collect();
        model.apply_page(&load, page(items));
    }

    assert_eq!(model.items().len(), 4_096);
    let range = model.rendered_range(1_000, 24);
    assert_eq!(range, 1_000..1_072);
    assert!(range.len() <= 24 * 3);
}

#[test]
fn shell_gallery_enumerates_through_the_same_local_store_path() {
    let limits = ResourceLimits::default();
    let mut model = DirectoryModel::new(limits.snapshot());
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../musheen-test-support/fixtures/shell-gallery");
    let load = model.begin_navigation(StorePath::from_unix_path(fixture.into_os_string()));

    let pages =
        futures_lite::future::block_on(enumerate_directory(&LocalStore::new(), &load, &limits))
            .expect("gallery enumeration succeeds");
    let returned = pages.iter().map(|page| page.items().len()).sum::<usize>();
    for page in pages {
        model.apply_page(&load, page);
    }

    assert_eq!(returned, 3);
    assert_eq!(model.items().len(), 3);
    assert_eq!(model.state(), &DirectoryState::Ready);
}

fn page(items: Vec<StoreItem>) -> Page<StoreItem> {
    let request = PageRequest::first(&ResourceLimits::default());
    Page::try_new(&request, items, None, TotalHint::Unknown).expect("test page is bounded")
}

fn item(index: usize, path: &str) -> StoreItem {
    let provider = ProviderId::new("local").expect("test provider ID is valid");
    StoreItem::new(
        ItemId::new(provider, index.to_be_bytes()).expect("test item ID is valid"),
        StorePath::from_unix_path(path),
        DisplayPath::new(path.rsplit('/').next().unwrap_or(path)),
        ItemKind::RegularFile,
        Some(index as u64),
    )
}
