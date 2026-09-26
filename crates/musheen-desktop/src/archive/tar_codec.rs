use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::Instant;

use musheen_core::{CancellationToken, ProviderId};

use super::ArchivePath;
use super::format::{ArchiveScanner, RawArchiveEntry, RawEntryKind};
use super::io::{PositionedFile, TimedReader};
use super::store::{
    AllocationLease, ArchiveError, ArchiveLimits, DecodeCounterState, elapsed_limit,
};

pub(crate) fn open_plain(
    source: PositionedFile,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
) -> TarScanner {
    TarScanner::new_seekable(source, provider, limits, counters)
}

pub(crate) fn open_stream(
    source: Box<dyn Read + Send>,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    compressed_bytes: u64,
) -> TarScanner {
    TarScanner::new_stream(source, provider, limits, counters, compressed_bytes)
}

enum TarInput {
    Seekable(TimedReader<PositionedFile>),
    Stream(Box<dyn Read + Send>),
}

impl TarInput {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Seekable(reader) => reader.read(bytes),
            Self::Stream(reader) => reader.read(bytes),
        }
    }

    fn seek_skip(&mut self, count: u64) -> std::io::Result<bool> {
        match self {
            Self::Seekable(reader) => {
                reader.seek(SeekFrom::Current(i64::try_from(count).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "tar offset is too large")
                })?))?;
                Ok(true)
            }
            Self::Stream(_) => Ok(false),
        }
    }
}

/// Reads a tar stream one header at a time. Listing, extraction and
/// browsing all read tar through it, so each sees the same entries, with the
/// same numbers and sizes, and its extension headers stay in the metadata
/// budget.
pub(crate) struct TarScanner {
    input: TarInput,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    pending_skip: u64,
    pending_path: Option<(Vec<u8>, AllocationLease)>,
    pending_size: Option<u64>,
    ordinal: u64,
    finished: bool,
    started: Instant,
    compressed: bool,
    streamed_bytes: u64,
    compressed_bytes: u64,
}

impl TarScanner {
    fn new_seekable(
        source: PositionedFile,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
    ) -> Self {
        Self {
            input: TarInput::Seekable(TimedReader::new(
                source,
                limits.max_elapsed,
                Arc::clone(&counters),
            )),
            provider,
            limits,
            counters,
            pending_skip: 0,
            pending_path: None,
            pending_size: None,
            ordinal: 0,
            finished: false,
            started: Instant::now(),
            compressed: false,
            streamed_bytes: 0,
            compressed_bytes: 0,
        }
    }

    fn new_stream(
        source: Box<dyn Read + Send>,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
        compressed_bytes: u64,
    ) -> Self {
        Self {
            input: TarInput::Stream(source),
            provider,
            limits,
            counters,
            pending_skip: 0,
            pending_path: None,
            pending_size: None,
            ordinal: 0,
            finished: false,
            started: Instant::now(),
            compressed: true,
            streamed_bytes: 0,
            compressed_bytes,
        }
    }

    fn record_stream_read(&mut self, count: usize) -> Result<(), ArchiveError> {
        if !self.compressed {
            return Ok(());
        }
        self.streamed_bytes = self.streamed_bytes.saturating_add(count as u64);
        let ratio_limit = self
            .compressed_bytes
            .saturating_mul(self.limits.max_compression_ratio);
        let (resource, maximum) = if self.streamed_bytes > self.limits.max_expanded_bytes {
            ("expanded bytes", self.limits.max_expanded_bytes)
        } else if self.streamed_bytes > ratio_limit {
            ("compression ratio", ratio_limit)
        } else {
            return Ok(());
        };
        Err(ArchiveError::LimitExceeded {
            resource,
            value: usize::try_from(self.streamed_bytes).unwrap_or(usize::MAX),
            maximum: usize::try_from(maximum).unwrap_or(usize::MAX),
        })
    }

    fn check_runtime(&self, cancellation: &CancellationToken) -> Result<(), ArchiveError> {
        cancellation
            .wait_if_paused()
            .map_err(|_| ArchiveError::Cancelled)?;
        if self.started.elapsed() > self.limits.max_elapsed {
            return Err(elapsed_limit(
                self.started.elapsed(),
                self.limits.max_elapsed,
            ));
        }
        Ok(())
    }

    fn read_exact(
        &mut self,
        mut bytes: &mut [u8],
        cancellation: &CancellationToken,
    ) -> Result<(), ArchiveError> {
        while !bytes.is_empty() {
            self.check_runtime(cancellation)?;
            let count = self
                .input
                .read(bytes)
                .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
            if count == 0 {
                return Err(ArchiveError::InvalidArchive);
            }
            self.record_stream_read(count)?;
            bytes = &mut bytes[count..];
        }
        Ok(())
    }

    fn skip(
        &mut self,
        mut count: u64,
        cancellation: &CancellationToken,
    ) -> Result<(), ArchiveError> {
        self.check_runtime(cancellation)?;
        if self
            .input
            .seek_skip(count)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?
        {
            return self.check_runtime(cancellation);
        }
        let mut buffer = [0_u8; 32 * 1_024];
        while count > 0 {
            self.check_runtime(cancellation)?;
            let requested = usize::try_from(count.min(buffer.len() as u64)).unwrap_or(buffer.len());
            let read = self
                .input
                .read(&mut buffer[..requested])
                .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
            if read == 0 {
                return Err(ArchiveError::InvalidArchive);
            }
            self.record_stream_read(read)?;
            count -= read as u64;
        }
        Ok(())
    }

    fn read_extension(
        &mut self,
        size: u64,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, AllocationLease), ArchiveError> {
        let size = usize::try_from(size).map_err(|_| ArchiveError::LimitExceeded {
            resource: "metadata bytes",
            value: usize::MAX,
            maximum: self.limits.max_metadata_bytes,
        })?;
        let allocation = self
            .counters
            .reserve(size.saturating_mul(2), self.limits.max_metadata_bytes)?;
        let mut bytes = vec![0_u8; size];
        self.read_exact(&mut bytes, cancellation)?;
        let padding = (512 - (size as u64 % 512)) % 512;
        self.skip(padding, cancellation)?;
        Ok((bytes, allocation))
    }
}

impl ArchiveScanner for TarScanner {
    fn next_entry(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        if self.finished {
            return Ok(None);
        }
        cancellation
            .wait_if_paused()
            .map_err(|_| ArchiveError::Cancelled)?;
        self.skip(self.pending_skip, cancellation)?;
        self.pending_skip = 0;
        loop {
            let mut block = [0_u8; 512];
            self.read_exact(&mut block, cancellation)?;
            if block.iter().all(|byte| *byte == 0) {
                self.finished = true;
                return Ok(None);
            }
            if !valid_tar_checksum(&block) {
                return Err(ArchiveError::InvalidArchive);
            }
            let header = tar::Header::from_byte_slice(&block);
            let size = header
                .entry_size()
                .map_err(|_| ArchiveError::InvalidArchive)?;
            let entry_type = header.entry_type();
            if entry_type.is_gnu_longname() {
                let (mut path, allocation) = self.read_extension(size, cancellation)?;
                while path.last().is_some_and(|byte| matches!(byte, 0 | b'\n')) {
                    path.pop();
                }
                self.pending_path = Some((path, allocation));
                continue;
            }
            if entry_type.is_pax_local_extensions() {
                let (bytes, allocation) = self.read_extension(size, cancellation)?;
                let (path, pax_size) = pax_records(&bytes)?;
                if pax_size.is_some() {
                    self.pending_size = pax_size;
                }
                if let Some(path) = path {
                    self.pending_path = Some((path, allocation));
                }
                continue;
            }
            if entry_type.is_pax_global_extensions() || entry_type.is_gnu_longlink() {
                let _discarded = self.read_extension(size, cancellation)?;
                continue;
            }
            // A pax size record replaces the header's size, as in tar-rs and
            // GNU tar.
            let size = self.pending_size.take().unwrap_or(size);
            let raw_path = self
                .pending_path
                .take()
                .map_or_else(|| header.path_bytes().into_owned(), |(path, _)| path);
            let allocation = self.counters.reserve(
                raw_path
                    .len()
                    .saturating_mul(2)
                    .saturating_add(std::mem::size_of::<RawArchiveEntry>()),
                self.limits.max_metadata_bytes,
            )?;
            let path = ArchivePath::normalize_bytes(&raw_path, self.limits.max_path_bytes)?;
            let kind = if entry_type.is_dir() {
                RawEntryKind::Directory
            } else if entry_type.is_file() {
                RawEntryKind::RegularFile
            } else if entry_type.is_symlink() {
                RawEntryKind::SymbolicLink
            } else if entry_type.is_hard_link() {
                RawEntryKind::HardLink
            } else {
                RawEntryKind::Other
            };
            self.pending_skip = size.saturating_add((512 - (size % 512)) % 512);
            let ordinal = self.ordinal;
            self.ordinal = self.ordinal.saturating_add(1);
            return Ok(Some(RawArchiveEntry {
                provider: self.provider.clone(),
                path,
                kind,
                size: (kind == RawEntryKind::RegularFile).then_some(size),
                compressed_size: (kind == RawEntryKind::RegularFile && !self.compressed)
                    .then_some(size),
                ordinal,
                _allocation: allocation,
            }));
        }
    }
}

impl TarScanner {
    /// Hands the bytes of the entry `next_entry` just returned, `size` of
    /// them, to `visit`, and skips whatever `visit` leaves.
    fn visit_current(
        &mut self,
        size: u64,
        cancellation: &CancellationToken,
        visit: &mut dyn FnMut(&mut dyn Read) -> Result<(), ArchiveError>,
    ) -> Result<(), ArchiveError> {
        let padding = self.pending_skip.saturating_sub(size);
        self.pending_skip = 0;
        let mut data = EntryData {
            scanner: self,
            remaining: size,
            cancellation,
            error: None,
        };
        let visited = visit(&mut data);
        let remaining = data.remaining;
        if let Some(error) = data.error.take() {
            return Err(error);
        }
        visited?;
        self.pending_skip = remaining.saturating_add(padding);
        Ok(())
    }
}

/// The bytes of one tar entry, read through the scanner so its limits,
/// counters and cancellation apply.
struct EntryData<'a> {
    scanner: &'a mut TarScanner,
    remaining: u64,
    cancellation: &'a CancellationToken,
    error: Option<ArchiveError>,
}

impl Read for EntryData<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 || buffer.is_empty() {
            return Ok(0);
        }
        let requested =
            usize::try_from(self.remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let scanner = &mut *self.scanner;
        let read = scanner.check_runtime(self.cancellation).and_then(|()| {
            let count = scanner
                .input
                .read(&mut buffer[..requested])
                .map_err(|_| time_or_invalid(&scanner.counters, &scanner.limits))?;
            if count == 0 {
                return Err(ArchiveError::InvalidArchive);
            }
            scanner.record_stream_read(count)?;
            Ok(count)
        });
        match read {
            Ok(count) => {
                self.remaining -= count as u64;
                Ok(count)
            }
            Err(error) => {
                self.error = Some(error);
                Err(std::io::Error::other("tar entry could not be read"))
            }
        }
    }
}

/// Copies the entry numbered `ordinal` into `destination`.
pub(crate) fn copy_entry(
    mut scanner: TarScanner,
    ordinal: u64,
    cancellation: &CancellationToken,
    destination: &mut dyn Write,
) -> Result<(), ArchiveError> {
    while let Some(entry) = scanner.next_entry(cancellation)? {
        if entry.ordinal == ordinal {
            return scanner.visit_current(
                entry.size.unwrap_or(0),
                cancellation,
                &mut |contents| {
                    std::io::copy(contents, destination)
                        .map(|_| ())
                        .map_err(|_| ArchiveError::Io)
                },
            );
        }
    }
    Err(ArchiveError::NotArchiveEntry)
}

/// Visits the regular files of a tar stream in archive order, in one pass,
/// handing each one `wanted` names to `visit` with a reader over its bytes.
/// A read error inside `visit` ends the pass with the scanner's own error.
pub(crate) fn copy_files_in_order(
    mut scanner: TarScanner,
    cancellation: &CancellationToken,
    wanted: &dyn Fn(u64) -> bool,
    visit: &mut dyn FnMut(u64, &mut dyn Read) -> Result<(), ArchiveError>,
) -> Result<(), ArchiveError> {
    while let Some(entry) = scanner.next_entry(cancellation)? {
        if entry.kind == RawEntryKind::RegularFile && wanted(entry.ordinal) {
            let ordinal = entry.ordinal;
            scanner.visit_current(entry.size.unwrap_or(0), cancellation, &mut |contents| {
                visit(ordinal, contents)
            })?;
        }
    }
    Ok(())
}

pub(crate) fn valid_tar_checksum(block: &[u8; 512]) -> bool {
    let header = tar::Header::from_byte_slice(block);
    let Ok(stored) = header.cksum() else {
        return false;
    };
    let calculated = block[..148]
        .iter()
        .chain([b' '; 8].iter())
        .chain(block[156..].iter())
        .map(|byte| u32::from(*byte))
        .sum::<u32>();
    stored == calculated
}

/// The path and size records of a pax extended header.
fn pax_records(bytes: &[u8]) -> Result<(Option<Vec<u8>>, Option<u64>), ArchiveError> {
    let mut offset = 0_usize;
    let mut path = None;
    let mut size = None;
    while offset < bytes.len() {
        let space = bytes[offset..]
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or(ArchiveError::InvalidArchive)?
            + offset;
        let length = std::str::from_utf8(&bytes[offset..space])
            .ok()
            .and_then(|text| text.parse::<usize>().ok())
            .ok_or(ArchiveError::InvalidArchive)?;
        if length == 0 || offset.saturating_add(length) > bytes.len() {
            return Err(ArchiveError::InvalidArchive);
        }
        let record = &bytes[space + 1..offset + length];
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(value.strip_suffix(b"\n").unwrap_or(value).to_vec());
        }
        if let Some(value) = record.strip_prefix(b"size=") {
            let value = value.strip_suffix(b"\n").unwrap_or(value);
            size = Some(
                std::str::from_utf8(value)
                    .ok()
                    .and_then(|text| text.parse::<u64>().ok())
                    .ok_or(ArchiveError::InvalidArchive)?,
            );
        }
        offset += length;
    }
    Ok((path, size))
}

fn time_or_invalid(counters: &DecodeCounterState, limits: &ArchiveLimits) -> ArchiveError {
    if counters.elapsed() > limits.max_elapsed {
        ArchiveError::LimitExceeded {
            resource: "archive metadata milliseconds",
            value: usize::try_from(counters.elapsed().as_millis()).unwrap_or(usize::MAX),
            maximum: usize::try_from(limits.max_elapsed.as_millis().max(1)).unwrap_or(usize::MAX),
        }
    } else {
        ArchiveError::InvalidArchive
    }
}
