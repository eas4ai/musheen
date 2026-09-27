use std::ffi::OsStr;
use std::fmt;
use std::fs::{File, FileType, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;

pub const MIME_SNIFF_BYTES: usize = 64 * 1024;
const GENERIC_MIME: &str = "application/octet-stream";

pub trait MimeBackend: Send + Sync {
    fn detect_name(&self, name: &OsStr) -> Option<Box<str>>;
    fn detect_content(&self, content: &[u8]) -> Option<Box<str>>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MimeSource {
    Name,
    Content,
    Fallback,
    FileType,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DetectedMime {
    mime_type: Box<str>,
    source: MimeSource,
    bytes_read: usize,
}

impl DetectedMime {
    pub fn mime_type(&self) -> &str {
        &self.mime_type
    }

    pub fn source(&self) -> MimeSource {
        self.source
    }

    pub fn bytes_read(&self) -> usize {
        self.bytes_read
    }
}

pub struct MimeDetector {
    primary: Arc<dyn MimeBackend>,
    fallback: Arc<dyn MimeBackend>,
}

impl Default for MimeDetector {
    fn default() -> Self {
        Self::with_backends(Arc::new(XdgMimeBackend), Arc::new(TreeMagicBackend))
    }
}

impl MimeDetector {
    pub fn with_backends(primary: Arc<dyn MimeBackend>, fallback: Arc<dyn MimeBackend>) -> Self {
        Self { primary, fallback }
    }

    pub fn detect(&self, path: &Path) -> Result<DetectedMime, MimeError> {
        let metadata = path.symlink_metadata().map_err(MimeError::Metadata)?;
        if metadata.file_type().is_dir() {
            return Ok(detected("inode/directory", MimeSource::FileType, 0));
        }
        if metadata.file_type().is_symlink() {
            return Ok(detected("inode/symlink", MimeSource::FileType, 0));
        }
        // A socket, pipe or device is named by its type and never opened:
        // opening a pipe waits for a writer, and opening a device may act on
        // it.
        if let Some(mime_type) = special_mime_type(metadata.file_type()) {
            return Ok(detected(mime_type, MimeSource::FileType, 0));
        }

        if let Some(value) = path
            .file_name()
            .and_then(|name| self.primary.detect_name(name))
            .filter(|value| is_specific(value))
        {
            return Ok(DetectedMime {
                mime_type: value,
                source: MimeSource::Name,
                bytes_read: 0,
            });
        }

        // Non-blocking, in case a pipe replaced the file since it was checked.
        let mut file: File = OpenOptions::new()
            .read(true)
            .custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits())
            .open(path)
            .map_err(MimeError::Open)?;
        let mut content = Vec::with_capacity(MIME_SNIFF_BYTES);
        file.by_ref()
            .take(MIME_SNIFF_BYTES as u64)
            .read_to_end(&mut content)
            .map_err(MimeError::Read)?;
        let bytes_read = content.len();

        if let Some(value) = self
            .primary
            .detect_content(&content)
            .filter(|value| is_specific(value))
        {
            return Ok(DetectedMime {
                mime_type: value,
                source: MimeSource::Content,
                bytes_read,
            });
        }
        if let Some(value) = self
            .fallback
            .detect_content(&content)
            .filter(|value| is_specific(value))
        {
            return Ok(DetectedMime {
                mime_type: value,
                source: MimeSource::Fallback,
                bytes_read,
            });
        }

        Ok(detected(GENERIC_MIME, MimeSource::Fallback, bytes_read))
    }
}

/// The shared-mime-info name of a socket, pipe or device.
fn special_mime_type(file_type: FileType) -> Option<&'static str> {
    if file_type.is_fifo() {
        Some("inode/fifo")
    } else if file_type.is_socket() {
        Some("inode/socket")
    } else if file_type.is_char_device() {
        Some("inode/chardevice")
    } else if file_type.is_block_device() {
        Some("inode/blockdevice")
    } else {
        None
    }
}

fn detected(mime_type: &str, source: MimeSource, bytes_read: usize) -> DetectedMime {
    DetectedMime {
        mime_type: mime_type.into(),
        source,
        bytes_read,
    }
}

fn is_specific(value: &str) -> bool {
    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "" | GENERIC_MIME | "application/x-zerosize" | "unknown/unknown"
    )
}

/// MIME types judged from file names alone, with the shared MIME database
/// loaded once for many names. Nothing is opened or read.
pub struct NameMimeTypes {
    database: xdg_mime::SharedMimeInfo,
}

impl NameMimeTypes {
    #[must_use]
    pub fn new() -> Self {
        Self {
            database: xdg_mime::SharedMimeInfo::new(),
        }
    }

    /// The MIME type the name `name` suggests, if a specific one.
    #[must_use]
    pub fn mime_type(&self, name: &OsStr) -> Option<Box<str>> {
        let name = name.to_str()?;
        self.database
            .get_mime_types_from_file_name(name)
            .into_iter()
            .map(|mime| mime.to_string())
            .find(|mime| is_specific(mime))
            .map(String::into_boxed_str)
    }
}

impl NameMimeTypes {
    /// Whether `mime_type` is `wanted`, an alias of it or a subclass of it
    /// (`text/x-csrc` is a `text/plain`); `wanted` may name a family, such
    /// as `image/*`.
    #[must_use]
    pub fn is_a(&self, mime_type: &str, wanted: &str) -> bool {
        match (
            mime_type.parse::<mime::Mime>(),
            wanted.parse::<mime::Mime>(),
        ) {
            (Ok(mime_type), Ok(wanted)) => self.database.mime_type_subclass(&mime_type, &wanted),
            _ => Self::same_or_family(mime_type, wanted),
        }
    }

    /// Whether `mime_type` is `wanted`, or in its family (`image/*`),
    /// without the database's aliases and subclasses.
    #[must_use]
    pub fn same_or_family(mime_type: &str, wanted: &str) -> bool {
        wanted.eq_ignore_ascii_case(mime_type)
            || wanted.strip_suffix("/*").is_some_and(|family| {
                mime_type
                    .split_once('/')
                    .is_some_and(|(kind, _)| kind.eq_ignore_ascii_case(family))
            })
    }
}

impl Default for NameMimeTypes {
    fn default() -> Self {
        Self::new()
    }
}

struct XdgMimeBackend;

impl MimeBackend for XdgMimeBackend {
    fn detect_name(&self, name: &OsStr) -> Option<Box<str>> {
        let name = name.to_str()?;
        xdg_mime::SharedMimeInfo::new()
            .get_mime_types_from_file_name(name)
            .into_iter()
            .next()
            .map(|mime| mime.to_string().into_boxed_str())
    }

    fn detect_content(&self, content: &[u8]) -> Option<Box<str>> {
        xdg_mime::SharedMimeInfo::new()
            .get_mime_type_for_data(content)
            .map(|(mime, _)| mime.to_string().into_boxed_str())
    }
}

struct TreeMagicBackend;

impl MimeBackend for TreeMagicBackend {
    fn detect_name(&self, _name: &OsStr) -> Option<Box<str>> {
        None
    }

    fn detect_content(&self, content: &[u8]) -> Option<Box<str>> {
        Some(tree_magic_mini::from_u8(content).into())
    }
}

#[derive(Debug)]
pub enum MimeError {
    Metadata(io::Error),
    Open(io::Error),
    Read(io::Error),
}

impl fmt::Display for MimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Metadata(error) => {
                write!(formatter, "could not inspect the selected item: {error}")
            }
            Self::Open(error) => write!(formatter, "could not open the selected item: {error}"),
            Self::Read(error) => {
                write!(formatter, "could not inspect the selected content: {error}")
            }
        }
    }
}

impl std::error::Error for MimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Metadata(error) | Self::Open(error) | Self::Read(error) => Some(error),
        }
    }
}
