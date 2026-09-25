use musheen_core::CancellationToken;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub const PREVIEW_INITIAL_BYTES: usize = 1024 * 1024;
pub const PREVIEW_LOAD_MORE_BYTES: usize = 16 * 1024 * 1024;
pub const PREVIEW_MAX_BYTES: usize = 64 * 1024 * 1024;
const READ_CHUNK_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PreviewKind {
    Text,
    Binary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreviewLimits {
    initial: usize,
    load_more: usize,
    ceiling: usize,
}

impl Default for PreviewLimits {
    fn default() -> Self {
        Self {
            initial: PREVIEW_INITIAL_BYTES,
            load_more: PREVIEW_LOAD_MORE_BYTES,
            ceiling: PREVIEW_MAX_BYTES,
        }
    }
}

impl PreviewLimits {
    pub fn new(initial: usize, load_more: usize, ceiling: usize) -> Result<Self, PreviewError> {
        if initial == 0
            || load_more == 0
            || ceiling == 0
            || initial > ceiling
            || initial > PREVIEW_INITIAL_BYTES
            || load_more > PREVIEW_LOAD_MORE_BYTES
            || ceiling > PREVIEW_MAX_BYTES
        {
            return Err(PreviewError::InvalidLimits);
        }
        Ok(Self {
            initial,
            load_more,
            ceiling,
        })
    }
}

#[derive(Debug)]
pub struct PreviewDocument {
    path: PathBuf,
    limits: PreviewLimits,
    total_size: u64,
    bytes: Vec<u8>,
    bytes_read: usize,
    kind: PreviewKind,
    valid_text_bytes: usize,
}

impl PreviewDocument {
    pub fn open(path: &Path, cancellation: CancellationToken) -> Result<Self, PreviewError> {
        Self::open_with_limits(path, cancellation, PreviewLimits::default())
    }

    pub fn open_with_limits(
        path: &Path,
        cancellation: CancellationToken,
        limits: PreviewLimits,
    ) -> Result<Self, PreviewError> {
        ensure_active(&cancellation)?;
        let mut file = File::open(path).map_err(PreviewError::Open)?;
        let total_size = file.metadata().map_err(PreviewError::Metadata)?.len();
        let mut document = Self {
            path: path.to_path_buf(),
            limits,
            total_size,
            bytes: Vec::with_capacity(limits.initial),
            bytes_read: 0,
            kind: PreviewKind::Text,
            valid_text_bytes: 0,
        };
        document.read_from(&mut file, limits.initial, &cancellation)?;
        Ok(document)
    }

    pub fn bytes_read(&self) -> usize {
        self.bytes_read
    }

    pub fn open_with_available(&self) -> bool {
        true
    }

    pub fn kind(&self) -> PreviewKind {
        self.kind
    }

    pub fn text(&self) -> Option<&str> {
        if self.kind == PreviewKind::Binary {
            return None;
        }
        std::str::from_utf8(&self.bytes[..self.valid_text_bytes]).ok()
    }

    pub fn limit_reached(&self) -> bool {
        self.bytes_read >= self.limits.ceiling && self.total_size > self.bytes_read as u64
    }

    pub fn has_more(&self) -> bool {
        self.bytes_read < self.limits.ceiling && (self.bytes_read as u64) < self.total_size
    }

    pub fn load_more(&mut self, cancellation: CancellationToken) -> Result<bool, PreviewError> {
        ensure_active(&cancellation)?;
        if self.bytes_read >= self.limits.ceiling || self.bytes_read as u64 >= self.total_size {
            return Ok(false);
        }
        let remaining = self.limits.ceiling - self.bytes_read;
        let requested = remaining.min(self.limits.load_more);
        let mut file = File::open(&self.path).map_err(PreviewError::Open)?;
        file.seek(SeekFrom::Start(self.bytes_read as u64))
            .map_err(PreviewError::Read)?;
        let before = self.bytes_read;
        self.read_from(&mut file, requested, &cancellation)?;
        Ok(self.bytes_read > before)
    }

    fn read_from(
        &mut self,
        file: &mut File,
        requested: usize,
        cancellation: &CancellationToken,
    ) -> Result<(), PreviewError> {
        let target = self
            .bytes_read
            .saturating_add(requested)
            .min(self.limits.ceiling);
        while self.bytes_read < target {
            ensure_active(cancellation)?;
            let chunk = (target - self.bytes_read).min(READ_CHUNK_BYTES);
            let start = self.bytes.len();
            self.bytes.resize(start + chunk, 0);
            let read = file
                .read(&mut self.bytes[start..])
                .map_err(PreviewError::Read)?;
            self.bytes.truncate(start + read);
            self.bytes_read += read;
            if read == 0 {
                break;
            }
        }
        ensure_active(cancellation)?;
        self.classify();
        Ok(())
    }

    fn classify(&mut self) {
        if self.bytes.contains(&0) {
            self.kind = PreviewKind::Binary;
            self.valid_text_bytes = 0;
            return;
        }
        match std::str::from_utf8(&self.bytes) {
            Ok(value) => {
                self.kind = PreviewKind::Text;
                self.valid_text_bytes = value.len();
            }
            Err(error)
                if error.error_len().is_none()
                    && (self.bytes_read as u64) < self.total_size
                    && self.bytes_read < self.limits.ceiling =>
            {
                self.kind = PreviewKind::Text;
                self.valid_text_bytes = error.valid_up_to();
            }
            Err(_) => {
                self.kind = PreviewKind::Binary;
                self.valid_text_bytes = 0;
            }
        }
    }
}

fn ensure_active(cancellation: &CancellationToken) -> Result<(), PreviewError> {
    if cancellation.is_cancelled() {
        Err(PreviewError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum PreviewError {
    InvalidLimits,
    Cancelled,
    Open(io::Error),
    Metadata(io::Error),
    Read(io::Error),
}

impl fmt::Display for PreviewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => {
                formatter.write_str("preview limits must be positive and ordered")
            }
            Self::Cancelled => formatter.write_str("preview cancelled"),
            Self::Open(error) => write!(formatter, "could not open the selected item: {error}"),
            Self::Metadata(error) => {
                write!(formatter, "could not inspect the selected item: {error}")
            }
            Self::Read(error) => write!(formatter, "could not read the selected item: {error}"),
        }
    }
}

impl std::error::Error for PreviewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open(error) | Self::Metadata(error) | Self::Read(error) => Some(error),
            Self::InvalidLimits | Self::Cancelled => None,
        }
    }
}
