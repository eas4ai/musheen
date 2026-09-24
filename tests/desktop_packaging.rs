use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

#[test]
fn desktop_and_appstream_metadata_use_the_runtime_application_id() {
    assert_eq!(
        musheen_desktop::MUSHEEN_FILE_MANAGER_NAME,
        musheen_ui::ApplicationIdentity::ID,
        "the private D-Bus service must use the application identity"
    );
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let desktop = fs::read_to_string(root.join("packaging/org.musheen.Musheen.desktop")).unwrap();
    let metainfo =
        fs::read_to_string(root.join("packaging/org.musheen.Musheen.metainfo.xml")).unwrap();
    let service = fs::read_to_string(root.join("packaging/org.musheen.Musheen.service")).unwrap();

    assert!(desktop.contains("Name=Musheen\n"));
    assert!(desktop.contains("Exec=musheen %f\n"));
    assert!(desktop.contains("Icon=org.musheen.Musheen\n"));
    assert!(desktop.contains("MimeType=inode/directory;\n"));
    assert!(!desktop.contains("DBusActivatable=true"));
    assert!(metainfo.contains("<id>org.musheen.Musheen</id>"));
    assert!(
        metainfo
            .contains("<launchable type=\"desktop-id\">org.musheen.Musheen.desktop</launchable>")
    );
    assert!(service.contains("Name=org.musheen.Musheen\n"));
    assert!(service.contains("Exec=/usr/bin/musheen\n"));
    assert!(root.join("assets/icons/musheen.svg").is_file());
}

#[test]
fn native_installer_stages_app_workers_metadata_icons_and_broker_without_host_writes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir().unwrap();
    let stage = temporary.path().join("stage");
    let fake_bin = temporary.path().join("bin");
    fs::create_dir(&fake_bin).unwrap();
    let app = temporary.path().join("musheen");
    let broker = temporary.path().join("musheen-broker");
    let archive_worker = temporary.path().join("musheen-archive-worker");
    let thumbnail_worker = temporary.path().join("musheen-thumbnail-worker");
    fs::write(&app, b"app fixture").unwrap();
    fs::write(&broker, b"broker fixture").unwrap();
    fs::write(&archive_worker, b"archive fixture").unwrap();
    fs::write(&thumbnail_worker, b"thumbnail fixture").unwrap();
    let rasterizer = fake_bin.join("rsvg-convert");
    fs::write(
        &rasterizer,
        "#!/bin/sh\nset -eu\nwhile [ \"$1\" != \"-o\" ]; do shift; done\nprintf 'png fixture' > \"$2\"\n",
    )
    .unwrap();
    fs::set_permissions(&rasterizer, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", fake_bin.display(), std::env::var("PATH").unwrap());

    let output = Command::new(root.join("packaging/install-app.sh"))
        .env("DESTDIR", &stage)
        .env("MUSHEEN_APP_BINARY", &app)
        .env("MUSHEEN_BROKER_BINARY", &broker)
        .env("MUSHEEN_ARCHIVE_WORKER_BINARY", &archive_worker)
        .env("MUSHEEN_THUMBNAIL_WORKER_BINARY", &thumbnail_worker)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");

    for (relative, mode) in [
        ("usr/bin/musheen", 0o755),
        ("usr/bin/musheen-archive-worker", 0o755),
        ("usr/bin/musheen-thumbnail-worker", 0o755),
        ("usr/lib/musheen/musheen-broker", 0o755),
        ("usr/share/applications/org.musheen.Musheen.desktop", 0o644),
        ("usr/share/metainfo/org.musheen.Musheen.metainfo.xml", 0o644),
        (
            "usr/share/dbus-1/services/org.musheen.Musheen.service",
            0o644,
        ),
        (
            "usr/share/icons/hicolor/scalable/apps/org.musheen.Musheen.svg",
            0o644,
        ),
        (
            "usr/share/icons/hicolor/48x48/apps/org.musheen.Musheen.png",
            0o644,
        ),
        (
            "usr/share/icons/hicolor/128x128/apps/org.musheen.Musheen.png",
            0o644,
        ),
        (
            "usr/share/icons/hicolor/256x256/apps/org.musheen.Musheen.png",
            0o644,
        ),
        (
            "usr/share/polkit-1/actions/org.musheen.Musheen.policy",
            0o644,
        ),
    ] {
        let metadata = fs::metadata(stage.join(relative))
            .unwrap_or_else(|error| panic!("missing staged {relative}: {error}"));
        assert_eq!(metadata.permissions().mode() & 0o777, mode, "{relative}");
    }
    assert_eq!(
        fs::read(stage.join("usr/bin/musheen-archive-worker")).unwrap(),
        b"archive fixture"
    );
    assert_eq!(
        fs::read(stage.join("usr/bin/musheen-thumbnail-worker")).unwrap(),
        b"thumbnail fixture"
    );
    assert!(
        !stage
            .join("usr/share/dbus-1/services/org.freedesktop.FileManager1.service")
            .exists(),
        "the package must not replace another file manager's D-Bus activation"
    );
}

#[test]
fn native_installer_rejects_a_symlinked_destination_before_writing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir().unwrap();
    let destination = temporary.path().join("real-stage");
    let link = temporary.path().join("stage-link");
    fs::create_dir(&destination).unwrap();
    symlink(&destination, &link).unwrap();
    let app = temporary.path().join("musheen");
    fs::write(&app, b"app fixture").unwrap();

    let output = Command::new(root.join("packaging/install-app.sh"))
        .env("DESTDIR", &link)
        .env("MUSHEEN_APP_BINARY", &app)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("symlink"));
    assert!(!destination.join("usr/bin/musheen").exists());
}

#[test]
fn arch_package_builds_all_features_and_stages_the_native_installer() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let recipe = fs::read_to_string(root.join("packaging/arch/PKGBUILD")).unwrap();
    assert!(recipe.contains("pkgname=musheen"));
    assert!(recipe.contains("--release --locked --all-features --jobs 8"));
    assert!(recipe.contains("CARGO_INCREMENTAL=0"));
    assert!(recipe.contains("DESTDIR=\"$pkgdir\" ./packaging/install-app.sh"));
    assert!(recipe.contains("MUSHEEN_ARCHIVE_WORKER_BINARY="));
    assert!(recipe.contains("MUSHEEN_THUMBNAIL_WORKER_BINARY="));
    assert!(recipe.contains("sha256sums=('__SOURCE_SHA256__')"));
    assert!(recipe.contains("options=('!debug' '!lto')"));
    assert!(recipe.contains("'git'"));
    assert!(recipe.contains("'jq'"));
    assert!(recipe.contains("'dbus'"));
    assert!(recipe.contains("'python'"));
    assert!(recipe.contains("'hicolor-icon-theme'"));
    assert!(recipe.contains("'acl'"));
}

#[test]
fn arch_container_checks_package_install_and_removal() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let container = fs::read_to_string(root.join("ci/arch-package.Dockerfile")).unwrap();
    assert!(container.contains("FROM archlinux:base-devel"));
    assert!(container.contains(" git "));
    assert!(container.contains(" jq "));
    assert!(container.contains(" dbus "));
    assert!(container.contains(" python "));
    assert!(container.contains(" hicolor-icon-theme "));
    assert!(container.contains("makepkg --noconfirm"));
    assert!(container.contains("pacman -U --noconfirm"));
    assert!(container.contains("FROM archlinux:base-devel AS runtime-check"));
    assert!(container.contains("test -x /usr/bin/musheen-archive-worker"));
    assert!(container.contains("test -x /usr/bin/musheen-thumbnail-worker"));
    assert!(container.contains("pacman -Rns --noconfirm musheen"));
    assert!(container.contains("FROM scratch AS artifact"));
    let runner = fs::read_to_string(root.join("scripts/build-arch-package.sh")).unwrap();
    assert!(runner.contains("MUSHEEN_SCRATCH_BASE"));
    assert!(runner.contains("flock -n 9"));
    assert!(runner.contains("--output"));
}
