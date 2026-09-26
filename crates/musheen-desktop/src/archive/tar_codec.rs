use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::Instant;

use musheen_core::{CancellationToken, ProviderId};

use super::ArchivePath;
use super::format::{ArchiveCopyContext, ArchiveScanner, RawArchiveEntry, RawEntryKind};
use super::io::{DecodeReader, PositionedFile, TimedReader};
use super::store::{
    AllocationLease, ArchiveError, ArchiveLimits, DecodeCounterState, elapsed_limit,
};

pub(crate) fn open_plain(
    source: PositionedFile,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
) -> Box<dyn ArchiveScanner> {
    Box::new(TarScanner::new_seekable(source, provider, limits, counters))
}

pub(crate) fn open_stream(
    source: Box<dyn Read + Send>,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    compressed_bytes: u64,
) -> Box<dyn ArchiveScanner> {
    Box::new(TarScanner::new_stream(
        source,
        provider,
        limits,
        counters,
        compressed_bytes,
    ))
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

struct TarScanner {
    input: TarInput,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    pending_skip: u64,
    pending_path: Option<(Vec<u8>, AllocationLease)>,
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
                if let Some(path) = pax_path(&bytes)? {
                    self.pending_path = Some((path, allocation));
                }
                continue;
            }
            if entry_type.is_pax_global_extensions() || entry_type.is_gnu_longlink() {
                let _discarded = self.read_extension(size, cancellation)?;
                continue;
            }
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

pub(crate) fn copy_tar<R: Read, W: Write>(
    reader: R,
    ordinal: u64,
    destination: &mut W,
) -> Result<(), ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let mut entries = archive
        .entries()
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let mut entry = entries
        .nth(usize::try_from(ordinal).map_err(|_| ArchiveError::NotArchiveEntry)?)
        .ok_or(ArchiveError::NotArchiveEntry)?
        .map_err(|_| ArchiveError::InvalidArchive)?;
    std::io::copy(&mut entry, destination).map_err(|_| ArchiveError::Io)?;
    Ok(())
}

/// Visits the regular files of a tar stream in archive order, in one pass,
/// handing each one `wanted` names to `visit` with a reader over its bytes.
pub(crate) fn copy_files_in_order<R: Read>(
    reader: R,
    wanted: &dyn Fn(u64) -> bool,
    visit: &mut dyn FnMut(u64, &mut dyn Read) -> Result<(), ArchiveError>,
) -> Result<(), ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let entries = archive
        .entries()
        .map_err(|_| ArchiveError::InvalidArchive)?;
    for (ordinal, entry) in entries.enumerate() {
        let mut entry = entry.map_err(|_| ArchiveError::InvalidArchive)?;
        let ordinal = u64::try_from(ordinal).map_err(|_| ArchiveError::InvalidArchive)?;
        if wanted(ordinal) {
            visit(ordinal, &mut entry)?;
        }
    }
    Ok(())
}

pub(crate) fn copy_guarded_tar<R: Read, W: Write>(
    reader: R,
    ordinal: u64,
    destination: &mut W,
    context: &ArchiveCopyContext<'_>,
    compressed_size: u64,
) -> Result<(), ArchiveError> {
    let mut guarded = DecodeReader::new(
        reader,
        context.cancellation.clone(),
        context.limits.max_elapsed,
        context.limits.max_expanded_bytes,
        compressed_size.saturating_mul(context.limits.max_compression_ratio),
    );
    let result = copy_tar(&mut guarded, ordinal, destination);
    guarded.take_error().map_or(result, Err)
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

fn pax_path(bytes: &[u8]) -> Result<Option<Vec<u8>>, ArchiveError> {
    let mut offset = 0_usize;
    let mut path = None;
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
        offset += length;
    }
    Ok(path)
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
