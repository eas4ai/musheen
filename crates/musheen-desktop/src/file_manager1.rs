use musheen_core::{BoxFuture, StorePath};
use std::error::Error;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;

pub const FILE_MANAGER_NAME: &str = "org.freedesktop.FileManager1";
pub const MUSHEEN_FILE_MANAGER_NAME: &str = "org.musheen.FileManager1";
pub const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";
const MAX_URIS: usize = 256;
const MAX_URI_BYTES: usize = 16 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileManagerRequest {
    ShowItems {
        locations: Vec<StorePath>,
        startup_id: Box<str>,
    },
    ShowFolders {
        locations: Vec<StorePath>,
        startup_id: Box<str>,
    },
    ShowItemProperties {
        locations: Vec<StorePath>,
        startup_id: Box<str>,
    },
}

impl FileManagerRequest {
    #[must_use]
    pub fn locations(&self) -> &[StorePath] {
        match self {
            Self::ShowItems { locations, .. }
            | Self::ShowFolders { locations, .. }
            | Self::ShowItemProperties { locations, .. } => locations,
        }
    }

    #[must_use]
    pub fn startup_id(&self) -> &str {
        match self {
            Self::ShowItems { startup_id, .. }
            | Self::ShowFolders { startup_id, .. }
            | Self::ShowItemProperties { startup_id, .. } => startup_id,
        }
    }
}

pub trait FileManagerRequestSink: Send + Sync + 'static {
    fn submit(
        &self,
        request: FileManagerRequest,
    ) -> BoxFuture<'static, Result<(), FileManagerError>>;
}

#[derive(Debug)]
pub struct FileManagerRequestEnvelope {
    request: FileManagerRequest,
    acknowledgement: async_channel::Sender<Result<(), FileManagerError>>,
}

#[derive(Clone, Debug)]
pub struct FileManagerAcknowledgement(async_channel::Sender<Result<(), FileManagerError>>);

impl FileManagerAcknowledgement {
    #[must_use]
    pub fn channel() -> (Self, async_channel::Receiver<Result<(), FileManagerError>>) {
        let (sender, receiver) = async_channel::bounded(1);
        (Self(sender), receiver)
    }

    pub fn complete(&self, result: Result<(), FileManagerError>) {
        let _ = self.0.try_send(result);
    }
}

impl FileManagerRequestEnvelope {
    #[must_use]
    pub fn new(
        request: FileManagerRequest,
    ) -> (Self, async_channel::Receiver<Result<(), FileManagerError>>) {
        let (acknowledgement, response) = async_channel::bounded(1);
        (
            Self {
                request,
                acknowledgement,
            },
            response,
        )
    }

    #[must_use]
    pub const fn request(&self) -> &FileManagerRequest {
        &self.request
    }

    pub fn acknowledge(self, result: Result<(), FileManagerError>) {
        let _ = self.acknowledgement.try_send(result);
    }

    #[must_use]
    pub fn into_parts(self) -> (FileManagerRequest, FileManagerAcknowledgement) {
        (
            self.request,
            FileManagerAcknowledgement(self.acknowledgement),
        )
    }
}

impl FileManagerRequestSink for async_channel::Sender<FileManagerRequestEnvelope> {
    fn submit(
        &self,
        request: FileManagerRequest,
    ) -> BoxFuture<'static, Result<(), FileManagerError>> {
        let sender = self.clone();
        Box::pin(async move {
            let (envelope, response) = FileManagerRequestEnvelope::new(request);
            sender.try_send(envelope).map_err(|error| match error {
                async_channel::TrySendError::Full(_) => FileManagerError::Busy,
                async_channel::TrySendError::Closed(_) => FileManagerError::Unavailable,
            })?;
            futures_lite::future::race(
                async {
                    response
                        .recv()
                        .await
                        .map_err(|_| FileManagerError::Unavailable)?
                },
                async {
                    async_io::Timer::after(REQUEST_TIMEOUT).await;
                    Err(FileManagerError::TimedOut)
                },
            )
            .await
        })
    }
}

#[derive(Clone)]
pub struct FileManager1 {
    sink: Arc<dyn FileManagerRequestSink>,
}

impl FileManager1 {
    #[must_use]
    pub fn new(sink: Arc<dyn FileManagerRequestSink>) -> Self {
        Self { sink }
    }

    pub async fn show_items(
        &self,
        uris: &[&str],
        startup_id: &str,
    ) -> Result<(), FileManagerError> {
        self.dispatch(uris, startup_id, |locations, startup_id| {
            FileManagerRequest::ShowItems {
                locations,
                startup_id,
            }
        })
        .await
    }

    pub async fn show_folders(
        &self,
        uris: &[&str],
        startup_id: &str,
    ) -> Result<(), FileManagerError> {
        self.dispatch(uris, startup_id, |locations, startup_id| {
            FileManagerRequest::ShowFolders {
                locations,
                startup_id,
            }
        })
        .await
    }

    pub async fn show_item_properties(
        &self,
        uris: &[&str],
        startup_id: &str,
    ) -> Result<(), FileManagerError> {
        self.dispatch(uris, startup_id, |locations, startup_id| {
            FileManagerRequest::ShowItemProperties {
                locations,
                startup_id,
            }
        })
        .await
    }

    async fn dispatch(
        &self,
        uris: &[&str],
        startup_id: &str,
        request: impl FnOnce(Vec<StorePath>, Box<str>) -> FileManagerRequest,
    ) -> Result<(), FileManagerError> {
        if uris.is_empty() || uris.len() > MAX_URIS || startup_id.len() > MAX_URI_BYTES {
            return Err(FileManagerError::InvalidRequest);
        }
        let locations = uris
            .iter()
            .map(|uri| parse_file_uri(uri))
            .collect::<Result<Vec<_>, _>>()?;
        validate_local_locations(locations.clone()).await?;
        self.sink
            .submit(request(locations, startup_id.into()))
            .await
    }
}

#[zbus::interface(name = "org.freedesktop.FileManager1")]
impl FileManager1 {
    #[zbus(name = "ShowItems")]
    async fn dbus_show_items(
        &self,
        uris: Vec<String>,
        startup_id: String,
    ) -> zbus::fdo::Result<()> {
        let uris = uris.iter().map(String::as_str).collect::<Vec<_>>();
        self.show_items(&uris, &startup_id)
            .await
            .map_err(dbus_error)
    }

    #[zbus(name = "ShowFolders")]
    async fn dbus_show_folders(
        &self,
        uris: Vec<String>,
        startup_id: String,
    ) -> zbus::fdo::Result<()> {
        let uris = uris.iter().map(String::as_str).collect::<Vec<_>>();
        self.show_folders(&uris, &startup_id)
            .await
            .map_err(dbus_error)
    }

    #[zbus(name = "ShowItemProperties")]
    async fn dbus_show_item_properties(
        &self,
        uris: Vec<String>,
        startup_id: String,
    ) -> zbus::fdo::Result<()> {
        let uris = uris.iter().map(String::as_str).collect::<Vec<_>>();
        self.show_item_properties(&uris, &startup_id)
            .await
            .map_err(dbus_error)
    }
}

async fn validate_local_locations(locations: Vec<StorePath>) -> Result<(), FileManagerError> {
    let (completed, result) = async_channel::bounded(1);
    std::thread::Builder::new()
        .name("musheen-file-manager1-validation".into())
        .spawn(move || {
            let outcome = locations.iter().try_for_each(|location| {
                let path = location
                    .as_unix_path()
                    .ok_or(FileManagerError::UnsupportedUri)?;
                std::fs::metadata(path)
                    .map(|_| ())
                    .map_err(|_| FileManagerError::Unreachable)
            });
            let _ = completed.try_send(outcome);
        })
        .map_err(|error| FileManagerError::Service(error.to_string().into()))?;
    futures_lite::future::race(
        async {
            result
                .recv()
                .await
                .unwrap_or(Err(FileManagerError::Unavailable))
        },
        async {
            async_io::Timer::after(REQUEST_TIMEOUT).await;
            Err(FileManagerError::TimedOut)
        },
    )
    .await
}

pub async fn serve_file_manager1(
    address: Option<&str>,
    sink: Arc<dyn FileManagerRequestSink>,
) -> Result<zbus::Connection, FileManagerError> {
    serve_file_manager1_named(address, FILE_MANAGER_NAME, sink).await
}

pub async fn serve_file_manager1_named(
    address: Option<&str>,
    name: &'static str,
    sink: Arc<dyn FileManagerRequestSink>,
) -> Result<zbus::Connection, FileManagerError> {
    let builder = match address {
        Some(address) => zbus::connection::Builder::address(address),
        None => zbus::connection::Builder::session(),
    }
    .map_err(|error| FileManagerError::Service(error.to_string().into()))?;
    builder
        .name(name)
        .map_err(|error| FileManagerError::Service(error.to_string().into()))?
        .serve_at(FILE_MANAGER_PATH, FileManager1::new(sink))
        .map_err(|error| FileManagerError::Service(error.to_string().into()))?
        .build()
        .await
        .map_err(|error| FileManagerError::Service(error.to_string().into()))
}

pub async fn forward_show_folders_to_musheen(
    address: Option<&str>,
    locations: &[StorePath],
    startup_id: &str,
) -> Result<(), FileManagerError> {
    if locations.is_empty() || locations.len() > MAX_URIS || startup_id.len() > MAX_URI_BYTES {
        return Err(FileManagerError::InvalidRequest);
    }
    let uris = locations
        .iter()
        .map(file_uri)
        .collect::<Result<Vec<_>, _>>()?;
    let connection = match address {
        Some(address) => zbus::connection::Builder::address(address),
        None => zbus::connection::Builder::session(),
    }
    .map_err(|error| FileManagerError::Service(error.to_string().into()))?
    .build()
    .await
    .map_err(|error| FileManagerError::Service(error.to_string().into()))?;
    let proxy = zbus::Proxy::new(
        &connection,
        MUSHEEN_FILE_MANAGER_NAME,
        FILE_MANAGER_PATH,
        FILE_MANAGER_NAME,
    )
    .await
    .map_err(|error| FileManagerError::Service(error.to_string().into()))?;
    proxy
        .call_method("ShowFolders", &(uris, startup_id))
        .await
        .map_err(|error| FileManagerError::Service(error.to_string().into()))?;
    Ok(())
}

fn file_uri(location: &StorePath) -> Result<String, FileManagerError> {
    let path = location
        .as_unix_path()
        .ok_or(FileManagerError::UnsupportedUri)?;
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str());
    #[cfg(not(unix))]
    let bytes = path
        .to_str()
        .ok_or(FileManagerError::InvalidUri)?
        .as_bytes();
    let mut uri = String::with_capacity(bytes.len().saturating_mul(3).saturating_add(7));
    uri.push_str("file://");
    for byte in bytes {
        match *byte {
            b'/' | b'-' | b'.' | b'_' | b'~' | b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => {
                uri.push(char::from(*byte));
            }
            byte => {
                use std::fmt::Write as _;
                write!(&mut uri, "%{byte:02X}").expect("writing to a String cannot fail");
            }
        }
    }
    if uri.len() > MAX_URI_BYTES {
        Err(FileManagerError::InvalidUri)
    } else {
        Ok(uri)
    }
}

pub(crate) fn parse_file_uri(uri: &str) -> Result<StorePath, FileManagerError> {
    if uri.len() > MAX_URI_BYTES {
        return Err(FileManagerError::InvalidUri);
    }
    let rest = uri
        .strip_prefix("file://")
        .ok_or(FileManagerError::UnsupportedUri)?;
    let path = if let Some(path) = rest.strip_prefix('/') {
        format!("/{path}")
    } else if let Some(path) = rest.strip_prefix("localhost/") {
        format!("/{path}")
    } else {
        return Err(FileManagerError::UnsupportedUri);
    };
    let bytes = percent_decode(path.as_bytes())?;
    if bytes.contains(&0) {
        return Err(FileManagerError::InvalidUri);
    }
    #[cfg(unix)]
    {
        Ok(StorePath::from_unix_path(std::ffi::OsString::from_vec(
            bytes,
        )))
    }
    #[cfg(not(unix))]
    {
        let path = String::from_utf8(bytes).map_err(|_| FileManagerError::InvalidUri)?;
        Ok(StorePath::from_unix_path(path))
    }
}

fn percent_decode(value: &[u8]) -> Result<Vec<u8>, FileManagerError> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        if value[index] != b'%' {
            decoded.push(value[index]);
            index += 1;
            continue;
        }
        let hex = value
            .get(index + 1..index + 3)
            .ok_or(FileManagerError::InvalidUri)?;
        let text = std::str::from_utf8(hex).map_err(|_| FileManagerError::InvalidUri)?;
        decoded.push(u8::from_str_radix(text, 16).map_err(|_| FileManagerError::InvalidUri)?);
        index += 3;
    }
    Ok(decoded)
}

fn dbus_error(error: FileManagerError) -> zbus::fdo::Error {
    match error {
        FileManagerError::InvalidRequest
        | FileManagerError::InvalidUri
        | FileManagerError::UnsupportedUri => zbus::fdo::Error::InvalidArgs(error.to_string()),
        FileManagerError::Busy
        | FileManagerError::Unavailable
        | FileManagerError::Unreachable
        | FileManagerError::Blocked
        | FileManagerError::TimedOut
        | FileManagerError::Service(_) => zbus::fdo::Error::Failed(error.to_string()),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileManagerError {
    InvalidRequest,
    InvalidUri,
    UnsupportedUri,
    Busy,
    Unavailable,
    Unreachable,
    Blocked,
    TimedOut,
    Service(Box<str>),
}

impl fmt::Display for FileManagerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest => formatter.write_str("the FileManager1 request is invalid"),
            Self::InvalidUri => formatter.write_str("the FileManager1 URI is malformed"),
            Self::UnsupportedUri => formatter.write_str("only local file URIs are supported"),
            Self::Busy => formatter.write_str("the application request queue is busy"),
            Self::Unavailable => formatter.write_str("the application request receiver is closed"),
            Self::Unreachable => formatter.write_str("a requested local path is unreachable"),
            Self::Blocked => formatter.write_str("the target window is blocked by a modal dialog"),
            Self::TimedOut => formatter.write_str("the FileManager1 request timed out"),
            Self::Service(reason) => write!(formatter, "FileManager1 service failed: {reason}"),
        }
    }
}

impl Error for FileManagerError {}
