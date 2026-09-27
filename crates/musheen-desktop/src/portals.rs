use musheen_core::{BoxFuture, CancellationToken};
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

const DESKTOP_PORTAL_DESTINATION: &str = "org.freedesktop.portal.Desktop";
const DESKTOP_PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const FILE_CHOOSER_INTERFACE: &str = "org.freedesktop.portal.FileChooser";
const REQUEST_INTERFACE: &str = "org.freedesktop.portal.Request";
pub const MUSHEEN_PORTAL_BACKEND: &str = "org.freedesktop.impl.portal.desktop.musheen";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxState {
    Host,
    Sandboxed,
}

impl SandboxState {
    #[must_use]
    pub fn detect() -> Self {
        if ashpd::is_sandboxed() {
            Self::Sandboxed
        } else {
            Self::Host
        }
    }
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

    fn routing_destination(&self) -> Option<&str> {
        None
    }
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
            && (self.own_backend.as_deref() == self.transport.routing_destination()
                || self.own_backend.as_deref() == request.backend_destination.as_deref())
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

#[derive(Clone, Debug)]
pub struct AshpdPortalTransport {
    connection: Option<zbus::Connection>,
    destination: Box<str>,
}

impl Default for AshpdPortalTransport {
    fn default() -> Self {
        Self {
            connection: None,
            destination: DESKTOP_PORTAL_DESTINATION.into(),
        }
    }
}

impl AshpdPortalTransport {
    #[must_use]
    pub fn with_connection(connection: zbus::Connection, destination: impl Into<Box<str>>) -> Self {
        Self {
            connection: Some(connection),
            destination: destination.into(),
        }
    }
}

impl PortalTransport for AshpdPortalTransport {
    fn choose(
        &self,
        request: PortalRequest,
        cancellation: CancellationToken,
    ) -> BoxFuture<'static, Result<PortalSelection, PortalError>> {
        let connection = self.connection.clone();
        let destination = self.destination.clone();
        Box::pin(
            async move { choose_via_dbus(connection, &destination, request, cancellation).await },
        )
    }

    fn routing_destination(&self) -> Option<&str> {
        Some(&self.destination)
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

async fn choose_via_dbus(
    connection: Option<zbus::Connection>,
    destination: &str,
    request: PortalRequest,
    cancellation: CancellationToken,
) -> Result<PortalSelection, PortalError> {
    use futures_lite::StreamExt as _;
    use std::collections::HashMap;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    let connection = match connection {
        Some(connection) => connection,
        None => zbus::Connection::session()
            .await
            .map_err(|error| PortalError::Unavailable(error.to_string().into()))?,
    };
    let token = ashpd::desktop::HandleToken::default();
    let sender = connection
        .unique_name()
        .ok_or_else(|| PortalError::Unavailable("the portal connection has no unique name".into()))?
        .trim_start_matches(':')
        .replace('.', "_");
    let predicted_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request_proxy = zbus::Proxy::new(
        &connection,
        destination,
        predicted_path.as_str(),
        REQUEST_INTERFACE,
    )
    .await
    .map_err(|error| PortalError::Unavailable(error.to_string().into()))?;
    let mut responses = request_proxy
        .receive_signal("Response")
        .await
        .map_err(|error| PortalError::Unavailable(error.to_string().into()))?;
    let chooser = zbus::Proxy::new(
        &connection,
        destination,
        DESKTOP_PORTAL_PATH,
        FILE_CHOOSER_INTERFACE,
    )
    .await
    .map_err(|error| PortalError::Unavailable(error.to_string().into()))?;
    let mut options = HashMap::<String, OwnedValue>::new();
    options.insert(
        "handle_token".into(),
        OwnedValue::from(zbus::zvariant::Str::from(token.to_string())),
    );
    let method = match request.kind() {
        PortalRequestKind::Open => "OpenFile",
        PortalRequestKind::Save => "SaveFile",
    };
    let portal_request = async {
        let reply = chooser
            .call_method(method, &("", request.title(), options))
            .await
            .map_err(|error| PortalError::Unavailable(error.to_string().into()))?;
        let returned_path = reply
            .body()
            .deserialize::<OwnedObjectPath>()
            .map_err(|error| PortalError::Unavailable(error.to_string().into()))?;
        if returned_path.as_str() != predicted_path {
            return Err(PortalError::Unavailable(
                "the portal returned an unexpected request identity".into(),
            ));
        }
        let signal = responses
            .next()
            .await
            .ok_or_else(|| PortalError::Unavailable("the portal request disconnected".into()))?;
        let (response, mut results) = signal
            .body()
            .deserialize::<(u32, HashMap<String, OwnedValue>)>()
            .map_err(|error| PortalError::Unavailable(error.to_string().into()))?;
        match response {
            0 => {}
            1 => return Err(PortalError::Cancelled),
            _ => {
                return Err(PortalError::Unavailable(
                    "the portal refused the file chooser request".into(),
                ));
            }
        }
        let uris = results
            .remove("uris")
            .ok_or(PortalError::InvalidSelection)
            .and_then(|value| {
                Vec::<String>::try_from(value).map_err(|_| PortalError::InvalidSelection)
            })?;
        selection_from_uris(&uris)
    };
    futures_lite::future::race(portal_request, async {
        Cancelled(cancellation).await;
        // Cancellation remains cancellation even if the portal disappeared while
        // the Close request was in flight. The explicit Close is best-effort and
        // prevents a live portal from keeping the abandoned chooser open.
        let _ = request_proxy.call_method("Close", &()).await;
        Err(PortalError::Cancelled)
    })
    .await
}

fn selection_from_uris(uris: &[String]) -> Result<PortalSelection, PortalError> {
    let mut paths = Vec::with_capacity(uris.len());
    for uri in uris {
        let path =
            crate::file_manager1::parse_file_uri(uri).map_err(|_| PortalError::InvalidSelection)?;
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
}

#[cfg(feature = "portal-backend")]
mod backend {
    use super::*;
    use ashpd::backend::file_chooser::FileChooserImpl;
    use ashpd::backend::request::RequestImpl;
    use ashpd::desktop::HandleToken;
    use ashpd::desktop::file_chooser::{
        FileFilter, OpenFileOptions, SaveFileOptions, SaveFilesOptions, SelectedFiles,
    };
    use ashpd::{MaybeAppID, WindowIdentifierType};
    use std::collections::HashMap;
    use std::ffi::OsStr;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum BackendChooserKind {
        Open,
        Save,
        SaveMany,
    }

    /// What a portal request asks the chooser for (SYS-027).
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct BackendChooserRequest {
        kind: BackendChooserKind,
        title: Box<str>,
        app_id: Option<Box<str>>,
        multiple: bool,
        directory: bool,
        current_folder: Option<PathBuf>,
        current_name: Option<Box<str>>,
        files: Vec<PathBuf>,
        filters: Vec<ChooserFilter>,
        current_filter: Option<usize>,
    }

    impl BackendChooserRequest {
        fn new(kind: BackendChooserKind, title: &str, app_id: Option<&MaybeAppID>) -> Self {
            Self {
                kind,
                title: title.into(),
                app_id: app_id.map(|id| id.to_string().into_boxed_str()),
                multiple: false,
                directory: false,
                current_folder: None,
                current_name: None,
                files: Vec::new(),
                filters: Vec::new(),
                current_filter: None,
            }
        }

        /// Takes the request's filters; a current filter the list lacks is
        /// added to it, as the portal allows.
        fn with_filters(mut self, filters: &[FileFilter], current: Option<&FileFilter>) -> Self {
            self.filters = filters.iter().map(ChooserFilter::from).collect();
            if let Some(current) = current.map(ChooserFilter::from) {
                let index = self
                    .filters
                    .iter()
                    .position(|filter| *filter == current)
                    .unwrap_or_else(|| {
                        self.filters.push(current);
                        self.filters.len() - 1
                    });
                self.current_filter = Some(index);
            }
            self
        }

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

        /// Whether Open may return several files.
        #[must_use]
        pub const fn multiple(&self) -> bool {
            self.multiple
        }

        /// Whether Open asks for folders instead of files.
        #[must_use]
        pub const fn directory(&self) -> bool {
            self.directory
        }

        /// The folder the chooser starts in, when the request names one.
        #[must_use]
        pub fn current_folder(&self) -> Option<&std::path::Path> {
            self.current_folder.as_deref()
        }

        /// The name Save suggests.
        #[must_use]
        pub fn current_name(&self) -> Option<&str> {
            self.current_name.as_deref()
        }

        /// The file names Save Many saves into the chosen folder.
        #[must_use]
        pub fn files(&self) -> &[PathBuf] {
            &self.files
        }

        #[must_use]
        pub fn filters(&self) -> &[ChooserFilter] {
            &self.filters
        }

        /// The index of the filter the chooser starts with.
        #[must_use]
        pub const fn current_filter(&self) -> Option<usize> {
            self.current_filter
        }
    }

    /// A filter a request offers: a label, and the name patterns and MIME
    /// types it matches.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ChooserFilter {
        label: Box<str>,
        patterns: Vec<Box<str>>,
        mime_types: Vec<Box<str>>,
    }

    impl ChooserFilter {
        #[must_use]
        pub fn label(&self) -> &str {
            &self.label
        }

        /// Whether the filter has MIME types, which need each file's type.
        #[must_use]
        pub fn needs_mime_types(&self) -> bool {
            !self.mime_types.is_empty()
        }

        /// Whether a file named `name`, whose name suggests `mime_type`,
        /// matches: a pattern matches the name, or a MIME type matches the
        /// file's (`image/*` matches every image type).
        #[must_use]
        pub fn matches(&self, name: &OsStr, mime_type: Option<&str>) -> bool {
            use std::os::unix::ffi::OsStrExt as _;

            self.patterns
                .iter()
                .any(|pattern| glob_matches(pattern.as_bytes(), name.as_bytes()))
                || mime_type.is_some_and(|mime_type| {
                    self.mime_types.iter().any(|wanted| {
                        wanted.eq_ignore_ascii_case(mime_type)
                            || wanted.strip_suffix("/*").is_some_and(|family| {
                                mime_type
                                    .split_once('/')
                                    .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case(family))
                            })
                    })
                })
        }
    }

    impl From<&FileFilter> for ChooserFilter {
        fn from(filter: &FileFilter) -> Self {
            Self {
                label: filter.label().into(),
                patterns: filter
                    .pattern_filters()
                    .into_iter()
                    .map(Into::into)
                    .collect(),
                mime_types: filter
                    .mimetype_filters()
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            }
        }
    }

    /// Whether `name` matches the shell pattern `pattern`: `*` matches any
    /// run, `?` one byte, and `[...]` one byte of a set or range, `!` or `^`
    /// first negating it.
    fn glob_matches(pattern: &[u8], name: &[u8]) -> bool {
        let (mut p, mut n) = (0, 0);
        let mut backtrack: Option<(usize, usize)> = None;
        while n < name.len() {
            if p < pattern.len() {
                match pattern[p] {
                    b'*' => {
                        backtrack = Some((p, n));
                        p += 1;
                        continue;
                    }
                    b'?' => {
                        p += 1;
                        n += 1;
                        continue;
                    }
                    b'[' => {
                        if let Some((matched, end)) = class_matches(&pattern[p..], name[n]) {
                            if matched {
                                p += end;
                                n += 1;
                                continue;
                            }
                        } else if name[n] == b'[' {
                            p += 1;
                            n += 1;
                            continue;
                        }
                    }
                    byte if byte == name[n] => {
                        p += 1;
                        n += 1;
                        continue;
                    }
                    _ => {}
                }
            }
            match backtrack {
                Some((star, taken)) => {
                    p = star + 1;
                    n = taken + 1;
                    backtrack = Some((star, taken + 1));
                }
                None => return false,
            }
        }
        pattern[p..].iter().all(|byte| *byte == b'*')
    }

    /// Whether `byte` is in the class at the start of `pattern`, and the
    /// class's length; `None` when the class is not closed.
    fn class_matches(pattern: &[u8], byte: u8) -> Option<(bool, usize)> {
        let mut index = 1;
        let negated = matches!(pattern.get(index), Some(b'!' | b'^'));
        if negated {
            index += 1;
        }
        let mut matched = false;
        let mut first = true;
        while let Some(&start) = pattern.get(index) {
            if start == b']' && !first {
                return Some((matched != negated, index + 1));
            }
            first = false;
            if pattern.get(index + 1) == Some(&b'-')
                && let Some(&end) = pattern.get(index + 2)
                && end != b']'
            {
                matched |= (start..=end).contains(&byte);
                index += 3;
            } else {
                matched |= start == byte;
                index += 1;
            }
        }
        None
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
            request: BackendChooserRequest,
        ) -> ashpd::backend::Result<SelectedFiles> {
            if request.app_id() == Some(self.own_app_id.as_ref()) {
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
            let (kind, multiple, names) = (request.kind, request.multiple, request.files.len());
            let decision = self.ui.choose(request, cancellation.clone()).await;
            self.cancellations
                .lock()
                .map_err(|_| ashpd::PortalError::Failed("request state is unavailable".into()))?
                .remove(&key);
            if cancellation.is_cancelled() {
                return Err(ashpd::PortalError::Cancelled(
                    "the user cancelled the chooser".into(),
                ));
            }
            // The chooser returns what the request asked for; anything else
            // is refused rather than passed on.
            let fits = |paths: &[PathBuf]| match kind {
                BackendChooserKind::Open => multiple || paths.len() == 1,
                BackendChooserKind::Save => paths.len() == 1,
                BackendChooserKind::SaveMany => paths.len() == names,
            };
            match decision {
                Ok(BackendChooserDecision::Confirmed(paths))
                    if !paths.is_empty() && fits(&paths) =>
                {
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
                Ok(BackendChooserDecision::Confirmed(_)) => {
                    Err(ashpd::PortalError::InvalidArgument(
                        "the selection does not fit the request".into(),
                    ))
                }
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
            options: OpenFileOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            let mut request =
                BackendChooserRequest::new(BackendChooserKind::Open, title, app_id.as_ref())
                    .with_filters(options.filters(), options.current_filter());
            request.multiple = options.multiple().unwrap_or(false);
            request.directory = options.directory().unwrap_or(false);
            request.current_folder = options
                .current_folder()
                .map(|folder| AsRef::<std::path::Path>::as_ref(folder).to_path_buf());
            self.handle(token, request).await
        }

        async fn save_file(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            _window_identifier: Option<WindowIdentifierType>,
            title: &str,
            options: SaveFileOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            let mut request =
                BackendChooserRequest::new(BackendChooserKind::Save, title, app_id.as_ref())
                    .with_filters(options.filters(), options.current_filter());
            // A current file names both the folder and the name.
            let current_file = options
                .current_file()
                .map(|file| AsRef::<std::path::Path>::as_ref(file).to_path_buf());
            request.current_folder = options
                .current_folder()
                .map(|folder| AsRef::<std::path::Path>::as_ref(folder).to_path_buf())
                .or_else(|| {
                    current_file
                        .as_deref()
                        .and_then(std::path::Path::parent)
                        .map(std::path::Path::to_path_buf)
                });
            request.current_name = options.current_name().map(Into::into).or_else(|| {
                current_file
                    .as_deref()
                    .and_then(std::path::Path::file_name)
                    .map(|name| name.to_string_lossy().into())
            });
            self.handle(token, request).await
        }

        async fn save_files(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            _window_identifier: Option<WindowIdentifierType>,
            title: &str,
            options: SaveFilesOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            let mut request =
                BackendChooserRequest::new(BackendChooserKind::SaveMany, title, app_id.as_ref());
            request.current_folder = options
                .current_folder()
                .map(|folder| AsRef::<std::path::Path>::as_ref(folder).to_path_buf());
            // Only plain names: a name with a folder in it could leave the
            // chosen folder.
            request.files = options
                .files()
                .iter()
                .map(|file| AsRef::<std::path::Path>::as_ref(file).to_path_buf())
                .filter(|file| {
                    let mut components = file.components();
                    matches!(
                        (components.next(), components.next()),
                        (Some(std::path::Component::Normal(_)), None)
                    )
                })
                .collect();
            if request.files.len() != options.files().len() || request.files.is_empty() {
                return Err(ashpd::PortalError::InvalidArgument(
                    "Save Many needs plain file names".into(),
                ));
            }
            self.handle(token, request).await
        }
    }

    struct FileChooserBackendInterface<U> {
        backend: Arc<AshpdFileChooserBackend<U>>,
    }

    struct FileChooserBackendRequest<U> {
        backend: Arc<AshpdFileChooserBackend<U>>,
        token: HandleToken,
    }

    /// Refuses a call from anyone but the portal service: the connection
    /// that owns org.freedesktop.portal.Desktop (SYS-027).
    async fn require_portal_service(
        connection: &zbus::Connection,
        header: &zbus::message::Header<'_>,
    ) -> ashpd::backend::Result<()> {
        let refused = || {
            ashpd::PortalError::NotAllowed(
                "only the portal service may call Musheen's portal backend".into(),
            )
        };
        let sender = header.sender().ok_or_else(refused)?;
        let portal = zbus::names::BusName::try_from(DESKTOP_PORTAL_DESTINATION)
            .map_err(|error| ashpd::PortalError::Failed(error.to_string()))?;
        let owner = zbus::fdo::DBusProxy::new(connection)
            .await
            .map_err(|error| ashpd::PortalError::Failed(error.to_string()))?
            .get_name_owner(portal)
            .await;
        match owner {
            Ok(owner) if owner.as_str() == sender.as_str() => Ok(()),
            _ => Err(refused()),
        }
    }

    #[zbus::interface(name = "org.freedesktop.impl.portal.Request")]
    impl<U: BackendChooserUi> FileChooserBackendRequest<U> {
        async fn close(
            &self,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> ashpd::backend::Result<()> {
            require_portal_service(connection, &header).await?;
            self.backend.close(self.token.clone()).await;
            Ok(())
        }
    }

    #[zbus::interface(name = "org.freedesktop.impl.portal.FileChooser")]
    impl<U: BackendChooserUi> FileChooserBackendInterface<U> {
        #[zbus(property(emits_changed_signal = "const"), name = "version")]
        fn version(&self) -> u32 {
            4
        }

        // The D-Bus signature fixes five arguments; zbus passes the object
        // server, the header and the connection as arguments too.
        #[allow(clippy::too_many_arguments)]
        #[zbus(name = "OpenFile", out_args("response", "results"))]
        async fn open_file(
            &self,
            handle: zbus::zvariant::OwnedObjectPath,
            app_id: ashpd::zvariant::Optional<MaybeAppID>,
            window_identifier: ashpd::zvariant::Optional<WindowIdentifierType>,
            title: String,
            options: OpenFileOptions,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> ashpd::backend::Result<ashpd::desktop::Response<SelectedFiles>> {
            require_portal_service(connection, &header).await?;
            let token = HandleToken::try_from(&handle)
                .map_err(|error| ashpd::PortalError::InvalidArgument(error.to_string()))?;
            server
                .at(
                    handle.clone(),
                    FileChooserBackendRequest {
                        backend: Arc::clone(&self.backend),
                        token: token.clone(),
                    },
                )
                .await?;
            let result = self
                .backend
                .open_file(
                    token,
                    app_id.into(),
                    window_identifier.into(),
                    &title,
                    options,
                )
                .await;
            server
                .remove::<FileChooserBackendRequest<U>, _>(&handle)
                .await?;
            match result {
                Ok(selected) => Ok(ashpd::desktop::Response::ok(selected)),
                Err(ashpd::PortalError::Cancelled(_)) => Ok(ashpd::desktop::Response::cancelled()),
                Err(error) => Err(error),
            }
        }

        // The D-Bus signature fixes five arguments; zbus passes the object
        // server, the header and the connection as arguments too.
        #[allow(clippy::too_many_arguments)]
        #[zbus(name = "SaveFile", out_args("response", "results"))]
        async fn save_file(
            &self,
            handle: zbus::zvariant::OwnedObjectPath,
            app_id: ashpd::zvariant::Optional<MaybeAppID>,
            window_identifier: ashpd::zvariant::Optional<WindowIdentifierType>,
            title: String,
            options: SaveFileOptions,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> ashpd::backend::Result<ashpd::desktop::Response<SelectedFiles>> {
            require_portal_service(connection, &header).await?;
            let token = HandleToken::try_from(&handle)
                .map_err(|error| ashpd::PortalError::InvalidArgument(error.to_string()))?;
            server
                .at(
                    handle.clone(),
                    FileChooserBackendRequest {
                        backend: Arc::clone(&self.backend),
                        token: token.clone(),
                    },
                )
                .await?;
            let result = self
                .backend
                .save_file(
                    token,
                    app_id.into(),
                    window_identifier.into(),
                    &title,
                    options,
                )
                .await;
            server
                .remove::<FileChooserBackendRequest<U>, _>(&handle)
                .await?;
            match result {
                Ok(selected) => Ok(ashpd::desktop::Response::ok(selected)),
                Err(ashpd::PortalError::Cancelled(_)) => Ok(ashpd::desktop::Response::cancelled()),
                Err(error) => Err(error),
            }
        }

        // The D-Bus signature fixes five arguments; zbus passes the object
        // server, the header and the connection as arguments too.
        #[allow(clippy::too_many_arguments)]
        #[zbus(name = "SaveFiles", out_args("response", "results"))]
        async fn save_files(
            &self,
            handle: zbus::zvariant::OwnedObjectPath,
            app_id: ashpd::zvariant::Optional<MaybeAppID>,
            window_identifier: ashpd::zvariant::Optional<WindowIdentifierType>,
            title: String,
            options: SaveFilesOptions,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> ashpd::backend::Result<ashpd::desktop::Response<SelectedFiles>> {
            require_portal_service(connection, &header).await?;
            let token = HandleToken::try_from(&handle)
                .map_err(|error| ashpd::PortalError::InvalidArgument(error.to_string()))?;
            server
                .at(
                    handle.clone(),
                    FileChooserBackendRequest {
                        backend: Arc::clone(&self.backend),
                        token: token.clone(),
                    },
                )
                .await?;
            let result = self
                .backend
                .save_files(
                    token,
                    app_id.into(),
                    window_identifier.into(),
                    &title,
                    options,
                )
                .await;
            server
                .remove::<FileChooserBackendRequest<U>, _>(&handle)
                .await?;
            match result {
                Ok(selected) => Ok(ashpd::desktop::Response::ok(selected)),
                Err(ashpd::PortalError::Cancelled(_)) => Ok(ashpd::desktop::Response::cancelled()),
                Err(error) => Err(error),
            }
        }
    }

    pub async fn serve_file_chooser_backend<U>(
        address: Option<&str>,
        ui: Arc<U>,
        service_name: &str,
        own_app_id: &str,
    ) -> Result<zbus::Connection, PortalError>
    where
        U: BackendChooserUi,
    {
        let builder = match address {
            Some(address) => zbus::connection::Builder::address(address),
            None => zbus::connection::Builder::session(),
        }
        .map_err(|error| PortalError::Unavailable(error.to_string().into()))?;
        builder
            .name(service_name)
            .map_err(|error| PortalError::Unavailable(error.to_string().into()))?
            .serve_at(
                DESKTOP_PORTAL_PATH,
                FileChooserBackendInterface {
                    backend: Arc::new(AshpdFileChooserBackend::new(ui, own_app_id)),
                },
            )
            .map_err(|error| PortalError::Unavailable(error.to_string().into()))?
            .build()
            .await
            .map_err(|error| PortalError::Unavailable(error.to_string().into()))
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
