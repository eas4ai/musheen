use std::path::Path;

use musheen_core::StorePath;
use musheen_ui::{
    Catalog, FocusTarget, Locale, SemanticRegion, ShellModel, TerminalDrawer, TerminalDrawerAction,
    TerminalInput, TerminalKey,
};

#[test]
fn drawer_follows_active_local_pane_and_restores_browser_focus() {
    let mut drawer = TerminalDrawer::new(false, 260.0);
    drawer.set_active_location(StorePath::from_unix_path("/tmp/one"));
    assert_eq!(drawer.cwd(), Some(Path::new("/tmp/one")));

    assert_eq!(drawer.toggle(), TerminalDrawerAction::FocusTerminal);
    assert!(drawer.is_open());
    drawer.set_active_location(StorePath::from_unix_path("/tmp/two"));
    assert_eq!(drawer.cwd(), Some(Path::new("/tmp/two")));

    drawer.set_follow_active_pane(false);
    drawer.set_active_location(StorePath::from_unix_path("/tmp/three"));
    assert_eq!(drawer.cwd(), Some(Path::new("/tmp/two")));
    assert_eq!(drawer.toggle(), TerminalDrawerAction::RestoreBrowserFocus);
}

#[test]
fn drawer_is_resizable_closable_and_in_the_accessible_focus_order() {
    let mut drawer = TerminalDrawer::new(true, 240.0);
    drawer.resize(10.0, 900.0);
    assert_eq!(drawer.height(), 120.0);
    drawer.resize(900.0, 500.0);
    assert_eq!(drawer.height(), 400.0);

    let shell = ShellModel::with_terminal(false, true);
    assert!(shell.semantic_regions().contains(&SemanticRegion::Terminal));
    assert!(shell.focus_order().contains(&FocusTarget::Terminal));
    assert_eq!(drawer.close(), TerminalDrawerAction::RestoreBrowserFocus);
}

#[test]
fn default_terminal_keyboard_map_includes_f4_and_navigation_keys() {
    let bindings = TerminalInput::default_bindings();
    assert_eq!(
        bindings.action_for(TerminalKey::F4),
        Some("terminal.toggle")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::Enter),
        Some("terminal.enter")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::Backspace),
        Some("terminal.backspace")
    );
    assert_eq!(bindings.action_for(TerminalKey::Tab), Some("terminal.tab"));
    assert_eq!(
        bindings.action_for(TerminalKey::ArrowUp),
        Some("terminal.up")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::ArrowDown),
        Some("terminal.down")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::ArrowLeft),
        Some("terminal.left")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::ArrowRight),
        Some("terminal.right")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::Home),
        Some("terminal.home")
    );
    assert_eq!(bindings.action_for(TerminalKey::End), Some("terminal.end"));
    assert_eq!(
        bindings.action_for(TerminalKey::PageUp),
        Some("terminal.page-up")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::PageDown),
        Some("terminal.page-down")
    );
    assert_eq!(bindings.bytes_for(TerminalKey::CtrlC), Some(&b"\x03"[..]));
    assert_eq!(bindings.bytes_for(TerminalKey::CtrlD), Some(&b"\x04"[..]));
    assert_eq!(bindings.bytes_for(TerminalKey::CtrlZ), Some(&b"\x1a"[..]));
    assert_eq!(
        bindings.action_for(TerminalKey::CtrlShiftC),
        Some("terminal.copy")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::CtrlShiftV),
        Some("terminal.paste")
    );
    assert_eq!(
        bindings.action_for(TerminalKey::ShiftInsert),
        Some("terminal.paste")
    );
}

#[test]
fn paste_and_exit_flow_is_explicit_and_restartable() {
    let mut drawer = TerminalDrawer::new(true, 240.0);
    assert!(drawer.request_paste("a\nb").requires_confirmation());
    assert!(!drawer.request_paste("echo ok").requires_confirmation());

    drawer.mark_child_exited();
    assert!(drawer.restart_available());
    drawer.mark_restarted();
    assert!(!drawer.restart_available());

    drawer.mark_foreground_job(true);
    assert_eq!(
        drawer.request_close(),
        TerminalDrawerAction::ConfirmTerminate
    );
    assert!(drawer.is_open());
}

#[test]
fn paste_confirmation_explains_sanitization_and_execution_risk() {
    let catalog = Catalog::load(Locale::EnUs).unwrap();

    assert_eq!(
        catalog.message("terminal-paste-warning").unwrap(),
        "This paste contains multiple lines or unsafe terminal controls. Unsafe controls will be removed; the remaining text may run multiple commands. Review it before sending."
    );
    assert_eq!(
        catalog.message("terminal-paste-continue").unwrap(),
        "Paste sanitized text"
    );
}
