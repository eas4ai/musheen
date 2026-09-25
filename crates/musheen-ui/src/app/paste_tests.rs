use std::path::Path;

use gpui_kit::Context;
use musheen_core::{CommandAction, CommandTargetRef, StorePath};

use super::{ClipboardAction, FileClipboard, MenuTarget, MusheenApp};

pub(super) fn assert_active_paste_targets_current_folder(state: &MusheenApp, expected: &Path) {
    let paste = state.active_command_request(CommandAction::PasteInto);
    assert_eq!(paste.target(), MenuTarget::Background);
    assert!(paste.selection().is_empty());
    assert_eq!(paste.location().as_unix_path(), Some(expected));
    assert!(
        state
            .shell
            .commands()
            .get("clipboard.paste_into")
            .unwrap()
            .state(paste.context())
            .is_enabled()
    );
}

pub(super) fn assert_rejected_cut_paste_does_not_capture_pending_drop(
    state: &mut MusheenApp,
    cx: &mut Context<MusheenApp>,
) {
    let pending_target = {
        let pending = state.pending_drop.as_ref().unwrap();
        CommandTargetRef::new(
            pending.payload.expected_identity(0).unwrap().clone(),
            pending.payload.sources()[0].clone(),
        )
        .unwrap()
    };
    state.file_clipboard = Some(FileClipboard {
        targets: vec![pending_target],
        action: ClipboardAction::Cut,
    });
    state.paste_file_clipboard(StorePath::from_unix_path("/tmp"), cx);
    assert!(
        !state
            .pending_drop
            .as_ref()
            .unwrap()
            .clear_cut_clipboard_on_success
    );
}
