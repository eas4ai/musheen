use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::time::Instant;

use musheen_core::{CancellationToken, ProviderId};

use super::ArchivePath;
use super::format::{
    ArchiveCopyContext, ArchiveFormat, ArchiveScanner, RawArchiveEntry, RawEntryKind,
};
use super::io::{PositionedFile, TimedReader};
use super::store::{
    ArchiveError, ArchiveLimits, ArchivePassword, ArchivePasswordProvider, DecodeCounterState,
    PasswordRequest, elapsed_limit,
};

pub(crate) fn open_scanner(
    source: PositionedFile,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    passwords: &dyn ArchivePasswordProvider,
) -> Result<Box<dyn ArchiveScanner>, ArchiveError> {
    Ok(Box::new(SevenZipScanner::new(
        source, provider, limits, counters, passwords,
    )?))
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
        cancellation
            .wait_if_paused()
            .map_err(|_| ArchiveError::Cancelled)?;
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

pub(crate) fn copy_seven_zip<R: Read + Seek, W: Write>(
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

/// Decodes every block of a 7z archive once, in archive order, and hands each
/// file `wanted` names to `visit` with a reader over its bytes. Whatever a
/// visit leaves unread is read to the end here, because the files of a solid
/// block decode one after another.
pub(crate) fn copy_files_in_order<R: Read + Seek>(
    mut reader: R,
    passwords: &dyn ArchivePasswordProvider,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
    wanted: &dyn Fn(u64) -> bool,
    visit: &mut dyn FnMut(u64, &mut dyn Read) -> Result<(), ArchiveError>,
) -> Result<(), ArchiveError> {
    let (archive, _metadata, password, budget) =
        read_seven_archive(&mut reader, passwords, limits, counters)?;
    reader
        .seek(SeekFrom::Start(0))
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let mut archive = sevenz_rust2::ArchiveReader::from_archive_sequential_with_memory_budget(
        archive, reader, password, budget,
    );
    let mut visited = std::collections::HashSet::new();
    let mut failure = None;
    let result = archive.for_each_entries_indexed(|index, _entry, contents| {
        let ordinal = index as u64;
        let mut outcome = if wanted(ordinal) && visited.insert(ordinal) {
            visit(ordinal, contents)
        } else {
            Ok(())
        };
        if outcome.is_ok() {
            // Read what the visit left, up to the expanded-bytes limit.
            let mut rest = contents.take(limits.max_expanded_bytes.saturating_add(1));
            outcome = match std::io::copy(&mut rest, &mut std::io::sink()) {
                Ok(count) if count > limits.max_expanded_bytes => {
                    Err(ArchiveError::LimitExceeded {
                        resource: "expanded bytes",
                        value: usize::try_from(count).unwrap_or(usize::MAX),
                        maximum: usize::try_from(limits.max_expanded_bytes).unwrap_or(usize::MAX),
                    })
                }
                Ok(_) => Ok(()),
                Err(_) => Err(ArchiveError::InvalidArchive),
            };
        }
        outcome.map_err(|error| {
            failure = Some(error);
            sevenz_rust2::Error::from(std::io::Error::other("archive decode stopped"))
        })
    });
    if let Some(error) = failure {
        return Err(error);
    }
    result.map_err(|error| map_seven_error(error, limits.max_metadata_bytes))
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
