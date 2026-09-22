use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::{Duration, Instant};

use musheen_core::CancellationToken;

use super::store::{ArchiveError, DecodeCounterState, elapsed_limit};

/// A logical cursor backed by positional reads.
///
/// Cloned Linux file descriptors share a kernel cursor. Archive decoders seek often, so each
/// decoder must keep its cursor in user space and use `read_at` to avoid moving another decoder.
pub(crate) struct PositionedFile {
    file: File,
    position: u64,
    length: u64,
}

impl PositionedFile {
    pub(crate) fn new(file: &File) -> io::Result<Self> {
        let clone = file.try_clone()?;
        let length = clone.metadata()?.len();
        Ok(Self {
            file: clone,
            position: 0,
            length,
        })
    }

    #[cfg(feature = "archive-libarchive")]
    pub(crate) fn into_inner(self) -> File {
        self.file
    }
}

impl Read for PositionedFile {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let count = self.file.read_at(buffer, self.position)?;
        self.position = self.position.saturating_add(count as u64);
        Ok(count)
    }
}

impl Seek for PositionedFile {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.position = match position {
            SeekFrom::Start(position) => position,
            SeekFrom::End(offset) => checked_seek_offset(self.length, offset)?,
            SeekFrom::Current(offset) => checked_seek_offset(self.position, offset)?,
        };
        Ok(self.position)
    }
}

pub(crate) fn checked_seek_offset(base: u64, offset: i64) -> io::Result<u64> {
    let position = if offset >= 0 {
        base.checked_add(offset as u64)
    } else {
        base.checked_sub(offset.unsigned_abs())
    };
    position.ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid seek"))
}

pub(crate) struct TimedReader<R> {
    inner: R,
    maximum_elapsed: Duration,
    counters: Arc<DecodeCounterState>,
    cancellation: Option<CancellationToken>,
}

impl<R> TimedReader<R> {
    pub(crate) fn new(
        inner: R,
        maximum_elapsed: Duration,
        counters: Arc<DecodeCounterState>,
    ) -> Self {
        Self {
            inner,
            maximum_elapsed,
            counters,
            cancellation: None,
        }
    }

    pub(crate) fn new_cancellable(
        inner: R,
        maximum_elapsed: Duration,
        counters: Arc<DecodeCounterState>,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            inner,
            maximum_elapsed,
            counters,
            cancellation: Some(cancellation),
        }
    }

    fn finish_call<T>(&self, started: Instant, result: io::Result<T>) -> io::Result<T> {
        self.counters.add_elapsed(started.elapsed());
        if self
            .cancellation
            .as_ref()
            .is_some_and(|cancellation| cancellation.wait_if_paused().is_err())
        {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive operation cancelled",
            ))
        } else if self.counters.elapsed() > self.maximum_elapsed {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "archive metadata time limit exceeded",
            ))
        } else {
            result
        }
    }
}

impl<R: Read> Read for TimedReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(|cancellation| cancellation.wait_if_paused().is_err())
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive operation cancelled",
            ));
        }
        let started = Instant::now();
        let result = self.inner.read(buffer);
        if let Ok(count) = result {
            self.counters.add_read_bytes(count as u64);
        }
        self.finish_call(started, result)
    }
}

impl<R: Seek> Seek for TimedReader<R> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        if self
            .cancellation
            .as_ref()
            .is_some_and(|cancellation| cancellation.wait_if_paused().is_err())
        {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "archive operation cancelled",
            ));
        }
        let started = Instant::now();
        let result = self.inner.seek(position);
        self.finish_call(started, result)
    }
}

pub(crate) struct DecodeReader<R> {
    inner: R,
    cancellation: CancellationToken,
    started: Instant,
    maximum_elapsed: Duration,
    read: u64,
    expanded_maximum: u64,
    ratio_maximum: u64,
    failure: Option<ArchiveError>,
}

impl<R> DecodeReader<R> {
    pub(crate) fn new(
        inner: R,
        cancellation: CancellationToken,
        maximum_elapsed: Duration,
        expanded_maximum: u64,
        ratio_maximum: u64,
    ) -> Self {
        Self {
            inner,
            cancellation,
            started: Instant::now(),
            maximum_elapsed,
            read: 0,
            expanded_maximum,
            ratio_maximum,
            failure: None,
        }
    }

    pub(crate) fn take_error(&mut self) -> Option<ArchiveError> {
        self.failure.take()
    }

    fn stop(&mut self, error: ArchiveError) -> io::Error {
        self.failure = Some(error);
        io::Error::other("archive decode stopped")
    }
}

impl<R: Read> Read for DecodeReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.cancellation.wait_if_paused().is_err() {
            return Err(self.stop(ArchiveError::Cancelled));
        }
        if self.started.elapsed() > self.maximum_elapsed {
            let error = elapsed_limit(self.started.elapsed(), self.maximum_elapsed);
            return Err(self.stop(error));
        }
        let count = self.inner.read(buffer)?;
        let next = self.read.saturating_add(count as u64);
        let limit = if next > self.expanded_maximum {
            Some(("expanded bytes", self.expanded_maximum))
        } else if next > self.ratio_maximum {
            Some(("compression ratio", self.ratio_maximum))
        } else {
            None
        };
        if let Some((resource, maximum)) = limit {
            return Err(self.stop(ArchiveError::LimitExceeded {
                resource,
                value: usize::try_from(next).unwrap_or(usize::MAX),
                maximum: usize::try_from(maximum).unwrap_or(usize::MAX),
            }));
        }
        self.read = next;
        Ok(count)
    }
}

pub(crate) struct BoundedWriter<W> {
    inner: W,
    written: u64,
    nested_maximum: u64,
    expanded_maximum: u64,
    ratio_maximum: u64,
    cancellation: CancellationToken,
    failure: Option<ArchiveError>,
    started: Instant,
    maximum_elapsed: Duration,
}

impl<W> BoundedWriter<W> {
    pub(crate) fn new(
        inner: W,
        nested_maximum: u64,
        expanded_maximum: u64,
        ratio_maximum: u64,
        cancellation: CancellationToken,
        maximum_elapsed: Duration,
    ) -> Self {
        Self {
            inner,
            written: 0,
            nested_maximum,
            expanded_maximum,
            ratio_maximum,
            cancellation,
            failure: None,
            started: Instant::now(),
            maximum_elapsed,
        }
    }

    pub(crate) fn take_error(&mut self) -> Option<ArchiveError> {
        self.failure.take()
    }
}

impl<W: io::Write> io::Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancellation.wait_if_paused().is_err() {
            self.failure = Some(ArchiveError::Cancelled);
            return Err(io::Error::other("archive operation cancelled"));
        }
        if self.started.elapsed() > self.maximum_elapsed {
            self.failure = Some(ArchiveError::LimitExceeded {
                resource: "archive decode milliseconds",
                value: usize::try_from(self.started.elapsed().as_millis()).unwrap_or(usize::MAX),
                maximum: usize::try_from(self.maximum_elapsed.as_millis()).unwrap_or(usize::MAX),
            });
            return Err(io::Error::other("archive decode time limit exceeded"));
        }
        let next = self.written.saturating_add(bytes.len() as u64);
        let limit = if next > self.nested_maximum {
            Some(("nested archive bytes", self.nested_maximum))
        } else if next > self.expanded_maximum {
            Some(("expanded bytes", self.expanded_maximum))
        } else if next > self.ratio_maximum {
            Some(("compression ratio", self.ratio_maximum))
        } else {
            None
        };
        if let Some((resource, maximum)) = limit {
            self.failure = Some(ArchiveError::LimitExceeded {
                resource,
                value: usize::try_from(next).unwrap_or(usize::MAX),
                maximum: usize::try_from(maximum).unwrap_or(usize::MAX),
            });
            return Err(io::Error::other("archive output limit exceeded"));
        }
        let count = self.inner.write(bytes)?;
        self.written = self.written.saturating_add(count as u64);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
