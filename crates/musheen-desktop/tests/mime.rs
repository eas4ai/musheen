use musheen_desktop::{MIME_SNIFF_BYTES, MimeBackend, MimeDetector, MimeSource};
use std::ffi::OsStr;
use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct SpyBackend {
    name_result: Option<&'static str>,
    content_result: Option<&'static str>,
    name_calls: AtomicUsize,
    content_calls: AtomicUsize,
    largest_content: AtomicUsize,
}

impl SpyBackend {
    fn new(name_result: Option<&'static str>, content_result: Option<&'static str>) -> Self {
        Self {
            name_result,
            content_result,
            name_calls: AtomicUsize::new(0),
            content_calls: AtomicUsize::new(0),
            largest_content: AtomicUsize::new(0),
        }
    }
}

impl MimeBackend for SpyBackend {
    fn detect_name(&self, _name: &OsStr) -> Option<Box<str>> {
        self.name_calls.fetch_add(1, Ordering::Relaxed);
        self.name_result.map(Into::into)
    }

    fn detect_content(&self, content: &[u8]) -> Option<Box<str>> {
        self.content_calls.fetch_add(1, Ordering::Relaxed);
        self.largest_content
            .fetch_max(content.len(), Ordering::Relaxed);
        self.content_result.map(Into::into)
    }
}

#[test]
fn specific_name_detection_avoids_content_and_fallback_work() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("report.txt");
    fs::write(&path, vec![b'a'; MIME_SNIFF_BYTES + 1]).unwrap();
    let primary = Arc::new(SpyBackend::new(Some("text/plain"), Some("image/png")));
    let fallback = Arc::new(SpyBackend::new(None, Some("application/pdf")));
    let detector = MimeDetector::with_backends(primary.clone(), fallback.clone());

    let detected = detector.detect(&path).unwrap();

    assert_eq!(detected.mime_type(), "text/plain");
    assert_eq!(detected.source(), MimeSource::Name);
    assert_eq!(detected.bytes_read(), 0);
    assert_eq!(primary.content_calls.load(Ordering::Relaxed), 0);
    assert_eq!(fallback.content_calls.load(Ordering::Relaxed), 0);
}

#[cfg(unix)]
#[test]
fn content_and_fallback_detection_are_bounded_for_non_utf8_names() {
    use std::os::unix::ffi::OsStringExt;

    let temporary = tempfile::tempdir().unwrap();
    let path = temporary
        .path()
        .join(std::ffi::OsString::from_vec(b"image-\xff".to_vec()));
    fs::write(&path, vec![0_u8; MIME_SNIFF_BYTES + 8]).unwrap();
    let primary = Arc::new(SpyBackend::new(
        Some("application/octet-stream"),
        Some("application/octet-stream"),
    ));
    let fallback = Arc::new(SpyBackend::new(None, Some("image/png")));
    let detector = MimeDetector::with_backends(primary.clone(), fallback.clone());

    let detected = detector.detect(&path).unwrap();

    assert_eq!(detected.mime_type(), "image/png");
    assert_eq!(detected.source(), MimeSource::Fallback);
    assert_eq!(detected.bytes_read(), MIME_SNIFF_BYTES);
    assert_eq!(primary.content_calls.load(Ordering::Relaxed), 1);
    assert_eq!(fallback.content_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        fallback.largest_content.load(Ordering::Relaxed),
        MIME_SNIFF_BYTES
    );
}

#[test]
fn a_specific_primary_content_result_cannot_be_replaced_by_fallback() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("unknown.data");
    fs::write(&path, b"specific content").unwrap();
    let primary = Arc::new(SpyBackend::new(None, Some("application/pdf")));
    let fallback = Arc::new(SpyBackend::new(None, Some("text/plain")));
    let detector = MimeDetector::with_backends(primary, fallback.clone());

    let detected = detector.detect(&path).unwrap();

    assert_eq!(detected.mime_type(), "application/pdf");
    assert_eq!(detected.source(), MimeSource::Content);
    assert_eq!(fallback.content_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn default_backends_detect_content_when_the_name_is_unknown() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("document.data");
    fs::write(&path, b"%PDF-1.7\n").unwrap();

    let detected = MimeDetector::default().detect(&path).unwrap();

    assert_eq!(detected.mime_type(), "application/pdf");
    assert!(matches!(
        detected.source(),
        MimeSource::Content | MimeSource::Fallback
    ));
    assert_eq!(detected.bytes_read(), 9);
}

#[cfg(unix)]
#[test]
fn sockets_pipes_and_devices_are_named_by_type_without_being_opened() {
    let temporary = tempfile::tempdir().unwrap();
    let socket = temporary.path().join("socket.txt");
    let pipe = temporary.path().join("pipe.txt");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    nix::unistd::mkfifo(&pipe, nix::sys::stat::Mode::S_IRWXU).unwrap();
    let detector = MimeDetector::default();
    assert_eq!(
        detector.detect(&socket).unwrap().mime_type(),
        "inode/socket"
    );
    // Opening the pipe would wait for a writer that never comes.
    assert_eq!(detector.detect(&pipe).unwrap().mime_type(), "inode/fifo");
    assert_eq!(
        detector
            .detect(std::path::Path::new("/dev/null"))
            .unwrap()
            .mime_type(),
        "inode/chardevice"
    );
}
