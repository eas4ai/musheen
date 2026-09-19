use musheen_core::{DisplayPath, ItemId, ItemKind, ProviderId, StoreItem, StorePath, WatchEvent};
use musheen_ui::sidebar::{PinStore, SidebarEntry, SidebarModel, SidebarSectionKind};
use musheen_ui::views::{
    AdaptiveLayout, ColumnKey, DirectoryViewModel, GroupKey, Layout, SelectionMode, SortDirection,
    SortKey, ViewPreferenceStore, ViewPreferences,
};

fn item(index: u64, name: &str, kind: ItemKind, size: Option<u64>) -> StoreItem {
    let provider = ProviderId::new("local").expect("provider ID is valid");
    StoreItem::new(
        ItemId::new(provider, index.to_be_bytes()).expect("item ID is valid"),
        StorePath::from_unix_path(format!("/fixture/{name}")),
        DisplayPath::new(name),
        kind,
        size,
    )
    .with_modified_unix_seconds(index as i64)
}

fn names(model: &DirectoryViewModel) -> Vec<&str> {
    model
        .visible_items()
        .iter()
        .map(|item| item.display_name().as_str())
        .collect()
}

#[test]
fn every_layout_projects_the_same_directory_model() {
    let mut model = DirectoryViewModel::new(4_096);
    model.extend([item(1, "one", ItemKind::RegularFile, Some(1))]);

    for layout in Layout::ALL {
        model.preferences_mut().layout = layout;
        assert_eq!(names(&model), vec!["one"]);
    }
    assert_eq!(AdaptiveLayout::resolve(719.0), Layout::List);
    assert_eq!(AdaptiveLayout::resolve(1_400.0), Layout::Grid);
}

#[test]
fn natural_sort_group_and_directories_first_are_independent() {
    let mut model = DirectoryViewModel::new(4_096);
    model.extend([
        item(1, "file10.txt", ItemKind::RegularFile, Some(10)),
        item(2, "z-folder", ItemKind::Directory, None),
        item(3, "file2.txt", ItemKind::RegularFile, Some(2)),
    ]);
    model.preferences_mut().sort.key = SortKey::Name;
    model.preferences_mut().sort.direction = SortDirection::Ascending;
    model.preferences_mut().directories_first = false;
    assert_eq!(names(&model), vec!["file2.txt", "file10.txt", "z-folder"]);

    model.preferences_mut().directories_first = true;
    model.preferences_mut().group = GroupKey::Kind;
    assert_eq!(model.preferences().sort.key, SortKey::Name);
    assert_eq!(model.preferences().group, GroupKey::Kind);
    assert_eq!(names(&model), vec!["z-folder", "file2.txt", "file10.txt"]);

    model.preferences_mut().directories_first = false;
    model.preferences_mut().group = GroupKey::None;
    model.preferences_mut().sort = musheen_ui::views::SortSpec {
        key: SortKey::Modified,
        direction: SortDirection::Descending,
    };
    assert_eq!(names(&model), vec!["file2.txt", "z-folder", "file10.txt"]);

    model.preferences_mut().sort.key = SortKey::Kind;
    model.preferences_mut().sort.direction = SortDirection::Ascending;
    assert_eq!(names(&model), vec!["z-folder", "file2.txt", "file10.txt"]);
}

#[test]
fn hidden_items_and_details_sort_are_scoped_to_one_view() {
    let mut first = DirectoryViewModel::new(4_096);
    first.extend([
        item(1, ".private", ItemKind::RegularFile, Some(50)),
        item(2, "small", ItemKind::RegularFile, Some(2)),
        item(3, "large", ItemKind::RegularFile, Some(20)),
    ]);
    let second = first.clone();

    first.preferences_mut().show_hidden = true;
    first.toggle_details_sort(ColumnKey::Size);
    assert_eq!(names(&first), vec!["small", "large", ".private"]);
    first.toggle_details_sort(ColumnKey::Size);
    assert_eq!(names(&first), vec![".private", "large", "small"]);
    assert_eq!(names(&second), vec!["large", "small"]);
}

#[test]
fn unknown_metadata_sorts_after_known_values_in_both_directions() {
    let mut model = DirectoryViewModel::new(4_096);
    model.extend([
        item(1, "known-small", ItemKind::RegularFile, Some(2)),
        item(2, "unknown", ItemKind::RegularFile, None),
        item(3, "known-large", ItemKind::RegularFile, Some(20)),
    ]);
    model.preferences_mut().directories_first = false;
    model.preferences_mut().sort.key = SortKey::Size;

    assert_eq!(names(&model), vec!["known-small", "known-large", "unknown"]);
    model.preferences_mut().sort.direction = SortDirection::Descending;
    assert_eq!(names(&model), vec!["known-large", "known-small", "unknown"]);
}

#[test]
fn selection_editing_and_scroll_anchor_survive_retention_and_reordering() {
    let first = item(1, "one", ItemKind::RegularFile, Some(1));
    let second = item(2, "two", ItemKind::RegularFile, Some(2));
    let first_id = first.id().clone();
    let second_id = second.id().clone();
    let mut model = DirectoryViewModel::new(2);
    model.extend([first, second]);
    model.rubber_band_select(0..=0, SelectionMode::Replace);
    model.set_editing(Some(second_id.clone()));
    model.set_scroll_anchor(Some(first_id.clone()), 18.0);
    model.extend([
        item(3, "three", ItemKind::RegularFile, Some(3)),
        item(4, "four", ItemKind::RegularFile, Some(4)),
    ]);

    assert!(model.item(&first_id).is_some());
    assert!(model.item(&second_id).is_some());
    assert_eq!(model.unpinned_model_count(), 2);
    assert_eq!(
        model.scroll_anchor().expect("anchor remains").item(),
        &first_id
    );
    assert_eq!(model.selected_ids(), &[first_id]);
}

#[test]
fn external_changes_preserve_stable_identity_selection_and_anchor() {
    let original = item(7, "old-name", ItemKind::RegularFile, Some(7));
    let id = original.id().clone();
    let mut model = DirectoryViewModel::new(4_096);
    model.extend([original]);
    model.select_visible_range(0..=0, SelectionMode::Replace);
    model.set_editing(Some(id.clone()));
    model.set_scroll_anchor(Some(id.clone()), 9.0);
    model.preferences_mut().group = GroupKey::Kind;

    model.apply_watch_event(WatchEvent::Renamed {
        previous_path: StorePath::from_unix_path("/fixture/old-name"),
        item: item(7, "new-name", ItemKind::RegularFile, Some(8)),
    });
    let created = item(8, "created", ItemKind::RegularFile, Some(1));
    let created_id = created.id().clone();
    model.apply_watch_event(WatchEvent::Created(created));
    model.apply_watch_event(WatchEvent::Changed(item(
        7,
        "new-name",
        ItemKind::RegularFile,
        Some(9),
    )));
    model.apply_watch_event(WatchEvent::Removed(created_id));

    assert_eq!(names(&model), vec!["new-name"]);
    assert_eq!(model.selected_ids(), std::slice::from_ref(&id));
    assert_eq!(model.scroll_anchor().expect("anchor remains").item(), &id);
    assert_eq!(model.editing(), Some(&id));
    assert_eq!(model.preferences().group, GroupKey::Kind);
    assert_eq!(model.item(&id).and_then(StoreItem::size), Some(9));
}

#[test]
fn per_directory_preferences_keep_lossless_paths_and_column_layouts() {
    let first = StorePath::from_unix_bytes(b"/one/bad-\xff".to_vec());
    let second = StorePath::from_unix_path("/two");
    let mut store = ViewPreferenceStore::new(ViewPreferences::default());
    let mut preferences = ViewPreferences {
        layout: Layout::Details,
        ..ViewPreferences::default()
    };
    preferences.columns.resize(ColumnKey::Name, 320.0).unwrap();
    preferences.columns.hide(ColumnKey::Modified).unwrap();
    preferences
        .columns
        .move_before(ColumnKey::Size, ColumnKey::Name)
        .unwrap();
    preferences.columns.move_right(ColumnKey::Name).unwrap();
    preferences.columns.move_left(ColumnKey::Name).unwrap();
    store.set(first.clone(), preferences.clone());

    assert_eq!(store.for_path(&first), &preferences);
    assert_eq!(store.for_path(&second), store.defaults());
    assert_eq!(
        store.for_path(&first).columns.visible_columns(),
        vec![ColumnKey::Size, ColumnKey::Name, ColumnKey::Kind]
    );
    assert!(store.for_path(&first).columns.is_visible(ColumnKey::Size));
    assert!(
        !store
            .for_path(&first)
            .columns
            .is_visible(ColumnKey::Modified)
    );
    assert_eq!(
        store.for_path(&first).columns.width(ColumnKey::Name),
        Some(320)
    );
}

#[test]
fn sidebar_sections_hide_when_empty_and_pins_are_shared_between_tabs() {
    let pins = PinStore::default();
    let mut first = SidebarModel::new(pins.clone());
    let second = SidebarModel::new(pins.clone());
    first.set_section_items(
        SidebarSectionKind::Mounts,
        [SidebarEntry::new("System", StorePath::from_unix_path("/"))],
    );
    for (kind, label, path) in [
        (SidebarSectionKind::Home, "Home", "/home/test"),
        (SidebarSectionKind::Remote, "Server", "/remote"),
        (SidebarSectionKind::Network, "Network", "/network"),
        (SidebarSectionKind::Tags, "Important", "/tag/important"),
    ] {
        first.set_section_items(
            kind,
            [SidebarEntry::new(label, StorePath::from_unix_path(path))],
        );
    }
    first.set_expanded(StorePath::from_unix_path("/projects"), true);
    first.set_section_collapsed(SidebarSectionKind::Tags, true);
    pins.replace([SidebarEntry::new(
        "Projects",
        StorePath::from_unix_path("/projects"),
    )]);

    assert!(
        first
            .sections()
            .iter()
            .any(|section| section.kind() == SidebarSectionKind::Pinned)
    );
    assert!(
        second
            .sections()
            .iter()
            .any(|section| section.kind() == SidebarSectionKind::Pinned)
    );
    assert!(first.is_expanded(&StorePath::from_unix_path("/projects")));
    assert!(!second.is_expanded(&StorePath::from_unix_path("/projects")));
    assert!(first.is_section_collapsed(SidebarSectionKind::Tags));
    assert!(!second.is_section_collapsed(SidebarSectionKind::Tags));
    assert_eq!(
        first
            .sections()
            .iter()
            .map(|section| section.kind())
            .collect::<Vec<_>>(),
        vec![
            SidebarSectionKind::Home,
            SidebarSectionKind::Pinned,
            SidebarSectionKind::Mounts,
            SidebarSectionKind::Remote,
            SidebarSectionKind::Network,
            SidebarSectionKind::Tags,
        ]
    );
    assert!(
        second
            .sections()
            .iter()
            .all(|section| section.kind() != SidebarSectionKind::Tags)
    );
}

#[test]
fn large_directories_keep_model_and_render_windows_bounded() {
    let mut model = DirectoryViewModel::new(4_096);
    for page_start in (0..1_000_000).step_by(512) {
        model.extend((page_start..page_start + 512).map(|index| {
            item(
                index,
                &format!("item-{index}"),
                ItemKind::RegularFile,
                Some(index),
            )
        }));
        assert!(model.unpinned_model_count() <= 4_096);
    }

    assert_eq!(model.unpinned_model_count(), 4_096);
    assert_eq!(model.rendered_range(1_000, 100), 1_000..1_300);
}
