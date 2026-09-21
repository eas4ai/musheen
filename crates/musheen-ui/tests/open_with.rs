use gpui_kit::component::Root;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext, px, size};
use musheen_desktop::{
    DesktopEntryCatalog, DesktopEntryLauncher, DesktopPaths, LaunchTarget, MimeAppsResolver,
    PreparedLaunch, ProcessRunner,
};
use musheen_ui::{
    ApplicationChoice, Catalog, Locale, OpenWithDialog, OpenWithIntent, OpenWithModel,
};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Default)]
struct RecordingRunner(Mutex<Vec<PreparedLaunch>>);

impl ProcessRunner for RecordingRunner {
    fn spawn(&self, launch: &PreparedLaunch) -> std::io::Result<()> {
        self.0.lock().unwrap().push(launch.clone());
        Ok(())
    }
}

#[test]
fn open_once_never_persists_and_make_default_is_explicit() {
    let temporary = tempfile::tempdir().unwrap();
    let config_home = temporary.path().join("config");
    let data_home = temporary.path().join("data");
    let application_path = data_home.join("applications/writer.desktop");
    fs::create_dir_all(application_path.parent().unwrap()).unwrap();
    fs::write(
        &application_path,
        "[Desktop Entry]\nType=Application\nName=Writer\nExec=/usr/bin/writer %F\nMimeType=text/plain;\n",
    )
    .unwrap();
    let paths = DesktopPaths::new(&config_home, &data_home)
        .with_current_desktops("GNOME")
        .with_executable_dirs([PathBuf::from("/usr/bin")]);
    let catalog = DesktopEntryCatalog::new(paths.clone());
    let resolver = MimeAppsResolver::new(paths.clone());
    let launcher = DesktopEntryLauncher::new(paths.executable_dirs().to_vec());
    let runner = RecordingRunner::default();
    let targets = [LaunchTarget::local("/tmp/report.txt")];

    let mut model = OpenWithModel::from_resolver("text/plain", &resolver, &catalog).unwrap();
    model.select("writer.desktop").unwrap();
    model
        .plan(OpenWithIntent::OpenOnce)
        .unwrap()
        .execute(&resolver, &catalog, &launcher, &runner, &targets, None)
        .unwrap();
    assert!(!config_home.join("mimeapps.list").exists());

    model
        .plan(OpenWithIntent::SetAsDefault)
        .unwrap()
        .execute(&resolver, &catalog, &launcher, &runner, &targets, None)
        .unwrap();
    let saved = fs::read_to_string(config_home.join("mimeapps.list")).unwrap();
    assert!(saved.contains("text/plain=writer.desktop;"));
    assert_eq!(runner.0.lock().unwrap().len(), 2);
}

#[gpui_kit::test]
async fn chooser_requires_a_selection_and_returns_the_explicit_intent(cx: &mut TestAppContext) {
    cx.update(gpui_kit::init);
    let model = OpenWithModel::new(
        "text/plain",
        vec![ApplicationChoice::new("writer.desktop", "Writer", true)],
    );
    let mut dialog = None;
    let handle = cx.open_window(size(px(560.), px(480.)), |window, cx| {
        let catalog = Catalog::load(Locale::EnUs).unwrap();
        let view = cx.new(|cx| OpenWithDialog::new(model, catalog, cx));
        dialog = Some(view.clone());
        Root::new(view, window, cx)
    });
    let dialog = dialog.expect("the chooser is constructed");

    cx.update_window(handle.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.find("open-with-dialog").visible());
        window.click("open-with-set-default", cx);
        assert!(dialog.read(cx).decision().is_none());
        window.click("open-with-application-writer.desktop", cx);
        window.click("open-with-set-default", cx);
    })
    .expect("the chooser accepts a selection");

    assert_eq!(
        cx.update(|cx| {
            dialog
                .read(cx)
                .decision()
                .map(|(desktop_id, intent)| (desktop_id.to_owned(), intent))
        }),
        Some(("writer.desktop".to_owned(), OpenWithIntent::SetAsDefault))
    );
}
