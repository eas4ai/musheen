use super::budget::{ArchiveMemoryLease, ArchiveOperationError};
use super::store::{AllocationLease, ArchiveError, ArchiveLimits, DecodeCounterState};
use miniz_oxide::deflate::core::CompressorOxide;
use miniz_oxide::inflate::stream::InflateState;
use std::io::BufRead;
use std::io::{self, Read, Write};
use std::mem::size_of;
use std::sync::Arc;

// These bounds match the pinned miniz_oxide 0.9.1 allocator layout. CompressorOxide
// owns the inline LZ code buffer; the remaining arrays are its four boxed allocations.
const FLATE_STREAM_BUFFER_BYTES: usize = 32 * 1024;
// `zip` 6.0.0 constructs `std::io::BufReader::new` around each compressed entry. The pinned Rust
// toolchain uses the standard library's documented default capacity of 8 KiB.
const ZIP_INPUT_BUFFER_BYTES: usize = 8 * 1024;
const MINIZ_DICTIONARY_BYTES: usize = 32_768 + 258;
const MINIZ_HASH_BYTES: usize = 2 * 32_768 * size_of::<u16>();
const MINIZ_HUFFMAN_BYTES: usize = 3 * 288 * (2 * size_of::<u16>() + size_of::<u8>());
const MINIZ_OUTPUT_BYTES: usize = (64 * 1024 * 13) / 10;
const ZSTD_MAX_WINDOW_LOG: u32 = 27;
const ZSTD_MAX_WINDOW_BYTES: usize = 1 << ZSTD_MAX_WINDOW_LOG;

pub(crate) const fn deflate_encoder_workspace_bytes() -> usize {
    size_of::<CompressorOxide>()
        + MINIZ_DICTIONARY_BYTES
        + MINIZ_HASH_BYTES
        + MINIZ_HUFFMAN_BYTES
        + MINIZ_OUTPUT_BYTES
        + FLATE_STREAM_BUFFER_BYTES
}

pub(crate) const fn gzip_encoder_workspace_bytes() -> usize {
    // GzEncoder additionally allocates its fixed ten-byte default header.
    deflate_encoder_workspace_bytes() + 10
}

pub(crate) const fn gzip_decoder_workspace_bytes() -> usize {
    size_of::<InflateState>() + FLATE_STREAM_BUFFER_BYTES
}

pub(crate) const fn zip_deflate_decoder_workspace_bytes() -> usize {
    size_of::<InflateState>() + ZIP_INPUT_BUFFER_BYTES
}

pub(crate) fn zip_zstd_decoder_workspace_bytes(frame_header: &[u8]) -> Result<usize, ArchiveError> {
    musheen_zstd_budget::decompression_stream_bytes_from_frame(frame_header)
        .and_then(|bytes| bytes.checked_add(ZIP_INPUT_BUFFER_BYTES))
        .ok_or(ArchiveError::InvalidArchive)
}

pub(crate) const fn zip_stored_reader_workspace_bytes() -> usize {
    ZIP_INPUT_BUFFER_BYTES
}

pub(crate) fn zstd_encoder_workspace_bytes() -> Result<usize, ArchiveOperationError> {
    musheen_zstd_budget::compression_stream_bytes(0).ok_or(ArchiveOperationError::Io)
}

pub(crate) fn zstd_decoder_workspace_bytes() -> Result<usize, ArchiveError> {
    musheen_zstd_budget::decompression_stream_bytes(ZSTD_MAX_WINDOW_BYTES)
        .ok_or(ArchiveError::InvalidArchive)
}

pub(crate) fn configure_zstd_decoder<R: BufRead>(
    decoder: &mut zstd::stream::read::Decoder<'_, R>,
) -> io::Result<()> {
    decoder.window_log_max(ZSTD_MAX_WINDOW_LOG)
}

pub(crate) fn reserve_decode_workspace(
    counters: &Arc<DecodeCounterState>,
    limits: &ArchiveLimits,
    bytes: usize,
) -> Result<AllocationLease, ArchiveError> {
    counters.reserve(bytes, limits.max_metadata_bytes)
}

pub(crate) struct WorkspaceReader<R> {
    inner: R,
    _workspace: AllocationLease,
}

impl<R> WorkspaceReader<R> {
    pub(crate) fn new(inner: R, workspace: AllocationLease) -> Self {
        Self {
            inner,
            _workspace: workspace,
        }
    }
}

impl<R: Read> Read for WorkspaceReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

pub(crate) struct WorkspaceWriter<W> {
    inner: W,
    _workspace: ArchiveMemoryLease,
}

impl<W> WorkspaceWriter<W> {
    pub(crate) fn new(inner: W, workspace: ArchiveMemoryLease) -> Self {
        Self {
            inner,
            _workspace: workspace,
        }
    }

    pub(crate) fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for WorkspaceWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
