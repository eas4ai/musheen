use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
use musheen_core::StorePath;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::os::unix::ffi::OsStrExt;
use std::sync::Mutex;

pub const URI_LIST: &str = "text/uri-list";
pub const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
pub const KDE_CUT_SELECTION: &str = "application/x-kde-cutselection";

/// Owns the platform selection for as long as the application is running.
/// Reading from another X11 selection owner can wait for its reply, so callers
/// must dispatch reads off the UI thread.
#[derive(Default)]
pub struct SystemFileClipboard {
    context: Mutex<Option<ClipboardContext>>,
}

impl SystemFileClipboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn publish(&self, payload: &ClipboardPayload) -> Result<(), SystemClipboardError> {
        let mut context = self
            .context
            .lock()
            .map_err(|_| SystemClipboardError::Poisoned)?;
        if context.is_none() {
            *context = Some(ClipboardContext::new().map_err(SystemClipboardError::Platform)?);
        }
        let context = context.as_ref().expect("clipboard context was initialized");
        let contents = payload
            .formats()
            .iter()
            .map(|(mime, bytes)| ClipboardContent::Other(mime.to_string(), bytes.clone()))
            .collect();
        context
            .set(contents)
            .map_err(SystemClipboardError::Platform)
    }

    pub fn read(&self) -> Result<Option<ClipboardPayload>, SystemClipboardError> {
        let mut context = self
            .context
            .lock()
            .map_err(|_| SystemClipboardError::Poisoned)?;
        if context.is_none() {
            *context = Some(ClipboardContext::new().map_err(SystemClipboardError::Platform)?);
        }
        let context = context.as_ref().expect("clipboard context was initialized");
        let offered = context
            .available_formats()
            .map_err(SystemClipboardError::Platform)?;
        if !offered
            .iter()
            .any(|mime| mime == URI_LIST || mime == GNOME_COPIED_FILES)
        {
            return Ok(None);
        }
        let mut formats = BTreeMap::new();
        for mime in [URI_LIST, GNOME_COPIED_FILES, KDE_CUT_SELECTION] {
            if offered.iter().any(|offered| offered == mime) {
                formats.insert(
                    Box::<str>::from(mime),
                    context
                        .get_buffer(mime)
                        .map_err(SystemClipboardError::Platform)?,
                );
            }
        }
        ClipboardPayload::parse(&formats)
            .map(Some)
            .map_err(SystemClipboardError::Payload)
    }
}

#[derive(Debug)]
pub enum SystemClipboardError {
    Platform(Box<dyn Error + Send + Sync>),
    Payload(ClipboardError),
    Poisoned,
}

impl fmt::Display for SystemClipboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Platform(error) => write!(formatter, "desktop clipboard: {error}"),
            Self::Payload(error) => write!(formatter, "desktop clipboard: {error}"),
            Self::Poisoned => formatter.write_str("desktop clipboard is unavailable"),
        }
    }
}

impl Error for SystemClipboardError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardOperation {
    Copy,
    Cut,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardPayload {
    operation: ClipboardOperation,
    paths: Vec<StorePath>,
    formats: BTreeMap<Box<str>, Vec<u8>>,
}

impl ClipboardPayload {
    pub fn new(
        operation: ClipboardOperation,
        paths: Vec<StorePath>,
    ) -> Result<Self, ClipboardError> {
        if paths.is_empty() {
            return Err(ClipboardError::Empty);
        }
        let uris = paths.iter().map(file_uri).collect::<Result<Vec<_>, _>>()?;
        let mut uri_list = Vec::new();
        for uri in &uris {
            uri_list.extend_from_slice(uri);
            uri_list.extend_from_slice(b"\r\n");
        }
        let mut gnome = match operation {
            ClipboardOperation::Copy => b"copy\n".to_vec(),
            ClipboardOperation::Cut => b"cut\n".to_vec(),
        };
        for (index, uri) in uris.iter().enumerate() {
            if index > 0 {
                gnome.push(b'\n');
            }
            gnome.extend_from_slice(uri);
        }
        let kde = match operation {
            ClipboardOperation::Copy => b"0".to_vec(),
            ClipboardOperation::Cut => b"1".to_vec(),
        };
        let formats = BTreeMap::from([
            (Box::<str>::from(URI_LIST), uri_list),
            (Box::<str>::from(GNOME_COPIED_FILES), gnome),
            (Box::<str>::from(KDE_CUT_SELECTION), kde),
        ]);
        Ok(Self {
            operation,
            paths,
            formats,
        })
    }

    pub fn parse(formats: &BTreeMap<Box<str>, Vec<u8>>) -> Result<Self, ClipboardError> {
        let gnome = formats.get(GNOME_COPIED_FILES).map(Vec::as_slice);
        let operation = if let Some(data) = gnome {
            match data.split(|byte| *byte == b'\n').next() {
                Some(b"copy") => ClipboardOperation::Copy,
                Some(b"cut") => ClipboardOperation::Cut,
                _ => return Err(ClipboardError::Malformed),
            }
        } else {
            match formats.get(KDE_CUT_SELECTION).map(Vec::as_slice) {
                Some(b"1") => ClipboardOperation::Cut,
                Some(b"0") | None => ClipboardOperation::Copy,
                _ => return Err(ClipboardError::Malformed),
            }
        };
        let uri_data = formats
            .get(URI_LIST)
            .map(Vec::as_slice)
            .or_else(|| gnome.and_then(gnome_uri_lines))
            .ok_or(ClipboardError::MissingUris)?;
        let paths = parse_uri_list(uri_data)?;
        if let (Some(_), Some(gnome)) = (formats.get(URI_LIST), gnome) {
            let gnome_paths = parse_uri_list(
                gnome_uri_lines(gnome).ok_or(ClipboardError::Malformed)?,
            )?;
            if gnome_paths != paths {
                return Err(ClipboardError::Malformed);
            }
        }
        Self::new(operation, paths)
    }

    #[must_use]
    pub const fn operation(&self) -> ClipboardOperation {
        self.operation
    }

    #[must_use]
    pub fn paths(&self) -> &[StorePath] {
        &self.paths
    }

    #[must_use]
    pub fn format(&self, mime: &str) -> Option<&[u8]> {
        self.formats.get(mime).map(Vec::as_slice)
    }

    #[must_use]
    pub const fn formats(&self) -> &BTreeMap<Box<str>, Vec<u8>> {
        &self.formats
    }
}

fn file_uri(path: &StorePath) -> Result<Vec<u8>, ClipboardError> {
    let path = path.as_unix_path().ok_or(ClipboardError::UnsupportedPath)?;
    if !path.is_absolute() {
        return Err(ClipboardError::UnsupportedPath);
    }
    let mut uri = b"file://".to_vec();
    for byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/".contains(byte) {
            uri.push(*byte);
        } else {
            uri.push(b'%');
            uri.push(hex(byte >> 4));
            uri.push(hex(byte & 0x0f));
        }
    }
    Ok(uri)
}

fn parse_uri_list(data: &[u8]) -> Result<Vec<StorePath>, ClipboardError> {
    let mut paths = Vec::new();
    for line in data.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let encoded = line
            .strip_prefix(b"file://")
            .filter(|path| path.starts_with(b"/"))
            .ok_or(ClipboardError::UnsupportedUri)?;
        let decoded = percent_decode(encoded)?;
        if decoded.contains(&0) {
            return Err(ClipboardError::Malformed);
        }
        paths.push(StorePath::from_unix_bytes(decoded));
    }
    if paths.is_empty() {
        return Err(ClipboardError::Empty);
    }
    Ok(paths)
}

fn gnome_uri_lines(data: &[u8]) -> Option<&[u8]> {
    let separator = data.iter().position(|byte| *byte == b'\n')?;
    data.get(separator + 1..)
}

fn percent_decode(encoded: &[u8]) -> Result<Vec<u8>, ClipboardError> {
    let mut decoded = Vec::with_capacity(encoded.len());
    let mut index = 0;
    while index < encoded.len() {
        if encoded[index] == b'%' {
            let high = encoded.get(index + 1).and_then(|byte| unhex(*byte));
            let low = encoded.get(index + 2).and_then(|byte| unhex(*byte));
            let (Some(high), Some(low)) = (high, low) else {
                return Err(ClipboardError::Malformed);
            };
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(encoded[index]);
            index += 1;
        }
    }
    Ok(decoded)
}

const fn hex(nibble: u8) -> u8 {
    match nibble {
        0..=9 => b'0' + nibble,
        _ => b'A' + nibble - 10,
    }
}

const fn unhex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardError {
    Empty,
    UnsupportedPath,
    UnsupportedUri,
    MissingUris,
    Malformed,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("the file clipboard is empty"),
            Self::UnsupportedPath => formatter.write_str("only absolute local paths are supported"),
            Self::UnsupportedUri => formatter.write_str("the clipboard contains a non-local URI"),
            Self::MissingUris => formatter.write_str("the clipboard has no file URI list"),
            Self::Malformed => formatter.write_str("the file clipboard data is malformed"),
        }
    }
}

impl Error for ClipboardError {}
