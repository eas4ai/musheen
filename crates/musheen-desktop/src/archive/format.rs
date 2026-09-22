use super::ArchivePath;
use super::store::{
    AllocationLease, ArchiveError, ArchiveLimits, ArchivePassword, ArchivePasswordProvider,
    DecodeCounterState, PasswordRequest,
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
    let source = file.try_clone().map_err(|_| ArchiveError::Io)?;
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
        ))),
    }
}

pub(crate) struct ArchiveCopyContext<'a> {
    pub(crate) passwords: &'a dyn ArchivePasswordProvider,
    pub(crate) limits: &'a ArchiveLimits,
    pub(crate) counters: &'a Arc<DecodeCounterState>,
    pub(crate) cancellation: &'a CancellationToken,
}

pub(crate) fn copy_entry<W: Write>(
    file: &File,
    format: ArchiveFormat,
    ordinal: u64,
    destination: &mut W,
    context: ArchiveCopyContext<'_>,
) -> Result<(), ArchiveError> {
    let source = file.try_clone().map_err(|_| ArchiveError::Io)?;
    let reader = TimedReader::new(
        source,
        context.limits.max_elapsed,
        Arc::clone(context.counters),
    );
    let mut destination = BoundedWriter::new(
        destination,
        context.limits.max_nested_archive_bytes,
        context.cancellation.clone(),
    );
    let result = match format {
        ArchiveFormat::Zip => copy_zip(reader, ordinal, context.passwords, &mut destination),
        ArchiveFormat::Tar => copy_tar(reader, ordinal, &mut destination),
        ArchiveFormat::TarGzip => copy_tar(
            flate2::read::GzDecoder::new(reader),
            ordinal,
            &mut destination,
        ),
        ArchiveFormat::TarZstd => {
            let decoder = zstd::stream::read::Decoder::new(reader)
                .map_err(|_| ArchiveError::InvalidArchive)?;
            copy_tar(decoder, ordinal, &mut destination)
        }
        ArchiveFormat::SevenZip => {
            copy_seven_zip(reader, ordinal, context.passwords, &mut destination)
        }
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Rar | ArchiveFormat::Iso => Err(ArchiveError::UnsupportedNestedFormat),
    };
    if context.cancellation.check().is_err() {
        return Err(ArchiveError::Cancelled);
    }
    result
}

struct ZipScanner {
    reader: TimedReader<File>,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    remaining: u64,
    central_end: u64,
    ordinal: u64,
}

impl ZipScanner {
    fn new(
        source: File,
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
        let extra_length = le_u16(&header[30..32]) as u64;
        let comment_length = le_u16(&header[32..34]) as u64;
        let trailing = (name_length as u64)
            .saturating_add(extra_length)
            .saturating_add(comment_length);
        if position.saturating_add(46).saturating_add(trailing) > self.central_end {
            return Err(ArchiveError::InvalidArchive);
        }
        let raw_allocation = self.counters.reserve(
            name_length
                .saturating_mul(2)
                .saturating_add(std::mem::size_of::<RawArchiveEntry>()),
            self.limits.max_metadata_bytes,
        )?;
        let mut name = vec![0_u8; name_length];
        self.reader
            .read_exact(&mut name)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        self.reader
            .seek(SeekFrom::Current(
                i64::try_from(extra_length.saturating_add(comment_length))
                    .map_err(|_| ArchiveError::InvalidArchive)?,
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
        let size = (kind == RawEntryKind::RegularFile).then_some(le_u32(&header[24..28]) as u64);
        let ordinal = self.ordinal;
        self.ordinal = self.ordinal.saturating_add(1);
        self.remaining -= 1;
        Ok(Some(RawArchiveEntry {
            provider: self.provider.clone(),
            path: name,
            kind,
            size,
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
    Seekable(TimedReader<File>),
    Stream(Box<dyn Read + Send>),
}

impl TarInput {
    fn read_exact(&mut self, bytes: &mut [u8]) -> std::io::Result<()> {
        match self {
            Self::Seekable(reader) => reader.read_exact(bytes),
            Self::Stream(reader) => reader.read_exact(bytes),
        }
    }

    fn skip(&mut self, count: u64) -> std::io::Result<()> {
        match self {
            Self::Seekable(reader) => {
                reader.seek(SeekFrom::Current(i64::try_from(count).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "tar offset is too large")
                })?))?;
                Ok(())
            }
            Self::Stream(reader) => {
                let copied = std::io::copy(&mut reader.take(count), &mut std::io::sink())?;
                if copied == count {
                    Ok(())
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "truncated tar entry",
                    ))
                }
            }
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
}

impl TarScanner {
    fn new_seekable(
        source: File,
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
        }
    }

    fn new_stream(
        source: Box<dyn Read + Send>,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
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
        }
    }

    fn read_extension(&mut self, size: u64) -> Result<(Vec<u8>, AllocationLease), ArchiveError> {
        let size = usize::try_from(size).map_err(|_| ArchiveError::LimitExceeded {
            resource: "metadata bytes",
            value: usize::MAX,
            maximum: self.limits.max_metadata_bytes,
        })?;
        let allocation = self
            .counters
            .reserve(size.saturating_mul(2), self.limits.max_metadata_bytes)?;
        let mut bytes = vec![0_u8; size];
        self.input
            .read_exact(&mut bytes)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        let padding = (512 - (size as u64 % 512)) % 512;
        self.input
            .skip(padding)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
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
        self.input
            .skip(self.pending_skip)
            .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
        self.pending_skip = 0;
        loop {
            let mut block = [0_u8; 512];
            self.input
                .read_exact(&mut block)
                .map_err(|_| time_or_invalid(&self.counters, &self.limits))?;
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
                let (mut path, allocation) = self.read_extension(size)?;
                while path.last().is_some_and(|byte| matches!(byte, 0 | b'\n')) {
                    path.pop();
                }
                self.pending_path = Some((path, allocation));
                continue;
            }
            if entry_type.is_pax_local_extensions() {
                let (bytes, allocation) = self.read_extension(size)?;
                if let Some(path) = pax_path(&bytes)? {
                    self.pending_path = Some((path, allocation));
                }
                continue;
            }
            if entry_type.is_pax_global_extensions() || entry_type.is_gnu_longlink() {
                let _discarded = self.read_extension(size)?;
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
    entries: std::vec::IntoIter<sevenz_rust2::ArchiveEntry>,
    next_index: u64,
    _codec_allocation: AllocationLease,
}

impl SevenZipScanner {
    fn new(
        source: File,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
        passwords: &dyn ArchivePasswordProvider,
    ) -> Result<Self, ArchiveError> {
        let mut reader = TimedReader::new(source, limits.max_elapsed, Arc::clone(&counters));
        let archive = read_seven_archive(&mut reader, passwords)?;
        if archive.files.len() > limits.max_entries {
            return Err(ArchiveError::LimitExceeded {
                resource: "archive entries",
                value: archive.files.len(),
                maximum: limits.max_entries,
            });
        }
        let allocated = archive
            .files
            .capacity()
            .saturating_mul(std::mem::size_of::<sevenz_rust2::ArchiveEntry>())
            .saturating_add(
                archive
                    .files
                    .iter()
                    .map(|entry| entry.name.capacity())
                    .sum::<usize>(),
            )
            .saturating_add(
                archive
                    .blocks
                    .capacity()
                    .saturating_mul(std::mem::size_of::<sevenz_rust2::Block>()),
            );
        let codec_allocation = counters.reserve(allocated, limits.max_metadata_bytes)?;
        Ok(Self {
            provider,
            limits,
            counters,
            entries: archive.files.into_iter(),
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
        for entry in self.entries.by_ref() {
            let ordinal = self.next_index;
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
                ordinal,
                _allocation: allocation,
            }));
        }
        Ok(None)
    }
}

#[cfg(feature = "archive-libarchive")]
struct LibarchiveScanner {
    source: File,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    next_index: usize,
    finished: bool,
}

#[cfg(feature = "archive-libarchive")]
impl LibarchiveScanner {
    fn new(
        source: File,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
    ) -> Self {
        Self {
            source,
            provider,
            limits,
            counters,
            next_index: 0,
            finished: false,
        }
    }
}

#[cfg(feature = "archive-libarchive")]
impl ArchiveScanner for LibarchiveScanner {
    fn next_entry(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        use compress_tools::{ArchiveContents, ArchiveIteratorBuilder};
        use std::sync::Mutex;

        if self.finished {
            return Ok(None);
        }
        cancellation.check().map_err(|_| ArchiveError::Cancelled)?;
        let found = Arc::new(Mutex::new(None::<(String, compress_tools::stat)>));
        let filter_found = Arc::clone(&found);
        let target = self.next_index;
        let allocation = self.counters.reserve(
            self.limits
                .max_path_bytes
                .saturating_mul(2)
                .saturating_add(std::mem::size_of::<RawArchiveEntry>())
                .saturating_add(std::mem::size_of::<compress_tools::stat>()),
            self.limits.max_metadata_bytes,
        )?;
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let filter_seen = Arc::clone(&seen);
        let source = TimedReader::new(
            self.source.try_clone().map_err(|_| ArchiveError::Io)?,
            self.limits.max_elapsed,
            Arc::clone(&self.counters),
        );
        let filter = move |name: &str, status: &compress_tools::stat| {
            let index = filter_seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if index == target {
                *filter_found
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) =
                    Some((name.to_owned(), *status));
                true
            } else {
                false
            }
        };
        let mut iterator = ArchiveIteratorBuilder::new(source)
            .filter(filter)
            .mtree_format(false)
            .build()
            .map_err(|_| ArchiveError::InvalidArchive)?;
        let event = iterator.next();
        drop(iterator);
        match event {
            Some(ArchiveContents::StartOfEntry(_, _)) => {}
            Some(ArchiveContents::Err(_)) => return Err(ArchiveError::InvalidArchive),
            None => {
                self.finished = true;
                return Ok(None);
            }
            _ => return Err(ArchiveError::InvalidArchive),
        }
        let Some((name, status)) = found
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        else {
            return Err(ArchiveError::InvalidArchive);
        };
        self.next_index = self.next_index.saturating_add(1);
        let path = ArchivePath::normalize_bytes(name.as_bytes(), self.limits.max_path_bytes)?;
        let file_type = status.st_mode & 0o170_000;
        let kind = match file_type {
            0o040_000 => RawEntryKind::Directory,
            0o100_000 if status.st_nlink > 1 => RawEntryKind::HardLink,
            0o100_000 => RawEntryKind::RegularFile,
            0o120_000 => RawEntryKind::SymbolicLink,
            _ => RawEntryKind::Other,
        };
        Ok(Some(RawArchiveEntry {
            provider: self.provider.clone(),
            path,
            kind,
            size: (kind == RawEntryKind::RegularFile)
                .then(|| u64::try_from(status.st_size).unwrap_or(0)),
            ordinal: self.next_index.saturating_sub(1) as u64,
            _allocation: allocation,
        }))
    }
}

fn read_seven_archive<R: Read + Seek>(
    reader: &mut R,
    passwords: &dyn ArchivePasswordProvider,
) -> Result<sevenz_rust2::Archive, ArchiveError> {
    match sevenz_rust2::Archive::read(reader, &sevenz_rust2::Password::empty()) {
        Ok(archive) => Ok(archive),
        Err(sevenz_rust2::Error::PasswordRequired) => {
            reader
                .seek(SeekFrom::Start(0))
                .map_err(|_| ArchiveError::InvalidArchive)?;
            let password = require_password(passwords, ArchiveFormat::SevenZip)?;
            let text = std::str::from_utf8(password.as_bytes())
                .map_err(|_| ArchiveError::InvalidPassword)?;
            let dependency_password = sevenz_rust2::Password::new(text);
            let result =
                sevenz_rust2::Archive::read(reader, &dependency_password).map_err(|error| {
                    match error {
                        sevenz_rust2::Error::MaybeBadPassword(_) => ArchiveError::InvalidPassword,
                        _ => ArchiveError::InvalidArchive,
                    }
                });
            drop(dependency_password);
            drop(password);
            result
        }
        Err(_) => Err(ArchiveError::InvalidArchive),
    }
}

fn copy_zip<R: Read + Seek, W: Write>(
    reader: R,
    ordinal: u64,
    passwords: &dyn ArchivePasswordProvider,
    destination: &mut W,
) -> Result<(), ArchiveError> {
    let index = usize::try_from(ordinal).map_err(|_| ArchiveError::NotArchiveEntry)?;
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

fn copy_seven_zip<R: Read + Seek, W: Write>(
    mut reader: R,
    ordinal: u64,
    passwords: &dyn ArchivePasswordProvider,
    destination: &mut W,
) -> Result<(), ArchiveError> {
    let password = match sevenz_rust2::Archive::read(&mut reader, &sevenz_rust2::Password::empty())
    {
        Ok(_) => sevenz_rust2::Password::empty(),
        Err(sevenz_rust2::Error::PasswordRequired) => {
            reader
                .seek(SeekFrom::Start(0))
                .map_err(|_| ArchiveError::InvalidArchive)?;
            let password = require_password(passwords, ArchiveFormat::SevenZip)?;
            let text = std::str::from_utf8(password.as_bytes())
                .map_err(|_| ArchiveError::InvalidPassword)?;
            sevenz_rust2::Password::new(text)
        }
        Err(_) => return Err(ArchiveError::InvalidArchive),
    };
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let mut archive = sevenz_rust2::ArchiveReader::new(reader, password)
        .map_err(|_| ArchiveError::InvalidPassword)?;
    let mut current = 0_u64;
    let mut found = false;
    archive
        .for_each_entries(|_entry, contents| {
            if current == ordinal {
                std::io::copy(contents, destination)?;
                found = true;
                Ok(false)
            } else {
                current = current.saturating_add(1);
                Ok(true)
            }
        })
        .map_err(|_| ArchiveError::InvalidArchive)?;
    found.then_some(()).ok_or(ArchiveError::NotArchiveEntry)
}

fn require_password(
    passwords: &dyn ArchivePasswordProvider,
    format: ArchiveFormat,
) -> Result<ArchivePassword, ArchiveError> {
    passwords
        .request_password(&PasswordRequest { format })?
        .ok_or(ArchiveError::PasswordRequired)
}

struct TimedReader<R> {
    inner: R,
    maximum_elapsed: std::time::Duration,
    counters: Arc<DecodeCounterState>,
}

impl<R> TimedReader<R> {
    fn new(
        inner: R,
        maximum_elapsed: std::time::Duration,
        counters: Arc<DecodeCounterState>,
    ) -> Self {
        Self {
            inner,
            maximum_elapsed,
            counters,
        }
    }

    fn finish_call<T>(&self, started: Instant, result: std::io::Result<T>) -> std::io::Result<T> {
        self.counters.add_elapsed(started.elapsed());
        if self.counters.elapsed() > self.maximum_elapsed {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "archive metadata time limit exceeded",
            ))
        } else {
            result
        }
    }
}

impl<R: Read> Read for TimedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let started = Instant::now();
        let result = self.inner.read(buffer);
        if let Ok(count) = result {
            self.counters.add_read_bytes(count as u64);
        }
        self.finish_call(started, result)
    }
}

impl<R: Seek> Seek for TimedReader<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let started = Instant::now();
        let result = self.inner.seek(position);
        self.finish_call(started, result)
    }
}

struct BoundedWriter<W> {
    inner: W,
    written: u64,
    maximum: u64,
    cancellation: CancellationToken,
}

impl<W> BoundedWriter<W> {
    fn new(inner: W, maximum: u64, cancellation: CancellationToken) -> Self {
        Self {
            inner,
            written: 0,
            maximum,
            cancellation,
        }
    }
}

impl<W: Write> Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.cancellation.check().map_err(std::io::Error::other)?;
        let next = self.written.saturating_add(bytes.len() as u64);
        if next > self.maximum {
            return Err(std::io::Error::other("nested archive byte limit exceeded"));
        }
        let count = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(count as u64);
        Ok(count)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
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
