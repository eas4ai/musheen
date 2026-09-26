use super::io::{BoundedWriter, PositionedFile, TimedReader};
use super::tar_codec::TarScanner;
use super::store::{
    AllocationLease, ArchiveError, ArchiveLimits, ArchivePasswordProvider, DecodeCounterState,
};
use super::workspace::{
    WorkspaceReader, configure_zstd_decoder, gzip_decoder_workspace_bytes,
    reserve_decode_workspace, zstd_decoder_workspace_bytes,
};
use musheen_core::{CancellationToken, ProviderId};
use std::fs::File;
use std::io::{Read, Write};
use std::sync::Arc;

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
    pub(crate) _allocation: AllocationLease,
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
        ArchiveFormat::Zip => {
            super::zip_codec::open_scanner(source, provider, limits.clone(), Arc::clone(counters))
        }
        ArchiveFormat::Tar | ArchiveFormat::TarGzip | ArchiveFormat::TarZstd => Ok(Box::new(
            tar_scanner(source, source_bytes, provider, format, limits, counters)?,
        )),
        ArchiveFormat::SevenZip => super::seven_codec::open_scanner(
            source,
            provider,
            limits.clone(),
            Arc::clone(counters),
            passwords,
        ),
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Rar | ArchiveFormat::Iso => super::libarchive_codec::open_scanner(
            source.into_inner(),
            provider,
            limits.clone(),
            Arc::clone(counters),
        ),
    }
}

/// A scanner over a plain, gzip or zstd tar.
fn tar_scanner(
    source: PositionedFile,
    source_bytes: u64,
    provider: ProviderId,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
    counters: &Arc<DecodeCounterState>,
) -> Result<TarScanner, ArchiveError> {
    match format {
        ArchiveFormat::Tar => Ok(super::tar_codec::open_plain(
            source,
            provider,
            limits.clone(),
            Arc::clone(counters),
        )),
        ArchiveFormat::TarGzip => {
            let workspace =
                reserve_decode_workspace(counters, limits, gzip_decoder_workspace_bytes())?;
            let decoder = flate2::read::GzDecoder::new(TimedReader::new(
                source,
                limits.max_elapsed,
                Arc::clone(counters),
            ));
            Ok(super::tar_codec::open_stream(
                Box::new(WorkspaceReader::new(decoder, workspace)),
                provider,
                limits.clone(),
                Arc::clone(counters),
                source_bytes,
            ))
        }
        ArchiveFormat::TarZstd => {
            let workspace =
                reserve_decode_workspace(counters, limits, zstd_decoder_workspace_bytes()?)?;
            let mut decoder = zstd::stream::read::Decoder::new(TimedReader::new(
                source,
                limits.max_elapsed,
                Arc::clone(counters),
            ))
            .map_err(|_| ArchiveError::InvalidArchive)?;
            configure_zstd_decoder(&mut decoder).map_err(|_| ArchiveError::InvalidArchive)?;
            Ok(super::tar_codec::open_stream(
                Box::new(WorkspaceReader::new(decoder, workspace)),
                provider,
                limits.clone(),
                Arc::clone(counters),
                source_bytes,
            ))
        }
        _ => Err(ArchiveError::UnsupportedNestedFormat),
    }
}

/// The provider named on the entries a copy scans; nothing reads it.
fn copy_provider() -> Result<ProviderId, ArchiveError> {
    ProviderId::new("archive-copy").map_err(|_| ArchiveError::Io)
}

pub(crate) struct ArchiveCopyContext<'a> {
    pub(crate) passwords: &'a dyn ArchivePasswordProvider,
    pub(crate) limits: &'a ArchiveLimits,
    pub(crate) counters: &'a Arc<DecodeCounterState>,
    pub(crate) cancellation: &'a CancellationToken,
    pub(crate) compressed_size: Option<u64>,
}

/// Decodes the archive once, in archive order, and hands each regular file
/// `wanted` names to `visit` with a reader over its bytes. Unlike
/// `copy_entry`, it never decodes an entry twice or restarts the archive for
/// the next entry. A `visit` error stops the pass.
pub(crate) fn copy_files_in_order(
    file: &File,
    format: ArchiveFormat,
    context: &ArchiveCopyContext<'_>,
    wanted: &dyn Fn(u64) -> bool,
    visit: &mut dyn FnMut(u64, &mut dyn Read) -> Result<(), ArchiveError>,
) -> Result<(), ArchiveError> {
    let source_bytes = file.metadata().map_err(|_| ArchiveError::Io)?.len();
    let source = PositionedFile::new(file).map_err(|_| ArchiveError::Io)?;
    let timed = |source| {
        TimedReader::new_cancellable(
            source,
            context.limits.max_elapsed,
            Arc::clone(context.counters),
            context.cancellation.clone(),
        )
    };
    let result = match format {
        ArchiveFormat::Zip => super::zip_codec::copy_files_in_order(
            timed(source),
            context.passwords,
            context.limits,
            context.counters,
            wanted,
            visit,
        ),
        ArchiveFormat::Tar | ArchiveFormat::TarGzip | ArchiveFormat::TarZstd => {
            super::tar_codec::copy_files_in_order(
                tar_scanner(
                    source,
                    source_bytes,
                    copy_provider()?,
                    format,
                    context.limits,
                    context.counters,
                )?,
                context.cancellation,
                wanted,
                visit,
            )
        }
        ArchiveFormat::SevenZip => super::seven_codec::copy_files_in_order(
            timed(source),
            context.passwords,
            context.limits,
            context.counters,
            wanted,
            visit,
        ),
        #[cfg(feature = "archive-libarchive")]
        ArchiveFormat::Rar | ArchiveFormat::Iso => Err(ArchiveError::UnsupportedNestedFormat),
    };
    if context.cancellation.wait_if_paused().is_err() {
        return Err(ArchiveError::Cancelled);
    }
    result
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
        ArchiveFormat::Zip => super::zip_codec::copy_entry(
            reader,
            ordinal,
            context.passwords,
            &mut destination,
            context.limits,
            context.counters,
        ),
        ArchiveFormat::Tar | ArchiveFormat::TarGzip | ArchiveFormat::TarZstd => {
            let source = PositionedFile::new(file).map_err(|_| ArchiveError::Io)?;
            let source_bytes = file.metadata().map_err(|_| ArchiveError::Io)?.len();
            super::tar_codec::copy_entry(
                tar_scanner(
                    source,
                    source_bytes,
                    copy_provider()?,
                    format,
                    context.limits,
                    context.counters,
                )?,
                ordinal,
                context.cancellation,
                &mut destination,
            )
        }
        ArchiveFormat::SevenZip => super::seven_codec::copy_seven_zip(
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
    if context.cancellation.wait_if_paused().is_err() {
        return Err(ArchiveError::Cancelled);
    }
    result
}
