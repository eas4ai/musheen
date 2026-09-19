use musheen_core::{CommandRegistry, ItemId, ProviderId, StorePath};
use musheen_ui::navigation::{
    ApplicationSession, BreadcrumbTrail, NavigationFocus, OmnibarMode, OmnibarState,
    OmnibarSubmission, SessionSink, SessionWriteDebouncer, WindowSession, suggest_local_paths,
};
use musheen_ui::views::{GroupKey, Layout, SortDirection, SortKey, ViewPreferences};
use std::ffi::OsString;
use std::time::Duration;

fn path(value: &str) -> StorePath {
    StorePath::from_unix_path(value)
}

fn item(key: &[u8]) -> ItemId {
    ItemId::new(
        ProviderId::new("local").expect("provider ID is valid"),
        key.to_vec(),
    )
    .expect("item ID is valid")
}

#[test]
fn pane_histories_and_selections_remain_independent() {
    let mut window = WindowSession::new(path("/left"));
    let left = window.focused_pane_id();
    let outcome = window.navigate_focused(path("/left/child"));
    assert_eq!(outcome.focus(), NavigationFocus::Content);

    let right = window
        .split_focused(path("/right"))
        .expect("a second pane is supported");
    window.navigate_focused(path("/right/child"));
    window
        .focused_tab_mut()
        .set_selection([item(b"right-selection")]);

    window.focus_pane(left).expect("left pane still exists");
    window
        .focused_tab_mut()
        .set_selection([item(b"left-selection")]);
    assert_eq!(window.focused_tab().selection(), &[item(b"left-selection")]);

    window.focus_pane(right).expect("right pane still exists");
    assert_eq!(window.focused_tab().location(), &path("/right/child"));
    assert_eq!(
        window.focused_tab().selection(),
        &[item(b"right-selection")]
    );

    window.focus_pane(left).expect("left pane still exists");
    assert_eq!(
        window.go_back().expect("left history has a back entry"),
        &path("/left")
    );
}

#[test]
fn duplicate_tabs_copy_state_without_sharing_future_changes() {
    let mut window = WindowSession::new(path("/projects"));
    window.navigate_focused(path("/projects/musheen"));
    window.focused_tab_mut().set_selection([item(b"selected")]);

    let original = window.focused_tab().id();
    let duplicate = window
        .duplicate_active_tab()
        .expect("the active tab can be duplicated");
    assert_ne!(duplicate, original);
    assert_eq!(window.focused_pane().tabs().len(), 2);
    assert_eq!(window.focused_tab().location(), &path("/projects/musheen"));

    window.navigate_focused(path("/tmp"));
    window
        .focused_pane_mut()
        .activate_tab(original)
        .expect("original tab remains available");
    assert_eq!(window.focused_tab().location(), &path("/projects/musheen"));
    assert_eq!(window.focused_tab().selection(), &[item(b"selected")]);
}

#[test]
fn tab_lifecycle_supports_reorder_move_reopen_and_tear_out() {
    let mut window = WindowSession::new(path("/one"));
    let first = window.focused_tab().id();
    let second = window
        .new_tab(path("/two"))
        .expect("second tab can be created");
    window
        .reorder_active_tab(0)
        .expect("active tab can be reordered");
    assert_eq!(window.focused_pane().tabs()[0].id(), second);
    assert_eq!(window.focused_pane().tabs()[1].id(), first);

    window.close_active_tab().expect("active tab can close");
    assert_eq!(window.focused_pane().tabs().len(), 1);
    let reopened = window
        .reopen_closed_tab()
        .expect("closed tab can be reopened");
    assert_eq!(reopened, second);
    assert_eq!(window.focused_tab().location(), &path("/two"));

    let right = window
        .split_focused(path("/right"))
        .expect("second pane can be created");
    let left = window.panes()[0].id();
    window.focus_pane(left).expect("left pane exists");
    window
        .move_active_tab_to(right)
        .expect("active tab can move to the other pane");
    assert_eq!(window.panes()[0].tabs().len(), 1);
    assert_eq!(window.panes()[1].tabs().len(), 2);

    window.focus_pane(right).expect("right pane exists");
    let torn_out = window
        .tear_out_active_tab()
        .expect("active tab can move to a new window");
    assert_eq!(torn_out.panes().len(), 1);
    assert_eq!(torn_out.focused_pane().tabs().len(), 1);
    assert_eq!(torn_out.focused_tab().location(), &path("/two"));
    assert_eq!(window.focused_pane().tabs().len(), 1);
}

#[test]
fn omnibar_mode_unambiguously_controls_submission() {
    let mut omnibar = OmnibarState::default();
    omnibar.enter(OmnibarMode::Path, "docs");
    assert_eq!(omnibar.submit(), OmnibarSubmission::Path("docs".to_owned()));

    omnibar.enter(OmnibarMode::Search, "type:image");
    assert_eq!(
        omnibar.submit(),
        OmnibarSubmission::Search("type:image".to_owned())
    );

    omnibar.enter(OmnibarMode::Command, "settings");
    assert_eq!(
        omnibar.submit(),
        OmnibarSubmission::Command("settings".to_owned())
    );
    omnibar.cancel();
    assert_eq!(omnibar.mode(), OmnibarMode::Path);
    assert!(omnibar.text().is_empty());
}

#[test]
fn path_input_resolves_from_the_active_tab_without_changing_provider_paths() {
    use musheen_ui::navigation::resolve_path_input;

    assert_eq!(
        resolve_path_input(&path("/projects/musheen"), "docs/spec"),
        Some(path("/projects/musheen/docs/spec"))
    );
    assert_eq!(
        resolve_path_input(&path("/projects/musheen"), "/tmp"),
        Some(path("/tmp"))
    );

    let remote = StorePath::from_provider_key(
        ProviderId::new("sftp").expect("provider ID is valid"),
        b"remote-key".to_vec(),
    )
    .expect("provider path is valid");
    assert_eq!(resolve_path_input(&remote, "child"), None);
}

#[test]
fn local_path_suggestions_are_relative_to_the_active_tab() {
    let children = [OsString::from("shared"), OsString::from("specific")];
    let left = suggest_local_paths(&path("/left"), "sha", &children);
    let right = suggest_local_paths(&path("/right"), "sha", &children);

    assert_eq!(left.len(), 1);
    assert_eq!(left[0].target(), &path("/left/shared"));
    assert_eq!(right.len(), 1);
    assert_eq!(right[0].target(), &path("/right/shared"));
}

#[test]
fn breadcrumbs_keep_lossless_targets_when_ancestors_overflow() {
    let trail = BreadcrumbTrail::from_path(&path("/one/two/three/four"), 3);

    assert_eq!(trail.visible().len(), 3);
    assert_eq!(trail.hidden().len(), 2);
    assert_eq!(
        trail.visible().last().expect("leaf exists").target(),
        &path("/one/two/three/four")
    );
    assert_eq!(trail.hidden()[0].target(), &path("/"));
    assert_eq!(trail.hidden()[1].target(), &path("/one"));
}

#[test]
fn session_round_trip_recovers_only_the_missing_location() {
    let missing = StorePath::from_unix_bytes(b"/gone/bad-\xff".to_vec());
    let fallback = path("/home/test");
    let mut session = WindowSession::new(path("/kept"));
    session
        .focused_pane_mut()
        .create_tab(missing.clone())
        .expect("a second tab is supported");
    let encoded = session.to_json().expect("session is serializable");

    let restored = WindowSession::restore_json(
        &encoded,
        |candidate| candidate != &missing,
        fallback.clone(),
    )
    .expect("session document is valid");

    assert_eq!(restored.focused_pane().tabs().len(), 2);
    assert!(
        restored
            .focused_pane()
            .tabs()
            .iter()
            .any(|tab| tab.location() == &path("/kept"))
    );
    assert!(
        restored
            .focused_pane()
            .tabs()
            .iter()
            .any(|tab| tab.location() == &fallback)
    );
}

#[test]
fn application_session_restores_multiple_windows_in_order() {
    let mut first = WindowSession::new(path("/first"));
    first.new_tab(path("/first/second-tab")).expect("tab opens");
    first
        .split_focused(path("/first/right"))
        .expect("first window splits");
    let mut second = WindowSession::new(path("/second"));
    second
        .split_focused(path("/second/right"))
        .expect("second window splits");
    let application =
        ApplicationSession::new(vec![first, second]).expect("two windows are supported");
    let encoded = application.to_json().expect("application session encodes");

    let restored = ApplicationSession::restore_json(&encoded, |_| true, path("/fallback"))
        .expect("application session restores");

    assert_eq!(restored.windows().len(), 2);
    assert_eq!(restored.windows()[0].panes().len(), 2);
    assert_eq!(restored.windows()[1].panes().len(), 2);
    assert_eq!(
        restored.windows()[0].focused_tab().location(),
        &path("/first/right")
    );
    assert_eq!(
        restored.windows()[1].focused_tab().location(),
        &path("/second/right")
    );
}

#[test]
fn application_session_migrates_a_legacy_single_window_document() {
    let legacy = WindowSession::new(path("/legacy"))
        .to_json()
        .expect("legacy window session encodes");

    let restored =
        ApplicationSession::restore_compatible_json(&legacy, |_| true, path("/fallback"))
            .expect("legacy document migrates");

    assert_eq!(restored.windows().len(), 1);
    assert_eq!(
        restored.windows()[0].focused_tab().location(),
        &path("/legacy")
    );
}

#[test]
fn session_preserves_lossless_per_directory_view_preferences() {
    let location = StorePath::from_unix_bytes(b"/view/bad-\xff".to_vec());
    let mut window = WindowSession::new(location.clone());
    let preferences = ViewPreferences {
        layout: Layout::Details,
        group: GroupKey::Kind,
        directories_first: false,
        show_hidden: true,
        sort: musheen_ui::views::SortSpec {
            key: SortKey::Modified,
            direction: SortDirection::Descending,
        },
        ..ViewPreferences::default()
    };
    window.set_preferences_for(location.clone(), preferences.clone());

    let encoded = window.to_json().expect("view preferences serialize");
    let restored = WindowSession::restore_json(&encoded, |_| true, path("/fallback"))
        .expect("view preferences restore");

    assert_eq!(restored.preferences_for(&location), &preferences);
}

#[derive(Default)]
struct RecordingSink {
    documents: Vec<Vec<u8>>,
}

impl SessionSink for RecordingSink {
    type Error = std::convert::Infallible;

    fn save_session(&mut self, document: &[u8]) -> Result<(), Self::Error> {
        self.documents.push(document.to_vec());
        Ok(())
    }
}

#[test]
fn session_writes_are_debounced_and_flush_the_latest_state() {
    let mut session = WindowSession::new(path("/first"));
    let mut writer = SessionWriteDebouncer::new(Duration::from_millis(250));
    let mut sink = RecordingSink::default();

    writer.mark_dirty(Duration::ZERO);
    assert!(
        !writer
            .flush_if_due(Duration::from_millis(249), &session, &mut sink)
            .expect("recording cannot fail")
    );
    session.navigate_focused(path("/latest"));
    writer.mark_dirty(Duration::from_millis(100));
    assert!(
        !writer
            .flush_if_due(Duration::from_millis(349), &session, &mut sink)
            .expect("recording cannot fail")
    );
    assert!(
        writer
            .flush_if_due(Duration::from_millis(350), &session, &mut sink)
            .expect("recording cannot fail")
    );
    assert_eq!(sink.documents.len(), 1);

    let decoded =
        WindowSession::restore_json(&sink.documents[0], |_| true, path("/unused-fallback"))
            .expect("recorded session is valid");
    assert_eq!(decoded.focused_tab().location(), &path("/latest"));
}

#[test]
fn tab_and_pane_navigation_commands_have_stable_registry_ids() {
    let registry = CommandRegistry::built_in();
    for id in [
        "tab.new",
        "tab.close",
        "tab.duplicate",
        "tab.reopen_closed",
        "tab.move_other_pane",
        "tab.move_left",
        "tab.move_right",
        "tab.tear_out",
        "pane.split",
        "pane.focus_next",
        "view.command",
    ] {
        assert!(
            registry.get(id).is_some(),
            "missing navigation command {id}"
        );
    }
}
