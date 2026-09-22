use super::store::{
    ArchiveError, ArchiveLimits, ArchivePasswordProvider, DecodeCounterState, PasswordRequest,
};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

#[cfg(feature = "archive-libarchive")]
use std::sync::Mutex;

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
    pub(crate) path: Vec<u8>,
    pub(crate) kind: RawEntryKind,
    pub(crate) size: Option<u64>,
}

pub(crate) fn read_entries(
    file: &File,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
    passwords: &dyn ArchivePasswordProvider,
) -> Result<Vec<RawArchiveEntry>, ArchiveError> {
    let source = file.try_clone().map_err(|_| ArchiveError::Io)?;
    let reader = CountedReader::new(source, limits.max_elapsed, Arc::clone(counters));
    match format {
        ArchiveFormat::Zip => read_zip(reader, limits, counters),
        ArchiveFormat::Tar => read_tar_seekable(reader, limits, counters),
        ArchiveFormat::TarGzip => read_tar(flate2::read::GzDecoder::new(reader), limits, counters),
        ArchiveFormat::TarZstd => {
            let decoder = zstd::stream::read::Decoder::new(reader)
                .map_err(|_| ArchiveError::InvalidArchive)?;
            read_tar(decoder, limits, counters)
        }
        ArchiveFormat::SevenZip => read_seven_zip(reader, limits, counters, passwords),
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Rar | ArchiveFormat::Iso => {
            read_libarchive(reader, limits, Arc::clone(counters))
        }
    }
}

fn read_zip<R: Read + Seek>(
    mut reader: R,
    limits: &ArchiveLimits,
    counters: &DecodeCounterState,
) -> Result<Vec<RawArchiveEntry>, ArchiveError> {
    preflight_zip(&mut reader, limits)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| ArchiveError::Io)?;
    let mut archive = zip::ZipArchive::new(reader).map_err(|_| ArchiveError::InvalidArchive)?;
    if archive.len() > limits.max_entries {
        return Err(ArchiveError::LimitExceeded {
            resource: "archive entries",
            value: archive.len(),
            maximum: limits.max_entries,
        });
    }

    let mut entries = Vec::with_capacity(archive.len().min(limits.max_entries));
    for index in 0..archive.len() {
        let file = archive
            .by_index_raw(index)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        let kind = if file.is_dir() {
            RawEntryKind::Directory
        } else if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            RawEntryKind::SymbolicLink
        } else {
            RawEntryKind::RegularFile
        };
        entries.push(RawArchiveEntry {
            path: normalize_raw_path(file.name_raw(), limits, counters)?,
            kind,
            size: (!file.is_dir()).then_some(file.size()),
        });
    }
    Ok(entries)
}

fn preflight_zip<R: Read + Seek>(
    reader: &mut R,
    limits: &ArchiveLimits,
) -> Result<(), ArchiveError> {
    let length = reader
        .seek(SeekFrom::End(0))
        .map_err(|_| ArchiveError::Io)?;
    let tail_length = length.min(65_557) as usize;
    reader
        .seek(SeekFrom::End(-(tail_length as i64)))
        .map_err(|_| ArchiveError::Io)?;
    let mut tail = vec![0; tail_length];
    reader
        .read_exact(&mut tail)
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let eocd = (0..tail.len().saturating_sub(3))
        .rev()
        .find(|offset| {
            if tail.get(*offset..*offset + 4) != Some(b"PK\x05\x06") || tail.len() < *offset + 22 {
                return false;
            }
            let comment_length =
                u16::from_le_bytes([tail[*offset + 20], tail[*offset + 21]]) as usize;
            *offset + 22 + comment_length == tail.len()
        })
        .ok_or(ArchiveError::InvalidArchive)?;
    if tail.len() < eocd + 22 {
        return Err(ArchiveError::InvalidArchive);
    }
    let disk = u16::from_le_bytes([tail[eocd + 4], tail[eocd + 5]]);
    let central_disk = u16::from_le_bytes([tail[eocd + 6], tail[eocd + 7]]);
    let disk_entries = u16::from_le_bytes([tail[eocd + 8], tail[eocd + 9]]);
    let total_entries = u16::from_le_bytes([tail[eocd + 10], tail[eocd + 11]]);
    if disk != 0 || central_disk != 0 || disk_entries != total_entries {
        return Err(ArchiveError::InvalidArchive);
    }
    let entry_count = total_entries as u64;
    let metadata_bytes = u32::from_le_bytes([
        tail[eocd + 12],
        tail[eocd + 13],
        tail[eocd + 14],
        tail[eocd + 15],
    ]) as u64;
    if entry_count == u16::MAX as u64 || metadata_bytes == u32::MAX as u64 {
        return preflight_zip64(reader, &tail, eocd, limits);
    }
    validate_zip_counts(entry_count, metadata_bytes, limits)
}

fn preflight_zip64<R: Read + Seek>(
    reader: &mut R,
    tail: &[u8],
    eocd: usize,
    limits: &ArchiveLimits,
) -> Result<(), ArchiveError> {
    if eocd < 20 || &tail[eocd - 20..eocd - 16] != b"PK\x06\x07" {
        return Err(ArchiveError::InvalidArchive);
    }
    let offset = u64::from_le_bytes(
        tail[eocd - 12..eocd - 4]
            .try_into()
            .map_err(|_| ArchiveError::InvalidArchive)?,
    );
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
    let entry_count = u64::from_le_bytes(record[32..40].try_into().expect("fixed slice"));
    let metadata_bytes = u64::from_le_bytes(record[40..48].try_into().expect("fixed slice"));
    validate_zip_counts(entry_count, metadata_bytes, limits)
}

fn validate_zip_counts(
    entry_count: u64,
    metadata_bytes: u64,
    limits: &ArchiveLimits,
) -> Result<(), ArchiveError> {
    let maximum_entries = limits.max_entries as u64;
    if entry_count > maximum_entries {
        return Err(ArchiveError::LimitExceeded {
            resource: "archive entries",
            value: usize::try_from(entry_count).unwrap_or(usize::MAX),
            maximum: limits.max_entries,
        });
    }
    let maximum_metadata = limits.max_metadata_bytes as u64;
    if metadata_bytes > maximum_metadata {
        return Err(ArchiveError::LimitExceeded {
            resource: "metadata bytes",
            value: usize::try_from(metadata_bytes).unwrap_or(usize::MAX),
            maximum: limits.max_metadata_bytes,
        });
    }
    Ok(())
}

fn read_tar<R: Read>(
    reader: R,
    limits: &ArchiveLimits,
    counters: &DecodeCounterState,
) -> Result<Vec<RawArchiveEntry>, ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let iterator = archive
        .entries()
        .map_err(|_| ArchiveError::InvalidArchive)?;
    collect_tar_entries(iterator, limits, counters)
}

fn read_tar_seekable<R: Read + Seek>(
    reader: R,
    limits: &ArchiveLimits,
    counters: &DecodeCounterState,
) -> Result<Vec<RawArchiveEntry>, ArchiveError> {
    let mut archive = tar::Archive::new(reader);
    let iterator = archive
        .entries_with_seek()
        .map_err(|_| ArchiveError::InvalidArchive)?;
    collect_tar_entries(iterator, limits, counters)
}

fn collect_tar_entries<R: Read>(
    iterator: tar::Entries<'_, R>,
    limits: &ArchiveLimits,
    counters: &DecodeCounterState,
) -> Result<Vec<RawArchiveEntry>, ArchiveError> {
    let mut entries = Vec::new();
    for entry in iterator {
        if entries.len() >= limits.max_entries {
            return Err(ArchiveError::LimitExceeded {
                resource: "archive entries",
                value: entries.len() + 1,
                maximum: limits.max_entries,
            });
        }
        let entry = entry.map_err(|_| ArchiveError::InvalidArchive)?;
        let kind = match entry.header().entry_type() {
            value if value.is_dir() => RawEntryKind::Directory,
            value if value.is_file() => RawEntryKind::RegularFile,
            value if value.is_symlink() => RawEntryKind::SymbolicLink,
            value if value.is_hard_link() => RawEntryKind::HardLink,
            _ => RawEntryKind::Other,
        };
        entries.push(RawArchiveEntry {
            path: normalize_raw_path(&entry.path_bytes(), limits, counters)?,
            kind,
            size: (kind == RawEntryKind::RegularFile).then_some(entry.size()),
        });
    }
    Ok(entries)
}

fn read_seven_zip<R: Read + Seek>(
    mut reader: R,
    limits: &ArchiveLimits,
    counters: &DecodeCounterState,
    passwords: &dyn ArchivePasswordProvider,
) -> Result<Vec<RawArchiveEntry>, ArchiveError> {
    let empty = sevenz_rust2::Password::empty();
    let parsed = sevenz_rust2::ArchiveReader::new(&mut reader, empty);
    let archive = match parsed {
        Ok(reader) => reader,
        Err(sevenz_rust2::Error::PasswordRequired) => {
            reader
                .seek(SeekFrom::Start(0))
                .map_err(|_| ArchiveError::InvalidArchive)?;
            let password = require_password(passwords, ArchiveFormat::SevenZip)?;
            let text = std::str::from_utf8(password.as_bytes())
                .map_err(|_| ArchiveError::InvalidPassword)?;
            sevenz_rust2::ArchiveReader::new(&mut reader, sevenz_rust2::Password::new(text))
                .map_err(|error| match error {
                    sevenz_rust2::Error::MaybeBadPassword(_) => ArchiveError::InvalidPassword,
                    _ => ArchiveError::InvalidArchive,
                })?
        }
        Err(_) => return Err(ArchiveError::InvalidArchive),
    };
    let files = &archive.archive().files;
    if files.len() > limits.max_entries {
        return Err(ArchiveError::LimitExceeded {
            resource: "archive entries",
            value: files.len(),
            maximum: limits.max_entries,
        });
    }
    files
        .iter()
        .filter(|entry| !entry.is_anti_item)
        .map(|entry| {
            Ok(RawArchiveEntry {
                path: normalize_raw_path(entry.name.as_bytes(), limits, counters)?,
                kind: if entry.is_directory {
                    RawEntryKind::Directory
                } else {
                    RawEntryKind::RegularFile
                },
                size: (!entry.is_directory).then_some(entry.size),
            })
        })
        .collect()
}

#[cfg(feature = "archive-libarchive")]
fn read_libarchive<R: Read + Seek>(
    reader: R,
    limits: &ArchiveLimits,
    counters: Arc<DecodeCounterState>,
) -> Result<Vec<RawArchiveEntry>, ArchiveError> {
    use compress_tools::{ArchiveContents, ArchiveIteratorBuilder};

    let collected = Arc::new(Mutex::new(Ok(Vec::<RawArchiveEntry>::new())));
    let filter_collected = Arc::clone(&collected);
    let max_entries = limits.max_entries;
    let max_path_bytes = limits.max_path_bytes;
    let max_metadata_bytes = limits.max_metadata_bytes;
    let filter_counters = Arc::clone(&counters);
    let filter = move |name: &str, status: &compress_tools::stat| {
        let mut collected = filter_collected
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(entries) = collected.as_mut() else {
            return true;
        };
        if entries.len() >= max_entries {
            *collected = Err(ArchiveError::LimitExceeded {
                resource: "archive entries",
                value: entries.len() + 1,
                maximum: max_entries,
            });
            return true;
        }
        let entry_limits = ArchiveLimits {
            max_entries,
            max_path_bytes,
            max_metadata_bytes,
            max_elapsed: std::time::Duration::MAX,
            max_nested_archives: usize::MAX,
        };
        let path = match normalize_raw_path(name.as_bytes(), &entry_limits, &filter_counters) {
            Ok(path) => path,
            Err(error) => {
                *collected = Err(error);
                return true;
            }
        };
        let file_type = status.st_mode & 0o170_000;
        let kind = match file_type {
            0o040_000 => RawEntryKind::Directory,
            0o100_000 if status.st_nlink > 1 => RawEntryKind::HardLink,
            0o100_000 => RawEntryKind::RegularFile,
            0o120_000 => RawEntryKind::SymbolicLink,
            _ => RawEntryKind::Other,
        };
        entries.push(RawArchiveEntry {
            path,
            kind,
            size: (kind == RawEntryKind::RegularFile)
                .then(|| u64::try_from(status.st_size).unwrap_or(0)),
        });
        false
    };
    let mut iterator = ArchiveIteratorBuilder::new(reader)
        .filter(filter)
        .mtree_format(false)
        .build()
        .map_err(|_| ArchiveError::InvalidArchive)?;
    for event in &mut iterator {
        match event {
            ArchiveContents::StartOfEntry(_, _) => break,
            ArchiveContents::DataChunk(_) => return Err(ArchiveError::InvalidArchive),
            ArchiveContents::EndOfEntry => {}
            ArchiveContents::Err(_) => return Err(ArchiveError::InvalidArchive),
        }
    }
    drop(iterator);
    let mut collected = collected
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    std::mem::replace(&mut *collected, Ok(Vec::new()))
}

fn normalize_raw_path(
    path: &[u8],
    limits: &ArchiveLimits,
    counters: &DecodeCounterState,
) -> Result<Vec<u8>, ArchiveError> {
    let path = super::ArchivePath::with_limit(path, limits.max_path_bytes)?;
    let bytes = path
        .as_bytes()
        .len()
        .saturating_add(std::mem::size_of::<RawArchiveEntry>());
    counters.reserve_metadata(bytes, limits.max_metadata_bytes)?;
    Ok(path.as_bytes().to_vec())
}

fn require_password(
    passwords: &dyn ArchivePasswordProvider,
    format: ArchiveFormat,
) -> Result<super::ArchivePassword, ArchiveError> {
    passwords
        .request_password(&PasswordRequest { format })?
        .ok_or(ArchiveError::PasswordRequired)
}

struct CountedReader<R> {
    inner: R,
    started: std::time::Instant,
    maximum_elapsed: std::time::Duration,
    counters: Arc<DecodeCounterState>,
}

impl<R> CountedReader<R> {
    fn new(
        inner: R,
        maximum_elapsed: std::time::Duration,
        counters: Arc<DecodeCounterState>,
    ) -> Self {
        Self {
            inner,
            started: std::time::Instant::now(),
            maximum_elapsed,
            counters,
        }
    }

    fn check_time(&self) -> std::io::Result<()> {
        if self.started.elapsed() > self.maximum_elapsed {
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "archive metadata time limit exceeded",
            ))
        } else {
            Ok(())
        }
    }
}

impl<R: Read> Read for CountedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.check_time()?;
        let count = self.inner.read(buffer)?;
        self.counters.add_read_bytes(count as u64);
        Ok(count)
    }
}

impl<R: Seek> Seek for CountedReader<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.check_time()?;
        self.inner.seek(position)
    }
}
