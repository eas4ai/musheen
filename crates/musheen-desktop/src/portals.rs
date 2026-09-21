use musheen_core::{BoxFuture, CancellationToken};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxState {
    Host,
    Sandboxed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortalRequestKind {
    Open,
    Save,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalRequest {
    kind: PortalRequestKind,
    title: Box<str>,
    backend_destination: Option<Box<str>>,
}

impl PortalRequest {
    #[must_use]
    pub fn open(title: impl Into<Box<str>>) -> Self {
        Self::new(PortalRequestKind::Open, title)
    }

    #[must_use]
    pub fn save(title: impl Into<Box<str>>) -> Self {
        Self::new(PortalRequestKind::Save, title)
    }

    fn new(kind: PortalRequestKind, title: impl Into<Box<str>>) -> Self {
        Self {
            kind,
            title: title.into(),
            backend_destination: None,
        }
    }

    #[must_use]
    pub fn with_backend_destination(mut self, destination: impl Into<Box<str>>) -> Self {
        self.backend_destination = Some(destination.into());
        self
    }

    #[must_use]
    pub const fn kind(&self) -> PortalRequestKind {
        self.kind
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortalSelection {
    paths: Vec<PathBuf>,
    document_grants: bool,
}

impl PortalSelection {
    #[must_use]
    pub const fn new(paths: Vec<PathBuf>, document_grants: bool) -> Self {
        Self {
            paths,
            document_grants,
        }
    }

    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

pub trait PortalTransport: Send + Sync + 'static {
    fn choose(
        &self,
        request: PortalRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<PortalSelection, PortalError>>;
}

pub struct PortalClient<T> {
    transport: T,
    sandbox: SandboxState,
    own_backend: Option<Box<str>>,
}

impl<T: PortalTransport> PortalClient<T> {
    #[must_use]
    pub const fn new(transport: T, sandbox: SandboxState, own_backend: Option<Box<str>>) -> Self {
        Self {
            transport,
            sandbox,
            own_backend,
        }
    }

    pub async fn choose(
        &self,
        request: PortalRequest,
        cancellation: CancellationToken,
    ) -> Result<PortalSelection, PortalError> {
        if cancellation.is_cancelled() {
            return Err(PortalError::Cancelled);
        }
        if self.own_backend.is_some()
            && self.own_backend.as_deref() == request.backend_destination.as_deref()
        {
            return Err(PortalError::RecursiveBackend);
        }
        let selection = self.transport.choose(request, cancellation).await?;
        if self.sandbox == SandboxState::Sandboxed && !selection.document_grants {
            return Err(PortalError::MissingDocumentGrant);
        }
        Ok(selection)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortalError {
    Cancelled,
    MissingDocumentGrant,
    RecursiveBackend,
    InvalidSelection,
    Unavailable(Box<str>),
}

impl fmt::Display for PortalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("the portal request was cancelled"),
            Self::MissingDocumentGrant => {
                formatter.write_str("the portal did not grant access to the selected document")
            }
            Self::RecursiveBackend => {
                formatter.write_str("the portal client cannot call Musheen's own backend")
            }
            Self::InvalidSelection => {
                formatter.write_str("the portal returned an invalid selection")
            }
            Self::Unavailable(reason) => write!(formatter, "the portal is unavailable: {reason}"),
        }
    }
}

impl Error for PortalError {}

#[derive(Clone, Copy, Debug, Default)]
pub struct AshpdPortalTransport;

impl PortalTransport for AshpdPortalTransport {
    fn choose(
        &self,
        request: PortalRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<PortalSelection, PortalError>> {
        Box::pin(async move {
            let portal_request = async {
                let selected = match request.kind() {
                    PortalRequestKind::Open => {
                        ashpd::desktop::file_chooser::SelectedFiles::open_file()
                            .title(request.title())
                            .send()
                            .await
                            .and_then(|request| request.response())
                    }
                    PortalRequestKind::Save => {
                        ashpd::desktop::file_chooser::SelectedFiles::save_file()
                            .title(request.title())
                            .send()
                            .await
                            .and_then(|request| request.response())
                    }
                }
                .map_err(map_ashpd_error)?;
                let mut paths = Vec::with_capacity(selected.uris().len());
                for uri in selected.uris() {
                    let path = crate::file_manager1::parse_file_uri(uri.as_str())
                        .map_err(|_| PortalError::InvalidSelection)?;
                    paths.push(
                        path.as_unix_path()
                            .ok_or(PortalError::InvalidSelection)?
                            .to_path_buf(),
                    );
                }
                if paths.is_empty() {
                    return Err(PortalError::InvalidSelection);
                }
                Ok(PortalSelection::new(paths, true))
            };
            futures_lite::future::race(portal_request, async move {
                Cancelled(cancellation).await;
                Err(PortalError::Cancelled)
            })
            .await
        })
    }
}

struct Cancelled(CancellationToken);

impl Future for Cancelled {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.0.is_cancelled() {
            Poll::Ready(())
        } else {
            self.0.register_waker(context.waker());
            if self.0.is_cancelled() {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }
    }
}

fn map_ashpd_error(error: ashpd::Error) -> PortalError {
    match error {
        ashpd::Error::Response(ashpd::desktop::ResponseError::Cancelled)
        | ashpd::Error::Portal(ashpd::PortalError::Cancelled(_)) => PortalError::Cancelled,
        other => PortalError::Unavailable(other.to_string().into()),
    }
}

#[cfg(feature = "portal-backend")]
mod backend {
    use super::*;
    use ashpd::backend::file_chooser::FileChooserImpl;
    use ashpd::backend::request::RequestImpl;
    use ashpd::desktop::HandleToken;
    use ashpd::desktop::file_chooser::{
        OpenFileOptions, SaveFileOptions, SaveFilesOptions, SelectedFiles,
    };
    use ashpd::{MaybeAppID, WindowIdentifierType};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum BackendChooserKind {
        Open,
        Save,
        SaveMany,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct BackendChooserRequest {
        kind: BackendChooserKind,
        title: Box<str>,
        app_id: Option<Box<str>>,
    }

    impl BackendChooserRequest {
        #[must_use]
        pub const fn kind(&self) -> BackendChooserKind {
            self.kind
        }

        #[must_use]
        pub fn title(&self) -> &str {
            &self.title
        }

        #[must_use]
        pub fn app_id(&self) -> Option<&str> {
            self.app_id.as_deref()
        }
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum BackendChooserDecision {
        Confirmed(Vec<PathBuf>),
        Cancelled,
    }

    pub trait BackendChooserUi: Send + Sync + 'static {
        fn choose(
            &self,
            request: BackendChooserRequest,
            cancellation: CancellationToken,
        ) -> BoxFuture<'static, Result<BackendChooserDecision, PortalError>>;
    }

    pub struct AshpdFileChooserBackend<U> {
        ui: Arc<U>,
        own_app_id: Box<str>,
        cancellations: Mutex<HashMap<Box<str>, CancellationToken>>,
    }

    impl<U> AshpdFileChooserBackend<U> {
        #[must_use]
        pub fn new(ui: Arc<U>, own_app_id: impl Into<Box<str>>) -> Self {
            Self {
                ui,
                own_app_id: own_app_id.into(),
                cancellations: Mutex::new(HashMap::new()),
            }
        }
    }

    impl<U: BackendChooserUi> AshpdFileChooserBackend<U> {
        async fn handle(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            title: &str,
            kind: BackendChooserKind,
        ) -> ashpd::backend::Result<SelectedFiles> {
            if app_id
                .as_ref()
                .is_some_and(|app_id| app_id.to_string() == self.own_app_id.as_ref())
            {
                return Err(ashpd::PortalError::NotAllowed(
                    "Musheen cannot route its portal client into its own backend".into(),
                ));
            }
            let key = token.to_string().into_boxed_str();
            let cancellation = CancellationToken::new();
            self.cancellations
                .lock()
                .map_err(|_| ashpd::PortalError::Failed("request state is unavailable".into()))?
                .insert(key.clone(), cancellation.clone());
            let request = BackendChooserRequest {
                kind,
                title: title.into(),
                app_id: app_id.map(|id| id.to_string().into_boxed_str()),
            };
            let decision = self.ui.choose(request, cancellation).await;
            self.cancellations
                .lock()
                .map_err(|_| ashpd::PortalError::Failed("request state is unavailable".into()))?
                .remove(&key);
            match decision {
                Ok(BackendChooserDecision::Confirmed(paths)) if !paths.is_empty() => {
                    let mut selected = SelectedFiles::default();
                    for path in paths {
                        let uri = path_to_uri(&path).map_err(|error| {
                            ashpd::PortalError::InvalidArgument(error.to_string())
                        })?;
                        selected = selected.uri(ashpd::Uri::parse(&uri).map_err(|error| {
                            ashpd::PortalError::InvalidArgument(error.to_string())
                        })?);
                    }
                    Ok(selected)
                }
                Ok(BackendChooserDecision::Confirmed(_)) => Err(
                    ashpd::PortalError::InvalidArgument("no files were selected".into()),
                ),
                Ok(BackendChooserDecision::Cancelled) | Err(PortalError::Cancelled) => Err(
                    ashpd::PortalError::Cancelled("the user cancelled the chooser".into()),
                ),
                Err(error) => Err(ashpd::PortalError::Failed(error.to_string())),
            }
        }
    }

    #[ashpd::async_trait::async_trait]
    impl<U: BackendChooserUi> RequestImpl for AshpdFileChooserBackend<U> {
        async fn close(&self, token: HandleToken) {
            if let Ok(cancellations) = self.cancellations.lock()
                && let Some(cancellation) = cancellations.get(token.to_string().as_str())
            {
                cancellation.cancel();
            }
        }
    }

    #[ashpd::async_trait::async_trait]
    impl<U: BackendChooserUi> FileChooserImpl for AshpdFileChooserBackend<U> {
        async fn open_file(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            _window_identifier: Option<WindowIdentifierType>,
            title: &str,
            _options: OpenFileOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            self.handle(token, app_id, title, BackendChooserKind::Open)
                .await
        }

        async fn save_file(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            _window_identifier: Option<WindowIdentifierType>,
            title: &str,
            _options: SaveFileOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            self.handle(token, app_id, title, BackendChooserKind::Save)
                .await
        }

        async fn save_files(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            _window_identifier: Option<WindowIdentifierType>,
            title: &str,
            _options: SaveFilesOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            self.handle(token, app_id, title, BackendChooserKind::SaveMany)
                .await
        }
    }

    #[cfg(unix)]
    fn path_to_uri(path: &std::path::Path) -> Result<String, PortalError> {
        use std::os::unix::ffi::OsStrExt as _;

        if !path.is_absolute() {
            return Err(PortalError::InvalidSelection);
        }
        let mut uri = String::from("file://");
        for byte in path.as_os_str().as_bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(byte) {
                uri.push(char::from(*byte));
            } else {
                use std::fmt::Write as _;
                write!(uri, "%{byte:02X}").expect("writing to a String cannot fail");
            }
        }
        Ok(uri)
    }

    #[cfg(not(unix))]
    fn path_to_uri(_path: &std::path::Path) -> Result<String, PortalError> {
        Err(PortalError::InvalidSelection)
    }
}

#[cfg(feature = "portal-backend")]
pub use backend::*;
