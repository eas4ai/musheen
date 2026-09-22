use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use musheen_core::{CancellationToken, ProviderId};

use super::ArchivePath;
use super::format::{ArchiveScanner, RawArchiveEntry, RawEntryKind};
use super::io::{PositionedFile, TimedReader, checked_seek_offset};
use super::store::{
    AllocationLease, ArchiveError, ArchiveLimits, ArchivePasswordProvider, DecodeCounterState,
};
use super::workspace::{
    reserve_decode_workspace, zip_deflate_decoder_workspace_bytes,
    zip_stored_reader_workspace_bytes, zip_zstd_decoder_workspace_bytes,
};

const STORED_METHOD: u16 = 0;
const DEFLATE_METHOD: u16 = 8;
const ZSTD_METHOD: u16 = 93;
const AES_METHOD: u16 = 99;
const ZSTD_FRAME_HEADER_MAX_BYTES: usize = 18;

pub(crate) fn open_scanner(
    source: PositionedFile,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
) -> Result<Box<dyn ArchiveScanner>, ArchiveError> {
    Ok(Box::new(ZipScanner::new(
        source, provider, limits, counters,
    )?))
}

pub(crate) fn copy_entry<R: Read + Seek, W: Write>(
    mut reader: R,
    ordinal: u64,
    passwords: &dyn ArchivePasswordProvider,
    destination: &mut W,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
) -> Result<(), ArchiveError> {
    let target = find_entry(&mut reader, ordinal, limits, counters)?;
    reader
        .seek(SeekFrom::Start(target.local_header_offset))
        .map_err(|_| ArchiveError::InvalidArchive)?;
    let mut local = [0_u8; 30];
    reader
        .read_exact(&mut local)
        .map_err(|_| ArchiveError::InvalidArchive)?;
    if &local[..4] != b"PK\x03\x04" {
        return Err(ArchiveError::InvalidArchive);
    }
    let name_length = le_u16(&local[26..28]) as usize;
    let extra_length = le_u16(&local[28..30]) as usize;
    // `ZipArchive` parses the synthetic single-entry central directory into owned name, extra,
    // index, and finder buffers. Reserve their hard upper bound before the parser allocates them.
    let _archive_workspace = counters.reserve(
        target
            .central_record
            .len()
            .saturating_sub(46)
            .saturating_mul(6)
            .saturating_add(8 * 1_024),
        limits.max_metadata_bytes,
    )?;
    let data_offset = target
        .local_header_offset
        .checked_add(30)
        .and_then(|offset| offset.checked_add(name_length as u64))
        .and_then(|offset| offset.checked_add(extra_length as u64))
        .ok_or(ArchiveError::InvalidArchive)?;
    let decoder_workspace = decoder_workspace_bytes(&mut reader, &target, data_offset)?;
    let _decoder_workspace = reserve_decode_workspace(counters, limits, decoder_workspace)?;
    let local_length = 30_u64
        .checked_add(name_length as u64)
        .and_then(|length| length.checked_add(extra_length as u64))
        .and_then(|length| length.checked_add(target.compressed_size))
        .ok_or(ArchiveError::InvalidArchive)?;
    let virtual_reader = SingleEntryZip::new(
        reader,
        target.local_header_offset,
        local_length,
        target.central_record,
    )?;
    let mut archive =
        zip::ZipArchive::new(virtual_reader).map_err(|_| ArchiveError::InvalidArchive)?;
    if target.encrypted {
        let password = passwords
            .request_password(&super::store::PasswordRequest {
                format: super::ArchiveFormat::Zip,
            })?
            .ok_or(ArchiveError::PasswordRequired)?;
        let mut entry = archive
            .by_index_decrypt(0, password.as_bytes())
            .map_err(|_| ArchiveError::InvalidPassword)?;
        std::io::copy(&mut entry, destination).map_err(|_| ArchiveError::Io)?;
    } else {
        let mut entry = archive
            .by_index(0)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        std::io::copy(&mut entry, destination).map_err(|_| ArchiveError::Io)?;
    }
    Ok(())
}

struct BudgetedZipEntry {
    local_header_offset: u64,
    compressed_size: u64,
    encrypted: bool,
    compression_method: u16,
    central_record: Vec<u8>,
    _allocation: AllocationLease,
    _raw_name: Vec<u8>,
    _decoded_name: String,
}

fn find_entry<R: Read + Seek>(
    reader: &mut R,
    ordinal: u64,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
) -> Result<BudgetedZipEntry, ArchiveError> {
    let (central_offset, central_size, entries, archive_prefix) =
        preflight(reader, limits, counters)?;
    if ordinal >= entries {
        return Err(ArchiveError::NotArchiveEntry);
    }
    let central_end = central_offset.saturating_add(central_size);
    reader
        .seek(SeekFrom::Start(central_offset))
        .map_err(|_| ArchiveError::InvalidArchive)?;
    for index in 0..entries {
        let position = reader
            .stream_position()
            .map_err(|_| ArchiveError::InvalidArchive)?;
        if position.saturating_add(46) > central_end {
            return Err(ArchiveError::InvalidArchive);
        }
        let mut header = [0_u8; 46];
        reader
            .read_exact(&mut header)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        if &header[..4] != b"PK\x01\x02" {
            return Err(ArchiveError::InvalidArchive);
        }
        let name_length = le_u16(&header[28..30]) as usize;
        let extra_length = le_u16(&header[30..32]) as usize;
        let comment_length = le_u16(&header[32..34]) as usize;
        let trailing = name_length
            .saturating_add(extra_length)
            .saturating_add(comment_length);
        if position.saturating_add(46).saturating_add(trailing as u64) > central_end {
            return Err(ArchiveError::InvalidArchive);
        }
        if index != ordinal {
            reader
                .seek(SeekFrom::Current(
                    i64::try_from(trailing).map_err(|_| ArchiveError::InvalidArchive)?,
                ))
                .map_err(|_| ArchiveError::InvalidArchive)?;
            continue;
        }
        // CP437 can expand to three UTF-8 bytes per input byte. Five times the raw length covers
        // both the raw buffer and a four-byte-per-scalar decoded String before either allocation.
        let allocation = counters.reserve(
            name_length
                .saturating_mul(5)
                .saturating_add(extra_length.saturating_mul(2))
                .saturating_add(4 * 1_024)
                .saturating_add(std::mem::size_of::<BudgetedZipEntry>()),
            limits.max_metadata_bytes,
        )?;
        let mut raw_name = vec![0_u8; name_length];
        reader
            .read_exact(&mut raw_name)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        let mut extra = vec![0_u8; extra_length];
        reader
            .read_exact(&mut extra)
            .map_err(|_| ArchiveError::InvalidArchive)?;
        reader
            .seek(SeekFrom::Current(
                i64::try_from(comment_length).map_err(|_| ArchiveError::InvalidArchive)?,
            ))
            .map_err(|_| ArchiveError::InvalidArchive)?;
        let flags = le_u16(&header[8..10]);
        let decoded_name = if flags & (1 << 11) != 0 {
            std::str::from_utf8(&raw_name)
                .map_err(|_| ArchiveError::InvalidArchive)?
                .to_owned()
        } else {
            decode_cp437(&raw_name)
        };
        let (compressed_size, _expanded_size) = entry_sizes(&header, &extra)?;
        let compression_method = compression_method(&header, &extra)?;
        let local_header_offset = local_header_offset(&header, &extra)?
            .checked_add(archive_prefix)
            .ok_or(ArchiveError::InvalidArchive)?;
        let mut central_header = header;
        if local_header_offset > u32::MAX as u64 {
            return Err(ArchiveError::UnsupportedNestedFormat);
        }
        central_header[32..34].copy_from_slice(&0_u16.to_le_bytes());
        central_header[34..36].copy_from_slice(&0_u16.to_le_bytes());
        central_header[42..46].copy_from_slice(&0_u32.to_le_bytes());
        let mut central_record = Vec::with_capacity(46 + raw_name.len() + extra.len());
        central_record.extend_from_slice(&central_header);
        central_record.extend_from_slice(&raw_name);
        central_record.extend_from_slice(&extra);
        return Ok(BudgetedZipEntry {
            local_header_offset,
            compressed_size,
            encrypted: flags & 1 != 0,
            compression_method,
            central_record,
            _allocation: allocation,
            _raw_name: raw_name,
            _decoded_name: decoded_name,
        });
    }
    Err(ArchiveError::NotArchiveEntry)
}

fn decoder_workspace_bytes<R: Read + Seek>(
    reader: &mut R,
    target: &BudgetedZipEntry,
    data_offset: u64,
) -> Result<usize, ArchiveError> {
    match target.compression_method {
        STORED_METHOD => Ok(zip_stored_reader_workspace_bytes()),
        DEFLATE_METHOD => Ok(zip_deflate_decoder_workspace_bytes()),
        ZSTD_METHOD if target.encrypted => Err(ArchiveError::UnsupportedNestedFormat),
        ZSTD_METHOD => {
            let prefix_length = usize::try_from(
                target
                    .compressed_size
                    .min(ZSTD_FRAME_HEADER_MAX_BYTES as u64),
            )
            .map_err(|_| ArchiveError::InvalidArchive)?;
            if prefix_length == 0 {
                return Err(ArchiveError::InvalidArchive);
            }
            let mut header = [0_u8; ZSTD_FRAME_HEADER_MAX_BYTES];
            reader
                .seek(SeekFrom::Start(data_offset))
                .map_err(|_| ArchiveError::InvalidArchive)?;
            reader
                .read_exact(&mut header[..prefix_length])
                .map_err(|_| ArchiveError::InvalidArchive)?;
            zip_zstd_decoder_workspace_bytes(&header[..prefix_length])
        }
        _ => Ok(zip_stored_reader_workspace_bytes()),
    }
}

fn compression_method(header: &[u8; 46], extra: &[u8]) -> Result<u16, ArchiveError> {
    let method = le_u16(&header[10..12]);
    if method != AES_METHOD {
        return Ok(method);
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
        if field_id == 0x9901 {
            return extra
                .get(offset + 5..offset + 7)
                .map(le_u16)
                .ok_or(ArchiveError::InvalidArchive);
        }
        offset = end;
    }
    Err(ArchiveError::InvalidArchive)
}

struct SingleEntryZip<R> {
    source: R,
    source_offset: u64,
    source_length: u64,
    central: Vec<u8>,
    eocd: [u8; 22],
    position: u64,
}

impl<R> SingleEntryZip<R> {
    fn new(
        source: R,
        source_offset: u64,
        source_length: u64,
        central: Vec<u8>,
    ) -> Result<Self, ArchiveError> {
        let central_offset =
            u32::try_from(source_length).map_err(|_| ArchiveError::UnsupportedNestedFormat)?;
        let central_size =
            u32::try_from(central.len()).map_err(|_| ArchiveError::UnsupportedNestedFormat)?;
        let mut eocd = [0_u8; 22];
        eocd[..4].copy_from_slice(b"PK\x05\x06");
        eocd[8..10].copy_from_slice(&1_u16.to_le_bytes());
        eocd[10..12].copy_from_slice(&1_u16.to_le_bytes());
        eocd[12..16].copy_from_slice(&central_size.to_le_bytes());
        eocd[16..20].copy_from_slice(&central_offset.to_le_bytes());
        Ok(Self {
            source,
            source_offset,
            source_length,
            central,
            eocd,
            position: 0,
        })
    }

    fn length(&self) -> u64 {
        self.source_length
            .saturating_add(self.central.len() as u64)
            .saturating_add(self.eocd.len() as u64)
    }
}

impl<R: Read + Seek> Read for SingleEntryZip<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() || self.position >= self.length() {
            return Ok(0);
        }
        let count = if self.position < self.source_length {
            let available = self.source_length - self.position;
            let requested =
                usize::try_from(available.min(buffer.len() as u64)).unwrap_or(buffer.len());
            self.source
                .seek(SeekFrom::Start(self.source_offset + self.position))?;
            self.source.read(&mut buffer[..requested])?
        } else if self.position < self.source_length + self.central.len() as u64 {
            let offset = (self.position - self.source_length) as usize;
            let count = (self.central.len() - offset).min(buffer.len());
            buffer[..count].copy_from_slice(&self.central[offset..offset + count]);
            count
        } else {
            let offset = (self.position - self.source_length - self.central.len() as u64) as usize;
            let count = (self.eocd.len() - offset).min(buffer.len());
            buffer[..count].copy_from_slice(&self.eocd[offset..offset + count]);
            count
        };
        self.position = self.position.saturating_add(count as u64);
        Ok(count)
    }
}

impl<R: Read + Seek> Seek for SingleEntryZip<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(position) => position,
            SeekFrom::End(offset) => checked_seek_offset(self.length(), offset)?,
            SeekFrom::Current(offset) => checked_seek_offset(self.position, offset)?,
        };
        Ok(self.position)
    }
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
        let (central_offset, central_size, entries, _) =
            preflight(&mut reader, &limits, &counters)?;
        reader
            .seek(SeekFrom::Start(central_offset))
            .map_err(|_| invalid_or_time(&counters, &limits))?;
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
            .map_err(|_| invalid_or_time(&self.counters, &self.limits))?;
        if position.saturating_add(46) > self.central_end {
            return Err(ArchiveError::InvalidArchive);
        }
        let mut header = [0_u8; 46];
        self.reader
            .read_exact(&mut header)
            .map_err(|_| invalid_or_time(&self.counters, &self.limits))?;
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
            .map_err(|_| invalid_or_time(&self.counters, &self.limits))?;
        let mut extra = vec![0_u8; extra_length];
        self.reader
            .read_exact(&mut extra)
            .map_err(|_| invalid_or_time(&self.counters, &self.limits))?;
        self.reader
            .seek(SeekFrom::Current(
                i64::try_from(comment_length).map_err(|_| ArchiveError::InvalidArchive)?,
            ))
            .map_err(|_| invalid_or_time(&self.counters, &self.limits))?;
        let directory_name = name.last() == Some(&b'/');
        let name = ArchivePath::normalize_bytes(&name, self.limits.max_path_bytes)?;
        let unix_mode = le_u32(&header[38..42]) >> 16;
        let file_type = unix_mode & 0o170_000;
        let kind = if directory_name || file_type == 0o040_000 {
            RawEntryKind::Directory
        } else if file_type == 0o120_000 {
            RawEntryKind::SymbolicLink
        } else {
            RawEntryKind::RegularFile
        };
        let (compressed_size, expanded_size) = entry_sizes(&header, &extra)?;
        let ordinal = self.ordinal;
        self.ordinal = self.ordinal.saturating_add(1);
        self.remaining -= 1;
        Ok(Some(RawArchiveEntry {
            provider: self.provider.clone(),
            path: name,
            kind,
            size: (kind == RawEntryKind::RegularFile).then_some(expanded_size),
            compressed_size: (kind == RawEntryKind::RegularFile).then_some(compressed_size),
            ordinal,
            _allocation: raw_allocation,
        }))
    }
}

pub(crate) fn preflight<R: Read + Seek>(
    reader: &mut R,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
) -> Result<(u64, u64, u64, u64), ArchiveError> {
    let length = reader
        .seek(SeekFrom::End(0))
        .map_err(|_| ArchiveError::Io)?;
    let tail_length = length.min(65_557) as usize;
    let _tail = counters.reserve(tail_length, limits.max_metadata_bytes)?;
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
            tail.get(*offset..*offset + 4) == Some(b"PK\x05\x06")
                && tail.len() >= *offset + 22
                && *offset + 22 + le_u16(&tail[*offset + 20..*offset + 22]) as usize == tail.len()
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
    let recorded_central_offset = central_offset;
    let eocd_offset = length
        .saturating_sub(tail_length as u64)
        .saturating_add(eocd as u64);
    let central_offset = resolve_central_offset(
        reader,
        central_offset,
        central_size,
        entries,
        eocd_offset,
        length,
    )?;
    let archive_prefix = central_offset.saturating_sub(recorded_central_offset);
    Ok((central_offset, central_size, entries, archive_prefix))
}

fn resolve_central_offset<R: Read + Seek>(
    reader: &mut R,
    recorded_offset: u64,
    central_size: u64,
    entries: u64,
    eocd_offset: u64,
    length: u64,
) -> Result<u64, ArchiveError> {
    let prefix = eocd_offset
        .checked_sub(central_size)
        .and_then(|end| end.checked_sub(recorded_offset));
    for candidate in [
        Some(recorded_offset),
        prefix.map(|value| recorded_offset + value),
    ]
    .into_iter()
    .flatten()
    {
        if candidate.saturating_add(central_size) > length {
            continue;
        }
        if entries == 0 && central_size == 0 {
            return Ok(candidate);
        }
        reader
            .seek(SeekFrom::Start(candidate))
            .map_err(|_| ArchiveError::InvalidArchive)?;
        let mut signature = [0_u8; 4];
        if reader.read_exact(&mut signature).is_ok() && signature == *b"PK\x01\x02" {
            return Ok(candidate);
        }
    }
    Err(ArchiveError::InvalidArchive)
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

fn entry_sizes(header: &[u8; 46], extra: &[u8]) -> Result<(u64, u64), ArchiveError> {
    let mut compressed = le_u32(&header[20..24]) as u64;
    let mut expanded = le_u32(&header[24..28]) as u64;
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
            let mut at = 0;
            if expanded == u32::MAX as u64 {
                expanded = field
                    .get(at..at + 8)
                    .map(le_u64)
                    .ok_or(ArchiveError::InvalidArchive)?;
                at += 8;
            }
            if compressed == u32::MAX as u64 {
                compressed = field
                    .get(at..at + 8)
                    .map(le_u64)
                    .ok_or(ArchiveError::InvalidArchive)?;
            }
            break;
        }
        offset = end;
    }
    if compressed == u32::MAX as u64 || expanded == u32::MAX as u64 {
        Err(ArchiveError::InvalidArchive)
    } else {
        Ok((compressed, expanded))
    }
}

fn local_header_offset(header: &[u8; 46], extra: &[u8]) -> Result<u64, ArchiveError> {
    let mut local = le_u32(&header[42..46]) as u64;
    if local != u32::MAX as u64 {
        return Ok(local);
    }
    let expanded_missing = le_u32(&header[24..28]) == u32::MAX;
    let compressed_missing = le_u32(&header[20..24]) == u32::MAX;
    let mut offset = 0_usize;
    while offset.saturating_add(4) <= extra.len() {
        let field_id = le_u16(&extra[offset..offset + 2]);
        let length = le_u16(&extra[offset + 2..offset + 4]) as usize;
        offset += 4;
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= extra.len())
            .ok_or(ArchiveError::InvalidArchive)?;
        if field_id == 0x0001 {
            let mut at = offset;
            if expanded_missing {
                at = at.saturating_add(8);
            }
            if compressed_missing {
                at = at.saturating_add(8);
            }
            local = extra
                .get(at..at + 8)
                .map(le_u64)
                .ok_or(ArchiveError::InvalidArchive)?;
            return Ok(local);
        }
        offset = end;
    }
    Err(ArchiveError::InvalidArchive)
}

fn decode_cp437(bytes: &[u8]) -> String {
    bytes.iter().copied().map(cp437_char).collect()
}

fn cp437_char(input: u8) -> char {
    let output = match input {
        0x00..=0x7f => input as u32,
        0x80 => 0x00c7,
        0x81 => 0x00fc,
        0x82 => 0x00e9,
        0x83 => 0x00e2,
        0x84 => 0x00e4,
        0x85 => 0x00e0,
        0x86 => 0x00e5,
        0x87 => 0x00e7,
        0x88 => 0x00ea,
        0x89 => 0x00eb,
        0x8a => 0x00e8,
        0x8b => 0x00ef,
        0x8c => 0x00ee,
        0x8d => 0x00ec,
        0x8e => 0x00c4,
        0x8f => 0x00c5,
        0x90 => 0x00c9,
        0x91 => 0x00e6,
        0x92 => 0x00c6,
        0x93 => 0x00f4,
        0x94 => 0x00f6,
        0x95 => 0x00f2,
        0x96 => 0x00fb,
        0x97 => 0x00f9,
        0x98 => 0x00ff,
        0x99 => 0x00d6,
        0x9a => 0x00dc,
        0x9b => 0x00a2,
        0x9c => 0x00a3,
        0x9d => 0x00a5,
        0x9e => 0x20a7,
        0x9f => 0x0192,
        0xa0 => 0x00e1,
        0xa1 => 0x00ed,
        0xa2 => 0x00f3,
        0xa3 => 0x00fa,
        0xa4 => 0x00f1,
        0xa5 => 0x00d1,
        0xa6 => 0x00aa,
        0xa7 => 0x00ba,
        0xa8 => 0x00bf,
        0xa9 => 0x2310,
        0xaa => 0x00ac,
        0xab => 0x00bd,
        0xac => 0x00bc,
        0xad => 0x00a1,
        0xae => 0x00ab,
        0xaf => 0x00bb,
        0xb0 => 0x2591,
        0xb1 => 0x2592,
        0xb2 => 0x2593,
        0xb3 => 0x2502,
        0xb4 => 0x2524,
        0xb5 => 0x2561,
        0xb6 => 0x2562,
        0xb7 => 0x2556,
        0xb8 => 0x2555,
        0xb9 => 0x2563,
        0xba => 0x2551,
        0xbb => 0x2557,
        0xbc => 0x255d,
        0xbd => 0x255c,
        0xbe => 0x255b,
        0xbf => 0x2510,
        0xc0 => 0x2514,
        0xc1 => 0x2534,
        0xc2 => 0x252c,
        0xc3 => 0x251c,
        0xc4 => 0x2500,
        0xc5 => 0x253c,
        0xc6 => 0x255e,
        0xc7 => 0x255f,
        0xc8 => 0x255a,
        0xc9 => 0x2554,
        0xca => 0x2569,
        0xcb => 0x2566,
        0xcc => 0x2560,
        0xcd => 0x2550,
        0xce => 0x256c,
        0xcf => 0x2567,
        0xd0 => 0x2568,
        0xd1 => 0x2564,
        0xd2 => 0x2565,
        0xd3 => 0x2559,
        0xd4 => 0x2558,
        0xd5 => 0x2552,
        0xd6 => 0x2553,
        0xd7 => 0x256b,
        0xd8 => 0x256a,
        0xd9 => 0x2518,
        0xda => 0x250c,
        0xdb => 0x2588,
        0xdc => 0x2584,
        0xdd => 0x258c,
        0xde => 0x2590,
        0xdf => 0x2580,
        0xe0 => 0x03b1,
        0xe1 => 0x00df,
        0xe2 => 0x0393,
        0xe3 => 0x03c0,
        0xe4 => 0x03a3,
        0xe5 => 0x03c3,
        0xe6 => 0x00b5,
        0xe7 => 0x03c4,
        0xe8 => 0x03a6,
        0xe9 => 0x0398,
        0xea => 0x03a9,
        0xeb => 0x03b4,
        0xec => 0x221e,
        0xed => 0x03c6,
        0xee => 0x03b5,
        0xef => 0x2229,
        0xf0 => 0x2261,
        0xf1 => 0x00b1,
        0xf2 => 0x2265,
        0xf3 => 0x2264,
        0xf4 => 0x2320,
        0xf5 => 0x2321,
        0xf6 => 0x00f7,
        0xf7 => 0x2248,
        0xf8 => 0x00b0,
        0xf9 => 0x2219,
        0xfa => 0x00b7,
        0xfb => 0x221a,
        0xfc => 0x207f,
        0xfd => 0x00b2,
        0xfe => 0x25a0,
        0xff => 0x00a0,
    };
    char::from_u32(output).expect("every CP437 scalar is valid")
}

fn invalid_or_time(counters: &DecodeCounterState, limits: &ArchiveLimits) -> ArchiveError {
    if counters.elapsed() > limits.max_elapsed {
        super::store::elapsed_limit(counters.elapsed(), limits.max_elapsed)
    } else {
        ArchiveError::InvalidArchive
    }
}

fn le_u16(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("two-byte ZIP field"))
}

fn le_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("four-byte ZIP field"))
}

fn le_u64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("eight-byte ZIP field"))
}
