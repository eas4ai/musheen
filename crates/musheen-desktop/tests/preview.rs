use musheen_core::CancellationToken;
use musheen_desktop::{
    PREVIEW_INITIAL_BYTES, PreviewDocument, PreviewError, PreviewKind, PreviewLimits,
};
use std::fs::{self, File};
use std::io::Write;

#[test]
fn selection_reads_only_the_initial_mebibyte_of_a_sparse_tibibyte_file() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("sparse.txt");
    let mut file = File::create(&path).unwrap();
    file.write_all(b"preview").unwrap();
    file.set_len(1_u64 << 40).unwrap();

    let preview = PreviewDocument::open(&path, CancellationToken::new()).unwrap();

    assert_eq!(preview.bytes_read(), PREVIEW_INITIAL_BYTES);
    assert!(preview.open_with_available());
    assert_eq!(preview.kind(), PreviewKind::Binary);
}

#[test]
fn explicit_loads_stop_at_the_configured_ceiling_without_lossy_text() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("bounded.txt");
    fs::write(&path, b"abcdefghijklmnopqrstuvwxyz0123456789").unwrap();
    let limits = PreviewLimits::new(8, 10, 24).unwrap();
    let mut preview =
        PreviewDocument::open_with_limits(&path, CancellationToken::new(), limits).unwrap();

    assert_eq!(preview.text(), Some("abcdefgh"));
    assert!(preview.load_more(CancellationToken::new()).unwrap());
    assert_eq!(preview.bytes_read(), 18);
    assert!(preview.load_more(CancellationToken::new()).unwrap());
    assert_eq!(preview.bytes_read(), 24);
    assert!(preview.limit_reached());
    assert!(!preview.load_more(CancellationToken::new()).unwrap());
    assert_eq!(preview.text(), Some("abcdefghijklmnopqrstuvwx"));

    let invalid = temporary.path().join("invalid.txt");
    fs::write(&invalid, [b'a', 0xff, b'b']).unwrap();
    let invalid = PreviewDocument::open(&invalid, CancellationToken::new()).unwrap();
    assert_eq!(invalid.kind(), PreviewKind::Binary);
    assert_eq!(invalid.text(), None);
    assert!(invalid.open_with_available());
}

#[test]
fn cancelled_preview_work_fails_before_reading() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("cancel.txt");
    fs::write(&path, b"not read").unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    assert!(matches!(
        PreviewDocument::open(&path, cancellation),
        Err(PreviewError::Cancelled)
    ));
}
