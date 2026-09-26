use musheen_core::{CancellationToken, PageRequest, ResourceLimits, Store, StorePath};
use musheen_desktop::{Clock, PrivilegeProvider, RootGrant, RootedStore};
use musheen_ui::{AppearanceMode, Catalog, ElevatedBrowser, Locale, RootedFilesystemStore};
use std::fs;
use std::path::Path;

#[derive(Clone)]
struct ClockAt(u64);

impl Clock for ClockAt {
    fn now_unix_millis(&self) -> u64 {
        self.0
    }
}

fn browser(root: &Path, locale: Locale) -> ElevatedBrowser<ClockAt> {
    let grant = RootGrant::open(root, "grant", 1_000, PrivilegeProvider::Polkit).unwrap();
    ElevatedBrowser::new(
        RootedStore::new(grant, ClockAt(100)),
        Catalog::load(locale).unwrap(),
    )
}

#[test]
fn warning_and_privilege_icon_are_permanent_localized_and_theme_independent() {
    let root = tempfile::tempdir().unwrap();
    for locale in [Locale::EnUs, Locale::EnXa, Locale::Ar] {
        let browser = browser(root.path(), locale);
        let expected = Catalog::load(locale)
            .unwrap()
            .message("elevated-browser-warning")
            .unwrap()
            .to_owned();
        for mode in [
            AppearanceMode::Light,
            AppearanceMode::Dark,
            AppearanceMode::HighContrast,
        ] {
            let chrome = browser.chrome(mode);
            assert_eq!(chrome.warning(), expected);
            assert_eq!(chrome.icon(), "shield");
            assert!(chrome.always_visible());
            assert!(chrome.distinct_from_ordinary_chrome());
            assert!(chrome.accessible_name().contains(&expected));
        }
    }
}

#[test]
fn navigation_and_breadcrumbs_remain_inside_the_granted_root() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("one/two")).unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("one/escape")).unwrap();
    let mut browser = browser(root.path(), Locale::EnUs);

    browser.navigate(Path::new("one/two")).unwrap();
    assert_eq!(browser.relative_location(), Path::new("one/two"));
    browser.navigate_breadcrumb(0).unwrap();
    assert_eq!(browser.relative_location(), Path::new("one"));
    assert!(browser.navigate(Path::new("../outside")).is_err());
    assert!(browser.navigate(Path::new("one/escape")).is_err());
    assert_eq!(browser.relative_location(), Path::new("one"));
}

#[test]
fn elevated_store_enumerates_through_the_granted_descriptor_after_root_path_replacement() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("protected");
    let moved = temporary.path().join("original");
    fs::create_dir(&root).unwrap();
    fs::write(root.join("original.txt"), b"original").unwrap();
    let grant = RootGrant::open(&root, "grant", 1_000, PrivilegeProvider::Polkit).unwrap();
    let store = RootedFilesystemStore::new(RootedStore::new(grant, ClockAt(100)));

    fs::rename(&root, &moved).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("replacement.txt"), b"replacement").unwrap();
    let page = futures_lite::future::block_on(store.read_directory(
        &StorePath::from_unix_path(root.as_os_str()),
        PageRequest::first(&ResourceLimits::default()),
        CancellationToken::new(),
    ))
    .unwrap();

    assert_eq!(page.items().len(), 1);
    assert_eq!(page.items()[0].display_name().as_str(), "original.txt");
    assert!(
        page.items()
            .iter()
            .all(|item| item.display_name().as_str() != "replacement.txt")
    );
}
