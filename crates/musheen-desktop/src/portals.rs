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
    use crate::NameMimeTypes;
    use ashpd::backend::file_chooser::FileChooserImpl;
    use ashpd::backend::request::RequestImpl;
    use ashpd::desktop::HandleToken;
    use ashpd::desktop::file_chooser::{
        Choice, FileFilter, OpenFileOptions, SaveFileOptions, SaveFilesOptions, SelectedFiles,
    };
    use ashpd::{MaybeAppID, WindowIdentifierType};
    use std::collections::HashMap;
    use std::ffi::{OsStr, OsString};
    use std::path::Path;
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
        accept_label: Option<Box<str>>,
        multiple: bool,
        directory: bool,
        current_folder: Option<PathBuf>,
        current_name: Option<OsString>,
        files: Vec<PathBuf>,
        filters: Vec<ChooserFilter>,
        current_filter: Option<usize>,
        choices: Vec<ChooserChoice>,
    }

    impl BackendChooserRequest {
        /// A request to open one file, titled `title`, as Musheen makes for
        /// itself when its backend is on.
        #[must_use]
        pub fn open(title: &str) -> Self {
            Self::new(BackendChooserKind::Open, title, None)
        }

        /// Starts the chooser in `folder`.
        #[must_use]
        pub fn with_current_folder(mut self, folder: PathBuf) -> Self {
            self.current_folder = Some(folder);
            self
        }

        fn new(kind: BackendChooserKind, title: &str, app_id: Option<&MaybeAppID>) -> Self {
            Self {
                kind,
                title: title.into(),
                app_id: app_id.map(|id| id.to_string().into_boxed_str()),
                accept_label: None,
                multiple: false,
                directory: false,
                current_folder: None,
                current_name: None,
                files: Vec::new(),
                filters: Vec::new(),
                current_filter: None,
                choices: Vec::new(),
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

        fn with_choices(mut self, choices: &[Choice], accept_label: Option<&str>) -> Self {
            self.choices = choices.iter().map(ChooserChoice::from).collect();
            self.accept_label = accept_label.map(Into::into);
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

        /// The label the request asks for on the accept button.
        #[must_use]
        pub fn accept_label(&self) -> Option<&str> {
            self.accept_label.as_deref()
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
        pub fn current_folder(&self) -> Option<&Path> {
            self.current_folder.as_deref()
        }

        /// The name Save suggests, exactly as the request gave it.
        #[must_use]
        pub fn current_name(&self) -> Option<&OsStr> {
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

        /// The choices the request offers beside the list.
        #[must_use]
        pub fn choices(&self) -> &[ChooserChoice] {
            &self.choices
        }
    }

    /// A choice a request offers: a checkbox when it has no options, or one
    /// option of several.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct ChooserChoice {
        id: Box<str>,
        label: Box<str>,
        options: Vec<(Box<str>, Box<str>)>,
        initial: Box<str>,
    }

    impl ChooserChoice {
        #[must_use]
        pub fn id(&self) -> &str {
            &self.id
        }

        #[must_use]
        pub fn label(&self) -> &str {
            &self.label
        }

        /// Each option's ID and label; none for a checkbox.
        #[must_use]
        pub fn options(&self) -> &[(Box<str>, Box<str>)] {
            &self.options
        }

        /// The value the choice starts with: an option's ID, or `true` or
        /// `false` for a checkbox.
        #[must_use]
        pub fn initial(&self) -> &str {
            &self.initial
        }
    }

    impl From<&Choice> for ChooserChoice {
        fn from(choice: &Choice) -> Self {
            let options: Vec<(Box<str>, Box<str>)> = choice
                .pairs()
                .into_iter()
                .map(|(id, label)| (id.into(), label.into()))
                .collect();
            let initial = if options.is_empty() {
                if choice.initial_selection() == "true" {
                    "true"
                } else {
                    "false"
                }
                .into()
            } else if options
                .iter()
                .any(|(id, _)| id.as_ref() == choice.initial_selection())
            {
                choice.initial_selection().into()
            } else {
                options[0].0.clone()
            };
            Self {
                id: choice.id().into(),
                label: choice.label().into(),
                options,
                initial,
            }
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
        /// matches: a pattern (wax syntax) matches the name, or the file's
        /// MIME type is one of the filter's, an alias of one or a subclass
        /// of one, as `types` knows them (`text/x-csrc` is a `text/plain`;
        /// `image/*` takes every image type). Without `types`, only equal
        /// types and families match.
        #[must_use]
        pub fn matches(
            &self,
            name: &OsStr,
            mime_type: Option<&str>,
            types: Option<&NameMimeTypes>,
        ) -> bool {
            self.patterns
                .iter()
                .any(|pattern| glob_matches(pattern, name))
                || mime_type.is_some_and(|mime_type| {
                    self.mime_types.iter().any(|wanted| match types {
                        Some(types) => types.is_a(mime_type, wanted),
                        None => NameMimeTypes::same_or_family(mime_type, wanted),
                    })
                })
        }

        fn to_file_filter(&self) -> FileFilter {
            let filter = self
                .patterns
                .iter()
                .fold(FileFilter::new(&self.label), |filter, pattern| {
                    filter.glob(pattern)
                });
            self.mime_types
                .iter()
                .fold(filter, |filter, mime_type| filter.mimetype(mime_type))
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

    /// Whether `name` matches the shell pattern `pattern`, as wax reads it
    /// (DEP-008). A pattern wax refuses matches nothing.
    fn glob_matches(pattern: &str, name: &OsStr) -> bool {
        use wax::Program as _;

        wax::Glob::new(pattern).is_ok_and(|glob| glob.is_match(Path::new(name)))
    }

    /// The user's confirmed selection, with the filter and choice values
    /// they left chosen.
    #[derive(Clone, Debug, Default, Eq, PartialEq)]
    pub struct ChooserSelection {
        pub paths: Vec<PathBuf>,
        /// The index of the chosen filter in the request's filters.
        pub filter: Option<usize>,
        /// Each choice's ID and chosen value.
        pub choices: Vec<(Box<str>, Box<str>)>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub enum BackendChooserDecision {
        Confirmed(Vec<PathBuf>),
        Selected(ChooserSelection),
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
        /// Pending requests by their full handle: a token is unique only
        /// for the app that chose it.
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

        fn cancel(&self, key: &str) {
            if let Ok(cancellations) = self.cancellations.lock()
                && let Some(cancellation) = cancellations.get(key)
            {
                cancellation.cancel();
            }
        }
    }

    fn open_request(
        title: &str,
        app_id: Option<&MaybeAppID>,
        options: &OpenFileOptions,
    ) -> BackendChooserRequest {
        let mut request = BackendChooserRequest::new(BackendChooserKind::Open, title, app_id)
            .with_filters(options.filters(), options.current_filter())
            .with_choices(options.choices(), options.accept_label());
        request.multiple = options.multiple().unwrap_or(false);
        request.directory = options.directory().unwrap_or(false);
        request.current_folder = options
            .current_folder()
            .map(|folder| AsRef::<Path>::as_ref(folder).to_path_buf());
        request
    }

    fn save_request(
        title: &str,
        app_id: Option<&MaybeAppID>,
        options: &SaveFileOptions,
    ) -> BackendChooserRequest {
        let mut request = BackendChooserRequest::new(BackendChooserKind::Save, title, app_id)
            .with_filters(options.filters(), options.current_filter())
            .with_choices(options.choices(), options.accept_label());
        // A current file names both the folder and the name.
        let current_file = options
            .current_file()
            .map(|file| AsRef::<Path>::as_ref(file).to_path_buf());
        request.current_folder = options
            .current_folder()
            .map(|folder| AsRef::<Path>::as_ref(folder).to_path_buf())
            .or_else(|| {
                current_file
                    .as_deref()
                    .and_then(Path::parent)
                    .map(Path::to_path_buf)
            });
        request.current_name = options.current_name().map(OsString::from).or_else(|| {
            current_file
                .as_deref()
                .and_then(Path::file_name)
                .map(OsStr::to_os_string)
        });
        request
    }

    fn save_many_request(
        title: &str,
        app_id: Option<&MaybeAppID>,
        options: &SaveFilesOptions,
    ) -> ashpd::backend::Result<BackendChooserRequest> {
        let mut request = BackendChooserRequest::new(BackendChooserKind::SaveMany, title, app_id)
            .with_choices(options.choices(), options.accept_label());
        request.current_folder = options
            .current_folder()
            .map(|folder| AsRef::<Path>::as_ref(folder).to_path_buf());
        // Only plain names: a name with a folder in it could leave the
        // chosen folder.
        request.files = options
            .files()
            .iter()
            .map(|file| AsRef::<Path>::as_ref(file).to_path_buf())
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
        Ok(request)
    }

    /// Whether `paths` is what a request of `kind` asked for: one file for
    /// Open unless `multiple`, one path for Save, and one path per name for
    /// Save Many.
    fn counts_fit(
        kind: BackendChooserKind,
        multiple: bool,
        names: usize,
        paths: &[PathBuf],
    ) -> bool {
        !paths.is_empty()
            && match kind {
                BackendChooserKind::Open => multiple || paths.len() == 1,
                BackendChooserKind::Save => paths.len() == 1,
                BackendChooserKind::SaveMany => paths.len() == names,
            }
    }

    /// Whether each path is the kind of item the request asked for: a
    /// folder for a folder request; a file, not a folder, to open; and, to
    /// save, a path that is not a folder, in a folder that exists.
    fn kinds_fit(kind: BackendChooserKind, directory: bool, paths: &[PathBuf]) -> bool {
        let is_folder = |path: &Path| std::fs::metadata(path).is_ok_and(|item| item.is_dir());
        paths.iter().all(|path| match kind {
            BackendChooserKind::Open if directory => is_folder(path),
            BackendChooserKind::Open => std::fs::metadata(path).is_ok_and(|item| !item.is_dir()),
            BackendChooserKind::Save | BackendChooserKind::SaveMany => {
                path.parent().is_some_and(is_folder) && !is_folder(path)
            }
        })
    }

    impl<U: BackendChooserUi> AshpdFileChooserBackend<U> {
        /// Answers `request`, known by `key`, the full request handle.
        async fn handle(
            &self,
            key: Box<str>,
            request: BackendChooserRequest,
        ) -> ashpd::backend::Result<SelectedFiles> {
            if request.app_id() == Some(self.own_app_id.as_ref()) {
                return Err(ashpd::PortalError::NotAllowed(
                    "Musheen cannot route its portal client into its own backend".into(),
                ));
            }
            let cancellation = CancellationToken::new();
            {
                let mut cancellations = self.cancellations.lock().map_err(|_| {
                    ashpd::PortalError::Failed("request state is unavailable".into())
                })?;
                if cancellations.contains_key(&key) {
                    return Err(ashpd::PortalError::InvalidArgument(
                        "the request handle is already in use".into(),
                    ));
                }
                cancellations.insert(key.clone(), cancellation.clone());
            }
            let (kind, multiple, directory, names) = (
                request.kind,
                request.multiple,
                request.directory,
                request.files.len(),
            );
            let filters = request.filters.clone();
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
            let selection = match decision {
                Ok(BackendChooserDecision::Confirmed(paths)) => ChooserSelection {
                    paths,
                    ..ChooserSelection::default()
                },
                Ok(BackendChooserDecision::Selected(selection)) => selection,
                Ok(BackendChooserDecision::Cancelled) | Err(PortalError::Cancelled) => {
                    return Err(ashpd::PortalError::Cancelled(
                        "the user cancelled the chooser".into(),
                    ));
                }
                Err(error) => return Err(ashpd::PortalError::Failed(error.to_string())),
            };
            // The chooser returns what the request asked for; anything else
            // is refused rather than passed on.
            if !counts_fit(kind, multiple, names, &selection.paths)
                || !kinds_fit(kind, directory, &selection.paths)
            {
                return Err(ashpd::PortalError::InvalidArgument(
                    "the selection does not fit the request".into(),
                ));
            }
            let mut selected = SelectedFiles::default();
            for path in &selection.paths {
                let uri = path_to_uri(path)
                    .map_err(|error| ashpd::PortalError::InvalidArgument(error.to_string()))?;
                selected = selected.uri(
                    ashpd::Uri::parse(&uri)
                        .map_err(|error| ashpd::PortalError::InvalidArgument(error.to_string()))?,
                );
            }
            // A file the user opens through Musheen may be saved back.
            if kind == BackendChooserKind::Open && !directory {
                selected = selected.writable(true);
            }
            if let Some(filter) = selection.filter.and_then(|index| filters.get(index)) {
                selected = selected.current_filter(filter.to_file_filter());
            }
            for (id, value) in &selection.choices {
                selected = selected.choice(id, value);
            }
            Ok(selected)
        }
    }

    #[ashpd::async_trait::async_trait]
    impl<U: BackendChooserUi> RequestImpl for AshpdFileChooserBackend<U> {
        async fn close(&self, token: HandleToken) {
            self.cancel(&token.to_string());
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
            let request = open_request(title, app_id.as_ref(), &options);
            self.handle(token.to_string().into(), request).await
        }

        async fn save_file(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            _window_identifier: Option<WindowIdentifierType>,
            title: &str,
            options: SaveFileOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            let request = save_request(title, app_id.as_ref(), &options);
            self.handle(token.to_string().into(), request).await
        }

        async fn save_files(
            &self,
            token: HandleToken,
            app_id: Option<MaybeAppID>,
            _window_identifier: Option<WindowIdentifierType>,
            title: &str,
            options: SaveFilesOptions,
        ) -> ashpd::backend::Result<SelectedFiles> {
            let request = save_many_request(title, app_id.as_ref(), &options)?;
            self.handle(token.to_string().into(), request).await
        }
    }

    struct FileChooserBackendInterface<U> {
        backend: Arc<AshpdFileChooserBackend<U>>,
    }

    struct FileChooserBackendRequest<U> {
        backend: Arc<AshpdFileChooserBackend<U>>,
        key: Box<str>,
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
            self.backend.cancel(&self.key);
            Ok(())
        }
    }

    impl<U: BackendChooserUi> FileChooserBackendInterface<U> {
        /// Serves one request: exports its Request object at `handle` for
        /// Close, answers it, and removes the object again.
        async fn serve(
            &self,
            handle: zbus::zvariant::OwnedObjectPath,
            request: ashpd::backend::Result<BackendChooserRequest>,
            server: &zbus::ObjectServer,
        ) -> ashpd::backend::Result<ashpd::desktop::Response<SelectedFiles>> {
            let request = request?;
            let key: Box<str> = handle.as_str().into();
            server
                .at(
                    handle.clone(),
                    FileChooserBackendRequest {
                        backend: Arc::clone(&self.backend),
                        key: key.clone(),
                    },
                )
                .await?;
            let result = self.backend.handle(key, request).await;
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
            _window_identifier: ashpd::zvariant::Optional<WindowIdentifierType>,
            title: String,
            options: OpenFileOptions,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> ashpd::backend::Result<ashpd::desktop::Response<SelectedFiles>> {
            require_portal_service(connection, &header).await?;
            let app_id: Option<MaybeAppID> = app_id.into();
            let request = open_request(&title, app_id.as_ref(), &options);
            self.serve(handle, Ok(request), server).await
        }

        // The D-Bus signature fixes five arguments; zbus passes the object
        // server, the header and the connection as arguments too.
        #[allow(clippy::too_many_arguments)]
        #[zbus(name = "SaveFile", out_args("response", "results"))]
        async fn save_file(
            &self,
            handle: zbus::zvariant::OwnedObjectPath,
            app_id: ashpd::zvariant::Optional<MaybeAppID>,
            _window_identifier: ashpd::zvariant::Optional<WindowIdentifierType>,
            title: String,
            options: SaveFileOptions,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> ashpd::backend::Result<ashpd::desktop::Response<SelectedFiles>> {
            require_portal_service(connection, &header).await?;
            let app_id: Option<MaybeAppID> = app_id.into();
            let request = save_request(&title, app_id.as_ref(), &options);
            self.serve(handle, Ok(request), server).await
        }

        // The D-Bus signature fixes five arguments; zbus passes the object
        // server, the header and the connection as arguments too.
        #[allow(clippy::too_many_arguments)]
        #[zbus(name = "SaveFiles", out_args("response", "results"))]
        async fn save_files(
            &self,
            handle: zbus::zvariant::OwnedObjectPath,
            app_id: ashpd::zvariant::Optional<MaybeAppID>,
            _window_identifier: ashpd::zvariant::Optional<WindowIdentifierType>,
            title: String,
            options: SaveFilesOptions,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            #[zbus(header)] header: zbus::message::Header<'_>,
            #[zbus(connection)] connection: &zbus::Connection,
        ) -> ashpd::backend::Result<ashpd::desktop::Response<SelectedFiles>> {
            require_portal_service(connection, &header).await?;
            let app_id: Option<MaybeAppID> = app_id.into();
            let request = save_many_request(&title, app_id.as_ref(), &options);
            self.serve(handle, request, server).await
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
