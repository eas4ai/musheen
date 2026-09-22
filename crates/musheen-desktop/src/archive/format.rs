use super::ArchivePath;
use super::io::{BoundedWriter, DecodeReader, PositionedFile, TimedReader};
use super::store::{
    AllocationLease, ArchiveError, ArchiveLimits, ArchivePassword, ArchivePasswordProvider,
    DecodeCounterState, PasswordRequest, elapsed_limit,
};
use musheen_core::{CancellationToken, ProviderId};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGzip,
    TarZstd,
    SevenZip,
    #[cfg(feature = "archive-libarchive")]
    Rar,
    #[cfg(feature = "archive-libarchive")]
    Iso,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RawEntryKind {
    Directory,
    RegularFile,
    SymbolicLink,
    HardLink,
    Other,
}

#[derive(Debug)]
pub(crate) struct RawArchiveEntry {
    pub(crate) provider: ProviderId,
    pub(crate) path: Vec<u8>,
    pub(crate) kind: RawEntryKind,
    pub(crate) size: Option<u64>,
    pub(crate) compressed_size: Option<u64>,
    pub(crate) ordinal: u64,
    _allocation: AllocationLease,
}

pub(crate) trait ArchiveScanner: Send {
    fn next_entry(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError>;
}

pub(crate) fn open_scanner(
    file: &File,
    provider: ProviderId,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
    passwords: &dyn ArchivePasswordProvider,
) -> Result<Box<dyn ArchiveScanner>, ArchiveError> {
    let source_bytes = file.metadata().map_err(|_| ArchiveError::Io)?.len();
    let source = PositionedFile::new(file).map_err(|_| ArchiveError::Io)?;
    match format {
        ArchiveFormat::Zip => Ok(Box::new(ZipScanner::new(
            source,
            provider,
            limits.clone(),
            Arc::clone(counters),
        )?)),
        ArchiveFormat::Tar => Ok(Box::new(TarScanner::new_seekable(
            source,
            provider,
            limits.clone(),
            Arc::clone(counters),
        ))),
        ArchiveFormat::TarGzip => Ok(Box::new(TarScanner::new_stream(
            Box::new(flate2::read::GzDecoder::new(TimedReader::new(
                source,
                limits.max_elapsed,
                Arc::clone(counters),
            ))),
            provider,
            limits.clone(),
            Arc::clone(counters),
            source_bytes,
        ))),
        ArchiveFormat::TarZstd => {
            let decoder = zstd::stream::read::Decoder::new(TimedReader::new(
                source,
                limits.max_elapsed,
                Arc::clone(counters),
            ))
            .map_err(|_| ArchiveError::InvalidArchive)?;
            Ok(Box::new(TarScanner::new_stream(
                Box::new(decoder),
                provider,
                limits.clone(),
                Arc::clone(counters),
                source_bytes,
            )))
        }
        ArchiveFormat::SevenZip => Ok(Box::new(SevenZipScanner::new(
            source,
            provider,
            limits.clone(),
            Arc::clone(counters),
            passwords,
        )?)),
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Rar | ArchiveFormat::Iso => Ok(Box::new(LibarchiveScanner::new(
            source,
            provider,
            limits.clone(),
            Arc::clone(counters),
        )?)),
    }
}

pub(crate) struct ArchiveCopyContext<'a> {
    pub(crate) passwords: &'a dyn ArchivePasswordProvider,
    pub(crate) limits: &'a ArchiveLimits,
    pub(crate) counters: &'a Arc<DecodeCounterState>,
    pub(crate) cancellation: &'a CancellationToken,
    pub(crate) compressed_size: Option<u64>,
}

pub(crate) fn copy_entry<W: Write>(
    file: &File,
    format: ArchiveFormat,
    ordinal: u64,
    destination: &mut W,
    context: ArchiveCopyContext<'_>,
) -> Result<(), ArchiveError> {
    let source = PositionedFile::new(file).map_err(|_| ArchiveError::Io)?;
    let reader = TimedReader::new_cancellable(
        source,
        context.limits.max_elapsed,
        Arc::clone(context.counters),
        context.cancellation.clone(),
    );
    let compressed_size = context.compressed_size.unwrap_or_else(|| {
        file.metadata()
            .map(|metadata| metadata.len())
            .unwrap_or_default()
    });
    let mut destination = BoundedWriter::new(
        destination,
        context.limits.max_nested_archive_bytes,
        context.limits.max_expanded_bytes,
        compressed_size.saturating_mul(context.limits.max_compression_ratio),
        context.cancellation.clone(),
        context.limits.max_elapsed,
    );
    let result = match format {
        ArchiveFormat::Zip => copy_zip(
            reader,
            ordinal,
            context.passwords,
            &mut destination,
            context.limits,
            context.counters,
        ),
        ArchiveFormat::Tar => copy_tar(reader, ordinal, &mut destination),
        ArchiveFormat::TarGzip => {
            let decoder = flate2::read::GzDecoder::new(reader);
            copy_guarded_tar(
                decoder,
                ordinal,
                &mut destination,
                &context,
                compressed_size,
            )
        }
        ArchiveFormat::TarZstd => {
            let decoder = zstd::stream::read::Decoder::new(reader)
                .map_err(|_| ArchiveError::InvalidArchive)?;
            copy_guarded_tar(
                decoder,
                ordinal,
                &mut destination,
                &context,
                compressed_size,
            )
        }
        ArchiveFormat::SevenZip => copy_seven_zip(
            reader,
            ordinal,
            context.passwords,
            &mut destination,
            &context,
        ),
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Rar | ArchiveFormat::Iso => Err(ArchiveError::UnsupportedNestedFormat),
    };
    if let Some(error) = destination.take_error() {
        return Err(error);
    }
    if context.cancellation.check().is_err() {
        return Err(ArchiveError::Cancelled);
    }
    result
}

struct ZipScanner {
    reader: TimedReader<PositionedFile>,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    remaining: u64,
    central_end: u64,
    ordinal: u64,
}

impl ZipScanner {
    fn new(
        source: PositionedFile,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
    ) -> Result<Self, ArchiveError> {
        let mut reader = TimedReader::new(source, limits.max_elapsed, Arc::clone(&counters));
        let (central_offset, central_size, entries) =
            preflight_zip(&mut reader, &limits, &counters)?;
        reader
            .seek(SeekFrom::Start(central_offset))
            .map_err(|_| time_or_invalid(&counters, &limits))?;
        Ok(Self {
            reader,
            provider,
            limits,
            counters,
            remaining: entries,
            central_end: central_offset.saturating_add(central_size),
            ordinal: 0,
        })
    }
}

impl ArchiveScanner for ZipScanner {
    fn next_entry(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
        if self.remaining == 0 {
            return Ok(None);
        }
        let position = self
            .reader
            .stream_position()
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        if position.saturating_add(46) > self.central_end {
            return Err(ArchiveError::InvalidArchive);
        }
        let mut header = [0_u8; 46];
        self.reader
            .read_exact(&mut header)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        if &header[..4] != b"PK\x01\x02" {
            return Err(ArchiveError::InvalidArchive);
        }
        let name_length = le_u16(&header[28..30]) as usize;
        let extra_length = le_u16(&header[30..32]) as usize;
        let comment_length = le_u16(&header[32..34]) as u64;
        let trailing = (name_length as u64)
            .saturating_add(extra_length as u64)
            .saturating_add(comment_length);
        if position.saturating_add(46).saturating_add(trailing) > self.central_end {
            return Err(ArchiveError::InvalidArchive);
        }
        let raw_allocation = self.counters.reserve(
            name_length
                .saturating_mul(2)
                .saturating_add(extra_length)
                .saturating_add(std::mem::size_of::<RawArchiveEntry>()),
            self.limits.max_metadata_bytes,
        )?;
        let mut name = vec![0_u8; name_length];
        self.reader
            .read_exact(&mut name)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        let mut extra = vec![0_u8; extra_length];
        self.reader
            .read_exact(&mut extra)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        self.reader
            .seek(SeekFrom::Current(
                i64::try_from(comment_length).map_err(|_| ArchiveError::InvalidArchive)?,
            ))
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        let directory_name = name.last() == Some(&b'/');
        let name = ArchivePath::normalize_bytes(&name, self.limits.max_path_bytes)?;
        let external_attributes = le_u32(&header[38..42]);
        let unix_mode = external_attributes >> 16;
        let file_type = unix_mode & 0o170_000;
        let kind = if directory_name || file_type == 0o040_000 {
            RawEntryKind::Directory
        } else if file_type == 0o120_000 {
            RawEntryKind::SymbolicLink
        } else {
            RawEntryKind::RegularFile
        };
        let (compressed_size, expanded_size) = zip_entry_sizes(&header, &extra)?;
        let size = (kind == RawEntryKind::RegularFile).then_some(expanded_size);
        let ordinal = self.ordinal;
        self.ordinal = self.ordinal.saturating_add(1);
        self.remaining -= 1;
        Ok(Some(RawArchiveEntry {
            provider: self.provider.clone(),
            path: name,
            kind,
            size,
            compressed_size: (kind == RawEntryKind::RegularFile).then_some(compressed_size),
            ordinal,
            _allocation: raw_allocation,
        }))
    }
}

fn preflight_zip<R: Read + Seek>(
    reader: &mut R,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
) -> Result<(u64, u64, u64), ArchiveError> {
    let length = reader
        .seek(SeekFrom::End(0))
        .map_err(|_| ArchiveError::Io)?;
    let tail_length = length.min(65_557) as usize;
    let _tail_allocation = counters.reserve(tail_length, limits.max_metadata_bytes)?;
    reader
        .seek(SeekFrom::End(-(tail_length as i64)))
        .map_err(|_| ArchiveError::Io)?;
    let mut tail = vec![0_u8; tail_length];
    reader
        .read_exact(&mut tail)
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let eocd = (0..tail.len().saturating_sub(3))
        .rev()
        .find(|offset| {
            if tail.get(*offset..*offset + 4) != Some(b"PK\x05\x06") || tail.len() < *offset + 22 {
                return false;
            }
            let comment_length = le_u16(&tail[*offset + 20..*offset + 22]) as usize;
            *offset + 22 + comment_length == tail.len()
        })
        .ok_or(ArchiveError::InvalidArchive)?;
    let disk = le_u16(&tail[eocd + 4..eocd + 6]);
    let central_disk = le_u16(&tail[eocd + 6..eocd + 8]);
    let disk_entries = le_u16(&tail[eocd + 8..eocd + 10]);
    let total_entries = le_u16(&tail[eocd + 10..eocd + 12]);
    if disk != 0 || central_disk != 0 || disk_entries != total_entries {
        return Err(ArchiveError::InvalidArchive);
    }
    let central_size = le_u32(&tail[eocd + 12..eocd + 16]) as u64;
    let central_offset = le_u32(&tail[eocd + 16..eocd + 20]) as u64;
    let (central_offset, central_size, entries) = if total_entries == u16::MAX
        || central_size == u32::MAX as u64
        || central_offset == u32::MAX as u64
    {
        preflight_zip64(reader, &tail, eocd)?
    } else {
        (central_offset, central_size, total_entries as u64)
    };
    if entries > limits.max_entries as u64 {
        return Err(ArchiveError::LimitExceeded {
            resource: "archive entries",
            value: usize::try_from(entries).unwrap_or(usize::MAX),
            maximum: limits.max_entries,
        });
    }
    if central_offset.saturating_add(central_size) > length {
        return Err(ArchiveError::InvalidArchive);
    }
    Ok((central_offset, central_size, entries))
}

fn preflight_zip64<R: Read + Seek>(
    reader: &mut R,
    tail: &[u8],
    eocd: usize,
) -> Result<(u64, u64, u64), ArchiveError> {
    if eocd < 20 || &tail[eocd - 20..eocd - 16] != b"PK\x06\x07" {
        return Err(ArchiveError::InvalidArchive);
    }
    let offset = le_u64(&tail[eocd - 12..eocd - 4]);
    reader
        .seek(SeekFrom::Start(offset))
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let mut record = [0_u8; 56];
    reader
        .read_exact(&mut record)
        .map_err(|_| ArchiveError::InvalidArchive)?;
    if &record[..4] != b"PK\x06\x06" {
        return Err(ArchiveError::InvalidArchive);
    }
    Ok((
        le_u64(&record[48..56]),
        le_u64(&record[40..48]),
        le_u64(&record[32..40]),
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
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
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
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
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

struct SevenZipScanner {
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    archive: sevenz_rust2::Archive,
    next_index: usize,
    _codec_allocation: sevenz_rust2::ArchiveMemoryLease,
}

struct SevenZipMemoryBudget {
    counters: Arc<DecodeCounterState>,
    maximum: usize,
}

impl sevenz_rust2::ArchiveMemoryBudget for SevenZipMemoryBudget {
    fn try_reserve(&self, bytes: usize) -> bool {
        self.counters.try_reserve_external(bytes, self.maximum)
    }

    fn release(&self, bytes: usize) {
        self.counters.release_external(bytes);
    }
}

impl SevenZipScanner {
    fn new(
        source: PositionedFile,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
        passwords: &dyn ArchivePasswordProvider,
    ) -> Result<Self, ArchiveError> {
        let mut reader = TimedReader::new(source, limits.max_elapsed, Arc::clone(&counters));
        let (archive, codec_allocation, _password, _budget) =
            read_seven_archive(&mut reader, passwords, &limits, &counters)?;
        if archive.files.len() > limits.max_entries {
            return Err(ArchiveError::LimitExceeded {
                resource: "archive entries",
                value: archive.files.len(),
                maximum: limits.max_entries,
            });
        }
        Ok(Self {
            provider,
            limits,
            counters,
            archive,
            next_index: 0,
            _codec_allocation: codec_allocation,
        })
    }
}

impl ArchiveScanner for SevenZipScanner {
    fn next_entry(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
        while let Some(entry) = self.archive.files.get(self.next_index) {
            let ordinal = self.next_index as u64;
            self.next_index = self.next_index.saturating_add(1);
            if entry.is_anti_item {
                continue;
            }
            let allocation = self.counters.reserve(
                entry
                    .name
                    .len()
                    .saturating_add(std::mem::size_of::<RawArchiveEntry>()),
                self.limits.max_metadata_bytes,
            )?;
            let path =
                ArchivePath::normalize_bytes(entry.name.as_bytes(), self.limits.max_path_bytes)?;
            return Ok(Some(RawArchiveEntry {
                provider: self.provider.clone(),
                path,
                kind: if entry.is_directory {
                    RawEntryKind::Directory
                } else {
                    RawEntryKind::RegularFile
                },
                size: (!entry.is_directory).then_some(entry.size),
                compressed_size: (!entry.is_directory)
                    .then(|| self.archive.compressed_size_for_file(self.next_index - 1))
                    .flatten(),
                ordinal,
                _allocation: allocation,
            }));
        }
        Ok(None)
    }
}

#[cfg(feature = "archive-libarchive")]
struct LibarchiveScanner {
    commands: std::sync::mpsc::SyncSender<LibarchiveCommand>,
    responses: std::sync::mpsc::Receiver<Result<Option<RawArchiveEntry>, ArchiveError>>,
    worker: Option<std::thread::JoinHandle<()>>,
}

#[cfg(feature = "archive-libarchive")]
enum LibarchiveCommand {
    Next(CancellationToken),
    Stop,
}

#[cfg(feature = "archive-libarchive")]
impl LibarchiveScanner {
    fn new(
        source: PositionedFile,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
    ) -> Result<Self, ArchiveError> {
        let (commands, command_receiver) = std::sync::mpsc::sync_channel(1);
        let (response_sender, responses) = std::sync::mpsc::sync_channel(1);
        let (ready_sender, ready_receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("musheen-libarchive-scan".into())
            .spawn(move || {
                libarchive_worker(
                    source,
                    provider,
                    limits,
                    counters,
                    command_receiver,
                    response_sender,
                    ready_sender,
                );
            })
            .map_err(|_| ArchiveError::Io)?;
        ready_receiver.recv().map_err(|_| ArchiveError::Io)??;
        Ok(Self {
            commands,
            responses,
            worker: Some(worker),
        })
    }
}

#[cfg(feature = "archive-libarchive")]
impl Drop for LibarchiveScanner {
    fn drop(&mut self) {
        let _ = self.commands.send(LibarchiveCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(feature = "archive-libarchive")]
impl ArchiveScanner for LibarchiveScanner {
    fn next_entry(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
        self.commands
            .send(LibarchiveCommand::Next(cancellation.clone()))
            .map_err(|_| ArchiveError::Io)?;
        self.responses.recv().map_err(|_| ArchiveError::Io)?
    }
}

#[cfg(feature = "archive-libarchive")]
fn libarchive_worker(
    source: PositionedFile,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    commands: std::sync::mpsc::Receiver<LibarchiveCommand>,
    responses: std::sync::mpsc::SyncSender<Result<Option<RawArchiveEntry>, ArchiveError>>,
    ready: std::sync::mpsc::SyncSender<Result<(), ArchiveError>>,
) {
    use compress_tools::{ArchiveContents, ArchiveIteratorBuilder};

    let iterator = ArchiveIteratorBuilder::new(TimedReader::new(
        source,
        limits.max_elapsed,
        Arc::clone(&counters),
    ))
    .mtree_format(false)
    .build()
    .map_err(|_| ArchiveError::InvalidArchive);
    let Ok(mut iterator) = iterator else {
        let _ = ready.send(Err(ArchiveError::InvalidArchive));
        return;
    };
    if ready.send(Ok(())).is_err() {
        return;
    }
    let mut ordinal = 0_u64;
    while let Ok(command) = commands.recv() {
        let LibarchiveCommand::Next(cancellation) = command else {
            return;
        };
        let result = (|| {
            cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
            let allocation = counters.reserve(
                limits
                    .max_path_bytes
                    .saturating_mul(2)
                    .saturating_add(std::mem::size_of::<RawArchiveEntry>())
                    .saturating_add(std::mem::size_of::<compress_tools::stat>()),
                limits.max_metadata_bytes,
            )?;
            let (name, status) = match iterator.next_header() {
                Some(ArchiveContents::StartOfEntry(name, status)) => (name, status),
                Some(ArchiveContents::Err(_)) => return Err(ArchiveError::InvalidArchive),
                None => return Ok(None),
                _ => return Err(ArchiveError::InvalidArchive),
            };
            cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
            let path = ArchivePath::normalize_bytes(name.as_bytes(), limits.max_path_bytes)?;
            let file_type = status.st_mode & 0o170_000;
            let kind = match file_type {
                0o040_000 => RawEntryKind::Directory,
                0o100_000 if status.st_nlink > 1 => RawEntryKind::HardLink,
                0o100_000 => RawEntryKind::RegularFile,
                0o120_000 => RawEntryKind::SymbolicLink,
                _ => RawEntryKind::Other,
            };
            let entry = RawArchiveEntry {
                provider: provider.clone(),
                path,
                kind,
                size: (kind == RawEntryKind::RegularFile)
                    .then(|| u64::try_from(status.st_size).unwrap_or(0)),
                compressed_size: None,
                ordinal,
                _allocation: allocation,
            };
            ordinal = ordinal.saturating_add(1);
            Ok(Some(entry))
        })();
        if responses.send(result).is_err() {
            return;
        }
    }
}

fn read_seven_archive<R: Read + Seek>(
    reader: &mut R,
    passwords: &dyn ArchivePasswordProvider,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
) -> Result<
    (
        sevenz_rust2::Archive,
        sevenz_rust2::ArchiveMemoryLease,
        sevenz_rust2::Password,
        Arc<dyn sevenz_rust2::ArchiveMemoryBudget>,
    ),
    ArchiveError,
> {
    let budget: Arc<dyn sevenz_rust2::ArchiveMemoryBudget> = Arc::new(SevenZipMemoryBudget {
        counters: Arc::clone(counters),
        maximum: limits.max_metadata_bytes,
    });
    let empty_password = sevenz_rust2::Password::empty();
    match sevenz_rust2::Archive::read_with_memory_budget(
        reader,
        &empty_password,
        Arc::clone(&budget),
    ) {
        Ok((archive, allocation)) => Ok((archive, allocation, empty_password, budget)),
        Err(sevenz_rust2::Error::PasswordRequired) => {
            reader
                .seek(SeekFrom::Start(0))
                .map_err(|_| ArchiveError::InvalidArchive)?;
            let password = require_password(passwords, ArchiveFormat::SevenZip)?;
            let text = std::str::from_utf8(password.as_bytes())
                .map_err(|_| ArchiveError::InvalidPassword)?;
            let dependency_password = sevenz_rust2::Password::new(text);
            let result = sevenz_rust2::Archive::read_with_memory_budget(
                reader,
                &dependency_password,
                Arc::clone(&budget),
            )
            .map(|(archive, allocation)| (archive, allocation, dependency_password, budget))
            .map_err(|error| map_seven_error(error, limits.max_metadata_bytes));
            drop(password);
            result
        }
        Err(error) => Err(map_seven_error(error, limits.max_metadata_bytes)),
    }
}

fn map_seven_error(error: sevenz_rust2::Error, maximum: usize) -> ArchiveError {
    match error {
        sevenz_rust2::Error::MaybeBadPassword(_) => ArchiveError::InvalidPassword,
        sevenz_rust2::Error::MemoryLimitExceeded { requested } => ArchiveError::LimitExceeded {
            resource: "metadata bytes",
            value: maximum.saturating_add(requested),
            maximum,
        },
        _ => ArchiveError::InvalidArchive,
    }
}

fn copy_zip<R: Read + Seek, W: Write>(
    mut reader: R,
    ordinal: u64,
    passwords: &dyn ArchivePasswordProvider,
    destination: &mut W,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
) -> Result<(), ArchiveError> {
    let index = usize::try_from(ordinal).map_err(|_| ArchiveError::NotArchiveEntry)?;
    let (_, central_size, entries) = preflight_zip(&mut reader, limits, counters)?;
    let central_bytes = usize::try_from(central_size).map_err(|_| ArchiveError::LimitExceeded {
        resource: "metadata bytes",
        value: usize::MAX,
        maximum: limits.max_metadata_bytes,
    })?;
    let entry_bytes = usize::try_from(entries)
        .unwrap_or(usize::MAX)
        .saturating_mul(256);
    let _metadata = counters.reserve(
        central_bytes.saturating_mul(2).saturating_add(entry_bytes),
        limits.max_metadata_bytes,
    )?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let mut archive = zip::ZipArchive::new(reader).map_err(|_| ArchiveError::InvalidArchive)?;
    let encrypted = archive
        .by_index_raw(index)
        .map_err(|_| ArchiveError::NotArchiveEntry)?
        .encrypted();
    if encrypted {
        let password = require_password(passwords, ArchiveFormat::Zip)?;
        let mut entry = archive
            .by_index_decrypt(index, password.as_bytes())
            .map_err(|_| ArchiveError::InvalidPassword)?;
        std::io::copy(&mut entry, destination).map_err(|_| ArchiveError::Io)?;
    } else {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| ArchiveError::NotArchiveEntry)?;
        std::io::copy(&mut entry, destination).map_err(|_| ArchiveError::Io)?;
    }
    Ok(())
}

fn copy_tar<R: Read, W: Write>(
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

fn copy_guarded_tar<R: Read, W: Write>(
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

fn copy_seven_zip<R: Read + Seek, W: Write>(
    mut reader: R,
    ordinal: u64,
    passwords: &dyn ArchivePasswordProvider,
    destination: &mut W,
    context: &ArchiveCopyContext<'_>,
) -> Result<(), ArchiveError> {
    let (archive, _metadata, password, budget) =
        read_seven_archive(&mut reader, passwords, context.limits, context.counters)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let mut archive = sevenz_rust2::ArchiveReader::from_archive_sequential_with_memory_budget(
        archive, reader, password, budget,
    );
    let mut found = false;
    let mut decode_error = None;
    let decode_started = Instant::now();
    let mut decoded_bytes = 0_u64;
    let target = usize::try_from(ordinal).map_err(|_| ArchiveError::NotArchiveEntry)?;
    let result = archive.for_each_entries_through_index(target, |index, _entry, contents| {
        if index == target {
            match copy_with_guards(
                contents,
                destination,
                context,
                decode_started,
                &mut decoded_bytes,
            ) {
                Ok(()) => {
                    found = true;
                    Ok(())
                }
                Err(error) => {
                    decode_error = Some(error);
                    Err(sevenz_rust2::Error::from(std::io::Error::other(
                        "archive decode stopped",
                    )))
                }
            }
        } else {
            match copy_with_guards(
                contents,
                &mut std::io::sink(),
                context,
                decode_started,
                &mut decoded_bytes,
            ) {
                Ok(()) => Ok(()),
                Err(error) => {
                    decode_error = Some(error);
                    Err(sevenz_rust2::Error::from(std::io::Error::other(
                        "archive decode stopped",
                    )))
                }
            }
        }
    });
    if let Some(error) = decode_error {
        return Err(error);
    }
    result.map_err(|error| map_seven_error(error, context.limits.max_metadata_bytes))?;
    found.then_some(()).ok_or(ArchiveError::NotArchiveEntry)
}

fn copy_with_guards<R: Read + ?Sized, W: Write>(
    reader: &mut R,
    destination: &mut W,
    context: &ArchiveCopyContext<'_>,
    started: Instant,
    decoded_bytes: &mut u64,
) -> Result<(), ArchiveError> {
    let compressed_size = context.compressed_size.unwrap_or(0);
    let ratio_limit = compressed_size.saturating_mul(context.limits.max_compression_ratio);
    let mut buffer = [0_u8; 32 * 1_024];
    loop {
        context
            .cancellation
            .check()
            .map_err(|_| ArchiveError::Cancelled)?;
        if started.elapsed() > context.limits.max_elapsed {
            return Err(elapsed_limit(started.elapsed(), context.limits.max_elapsed));
        }
        let count = reader
            .read(&mut buffer)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        if count == 0 {
            return Ok(());
        }
        *decoded_bytes = decoded_bytes.saturating_add(count as u64);
        if *decoded_bytes > context.limits.max_expanded_bytes {
            return Err(ArchiveError::LimitExceeded {
                resource: "expanded bytes",
                value: usize::try_from(*decoded_bytes).unwrap_or(usize::MAX),
                maximum: usize::try_from(context.limits.max_expanded_bytes).unwrap_or(usize::MAX),
            });
        }
        if *decoded_bytes > ratio_limit {
            return Err(ArchiveError::LimitExceeded {
                resource: "compression ratio",
                value: usize::try_from(*decoded_bytes).unwrap_or(usize::MAX),
                maximum: usize::try_from(ratio_limit).unwrap_or(usize::MAX),
            });
        }
        destination
            .write_all(&buffer[..count])
            .map_err(|_| ArchiveError::Io)?;
    }
}

fn require_password(
    passwords: &dyn ArchivePasswordProvider,
    format: ArchiveFormat,
) -> Result<ArchivePassword, ArchiveError> {
    passwords
        .request_password(&PasswordRequest { format })?
        .ok_or(ArchiveError::PasswordRequired)
}

fn valid_tar_checksum(block: &[u8; 512]) -> bool {
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

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn zip_entry_sizes(header: &[u8; 46], extra: &[u8]) -> Result<(u64, u64), ArchiveError> {
    let mut compressed = le_u32(&header[20..24]) as u64;
    let mut expanded = le_u32(&header[24..28]) as u64;
    if compressed != u32::MAX as u64 && expanded != u32::MAX as u64 {
        return Ok((compressed, expanded));
    }
    let mut offset = 0_usize;
    while offset.saturating_add(4) <= extra.len() {
        let field_id = le_u16(&extra[offset..offset + 2]);
        let length = le_u16(&extra[offset + 2..offset + 4]) as usize;
        offset = offset.saturating_add(4);
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= extra.len())
            .ok_or(ArchiveError::InvalidArchive)?;
        if field_id == 0x0001 {
            let field = &extra[offset..end];
            let mut field_offset = 0_usize;
            if expanded == u32::MAX as u64 {
                expanded = field
                    .get(field_offset..field_offset + 8)
                    .map(le_u64)
                    .ok_or(ArchiveError::InvalidArchive)?;
                field_offset += 8;
            }
            if compressed == u32::MAX as u64 {
                compressed = field
                    .get(field_offset..field_offset + 8)
                    .map(le_u64)
                    .ok_or(ArchiveError::InvalidArchive)?;
            }
            return Ok((compressed, expanded));
        }
        offset = end;
    }
    Err(ArchiveError::InvalidArchive)
}
