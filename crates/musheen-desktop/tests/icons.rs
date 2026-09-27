use musheen_desktop::{
    ApplicationIcon, ApplicationIconProvider, DesktopEntryCatalog, DesktopPaths,
    FreedesktopIconProvider,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

struct RecordingIconProvider {
    requests: Mutex<Vec<ApplicationIcon>>,
    resolved: PathBuf,
}

impl ApplicationIconProvider for RecordingIconProvider {
    fn resolve(&self, icon: &ApplicationIcon) -> Option<PathBuf> {
        self.requests.lock().unwrap().push(icon.clone());
        Some(self.resolved.clone())
    }
}

fn desktop(path: &Path, name: &str, icon: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\nExec=/usr/bin/true %F\nIcon={icon}\nMimeType=text/plain;\n"
        ),
    )
    .unwrap();
}

#[test]
fn catalog_keeps_named_and_absolute_icons_behind_the_provider_contract() {
    let temporary = tempfile::tempdir().unwrap();
    let data_home = temporary.path().join("data");
    let icon_path = temporary.path().join("writer.svg");
    fs::write(&icon_path, "<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
    desktop(
        &data_home.join("applications/named.desktop"),
        "Named",
        "document-edit",
    );
    desktop(
        &data_home.join("applications/absolute.desktop"),
        "Absolute",
        icon_path.to_str().unwrap(),
    );
    let catalog = DesktopEntryCatalog::new(DesktopPaths::new(
        temporary.path().join("config"),
        &data_home,
    ));

    let named = catalog.load("named.desktop").unwrap().unwrap();
    assert_eq!(named.icon(), Some(&ApplicationIcon::name("document-edit")));
    let absolute = catalog.load("absolute.desktop").unwrap().unwrap();
    assert_eq!(absolute.icon(), Some(&ApplicationIcon::path(&icon_path)));

    let provider = RecordingIconProvider {
        requests: Mutex::new(Vec::new()),
        resolved: icon_path.clone(),
    };
    assert_eq!(named.resolve_icon(&provider), Some(icon_path.clone()));
    assert_eq!(
        provider.requests.lock().unwrap().as_slice(),
        &[ApplicationIcon::name("document-edit")]
    );
    assert_eq!(
        absolute.resolve_icon(&FreedesktopIconProvider),
        Some(icon_path)
    );
}
