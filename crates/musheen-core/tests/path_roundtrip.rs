#[cfg(unix)]
use musheen_core::{DisplayPath, ItemId, ProviderId, StorePath};

#[cfg(unix)]
#[test]
fn unix_store_path_round_trips_non_utf8_bytes() {
    let raw = b"bad-\xff-name";

    let path = StorePath::from_unix_bytes(raw.to_vec());

    assert_eq!(path.unix_bytes(), Some(raw.as_slice()));
}

#[cfg(unix)]
#[test]
fn display_path_is_a_separate_lossy_view() {
    let path = StorePath::from_unix_bytes(b"bad-\xff-name".to_vec());

    let display = DisplayPath::from_store_path(&path);

    assert_eq!(display.as_str(), "bad-�-name");
    assert_eq!(path.unix_bytes(), Some(b"bad-\xff-name".as_slice()));
}

#[cfg(unix)]
#[test]
fn provider_paths_preserve_opaque_keys() {
    let provider = ProviderId::new("archive.reader").expect("the provider ID is valid");
    let path = StorePath::from_provider_key(provider.clone(), b"entry\0key".to_vec())
        .expect("the provider key is valid");

    assert_eq!(
        path.provider_key(),
        Some((&provider, b"entry\0key".as_slice()))
    );
    assert_eq!(path.unix_bytes(), None);
}

#[cfg(unix)]
#[test]
fn item_ids_compare_as_opaque_provider_scoped_values() {
    let provider = ProviderId::new("local").expect("the provider ID is valid");
    let first =
        ItemId::new(provider.clone(), b"device:inode".to_vec()).expect("the item ID is valid");
    let same =
        ItemId::new(provider.clone(), b"device:inode".to_vec()).expect("the item ID is valid");
    let different = ItemId::new(provider, b"other:inode".to_vec()).expect("the item ID is valid");

    assert_eq!(first, same);
    assert_ne!(first, different);
    assert!(!format!("{first:?}").contains("device:inode"));
}

#[cfg(unix)]
#[test]
fn malformed_provider_and_item_ids_are_rejected() {
    assert!(ProviderId::new("Upper Case").is_err());

    let provider = ProviderId::new("local").expect("the provider ID is valid");
    assert!(ItemId::new(provider, Vec::new()).is_err());
}
