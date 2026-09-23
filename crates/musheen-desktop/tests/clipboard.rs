use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
use musheen_core::StorePath;
use musheen_desktop::{
    ClipboardOperation, ClipboardPayload, GNOME_COPIED_FILES, KDE_CUT_SELECTION,
    SystemFileClipboard, URI_LIST,
};
use std::os::unix::ffi::OsStringExt;

#[test]
fn freedesktop_copy_and_cut_formats_are_consistent() {
    let paths = vec![local(b"/work/a b"), local(b"/work/non-utf8-\xff")];
    let copied = ClipboardPayload::new(ClipboardOperation::Copy, paths.clone()).unwrap();
    let uris = copied.format(URI_LIST).unwrap();
    assert_eq!(uris, b"file:///work/a%20b\r\nfile:///work/non-utf8-%FF\r\n");
    assert_eq!(copied.format(KDE_CUT_SELECTION), Some(b"0".as_slice()));
    assert!(
        copied
            .format(GNOME_COPIED_FILES)
            .unwrap()
            .starts_with(b"copy\n")
    );

    let cut = ClipboardPayload::new(ClipboardOperation::Cut, paths.clone()).unwrap();
    assert_eq!(cut.format(KDE_CUT_SELECTION), Some(b"1".as_slice()));
    assert!(
        cut.format(GNOME_COPIED_FILES)
            .unwrap()
            .starts_with(b"cut\n")
    );
    assert_eq!(
        ClipboardPayload::parse(cut.formats()).unwrap().paths(),
        paths
    );
}

#[test]
fn clipboard_rejects_remote_relative_and_malformed_file_uris() {
    let remote = StorePath::from_provider_key(
        musheen_core::ProviderId::new("remote").unwrap(),
        b"key".to_vec(),
    )
    .unwrap();
    assert!(ClipboardPayload::new(ClipboardOperation::Copy, vec![remote]).is_err());
    assert!(ClipboardPayload::new(ClipboardOperation::Copy, vec![local(b"relative")]).is_err());

    let formats = [(Box::<str>::from(URI_LIST), b"file://host/path\r\n".to_vec())]
        .into_iter()
        .collect();
    assert!(ClipboardPayload::parse(&formats).is_err());
}

#[test]
fn clipboard_rejects_conflicting_gnome_and_uri_targets() {
    let formats = [
        (Box::<str>::from(URI_LIST), b"file:///tmp/one\r\n".to_vec()),
        (
            Box::<str>::from(GNOME_COPIED_FILES),
            b"cut\nfile:///tmp/two".to_vec(),
        ),
    ]
    .into_iter()
    .collect();

    assert!(ClipboardPayload::parse(&formats).is_err());
}

#[test]
#[ignore = "requires a dedicated X11 display to avoid changing the user's clipboard"]
fn system_clipboard_exchanges_file_formats_with_another_client() {
    let system = SystemFileClipboard::new();
    let other = ClipboardContext::new().unwrap();
    let copied = ClipboardPayload::new(
        ClipboardOperation::Copy,
        vec![local(b"/tmp/musheen-copy.txt")],
    )
    .unwrap();

    system.publish(&copied).unwrap();
    let offered = other.available_formats().unwrap();
    for mime in [URI_LIST, GNOME_COPIED_FILES, KDE_CUT_SELECTION] {
        assert!(
            offered.iter().any(|format| format == mime),
            "missing {mime}"
        );
        assert_eq!(
            other.get_buffer(mime).unwrap(),
            copied.format(mime).unwrap()
        );
    }

    let cut = ClipboardPayload::new(
        ClipboardOperation::Cut,
        vec![local(b"/tmp/musheen-cut.txt")],
    )
    .unwrap();
    other
        .set(
            cut.formats()
                .iter()
                .map(|(mime, bytes)| ClipboardContent::Other(mime.to_string(), bytes.clone()))
                .collect(),
        )
        .unwrap();
    assert_eq!(system.read().unwrap(), Some(cut));
}

fn local(bytes: &[u8]) -> StorePath {
    StorePath::from_unix_path(std::ffi::OsString::from_vec(bytes.to_vec()))
}
