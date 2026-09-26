use musheen_core::CancellationToken;
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const CHECKSUM_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChecksumAlgorithm {
    Blake3,
    Sha256,
}

impl ChecksumAlgorithm {
    pub fn label(self) -> &'static str {
        match self {
            Self::Blake3 => "BLAKE3",
            Self::Sha256 => "SHA-256",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileFingerprint {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl FileFingerprint {
    fn from_metadata(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }

    pub fn size(self) -> u64 {
        self.size
    }

    pub fn device(self) -> u64 {
        self.device
    }

    pub fn inode(self) -> u64 {
        self.inode
    }

    pub fn modified_seconds(self) -> i64 {
        self.modified_seconds
    }

    pub fn modified_nanoseconds(self) -> i64 {
        self.modified_nanoseconds
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChecksumResult {
    path: PathBuf,
    algorithm: ChecksumAlgorithm,
    hex_digest: Box<str>,
    fingerprint: FileFingerprint,
}

impl ChecksumResult {
    pub fn algorithm(&self) -> ChecksumAlgorithm {
        self.algorithm
    }

    pub fn hex_digest(&self) -> &str {
        &self.hex_digest
    }

    pub fn fingerprint(&self) -> FileFingerprint {
        self.fingerprint
    }

    pub fn is_current(&self) -> Result<bool, ChecksumError> {
        Ok(fingerprint(&self.path)? == self.fingerprint)
    }
}

pub struct ChecksumService;

impl ChecksumService {
    pub fn compute(
        path: &Path,
        algorithm: ChecksumAlgorithm,
        cancellation: CancellationToken,
    ) -> Result<ChecksumResult, ChecksumError> {
        Self::compute_with_progress(path, algorithm, cancellation, |_| {})
    }

    pub fn compute_with_progress(
        path: &Path,
        algorithm: ChecksumAlgorithm,
        cancellation: CancellationToken,
        mut progress: impl FnMut(u64),
    ) -> Result<ChecksumResult, ChecksumError> {
        ensure_active(&cancellation)?;
        let before = fingerprint(path)?;
        let mut file = File::open(path).map_err(ChecksumError::Io)?;
        let opened = FileFingerprint::from_metadata(&file.metadata().map_err(ChecksumError::Io)?);
        if opened != before {
            return Err(ChecksumError::Changed);
        }
        let mut hasher = StreamingHasher::new(algorithm);
        let mut buffer = [0_u8; CHECKSUM_BUFFER_BYTES];
        let mut bytes_read = 0_u64;
        loop {
            ensure_active(&cancellation)?;
            let read = file.read(&mut buffer).map_err(ChecksumError::Io)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            bytes_read = bytes_read.saturating_add(read as u64);
            progress(bytes_read);
        }
        ensure_active(&cancellation)?;
        if fingerprint(path)? != before {
            return Err(ChecksumError::Changed);
        }
        Ok(ChecksumResult {
            path: path.to_path_buf(),
            algorithm,
            hex_digest: hasher.finalize(),
            fingerprint: before,
        })
    }
}

enum StreamingHasher {
    Blake3(Box<blake3::Hasher>),
    Sha256(Sha256),
}

impl StreamingHasher {
    fn new(algorithm: ChecksumAlgorithm) -> Self {
        match algorithm {
            ChecksumAlgorithm::Blake3 => Self::Blake3(Box::new(blake3::Hasher::new())),
            ChecksumAlgorithm::Sha256 => Self::Sha256(Sha256::new()),
        }
    }

    fn update(&mut self, bytes: &[u8]) {
        match self {
            Self::Blake3(hasher) => {
                hasher.update(bytes);
            }
            Self::Sha256(hasher) => hasher.update(bytes),
        }
    }

    fn finalize(self) -> Box<str> {
        match self {
            Self::Blake3(hasher) => hasher.finalize().to_hex().as_str().into(),
            Self::Sha256(hasher) => hex_bytes(&hasher.finalize()).into_boxed_str(),
        }
    }
}

fn fingerprint(path: &Path) -> Result<FileFingerprint, ChecksumError> {
    fs::metadata(path)
        .map(|metadata| FileFingerprint::from_metadata(&metadata))
        .map_err(ChecksumError::Io)
}

fn hex_bytes(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn ensure_active(cancellation: &CancellationToken) -> Result<(), ChecksumError> {
    if cancellation.is_cancelled() {
        Err(ChecksumError::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Debug)]
pub enum ChecksumError {
    Cancelled,
    Changed,
    Io(io::Error),
}

impl fmt::Display for ChecksumError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("checksum cancelled"),
            Self::Changed => formatter.write_str("the file changed while its checksum was read"),
            Self::Io(error) => write!(formatter, "checksum I/O failed: {error}"),
        }
    }
}

impl std::error::Error for ChecksumError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Cancelled | Self::Changed => None,
        }
    }
}
