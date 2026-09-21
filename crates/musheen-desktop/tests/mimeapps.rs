use musheen_desktop::{DesktopEntryCatalog, DesktopPaths, MimeAppsResolver};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn desktop(path: &Path, name: &str, mime_types: &[&str], extra: &str) {
    let mime_types = mime_types.join(";");
    write(
        path,
        &format!(
            "[Desktop Entry]\nType=Application\nName={name}\nExec=/usr/bin/true %F\nMimeType={mime_types};\n{extra}"
        ),
    );
}

struct Fixture {
    _temporary: tempfile::TempDir,
    config_home: PathBuf,
    config_dir: PathBuf,
    data_home: PathBuf,
    data_dir: PathBuf,
    bin_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let fixture = Self {
            config_home: root.join("config-home"),
            config_dir: root.join("config-dir"),
            data_home: root.join("data-home"),
            data_dir: root.join("data-dir"),
            bin_dir: root.join("bin"),
            _temporary: temporary,
        };
        for directory in [
            &fixture.config_home,
            &fixture.config_dir,
            &fixture.data_home,
            &fixture.data_dir,
            &fixture.bin_dir,
        ] {
            fs::create_dir_all(directory).unwrap();
        }
        fixture
    }

    fn paths(&self) -> DesktopPaths {
        DesktopPaths::new(&self.config_home, &self.data_home)
            .with_config_dirs([self.config_dir.clone()])
            .with_data_dirs([self.data_dir.clone()])
            .with_current_desktops("GNOME:Unity")
            .with_executable_dirs([self.bin_dir.clone(), PathBuf::from("/usr/bin")])
    }

    fn user_app(&self, id: &str) -> PathBuf {
        self.data_home.join("applications").join(id)
    }

    fn system_app(&self, id: &str) -> PathBuf {
        self.data_dir.join("applications").join(id)
    }
}

#[test]
fn default_resolution_follows_all_eight_precedence_tiers() {
    let fixture = Fixture::new();
    let locations = [
        fixture.config_home.join("gnome-mimeapps.list"),
        fixture.config_home.join("mimeapps.list"),
        fixture.config_dir.join("gnome-mimeapps.list"),
        fixture.config_dir.join("mimeapps.list"),
        fixture.data_home.join("applications/gnome-mimeapps.list"),
        fixture.data_home.join("applications/mimeapps.list"),
        fixture.data_dir.join("applications/gnome-mimeapps.list"),
        fixture.data_dir.join("applications/mimeapps.list"),
    ];
    let mime_types = (0..locations.len())
        .map(|index| format!("application/x-musheen-tier-{index}"))
        .collect::<Vec<_>>();

    for (index, location) in locations.iter().enumerate() {
        let mut values = String::from("[Default Applications]\n");
        for mime_type in mime_types.iter().take(index + 1) {
            values.push_str(&format!("{mime_type}=tier-{index}.desktop;\n"));
        }
        write(location, &values);
        let mime_refs = mime_types.iter().map(String::as_str).collect::<Vec<_>>();
        desktop(
            &fixture.user_app(&format!("tier-{index}.desktop")),
            &format!("Tier {index}"),
            &mime_refs,
            "",
        );
    }

    let paths = fixture.paths();
    let catalog = DesktopEntryCatalog::new(paths.clone());
    let resolver = MimeAppsResolver::new(paths);

    for (index, mime_type) in mime_types.iter().enumerate() {
        let resolved = resolver.default_for(mime_type, &catalog).unwrap().unwrap();
        assert_eq!(resolved.desktop_id(), format!("tier-{index}.desktop"));
    }
}

#[test]
fn every_xdg_data_directory_contributes_entries_and_associations_in_order() {
    let fixture = Fixture::new();
    let second_data_dir = fixture._temporary.path().join("second-data-dir");
    desktop(
        &second_data_dir.join("applications/second.desktop"),
        "Second data directory",
        &["text/plain"],
        "",
    );
    write(
        &second_data_dir.join("applications/mimeapps.list"),
        "[Default Applications]\ntext/plain=second.desktop;\n",
    );
    let paths = DesktopPaths::new(&fixture.config_home, &fixture.data_home)
        .with_data_dirs([fixture.data_dir.clone(), second_data_dir])
        .with_current_desktops("GNOME")
        .with_executable_dirs([PathBuf::from("/usr/bin")]);
    let catalog = DesktopEntryCatalog::new(paths.clone());
    let application = MimeAppsResolver::new(paths)
        .default_for("text/plain", &catalog)
        .unwrap()
        .unwrap();
    assert_eq!(application.desktop_id(), "second.desktop");
}

#[test]
fn additions_removals_entry_precedence_and_visibility_follow_the_spec() {
    let fixture = Fixture::new();
    write(
        &fixture.config_home.join("mimeapps.list"),
        "[Added Associations]\ntext/plain=added.desktop;../escape.desktop;\n\
         [Removed Associations]\ntext/plain=removed.desktop;system.desktop;\n",
    );
    write(
        &fixture.data_dir.join("applications/mimeapps.list"),
        "[Removed Associations]\ntext/plain=overridden.desktop;\n",
    );
    write(
        &fixture.data_home.join("applications/mimeapps.list"),
        "[Added Associations]\ntext/plain=data-added.desktop;\n",
    );

    desktop(
        &fixture.user_app("declared.desktop"),
        "Declared",
        &["text/plain"],
        "",
    );
    desktop(&fixture.user_app("added.desktop"), "Added", &[], "");
    desktop(
        &fixture.user_app("data-added.desktop"),
        "Data-level Added",
        &[],
        "",
    );
    desktop(
        &fixture.user_app("removed.desktop"),
        "Removed",
        &["text/plain"],
        "",
    );
    desktop(
        &fixture.user_app("only-gnome.desktop"),
        "Only GNOME",
        &["text/plain"],
        "OnlyShowIn=GNOME;\n",
    );
    desktop(
        &fixture.user_app("only-kde.desktop"),
        "Only KDE",
        &["text/plain"],
        "OnlyShowIn=KDE;\n",
    );
    desktop(
        &fixture.user_app("wrong-case-gnome.desktop"),
        "Wrong-case GNOME",
        &["text/plain"],
        "OnlyShowIn=gnome;\n",
    );
    desktop(
        &fixture.user_app("not-gnome.desktop"),
        "Not GNOME",
        &["text/plain"],
        "NotShowIn=GNOME;\n",
    );
    desktop(
        &fixture.user_app("no-display.desktop"),
        "No Display",
        &["text/plain"],
        "NoDisplay=true\n",
    );
    desktop(
        &fixture.user_app("overridden.desktop"),
        "User Override",
        &["text/plain"],
        "",
    );
    desktop(
        &fixture.system_app("overridden.desktop"),
        "System Override",
        &["text/plain"],
        "",
    );
    desktop(
        &fixture.system_app("system.desktop"),
        "System",
        &["text/plain"],
        "",
    );
    desktop(
        &fixture.system_app("masked.desktop"),
        "Masked System App",
        &["text/plain"],
        "",
    );
    desktop(
        &fixture.user_app("masked.desktop"),
        "Mask",
        &["text/plain"],
        "Hidden=true\n",
    );
    write(
        &fixture.user_app("invalid.desktop"),
        "[Desktop Entry]\nType=Link\nName=Not an application\nURL=https://example.test\n",
    );

    let paths = fixture.paths();
    let catalog = DesktopEntryCatalog::new(paths.clone());
    let resolver = MimeAppsResolver::new(paths);
    let all = resolver.associations_for("text/plain", &catalog).unwrap();
    let all_ids = all
        .iter()
        .map(|application| application.desktop_id())
        .collect::<Vec<_>>();
    assert_eq!(all_ids[0], "added.desktop");
    assert!(all_ids.contains(&"data-added.desktop"));
    assert!(all_ids.contains(&"declared.desktop"));
    assert!(all_ids.contains(&"overridden.desktop"));
    assert!(all_ids.contains(&"no-display.desktop"));
    assert!(!all_ids.contains(&"removed.desktop"));
    assert!(!all_ids.contains(&"system.desktop"));
    assert!(!all_ids.contains(&"masked.desktop"));
    assert!(!all_ids.contains(&"invalid.desktop"));

    let visible = resolver
        .visible_applications_for("text/plain", &catalog)
        .unwrap();
    let visible_ids = visible
        .iter()
        .map(|application| application.desktop_id())
        .collect::<Vec<_>>();
    assert!(visible_ids.contains(&"only-gnome.desktop"));
    assert!(!visible_ids.contains(&"only-kde.desktop"));
    assert!(!visible_ids.contains(&"wrong-case-gnome.desktop"));
    assert!(!visible_ids.contains(&"not-gnome.desktop"));
    assert!(!visible_ids.contains(&"no-display.desktop"));
}

#[test]
fn setting_a_default_is_explicit_idempotent_and_preserves_other_associations() {
    let fixture = Fixture::new();
    desktop(&fixture.user_app("writer.desktop"), "Writer", &[], "");
    write(
        &fixture.config_home.join("mimeapps.list"),
        "[Default Applications]\nx-scheme-handler/http=browser.desktop;\n\
         [Removed Associations]\ntext/plain=old.desktop;writer.desktop;\n",
    );
    let paths = fixture.paths();
    let catalog = DesktopEntryCatalog::new(paths.clone());
    let resolver = MimeAppsResolver::new(paths);

    resolver
        .set_default("text/plain", "writer.desktop", &catalog)
        .unwrap();
    resolver
        .set_default("text/plain", "writer.desktop", &catalog)
        .unwrap();

    let saved = fs::read_to_string(fixture.config_home.join("mimeapps.list")).unwrap();
    assert!(saved.contains("x-scheme-handler/http=browser.desktop;"));
    assert!(saved.contains("text/plain=old.desktop;"));
    assert!(!saved.contains("text/plain=old.desktop;writer.desktop;"));
    assert!(!saved.contains("[Removed Associations]\ntext/plain=writer.desktop;"));
    assert_eq!(saved.matches("text/plain=writer.desktop;").count(), 2);
    let resolved = resolver
        .default_for("text/plain", &catalog)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.desktop_id(), "writer.desktop");

    assert!(
        resolver
            .set_default("text/plain", "../escape.desktop", &catalog)
            .is_err()
    );
}

#[test]
fn desktop_order_and_each_xdg_config_directory_participate_in_precedence() {
    let fixture = Fixture::new();
    let second_config_dir = fixture.config_home.join("second-config-dir");
    desktop(
        &fixture.user_app("gnome.desktop"),
        "GNOME Viewer",
        &["text/plain"],
        "",
    );
    desktop(
        &fixture.user_app("unity.desktop"),
        "Unity Viewer",
        &["text/plain"],
        "",
    );
    desktop(
        &fixture.user_app("second.desktop"),
        "Second Config Viewer",
        &["image/png"],
        "",
    );
    write(
        &fixture.config_dir.join("gnome-mimeapps.list"),
        "[Default Applications]\ntext/plain=gnome.desktop;\n",
    );
    write(
        &fixture.config_dir.join("unity-mimeapps.list"),
        "[Default Applications]\ntext/plain=unity.desktop;\n",
    );
    write(
        &second_config_dir.join("mimeapps.list"),
        "[Default Applications]\nimage/png=second.desktop;\n",
    );

    for (desktops, expected) in [
        ("GNOME:Unity", "gnome.desktop"),
        ("Unity:GNOME", "unity.desktop"),
    ] {
        let paths = DesktopPaths::new(&fixture.config_home, &fixture.data_home)
            .with_config_dirs([fixture.config_dir.clone(), second_config_dir.clone()])
            .with_data_dirs([fixture.data_dir.clone()])
            .with_current_desktops(desktops)
            .with_executable_dirs([fixture.bin_dir.clone(), PathBuf::from("/usr/bin")]);
        let catalog = DesktopEntryCatalog::new(paths.clone());
        let resolver = MimeAppsResolver::new(paths);
        assert_eq!(
            resolver
                .default_for("text/plain", &catalog)
                .unwrap()
                .unwrap()
                .desktop_id(),
            expected
        );
        assert_eq!(
            resolver
                .default_for("image/png", &catalog)
                .unwrap()
                .unwrap()
                .desktop_id(),
            "second.desktop"
        );
    }
}

#[test]
fn unavailable_try_exec_entries_are_filtered_and_default_resolution_falls_through() {
    let fixture = Fixture::new();
    desktop(
        &fixture.user_app("missing.desktop"),
        "Missing Viewer",
        &["text/plain"],
        "TryExec=definitely-not-installed-musheen-fixture\n",
    );
    desktop(
        &fixture.user_app("available.desktop"),
        "Available Viewer",
        &["text/plain"],
        "",
    );
    write(
        &fixture.config_home.join("mimeapps.list"),
        "[Default Applications]\ntext/plain=missing.desktop;available.desktop;\n",
    );
    let paths = fixture.paths();
    let catalog = DesktopEntryCatalog::new(paths.clone());
    let resolver = MimeAppsResolver::new(paths);

    let applications = resolver.associations_for("text/plain", &catalog).unwrap();
    assert_eq!(
        applications
            .iter()
            .map(|application| application.desktop_id())
            .collect::<Vec<_>>(),
        ["available.desktop"]
    );
    assert_eq!(
        resolver
            .default_for("text/plain", &catalog)
            .unwrap()
            .unwrap()
            .desktop_id(),
        "available.desktop"
    );
}

#[test]
fn concurrent_default_writers_share_one_stable_lock_without_losing_associations() {
    let fixture = Fixture::new();
    desktop(&fixture.user_app("writer.desktop"), "Writer", &[], "");
    desktop(&fixture.user_app("viewer.desktop"), "Viewer", &[], "");
    let paths = fixture.paths();
    let barrier = Arc::new(Barrier::new(3));
    let workers = ["writer.desktop", "viewer.desktop"].map(|desktop_id| {
        let paths = paths.clone();
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            let catalog = DesktopEntryCatalog::new(paths.clone());
            let resolver = MimeAppsResolver::new(paths);
            barrier.wait();
            resolver
                .set_default("text/plain", desktop_id, &catalog)
                .unwrap();
        })
    });
    barrier.wait();
    for worker in workers {
        worker.join().unwrap();
    }

    let catalog = DesktopEntryCatalog::new(paths.clone());
    let applications = MimeAppsResolver::new(paths)
        .associations_for("text/plain", &catalog)
        .unwrap();
    let ids = applications
        .iter()
        .map(|application| application.desktop_id())
        .collect::<Vec<_>>();
    assert!(ids.contains(&"writer.desktop"));
    assert!(ids.contains(&"viewer.desktop"));
}
