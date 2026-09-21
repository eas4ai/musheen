#![cfg(unix)]

use musheen_desktop::{
    DesktopEntryCatalog, DesktopEntryLauncher, DesktopPaths, LaunchError, LaunchTarget,
    PreparedLaunch, ProcessRunner, TerminalCommand,
};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

struct Fixture {
    _temporary: tempfile::TempDir,
    data_home: PathBuf,
    bin_dir: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().unwrap();
        let data_home = temporary.path().join("data");
        let bin_dir = temporary.path().join("bin");
        fs::create_dir_all(data_home.join("applications")).unwrap();
        fs::create_dir_all(&bin_dir).unwrap();
        Self {
            _temporary: temporary,
            data_home,
            bin_dir,
        }
    }

    fn paths(&self) -> DesktopPaths {
        DesktopPaths::new(self.data_home.join("config"), &self.data_home)
            .with_current_desktops("GNOME")
            .with_executable_dirs([self.bin_dir.clone(), PathBuf::from("/usr/bin")])
    }

    fn application(
        &self,
        id: &str,
        exec: &str,
        extra: &str,
    ) -> musheen_desktop::DesktopApplication {
        let path = self.data_home.join("applications").join(id);
        write(
            &path,
            &format!(
                "[Desktop Entry]\nType=Application\nName=Fixture Viewer\nIcon=fixture-viewer\nExec={exec}\nMimeType=text/plain;\n{extra}"
            ),
        );
        DesktopEntryCatalog::new(self.paths())
            .load(id)
            .unwrap()
            .unwrap()
    }

    fn executable(&self, name: &str) -> PathBuf {
        let path = self.bin_dir.join(name);
        write(&path, "#!/bin/sh\nexit 0\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
}

#[derive(Default)]
struct RecordingRunner {
    launches: Mutex<Vec<PreparedLaunch>>,
}

impl ProcessRunner for RecordingRunner {
    fn spawn(&self, launch: &PreparedLaunch) -> std::io::Result<()> {
        self.launches.lock().unwrap().push(launch.clone());
        Ok(())
    }
}

fn arguments(launch: &PreparedLaunch) -> Vec<Vec<u8>> {
    launch
        .arguments()
        .iter()
        .map(|value| value.as_bytes().to_vec())
        .collect()
}

#[test]
fn file_placeholders_preserve_argument_boundaries_without_a_shell() {
    let fixture = Fixture::new();
    let application = fixture.application(
        "viewer.desktop",
        "/usr/bin/viewer --label \"two words\" -- %F",
        "",
    );
    let targets = [
        LaunchTarget::local("/tmp/report;touch PWNED"),
        LaunchTarget::local("/tmp/$(touch PWNED)"),
    ];
    let launcher = DesktopEntryLauncher::new(fixture.paths().executable_dirs().to_vec());

    let prepared = launcher.prepare(&application, &targets, None).unwrap();
    assert_eq!(prepared.launches().len(), 1);
    let launch = &prepared.launches()[0];
    assert_eq!(launch.program(), OsStr::new("/usr/bin/viewer"));
    assert_eq!(
        arguments(launch),
        vec![
            b"--label".to_vec(),
            b"two words".to_vec(),
            b"--".to_vec(),
            b"/tmp/report;touch PWNED".to_vec(),
            b"/tmp/$(touch PWNED)".to_vec(),
        ]
    );

    let runner = RecordingRunner::default();
    launcher.launch(&prepared, &runner).unwrap();
    assert_eq!(
        runner.launches.lock().unwrap().as_slice(),
        prepared.launches()
    );
}

#[test]
fn singular_and_plural_file_and_uri_codes_expand_exactly() {
    let fixture = Fixture::new();
    let launcher = DesktopEntryLauncher::new(fixture.paths().executable_dirs().to_vec());
    let local = [
        LaunchTarget::local("/tmp/one name.txt"),
        LaunchTarget::local("/tmp/two.txt"),
    ];

    let singular_files = fixture.application("single-file.desktop", "/bin/view %f", "");
    let prepared = launcher.prepare(&singular_files, &local, None).unwrap();
    assert_eq!(prepared.launches().len(), 2);
    assert_eq!(
        arguments(&prepared.launches()[0]),
        vec![b"/tmp/one name.txt".to_vec()]
    );
    assert_eq!(
        arguments(&prepared.launches()[1]),
        vec![b"/tmp/two.txt".to_vec()]
    );

    let plural_files = fixture.application("files.desktop", "/bin/view %F", "");
    let prepared = launcher.prepare(&plural_files, &local, None).unwrap();
    assert_eq!(prepared.launches().len(), 1);
    assert_eq!(
        arguments(&prepared.launches()[0]),
        vec![b"/tmp/one name.txt".to_vec(), b"/tmp/two.txt".to_vec()]
    );

    let uri_targets = [
        LaunchTarget::uri("https://example.test/a%20b").unwrap(),
        LaunchTarget::local("/tmp/local.txt"),
    ];
    let singular_uris = fixture.application("single-uri.desktop", "/bin/view %u", "");
    let prepared = launcher
        .prepare(&singular_uris, &uri_targets, None)
        .unwrap();
    assert_eq!(prepared.launches().len(), 2);
    assert_eq!(
        arguments(&prepared.launches()[0]),
        vec![b"https://example.test/a%20b".to_vec()]
    );
    assert_eq!(
        arguments(&prepared.launches()[1]),
        vec![b"file:///tmp/local.txt".to_vec()]
    );

    let plural_uris = fixture.application("uris.desktop", "/bin/view %U", "");
    let prepared = launcher.prepare(&plural_uris, &uri_targets, None).unwrap();
    assert_eq!(prepared.launches().len(), 1);
    assert_eq!(
        arguments(&prepared.launches()[0]),
        vec![
            b"https://example.test/a%20b".to_vec(),
            b"file:///tmp/local.txt".to_vec(),
        ]
    );
}

#[test]
fn every_unrepresentable_target_is_reported_without_partial_launch() {
    let fixture = Fixture::new();
    let launcher = DesktopEntryLauncher::new(fixture.paths().executable_dirs().to_vec());
    let uri_application = fixture.application("uris.desktop", "/bin/view %U", "");
    let invalid_one = PathBuf::from(OsString::from_vec(b"/tmp/a-\xff".to_vec()));
    let invalid_two = PathBuf::from(OsString::from_vec(b"/tmp/b-\xfe".to_vec()));
    let error = launcher
        .prepare(
            &uri_application,
            &[
                LaunchTarget::local(invalid_one),
                LaunchTarget::local("/tmp/valid"),
                LaunchTarget::local(invalid_two),
            ],
            None,
        )
        .unwrap_err();
    let LaunchError::UnrepresentableTargets(refusals) = error else {
        panic!("expected URI boundary refusals");
    };
    assert_eq!(
        refusals
            .iter()
            .map(|refusal| refusal.index())
            .collect::<Vec<_>>(),
        vec![0, 2]
    );

    let files_application = fixture.application("files.desktop", "/bin/view %F", "");
    let error = launcher
        .prepare(
            &files_application,
            &[
                LaunchTarget::uri("https://example.test/one").unwrap(),
                LaunchTarget::uri("sftp://example.test/two").unwrap(),
            ],
            None,
        )
        .unwrap_err();
    let LaunchError::UnrepresentableTargets(refusals) = error else {
        panic!("expected path boundary refusals");
    };
    assert_eq!(refusals.len(), 2);
}

#[test]
fn invalid_exec_entries_and_missing_try_exec_are_refused() {
    let fixture = Fixture::new();
    let launcher = DesktopEntryLauncher::new(fixture.paths().executable_dirs().to_vec());
    for (id, exec) in [
        ("unknown.desktop", "/bin/view %x"),
        ("embedded.desktop", "/bin/view --files=%F"),
        ("quoted.desktop", "/bin/view \"%f\""),
        ("unclosed.desktop", "/bin/view \"unterminated"),
    ] {
        let application = fixture.application(id, exec, "");
        assert!(
            launcher
                .prepare(&application, &[LaunchTarget::local("/tmp/a")], None)
                .is_err(),
            "{id} must be rejected"
        );
    }

    let missing = fixture.application(
        "missing.desktop",
        "/bin/view %f",
        "TryExec=definitely-not-installed-musheen-fixture\n",
    );
    assert!(matches!(
        launcher.prepare(&missing, &[LaunchTarget::local("/tmp/a")], None),
        Err(LaunchError::TryExecUnavailable(_))
    ));

    fixture.executable("available-viewer");
    let available = fixture.application(
        "available.desktop",
        "/bin/view %f",
        "TryExec=available-viewer\n",
    );
    assert!(
        launcher
            .prepare(&available, &[LaunchTarget::local("/tmp/a")], None)
            .is_ok()
    );
}

#[test]
fn terminal_entries_are_wrapped_as_argv_without_interpolation() {
    let fixture = Fixture::new();
    let application = fixture.application(
        "terminal.desktop",
        "/usr/bin/tool --mode safe %f",
        "Terminal=true\nPath=/tmp\n",
    );
    let terminal = TerminalCommand::new("/usr/bin/xterm", ["--title", "Musheen", "-e"]).unwrap();
    let launcher = DesktopEntryLauncher::new(fixture.paths().executable_dirs().to_vec());
    let prepared = launcher
        .prepare(
            &application,
            &[LaunchTarget::local("/tmp/a;not-a-command")],
            Some(&terminal),
        )
        .unwrap();
    let launch = &prepared.launches()[0];
    assert_eq!(launch.program(), OsStr::new("/usr/bin/xterm"));
    assert_eq!(launch.working_directory(), Some(Path::new("/tmp")));
    assert_eq!(
        arguments(launch),
        vec![
            b"--title".to_vec(),
            b"Musheen".to_vec(),
            b"-e".to_vec(),
            b"/usr/bin/tool".to_vec(),
            b"--mode".to_vec(),
            b"safe".to_vec(),
            b"/tmp/a;not-a-command".to_vec(),
        ]
    );
}
