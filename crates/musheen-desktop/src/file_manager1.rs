use musheen_core::StorePath;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;

pub const FILE_MANAGER_NAME: &str = "org.freedesktop.FileManager1";
pub const FILE_MANAGER_PATH: &str = "/org/freedesktop/FileManager1";
const MAX_URIS: usize = 256;
const MAX_URI_BYTES: usize = 16 * 1024;

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
    fn submit(&self, request: FileManagerRequest) -> Result<(), FileManagerError>;
}

impl FileManagerRequestSink for async_channel::Sender<FileManagerRequest> {
    fn submit(&self, request: FileManagerRequest) -> Result<(), FileManagerError> {
        self.try_send(request).map_err(|error| match error {
            async_channel::TrySendError::Full(_) => FileManagerError::Busy,
            async_channel::TrySendError::Closed(_) => FileManagerError::Unavailable,
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

    pub fn show_items(&self, uris: &[&str], startup_id: &str) -> Result<(), FileManagerError> {
        self.dispatch(uris, startup_id, |locations, startup_id| {
            FileManagerRequest::ShowItems {
                locations,
                startup_id,
            }
        })
    }

    pub fn show_folders(&self, uris: &[&str], startup_id: &str) -> Result<(), FileManagerError> {
        self.dispatch(uris, startup_id, |locations, startup_id| {
            FileManagerRequest::ShowFolders {
                locations,
                startup_id,
            }
        })
    }

    pub fn show_item_properties(
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
    }

    fn dispatch(
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
        self.sink.submit(request(locations, startup_id.into()))
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
        self.show_items(&uris, &startup_id).map_err(dbus_error)
    }

    #[zbus(name = "ShowFolders")]
    async fn dbus_show_folders(
        &self,
        uris: Vec<String>,
        startup_id: String,
    ) -> zbus::fdo::Result<()> {
        let uris = uris.iter().map(String::as_str).collect::<Vec<_>>();
        self.show_folders(&uris, &startup_id).map_err(dbus_error)
    }

    #[zbus(name = "ShowItemProperties")]
    async fn dbus_show_item_properties(
        &self,
        uris: Vec<String>,
        startup_id: String,
    ) -> zbus::fdo::Result<()> {
        let uris = uris.iter().map(String::as_str).collect::<Vec<_>>();
        self.show_item_properties(&uris, &startup_id)
            .map_err(dbus_error)
    }
}

pub async fn serve_file_manager1(
    address: Option<&str>,
    sink: Arc<dyn FileManagerRequestSink>,
) -> Result<zbus::Connection, FileManagerError> {
    let builder = match address {
        Some(address) => zbus::connection::Builder::address(address),
        None => zbus::connection::Builder::session(),
    }
    .map_err(|error| FileManagerError::Service(error.to_string().into()))?;
    builder
        .name(FILE_MANAGER_NAME)
        .map_err(|error| FileManagerError::Service(error.to_string().into()))?
        .serve_at(FILE_MANAGER_PATH, FileManager1::new(sink))
        .map_err(|error| FileManagerError::Service(error.to_string().into()))?
        .build()
        .await
        .map_err(|error| FileManagerError::Service(error.to_string().into()))
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
        FileManagerError::Busy | FileManagerError::Unavailable | FileManagerError::Service(_) => {
            zbus::fdo::Error::Failed(error.to_string())
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FileManagerError {
    InvalidRequest,
    InvalidUri,
    UnsupportedUri,
    Busy,
    Unavailable,
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
            Self::Service(reason) => write!(formatter, "FileManager1 service failed: {reason}"),
        }
    }
}

impl Error for FileManagerError {}
