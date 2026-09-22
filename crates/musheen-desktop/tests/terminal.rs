use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use musheen_core::{ProviderId, StorePath};
use musheen_desktop::{
    DesktopEntryTerminalLauncher, DesktopPaths, ExternalTerminalCommand, PasteDisposition,
    PreparedLaunch, ProcessRunner, PtyEvent, TerminalExit, TerminalModel, TerminalProfile,
    TerminalSession, TerminalSize,
};

fn size() -> TerminalSize {
    TerminalSize::new(80, 24, 8, 16).expect("valid terminal size")
}

#[test]
fn profile_uses_configured_shell_and_active_local_pane_cwd() {
    let profile =
        TerminalProfile::new("fish", "/usr/bin/fish", ["--login"]).expect("valid profile");
    let cwd = StorePath::from_unix_path(OsString::from_vec(b"/tmp/pane-\xff".to_vec()));
    let launch = profile.prepare(&cwd).expect("local cwd is representable");

    assert_eq!(launch.program(), OsStr::new("/usr/bin/fish"));
    assert_eq!(launch.arguments(), &[OsString::from("--login")]);
    assert_eq!(
        launch
            .working_directory()
            .unwrap()
            .as_os_str()
            .as_encoded_bytes(),
        b"/tmp/pane-\xff"
    );

    let remote =
        StorePath::from_provider_key(ProviderId::new("sftp.example").unwrap(), b"remote".to_vec())
            .unwrap();
    assert!(profile.prepare(&remote).is_err());
}

#[test]
fn external_terminal_keeps_hostile_cwd_out_of_argv_and_shells() {
    let command = ExternalTerminalCommand::new("kgx", ["--wait"]).unwrap();
    let cwd = StorePath::from_unix_path("/tmp/$(touch NEVER);'quoted'");
    let launch = command.prepare(&cwd).unwrap();

    assert_eq!(launch.program(), OsStr::new("kgx"));
    assert_eq!(launch.arguments(), &[OsString::from("--wait")]);
    assert_eq!(
        launch.working_directory(),
        Some(Path::new("/tmp/$(touch NEVER);'quoted'"))
    );

    let remote =
        StorePath::from_provider_key(ProviderId::new("sftp.example").unwrap(), b"remote".to_vec())
            .unwrap();
    assert!(command.prepare(&remote).is_err());
}

#[test]
fn configured_desktop_entry_launch_keeps_cwd_as_process_metadata() {
    let fixture = tempfile::tempdir().unwrap();
    let applications = fixture.path().join("data/applications");
    std::fs::create_dir_all(&applications).unwrap();
    std::fs::write(
        applications.join("org.example.Terminal.desktop"),
        "[Desktop Entry]\nType=Application\nName=Fixture Terminal\nExec=/bin/echo --fixture\n",
    )
    .unwrap();
    let paths = DesktopPaths::new(fixture.path().join("config"), fixture.path().join("data"))
        .with_executable_dirs([PathBuf::from("/bin")]);
    let launcher =
        DesktopEntryTerminalLauncher::new("org.example.Terminal.desktop", paths).unwrap();
    let cwd = StorePath::from_unix_path("/tmp/$(never-run)");
    let prepared = launcher.prepare(&cwd).unwrap();

    let launch = &prepared.launches()[0];
    assert_eq!(launch.program(), OsStr::new("/bin/echo"));
    assert_eq!(launch.arguments(), &[OsString::from("--fixture")]);
    assert_eq!(
        launch.working_directory(),
        Some(Path::new("/tmp/$(never-run)"))
    );
    let runner = RecordingRunner::default();
    launcher.launch(&prepared, &runner).unwrap();
    assert_eq!(runner.launches.lock().unwrap().len(), 1);
}

#[derive(Default)]
struct RecordingRunner {
    launches: Mutex<Vec<RecordedLaunch>>,
}

type RecordedLaunch = (OsString, Vec<OsString>, Option<PathBuf>);

impl ProcessRunner for RecordingRunner {
    fn spawn(&self, launch: &PreparedLaunch) -> std::io::Result<()> {
        self.launches.lock().unwrap().push((
            launch.program().to_os_string(),
            launch.arguments().to_vec(),
            launch.working_directory().map(Path::to_path_buf),
        ));
        Ok(())
    }
}

#[test]
fn emulator_tracks_wide_cells_title_resize_and_ignores_unsupported_controls() {
    let mut terminal = TerminalModel::new(size());
    terminal.feed(b"plain \xe7\x95\x8c\x1b]0;build shell\x07\x1bPunsupported\x1b\\done");

    assert_eq!(terminal.title(), Some("build shell"));
    assert!(
        terminal
            .cells()
            .iter()
            .any(|cell| cell.character() == '界' && cell.width() == 2)
    );
    assert!(!terminal.visible_text().contains("unsupported"));
    assert!(terminal.visible_text().contains("done"));

    terminal.resize(TerminalSize::new(132, 42, 9, 18).unwrap());
    assert_eq!(terminal.size().columns(), 132);
    assert_eq!(terminal.size().rows(), 42);
}

#[test]
fn paste_confirmation_is_required_for_multiline_and_controls() {
    let terminal = TerminalModel::new(size());
    assert_eq!(
        terminal.classify_paste("cargo test"),
        PasteDisposition::Safe
    );
    assert_eq!(
        terminal.classify_paste("first\nsecond"),
        PasteDisposition::ConfirmationRequired
    );
    assert_eq!(
        terminal.classify_paste("echo \u{1b}[31m"),
        PasteDisposition::ConfirmationRequired
    );
    assert_eq!(
        terminal.encode_paste("a\nb", true),
        b"\x1b[200~a\nb\x1b[201~"
    );
}

#[test]
fn scrollback_drops_oldest_complete_lines_at_both_limits() {
    let mut terminal = TerminalModel::new(size());
    for line in 0..1_000_000_u32 {
        terminal.feed(format!("line-{line}\r\n").as_bytes());
    }
    assert_eq!(terminal.scrollback_line_count(), 10_000);
    assert!(!terminal.scrollback_text().contains("line-0\n"));
    assert!(terminal.scrollback_text().contains("line-999999"));

    let long = vec![b'x'; 64 * 1024 * 1024 + 1];
    terminal.feed(&long);
    assert!(terminal.scrollback_bytes() <= 64 * 1024 * 1024);
    terminal.feed(b"\nkept\n");
    assert!(terminal.scrollback_bytes() <= 64 * 1024 * 1024);
    assert!(terminal.scrollback_text().ends_with("kept\n"));

    for _ in 0..20_000 {
        terminal.feed(b"\x1b[31mred\x1b[0m\n");
    }
    assert_eq!(terminal.scrollback_line_count(), 10_000);
    assert!(terminal.scrollback_bytes() <= 64 * 1024 * 1024);
}

#[test]
fn portable_pty_reports_normal_signal_and_loaded_child_exit_and_can_restart() {
    for (script, expected) in [
        ("printf normal", TerminalExit::Code(0)),
        ("kill -TERM $$", TerminalExit::Signal),
        (
            "i=0; while [ $i -lt 2000 ]; do printf 'line-%s\\n' $i; i=$((i+1)); done",
            TerminalExit::Code(0),
        ),
    ] {
        let profile = TerminalProfile::new("test", "/bin/sh", ["-c", script]).unwrap();
        let mut session =
            TerminalSession::spawn(profile, &StorePath::from_unix_path("/tmp"), size()).unwrap();
        session
            .resize(TerminalSize::new(100, 30, 8, 16).unwrap())
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exit = None;
        while Instant::now() < deadline {
            match session.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyEvent::Output(_)) => {}
                Ok(PtyEvent::Exited(status)) => {
                    exit = Some(status);
                    break;
                }
                Ok(PtyEvent::ReadFailed) => panic!("PTY reader failed for {script}"),
                Err(_) => {}
            }
        }
        assert_eq!(
            exit,
            Some(expected),
            "script did not exit as expected: {script}"
        );
        session.restart().unwrap();
        assert!(session.is_running());
        session.terminate().unwrap();
    }
}
