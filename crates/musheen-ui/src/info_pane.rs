use musheen_core::{CancellationToken, ItemKind};
use musheen_desktop::{PreviewDocument, PreviewKind};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InfoPaneDetails {
    name: Box<str>,
    kind: ItemKind,
    size: Option<u64>,
    modified_unix_seconds: Option<i64>,
}

impl InfoPaneDetails {
    pub fn new(
        name: impl Into<Box<str>>,
        kind: ItemKind,
        size: Option<u64>,
        modified_unix_seconds: Option<i64>,
    ) -> Self {
        Self {
            name: name.into(),
            kind,
            size,
            modified_unix_seconds,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> ItemKind {
        self.kind
    }

    pub fn size(&self) -> Option<u64> {
        self.size
    }

    pub fn modified_unix_seconds(&self) -> Option<i64> {
        self.modified_unix_seconds
    }
}

#[derive(Debug)]
pub enum PreviewPresentation {
    Text(PreviewDocument),
    Binary { bytes_read: usize },
    Thumbnail(PathBuf),
    DetailsOnly,
}

#[derive(Debug)]
pub enum InfoPaneState {
    Empty,
    Multiple {
        count: usize,
    },
    Loading {
        details: InfoPaneDetails,
        path: PathBuf,
    },
    Ready {
        details: InfoPaneDetails,
        path: PathBuf,
        mime_type: Box<str>,
        preview: PreviewPresentation,
    },
    Error {
        details: InfoPaneDetails,
        path: PathBuf,
        message: Box<str>,
    },
}

#[derive(Debug)]
pub enum InfoPaneResult {
    Details {
        mime_type: Box<str>,
    },
    Preview {
        mime_type: Box<str>,
        document: PreviewDocument,
    },
    Thumbnail {
        mime_type: Box<str>,
        path: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub struct InfoPaneWork {
    generation: u64,
    details: InfoPaneDetails,
    path: PathBuf,
    cancellation: CancellationToken,
}

impl InfoPaneWork {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn details(&self) -> &InfoPaneDetails {
        &self.details
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }
}

#[derive(Debug)]
pub struct InfoPaneLoadMoreWork {
    generation: u64,
    mime_type: Box<str>,
    document: PreviewDocument,
    cancellation: CancellationToken,
}

impl InfoPaneLoadMoreWork {
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn document_mut(&mut self) -> &mut PreviewDocument {
        &mut self.document
    }

    pub fn into_result_parts(self) -> (Box<str>, PreviewDocument) {
        (self.mime_type, self.document)
    }
}

#[derive(Debug)]
pub struct InfoPaneModel {
    state: InfoPaneState,
    generation: u64,
    cancellation: Option<CancellationToken>,
    retry_selection: Option<(InfoPaneDetails, PathBuf)>,
}

impl Default for InfoPaneModel {
    fn default() -> Self {
        Self {
            state: InfoPaneState::Empty,
            generation: 0,
            cancellation: None,
            retry_selection: None,
        }
    }
}

impl InfoPaneModel {
    pub fn state(&self) -> &InfoPaneState {
        &self.state
    }

    pub fn begin(&mut self, details: InfoPaneDetails, path: PathBuf) -> InfoPaneWork {
        self.cancel_active();
        self.generation = self.generation.wrapping_add(1);
        let cancellation = CancellationToken::new();
        self.cancellation = Some(cancellation.clone());
        self.retry_selection = Some((details.clone(), path.clone()));
        self.state = InfoPaneState::Loading {
            details: details.clone(),
            path: path.clone(),
        };
        InfoPaneWork {
            generation: self.generation,
            details,
            path,
            cancellation,
        }
    }

    pub fn clear(&mut self) {
        self.cancel_active();
        self.retry_selection = None;
        self.state = InfoPaneState::Empty;
    }

    pub fn show_multiple(&mut self, count: usize) {
        self.cancel_active();
        self.retry_selection = None;
        self.state = if count == 0 {
            InfoPaneState::Empty
        } else {
            InfoPaneState::Multiple { count }
        };
    }

    pub fn complete(&mut self, generation: u64, result: InfoPaneResult) -> bool {
        if generation != self.generation || self.is_cancelled() {
            return false;
        }
        let Some((details, path)) = self.retry_selection.clone() else {
            return false;
        };
        let (mime_type, preview) = match result {
            InfoPaneResult::Details { mime_type } => (mime_type, PreviewPresentation::DetailsOnly),
            InfoPaneResult::Preview {
                mime_type,
                document,
            } => {
                let preview = if document.kind() == PreviewKind::Text {
                    PreviewPresentation::Text(document)
                } else {
                    PreviewPresentation::Binary {
                        bytes_read: document.bytes_read(),
                    }
                };
                (mime_type, preview)
            }
            InfoPaneResult::Thumbnail { mime_type, path } => {
                (mime_type, PreviewPresentation::Thumbnail(path))
            }
        };
        self.cancel_active();
        self.state = InfoPaneState::Ready {
            details,
            path,
            mime_type,
            preview,
        };
        true
    }

    pub fn fail(&mut self, generation: u64, message: impl Into<Box<str>>) -> bool {
        if generation != self.generation || self.is_cancelled() {
            return false;
        }
        let Some((details, path)) = self.retry_selection.clone() else {
            return false;
        };
        self.cancel_active();
        self.state = InfoPaneState::Error {
            details,
            path,
            message: message.into(),
        };
        true
    }

    pub fn retry(&mut self) -> Option<InfoPaneWork> {
        let (details, path) = self.retry_selection.clone()?;
        Some(self.begin(details, path))
    }

    pub fn begin_load_more(&mut self) -> Option<InfoPaneLoadMoreWork> {
        let previous = std::mem::replace(&mut self.state, InfoPaneState::Empty);
        let InfoPaneState::Ready {
            details,
            path,
            mime_type,
            preview: PreviewPresentation::Text(document),
        } = previous
        else {
            self.state = previous;
            return None;
        };
        if !document.has_more() {
            self.state = InfoPaneState::Ready {
                details,
                path,
                mime_type,
                preview: PreviewPresentation::Text(document),
            };
            return None;
        }
        self.cancel_active();
        self.generation = self.generation.wrapping_add(1);
        let cancellation = CancellationToken::new();
        self.cancellation = Some(cancellation.clone());
        self.state = InfoPaneState::Loading { details, path };
        Some(InfoPaneLoadMoreWork {
            generation: self.generation,
            mime_type,
            document,
            cancellation,
        })
    }

    pub fn cancel_active(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
        }
    }

    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }
}

impl Drop for InfoPaneModel {
    fn drop(&mut self) {
        self.cancel_active();
    }
}
