use fast_image_resize::{PixelType, Resizer, images::Image};
use md5::{Digest, Md5};
use musheen_core::CancellationToken;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PIXELS: u64 = 50_000_000;
const MAX_DECODED_BYTES: u64 = 128 * 1024 * 1024;
const DEFAULT_WORKERS: usize = 4;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThumbnailSize {
    Normal,
    Large,
    XLarge,
    XxLarge,
}

impl ThumbnailSize {
    fn directory(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
            Self::XLarge => "x-large",
            Self::XxLarge => "xx-large",
        }
    }

    pub fn pixels(self) -> u32 {
        match self {
            Self::Normal => 128,
            Self::Large => 256,
            Self::XLarge => 512,
            Self::XxLarge => 1024,
        }
    }

    fn argument(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
            Self::XLarge => "x-large",
            Self::XxLarge => "xx-large",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "normal" => Some(Self::Normal),
            "large" => Some(Self::Large),
            "x-large" => Some(Self::XLarge),
            "xx-large" => Some(Self::XxLarge),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThumbnailRequest {
    path: PathBuf,
    uri: Box<str>,
    mtime: u64,
    size: ThumbnailSize,
}

impl ThumbnailRequest {
    pub fn new(path: &Path, mtime: u64, size: ThumbnailSize) -> Result<Self, ThumbnailError> {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(ThumbnailError::Io)?
                .join(path)
        };
        Ok(Self {
            uri: file_uri(&absolute).into_boxed_str(),
            path: absolute,
            mtime,
            size,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn mtime(&self) -> u64 {
        self.mtime
    }

    pub fn size(&self) -> ThumbnailSize {
        self.size
    }

    fn key(&self) -> String {
        format!("{:x}", Md5::digest(self.uri.as_bytes()))
    }
}

#[cfg(unix)]
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(&mut uri, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    uri
}

#[cfg(not(unix))]
fn file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThumbnailLookup {
    Hit(PathBuf),
    Miss,
    Failed { reason: Box<str> },
}

#[derive(Clone, Debug)]
pub struct ThumbnailCache {
    root: PathBuf,
}

impl ThumbnailCache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn for_user() -> Result<Self, ThumbnailError> {
        if let Some(root) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
            return Ok(Self::new(PathBuf::from(root).join("thumbnails")));
        }
        let home = std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ThumbnailError::Cache("HOME and XDG_CACHE_HOME are unset".into()))?;
        Ok(Self::new(PathBuf::from(home).join(".cache/thumbnails")))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn lookup(&self, request: &ThumbnailRequest) -> Result<ThumbnailLookup, ThumbnailError> {
        let thumbnail = self.thumbnail_path(request);
        if thumbnail.is_file() {
            if metadata_matches(&thumbnail, request)? {
                return Ok(ThumbnailLookup::Hit(thumbnail));
            }
            return Ok(ThumbnailLookup::Miss);
        }

        let failure = self.failure_path(request);
        if failure.is_file() {
            let metadata = read_text_metadata(&failure)?;
            if metadata_value(&metadata, "Thumb::URI") == Some(request.uri())
                && metadata_value(&metadata, "Thumb::MTime")
                    == Some(request.mtime().to_string().as_str())
            {
                let reason = metadata_value(&metadata, "Thumb::Error")
                    .unwrap_or("thumbnail generation failed")
                    .into();
                return Ok(ThumbnailLookup::Failed { reason });
            }
        }
        Ok(ThumbnailLookup::Miss)
    }

    pub fn store_rgba(
        &self,
        request: &ThumbnailRequest,
        width: u32,
        height: u32,
        rgba: &[u8],
    ) -> Result<PathBuf, ThumbnailError> {
        let expected = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| ThumbnailError::Worker("thumbnail dimensions overflow".into()))?;
        if expected != rgba.len() as u64 || width == 0 || height == 0 {
            return Err(ThumbnailError::Worker(
                "thumbnail pixels do not match its dimensions".into(),
            ));
        }
        let path = self.thumbnail_path(request);
        write_png_atomic(&path, width, height, rgba, request, None)?;
        let failure = self.failure_path(request);
        if failure.exists() {
            let _ = fs::remove_file(failure);
        }
        Ok(path)
    }

    pub fn record_failure(
        &self,
        request: &ThumbnailRequest,
        reason: &str,
    ) -> Result<(), ThumbnailError> {
        let path = self.failure_path(request);
        let sanitized: String = reason
            .chars()
            .map(|character| if character.is_ascii() { character } else { '?' })
            .take(1024)
            .collect();
        write_png_atomic(&path, 1, 1, &[0, 0, 0, 0], request, Some(&sanitized))?;
        Ok(())
    }

    fn thumbnail_path(&self, request: &ThumbnailRequest) -> PathBuf {
        self.root
            .join(request.size.directory())
            .join(format!("{}.png", request.key()))
    }

    fn failure_path(&self, request: &ThumbnailRequest) -> PathBuf {
        self.root
            .join("fail")
            .join("musheen")
            .join(format!("{}.png", request.key()))
    }
}

fn metadata_matches(path: &Path, request: &ThumbnailRequest) -> Result<bool, ThumbnailError> {
    let metadata = read_text_metadata(path)?;
    Ok(
        metadata_value(&metadata, "Thumb::URI") == Some(request.uri())
            && metadata_value(&metadata, "Thumb::MTime")
                == Some(request.mtime().to_string().as_str()),
    )
}

fn read_text_metadata(path: &Path) -> Result<Vec<(String, String)>, ThumbnailError> {
    let decoder = png::Decoder::new(BufReader::new(
        File::open(path).map_err(ThumbnailError::Io)?,
    ));
    let reader = decoder
        .read_info()
        .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
    Ok(reader
        .info()
        .uncompressed_latin1_text
        .iter()
        .map(|chunk| (chunk.keyword.clone(), chunk.text.clone()))
        .collect())
}

fn metadata_value<'a>(metadata: &'a [(String, String)], key: &str) -> Option<&'a str> {
    metadata
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str())
}

fn write_png_atomic(
    path: &Path,
    width: u32,
    height: u32,
    rgba: &[u8],
    request: &ThumbnailRequest,
    failure: Option<&str>,
) -> Result<(), ThumbnailError> {
    let parent = path
        .parent()
        .ok_or_else(|| ThumbnailError::Cache("thumbnail path has no parent".into()))?;
    fs::create_dir_all(parent).map_err(ThumbnailError::Io)?;
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".musheen-{}-{sequence}.tmp", std::process::id()));
    let result = (|| {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(ThumbnailError::Io)?;
        let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .add_text_chunk("Thumb::URI".into(), request.uri().into())
            .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
        encoder
            .add_text_chunk("Thumb::MTime".into(), request.mtime().to_string())
            .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
        encoder
            .add_text_chunk("Software".into(), "Musheen".into())
            .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
        if let Some(failure) = failure {
            encoder
                .add_text_chunk("Thumb::Error".into(), failure.into())
                .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
        }
        let mut writer = encoder
            .write_header()
            .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
        writer
            .write_image_data(rgba)
            .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
        writer
            .finish()
            .map_err(|error| ThumbnailError::Cache(error.to_string().into()))?;
        fs::rename(&temporary, path).map_err(ThumbnailError::Io)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThumbnailMode {
    CacheOnly,
    Generate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ThumbnailLimits {
    workers: usize,
    timeout: Duration,
}

impl Default for ThumbnailLimits {
    fn default() -> Self {
        Self {
            workers: DEFAULT_WORKERS,
            timeout: DEFAULT_TIMEOUT,
        }
    }
}

impl ThumbnailLimits {
    pub fn new(workers: usize, timeout: Duration) -> Result<Self, ThumbnailError> {
        if workers == 0
            || workers > DEFAULT_WORKERS
            || timeout.is_zero()
            || timeout > DEFAULT_TIMEOUT
        {
            return Err(ThumbnailError::InvalidLimits);
        }
        Ok(Self { workers, timeout })
    }
}

#[derive(Default)]
struct PoolState {
    active: usize,
    max_observed: usize,
}

struct WorkerPool {
    state: Mutex<PoolState>,
    changed: Condvar,
    maximum: usize,
}

impl WorkerPool {
    fn acquire(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<WorkerPermit<'_>, ThumbnailError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.active >= self.maximum {
            if cancellation.is_cancelled() {
                return Err(ThumbnailError::Cancelled);
            }
            let (next, _) = self
                .changed
                .wait_timeout(state, Duration::from_millis(20))
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state = next;
        }
        state.active += 1;
        state.max_observed = state.max_observed.max(state.active);
        Ok(WorkerPermit { pool: self })
    }
}

struct WorkerPermit<'a> {
    pool: &'a WorkerPool,
}

impl Drop for WorkerPermit<'_> {
    fn drop(&mut self) {
        let mut state = self
            .pool
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.active -= 1;
        self.pool.changed.notify_one();
    }
}

pub struct ThumbnailService {
    cache: ThumbnailCache,
    worker: PathBuf,
    limits: ThumbnailLimits,
    pool: WorkerPool,
}

impl ThumbnailService {
    pub fn with_worker(
        cache: ThumbnailCache,
        worker: impl Into<PathBuf>,
        limits: ThumbnailLimits,
    ) -> Self {
        Self {
            cache,
            worker: worker.into(),
            limits,
            pool: WorkerPool {
                state: Mutex::new(PoolState::default()),
                changed: Condvar::new(),
                maximum: limits.workers,
            },
        }
    }

    pub fn resolve(
        &self,
        request: &ThumbnailRequest,
        mode: ThumbnailMode,
        cancellation: CancellationToken,
    ) -> Result<ThumbnailLookup, ThumbnailError> {
        if cancellation.is_cancelled() {
            return Err(ThumbnailError::Cancelled);
        }
        let cached = self.cache.lookup(request)?;
        if mode == ThumbnailMode::CacheOnly || cached != ThumbnailLookup::Miss {
            return Ok(cached);
        }

        let _permit = self.pool.acquire(&cancellation)?;
        let cached = self.cache.lookup(request)?;
        if cached != ThumbnailLookup::Miss {
            return Ok(cached);
        }
        let child = self.spawn_worker(request)?;
        self.wait_for_worker(child, request, &cancellation)?;
        match self.cache.lookup(request)? {
            ThumbnailLookup::Hit(path) => Ok(ThumbnailLookup::Hit(path)),
            _ => Err(ThumbnailError::Worker(
                "thumbnail worker produced no valid cache entry".into(),
            )),
        }
    }

    fn spawn_worker(
        &self,
        request: &ThumbnailRequest,
    ) -> Result<std::process::Child, ThumbnailError> {
        Command::new(&self.worker)
            .arg("--source")
            .arg(request.path())
            .arg("--cache-root")
            .arg(self.cache.root())
            .arg("--mtime")
            .arg(request.mtime().to_string())
            .arg("--size")
            .arg(request.size().argument())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(ThumbnailError::Io)
    }

    fn wait_for_worker(
        &self,
        mut child: std::process::Child,
        request: &ThumbnailRequest,
        cancellation: &CancellationToken,
    ) -> Result<(), ThumbnailError> {
        let started = Instant::now();
        loop {
            if cancellation.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ThumbnailError::Cancelled);
            }
            if started.elapsed() >= self.limits.timeout {
                let _ = child.kill();
                let _ = child.wait();
                self.cache
                    .record_failure(request, "thumbnail worker timed out")?;
                return Err(ThumbnailError::Timeout);
            }
            if let Some(status) = child.try_wait().map_err(ThumbnailError::Io)? {
                if !status.success() {
                    self.cache
                        .record_failure(request, "thumbnail worker rejected the source")?;
                    return Err(ThumbnailError::Worker(
                        "thumbnail worker rejected the source".into(),
                    ));
                }
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn max_observed_workers(&self) -> usize {
        self.pool
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .max_observed
    }
}

pub fn generate_thumbnail(
    cache: &ThumbnailCache,
    request: &ThumbnailRequest,
) -> Result<PathBuf, ThumbnailError> {
    let source_size = fs::metadata(request.path())
        .map_err(ThumbnailError::Io)?
        .len();
    if source_size > MAX_SOURCE_BYTES {
        return Err(ThumbnailError::Worker(
            "image source exceeds the 64 MiB limit".into(),
        ));
    }
    let reader = image::ImageReader::open(request.path())
        .map_err(ThumbnailError::Io)?
        .with_guessed_format()
        .map_err(ThumbnailError::Io)?;
    let (width, height) = reader
        .into_dimensions()
        .map_err(|error| ThumbnailError::Worker(error.to_string().into()))?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| ThumbnailError::Worker("image dimensions overflow".into()))?;
    if pixels > MAX_PIXELS || pixels.saturating_mul(4) > MAX_DECODED_BYTES {
        return Err(ThumbnailError::Worker(
            "decoded image exceeds the thumbnail safety limits".into(),
        ));
    }

    let decoded = image::ImageReader::open(request.path())
        .map_err(ThumbnailError::Io)?
        .with_guessed_format()
        .map_err(ThumbnailError::Io)?
        .decode()
        .map_err(|error| ThumbnailError::Worker(error.to_string().into()))?
        .into_rgba8();
    let maximum = request.size().pixels();
    let scale = (maximum as f64 / width as f64)
        .min(maximum as f64 / height as f64)
        .min(1.0);
    let target_width = ((width as f64 * scale).round() as u32).max(1);
    let target_height = ((height as f64 * scale).round() as u32).max(1);
    let source = Image::from_vec_u8(width, height, decoded.into_raw(), PixelType::U8x4)
        .map_err(|error| ThumbnailError::Worker(error.to_string().into()))?;
    let mut target = Image::new(target_width, target_height, PixelType::U8x4);
    Resizer::new()
        .resize(&source, &mut target, None)
        .map_err(|error| ThumbnailError::Worker(error.to_string().into()))?;
    cache.store_rgba(request, target_width, target_height, target.buffer())
}

#[derive(Debug)]
pub enum ThumbnailError {
    InvalidLimits,
    Cancelled,
    Timeout,
    Io(io::Error),
    Cache(Box<str>),
    Worker(Box<str>),
}

impl fmt::Display for ThumbnailError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLimits => formatter.write_str(
                "thumbnail limits must be positive and within the worker and timeout budgets",
            ),
            Self::Cancelled => formatter.write_str("thumbnail generation cancelled"),
            Self::Timeout => formatter.write_str("thumbnail generation timed out"),
            Self::Io(error) => write!(formatter, "thumbnail I/O failed: {error}"),
            Self::Cache(error) => write!(formatter, "thumbnail cache failed: {error}"),
            Self::Worker(error) => write!(formatter, "thumbnail worker failed: {error}"),
        }
    }
}

impl std::error::Error for ThumbnailError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}
