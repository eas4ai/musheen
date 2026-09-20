use musheen_core::{
    CapabilityKind, CapabilityMatrix, CapabilityReason, CapabilityState, ItemId, ProviderId,
    StorePath,
};
use musheen_desktop::{
    BackendTagService, CatalogDocument, CatalogStore, FolderIdentity, FolderPreference,
    FolderPreferenceCatalog, FolderSortDirection, FolderSortKey, FolderView, HomeItemKind,
    HomeModel, MountShortcut, PinCatalog, PinError, PinState, RecentLocations, TagBackend,
    TagCatalog, TagMoveOutcome, TagService, TagStorage, XattrTagBackend, XattrTagError,
};
use std::collections::BTreeSet;
use std::path::Path;

fn provider(name: &str) -> ProviderId {
    ProviderId::new(name).expect("valid provider")
}

fn item(provider_name: &str, key: &[u8]) -> ItemId {
    ItemId::new(provider(provider_name), key.to_vec()).expect("valid item identity")
}

fn location(provider_name: &str, key: &[u8]) -> FolderIdentity {
    FolderIdentity::new(provider(provider_name), key.to_vec()).expect("valid location identity")
}

fn path(bytes: &[u8]) -> StorePath {
    StorePath::from_unix_bytes(bytes.to_vec())
}

fn tags(names: &[&str]) -> BTreeSet<Box<str>> {
    names.iter().map(|name| Box::<str>::from(*name)).collect()
}

#[test]
fn fallback_tags_follow_identity_not_reused_paths() {
    let original = item("local", b"inode:41");
    let replacement = item("local", b"inode:92");
    let old_path = path(b"/media/disk/report.txt");
    let renamed_path = path(b"/media/disk/final.txt");
    let mut catalog = TagCatalog::default();

    catalog
        .assign(&original, old_path.clone(), "important")
        .unwrap();
    catalog.observe_present(&original, renamed_path.clone());

    assert_eq!(catalog.tags_for(&original), tags(&["important"]));
    assert_eq!(catalog.path_hint(&original), Some(&renamed_path));
    assert!(catalog.tags_for(&replacement).is_empty());

    catalog.observe_missing(&original);
    assert!(catalog.is_orphaned(&original));
    assert_eq!(catalog.cleanup_reviewed_orphans([&replacement]), 0);
    assert_eq!(catalog.cleanup_reviewed_orphans([&original]), 1);
    assert!(catalog.tags_for(&original).is_empty());
}

#[test]
fn app_driven_moves_preserve_tags_or_report_an_unsupported_destination() {
    let source = item("local", b"dev1:inode:7");
    let same_store = item("local", b"dev1:inode:7");
    let cross_store = item("smb-account", b"remote-file-4");
    let unsupported = item("archive", b"entry-2");
    let mut catalog = TagCatalog::default();
    catalog
        .assign(&source, path(b"/source/file"), "reviewed")
        .unwrap();

    assert_eq!(
        catalog.note_app_move(&source, same_store.clone(), path(b"/source/renamed"), true,),
        TagMoveOutcome::Preserved
    );
    assert_eq!(catalog.tags_for(&same_store), tags(&["reviewed"]));

    assert_eq!(
        catalog.note_app_move(
            &same_store,
            cross_store.clone(),
            StorePath::from_provider_key(provider("smb-account"), b"share/final".to_vec()).unwrap(),
            true,
        ),
        TagMoveOutcome::Preserved
    );
    assert!(catalog.tags_for(&same_store).is_empty());
    assert_eq!(catalog.tags_for(&cross_store), tags(&["reviewed"]));

    assert_eq!(
        catalog.note_app_move(
            &cross_store,
            unsupported.clone(),
            StorePath::from_provider_key(provider("archive"), b"entry-2".to_vec()).unwrap(),
            false,
        ),
        TagMoveOutcome::UnsupportedDestination
    );
    assert_eq!(catalog.tags_for(&cross_store), tags(&["reviewed"]));
    assert!(catalog.tags_for(&unsupported).is_empty());
}

#[test]
fn tag_rename_and_delete_are_catalog_wide() {
    let first = item("local", b"one");
    let second = item("local", b"two");
    let mut catalog = TagCatalog::default();
    catalog.assign(&first, path(b"/one"), "todo").unwrap();
    catalog.assign(&second, path(b"/two"), "todo").unwrap();
    catalog.assign(&second, path(b"/two"), "keep").unwrap();

    assert_eq!(catalog.rename("todo", "next").unwrap(), 2);
    assert_eq!(catalog.tags_for(&first), tags(&["next"]));
    assert_eq!(catalog.delete("next"), 2);
    assert_eq!(catalog.tags_for(&second), tags(&["keep"]));
}

fn tag_service_contract<B: TagBackend>(backend: B, target: ItemId, hint: StorePath)
where
    B::Error: std::fmt::Debug,
{
    let mut service = BackendTagService::new(backend);
    service.assign(&target, &hint, "blue").unwrap();
    service.assign(&target, &hint, "green").unwrap();
    service.remove(&target, &hint, "blue").unwrap();
    assert_eq!(service.tags(&target, &hint).unwrap(), tags(&["green"]));
}

fn tag_capabilities(tags: bool, xattrs: bool) -> CapabilityMatrix {
    CapabilityMatrix::new(|kind| {
        let supported = match kind {
            CapabilityKind::Tags => tags,
            CapabilityKind::ExtendedAttributes => xattrs,
            _ => false,
        };
        if supported {
            CapabilityState::Supported
        } else {
            CapabilityState::Unsupported(
                CapabilityReason::new("test provider does not support this capability").unwrap(),
            )
        }
    })
}

#[test]
fn production_tag_service_routes_by_live_capability_and_user_opt_in() {
    let temporary = tempfile::tempdir().unwrap();
    let xattr_path = temporary.path().join("xattr");
    let fallback_path = temporary.path().join("fallback");
    std::fs::write(&xattr_path, b"xattr").unwrap();
    std::fs::write(&fallback_path, b"fallback").unwrap();
    let local = item("local", b"xattr");
    let fallback = item("local", b"fallback");
    let remote = item("remote", b"opaque");
    let mut catalog = TagCatalog::default();
    let mut service = TagService::new(&mut catalog, true);

    assert_eq!(
        service
            .assign(
                &local,
                &StorePath::from_unix_path(xattr_path.clone()),
                &tag_capabilities(true, true),
                "native",
            )
            .unwrap(),
        TagStorage::ExtendedAttribute
    );
    assert!(
        xattr::get(&xattr_path, "user.musheen.tags")
            .unwrap()
            .is_some()
    );

    service.set_xattr_opt_in(false);
    assert_eq!(
        service
            .assign(
                &fallback,
                &StorePath::from_unix_path(fallback_path.clone()),
                &tag_capabilities(true, true),
                "private",
            )
            .unwrap(),
        TagStorage::AppCatalog
    );
    assert!(
        xattr::get(&fallback_path, "user.musheen.tags")
            .unwrap()
            .is_none()
    );

    service.set_xattr_opt_in(true);
    let remote_path = StorePath::from_provider_key(provider("remote"), b"opaque".to_vec()).unwrap();
    assert_eq!(
        service
            .assign(
                &remote,
                &remote_path,
                &tag_capabilities(true, false),
                "remote",
            )
            .unwrap(),
        TagStorage::AppCatalog
    );
    assert_eq!(service.tags_for(&remote), tags(&["remote"]));
}

#[test]
fn one_tag_service_contract_covers_app_owned_fallback_and_opt_in_xattrs() {
    tag_service_contract(
        TagCatalog::default(),
        item("fallback", b"stable"),
        path(b"/fallback/target"),
    );

    let temporary = tempfile::tempdir().unwrap();
    let xattr_path = temporary.path().join("target");
    std::fs::write(&xattr_path, b"content").unwrap();
    tag_service_contract(
        XattrTagBackend::new(true, true),
        item("local", b"xattr-stable"),
        StorePath::from_unix_path(xattr_path.into_os_string()),
    );

    let mut disabled = BackendTagService::new(XattrTagBackend::new(true, false));
    assert!(matches!(
        disabled.assign(&item("local", b"disabled"), &path(b"/disabled"), "blue"),
        Err(musheen_desktop::TagServiceError::Backend(
            XattrTagError::MetadataDisabled
        ))
    ));
}

#[test]
fn completed_renames_and_moves_update_tags_through_capability_aware_operation_hooks() {
    let source = item("local", b"source");
    let destination = item("remote", b"destination");
    let destination_path =
        StorePath::from_provider_key(provider("remote"), b"destination".to_vec()).unwrap();
    let mut catalog = CatalogDocument::default();
    catalog
        .tags_mut()
        .assign(&source, path(b"/source"), "keep")
        .unwrap();

    let renamed_path = path(b"/renamed");
    assert_eq!(
        catalog.note_completed_rename(
            &source,
            renamed_path.clone(),
            &tag_capabilities(true, false),
        ),
        TagMoveOutcome::Preserved
    );
    assert_eq!(catalog.tags().path_hint(&source), Some(&renamed_path));

    assert_eq!(
        catalog.note_completed_move(
            &source,
            destination.clone(),
            destination_path.clone(),
            &tag_capabilities(true, false),
        ),
        TagMoveOutcome::Preserved
    );
    assert_eq!(catalog.tags().tags_for(&destination), tags(&["keep"]));
    assert_eq!(
        catalog.tags().path_hint(&destination),
        Some(&destination_path)
    );
}

#[test]
fn pins_preserve_label_order_and_unavailable_targets_do_not_block_siblings() {
    let local = item("local", b"directory-a");
    let remote = item("smb-account", b"share-b");
    let available = item("local", b"directory-c");
    let mut pins = PinCatalog::default();

    pins.pin(local.clone(), path(b"/mnt/removable/a"), "First")
        .unwrap();
    pins.pin(
        remote.clone(),
        StorePath::from_provider_key(provider("smb-account"), b"share-b".to_vec()).unwrap(),
        "Remote",
    )
    .unwrap();
    pins.pin(available.clone(), path(b"/srv/c"), "Last")
        .unwrap();
    assert_eq!(
        pins.pin(local.clone(), path(b"/elsewhere"), "Duplicate"),
        Err(PinError::Duplicate)
    );

    pins.mark_unavailable(&local, "the removable volume is absent");
    pins.mark_unavailable(&remote, "the remote account is offline");

    let entries = pins.entries();
    assert_eq!(
        entries.iter().map(|pin| pin.label()).collect::<Vec<_>>(),
        ["First", "Remote", "Last"]
    );
    assert!(matches!(entries[0].state(), PinState::Unavailable(_)));
    assert!(matches!(entries[1].state(), PinState::Unavailable(_)));
    assert_eq!(entries[2].state(), &PinState::Available);
}

#[test]
fn home_composes_owning_models_and_mutates_them_in_place() {
    let pinned = item("local", b"pinned");
    let tagged = item("local", b"tagged");
    let recent_id = location("local", b"recent");
    let mut pins = PinCatalog::default();
    pins.pin(pinned.clone(), path(b"/pinned"), "Pinned")
        .unwrap();
    pins.mark_unavailable(&pinned, "volume removed");
    let mut tag_catalog = TagCatalog::default();
    tag_catalog
        .assign(&tagged, path(b"/tagged"), "work")
        .unwrap();
    let mut recents = RecentLocations::default();
    recents.record(recent_id, path(b"/recent"), "Recent");
    let mounts = [MountShortcut::new(
        location("local", b"mount"),
        path(b"/mnt/usb"),
        "USB",
    )];

    {
        let mut home = HomeModel::new(&mut recents, &mut pins, &mut tag_catalog, &mounts);
        let sections = home.sections();
        assert_eq!(
            sections
                .iter()
                .flat_map(|section| section.items())
                .map(|item| item.kind())
                .collect::<Vec<_>>(),
            [
                HomeItemKind::Recent,
                HomeItemKind::Pin,
                HomeItemKind::Mount,
                HomeItemKind::Tag,
            ]
        );
        let pin = sections
            .iter()
            .find(|section| section.kind() == HomeItemKind::Pin)
            .unwrap()
            .items()
            .first()
            .unwrap();
        assert_eq!(pin.identity(), Some(&pinned));
        assert_eq!(pin.unavailable_reason(), Some("volume removed"));
        assert!(home.unpin(&pinned));
        assert_eq!(home.rename_tag("work", "office").unwrap(), 1);
        home.clear_recent_locations();
    }

    assert!(pins.entries().is_empty());
    assert!(recents.entries().is_empty());
    assert_eq!(tag_catalog.tags_for(&tagged), tags(&["office"]));
}

#[test]
fn disabling_or_clearing_recents_does_not_touch_other_catalog_models() {
    let pinned = item("local", b"pin");
    let tagged = item("local", b"tag");
    let mut document = CatalogDocument::default();
    document
        .pins_mut()
        .pin(pinned.clone(), path(b"/pin"), "Pin")
        .unwrap();
    document
        .tags_mut()
        .assign(&tagged, path(b"/tag"), "keep")
        .unwrap();
    document
        .recents_mut()
        .record(location("local", b"before"), path(b"/before"), "Before");

    document.recents_mut().set_recording_enabled(false);
    document.recents_mut().clear();
    document
        .recents_mut()
        .record(location("local", b"after"), path(b"/after"), "After");

    assert!(document.recents().entries().is_empty());
    assert_eq!(document.pins().entries().len(), 1);
    assert_eq!(document.tags().tags_for(&tagged), tags(&["keep"]));
}

#[test]
fn folder_preferences_inherit_by_lossless_identity_without_touching_paths() {
    let volume = location("local", b"uuid:USB\0root");
    let folder = location("local", b"uuid:USB\0folder\xff");
    let inaccessible = location("smb-account", b"offline-share/private");
    let parent_preferences = FolderPreference::new(
        FolderView::Details,
        FolderSortKey::Modified,
        FolderSortDirection::Descending,
    )
    .with_icon_size(72);
    let child_preferences = FolderPreference::new(
        FolderView::Grid,
        FolderSortKey::Name,
        FolderSortDirection::Ascending,
    );
    let mut catalog = FolderPreferenceCatalog::default();

    catalog.set(
        volume.clone(),
        path(b"/run/media/user/USB"),
        None,
        parent_preferences.clone(),
    );
    catalog.remember_location(
        folder.clone(),
        path(b"/run/media/user/USB/folder\xff"),
        Some(volume.clone()),
    );
    catalog.remember_location(
        inaccessible.clone(),
        StorePath::from_provider_key(provider("smb-account"), b"offline-share/private".to_vec())
            .unwrap(),
        Some(volume.clone()),
    );

    assert_eq!(catalog.resolve(&folder), &parent_preferences);
    assert_eq!(catalog.resolve(&inaccessible), &parent_preferences);
    assert_eq!(catalog.resolve(&folder).icon_size(), 72);
    assert_eq!(
        catalog.identity_for_path(
            &StorePath::from_provider_key(
                provider("smb-account"),
                b"offline-share/private".to_vec(),
            )
            .unwrap(),
        ),
        Some(&inaccessible)
    );
    assert!(
        catalog
            .resolve_recorded(&location("local", b"unseen"))
            .is_none()
    );
    catalog.set(
        folder.clone(),
        path(b"/run/media/user/USB/folder\xff"),
        Some(volume),
        child_preferences.clone(),
    );
    assert_eq!(catalog.resolve(&folder), &child_preferences);
}

#[test]
fn folder_identity_reuses_the_provider_item_identity_resolved_by_operations() {
    let resolved = item("local", b"device:inode");
    let identity = FolderIdentity::from_item(resolved.clone());

    assert_eq!(identity.as_item(), &resolved);
    assert_eq!(identity.provider(), resolved.provider());
}

#[test]
fn catalog_uses_xdg_data_and_recovers_last_known_good_after_interrupted_write() {
    let root = tempfile::tempdir().unwrap();
    let store = CatalogStore::from_data_home(root.path());
    assert_eq!(store.path(), root.path().join("musheen/catalog.json"));

    let mut first = CatalogDocument::default();
    first
        .pins_mut()
        .pin(item("local", b"first"), path(b"/first"), "First")
        .unwrap();
    store.save(&first).unwrap();

    let mut second = first.clone();
    second
        .pins_mut()
        .pin(item("local", b"second"), path(b"/second"), "Second")
        .unwrap();
    store.save(&second).unwrap();
    assert_eq!(store.load().unwrap(), second);

    std::fs::write(store.path(), b"{ interrupted").unwrap();
    std::fs::write(
        store
            .path()
            .parent()
            .unwrap()
            .join(".catalog.json.tmp.interrupted"),
        b"partial",
    )
    .unwrap();
    assert_eq!(store.load().unwrap(), first);
    assert_eq!(store.load().unwrap(), first);
    assert!(Path::new(store.path()).exists());
}
