use musheen_core::BoxFuture;
use musheen_desktop::{FileManager1, FileManagerError, FileManagerRequest, FileManagerRequestSink};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct AcceptingSink(AtomicUsize);

impl FileManagerRequestSink for AcceptingSink {
    fn submit(
        &self,
        _request: FileManagerRequest,
    ) -> BoxFuture<'static, Result<(), FileManagerError>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }
}

pub struct UsableFileManager {
    _file: tempfile::NamedTempFile,
    uri: String,
    service: FileManager1,
    sink: Arc<AcceptingSink>,
}

impl UsableFileManager {
    pub fn new() -> Self {
        let file = tempfile::NamedTempFile::new().unwrap();
        let uri = format!("file://{}", file.path().display());
        let sink = Arc::new(AcceptingSink::default());
        Self {
            _file: file,
            uri,
            service: FileManager1::new(sink.clone()),
            sink,
        }
    }

    pub fn show(&self, startup_id: &str) {
        futures_lite::future::block_on(self.service.show_items(&[&self.uri], startup_id)).unwrap();
    }

    pub fn calls(&self) -> usize {
        self.sink.0.load(Ordering::SeqCst)
    }
}
