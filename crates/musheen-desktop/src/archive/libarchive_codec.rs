use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use compress_tools::{ArchiveContents, ArchiveIteratorBuilder};
use musheen_core::{CancellationToken, ProviderId};

use super::ArchivePath;
use super::format::{ArchiveScanner, RawArchiveEntry, RawEntryKind};
use super::store::{ArchiveError, ArchiveLimits, DecodeCounterState, elapsed_limit};

const FRAME_END: u8 = 0;
const FRAME_ENTRY: u8 = 1;
const FRAME_INVALID: u8 = 2;
const FRAME_TIME_LIMIT: u8 = 3;
const FRAME_EXPANDED_LIMIT: u8 = 4;
const FRAME_PATH_LIMIT: u8 = 5;

pub(crate) fn open_scanner(
    source: File,
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
) -> Result<Box<dyn ArchiveScanner>, ArchiveError> {
    Ok(Box::new(LibarchiveScanner::new(
        source, provider, limits, counters,
    )?))
}

struct LibarchiveScanner {
    provider: ProviderId,
    limits: ArchiveLimits,
    counters: Arc<DecodeCounterState>,
    child: Option<Child>,
    responses: Option<Receiver<WorkerFrame>>,
    reader: Option<JoinHandle<()>>,
    started: Instant,
    ordinal: u64,
    reported_bytes: u64,
}

enum WorkerFrame {
    Entry {
        name: Vec<u8>,
        kind: RawEntryKind,
        size: Option<u64>,
        bytes_read: u64,
    },
    End,
    Error(ArchiveError),
}

impl LibarchiveScanner {
    fn new(
        source: File,
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
    ) -> Result<Self, ArchiveError> {
        let worker = worker_path()?;
        let mut child = Command::new(worker)
            .arg(limits.max_path_bytes.to_string())
            .arg(limits.max_expanded_bytes.to_string())
            .arg(limits.max_elapsed.as_millis().max(1).to_string())
            .stdin(Stdio::from(source))
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| ArchiveError::Io)?;
        let stdout = child.stdout.take().ok_or(ArchiveError::Io)?;
        let (sender, responses) = mpsc::sync_channel(1);
        let reader = std::thread::Builder::new()
            .name("musheen-libarchive-protocol".into())
            .spawn(move || {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let frame = read_frame(&mut stdout);
                    let terminal = !matches!(frame, Ok(WorkerFrame::Entry { .. }));
                    let frame = frame.unwrap_or(WorkerFrame::Error(ArchiveError::InvalidArchive));
                    if sender.send(frame).is_err() || terminal {
                        return;
                    }
                }
            })
            .map_err(|_| ArchiveError::Io)?;
        Ok(Self {
            provider,
            limits,
            counters,
            child: Some(child),
            responses: Some(responses),
            reader: Some(reader),
            started: Instant::now(),
            ordinal: 0,
            reported_bytes: 0,
        })
    }

    fn terminate(&mut self) {
        self.responses.take();
        let mut reaped = true;
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            reaped = false;
            let deadline = Instant::now() + Duration::from_millis(100);
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        reaped = true;
                        break;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                    Err(_) => break,
                }
            }
            if !reaped {
                // A process stuck in an uninterruptible kernel wait must not hang archive teardown.
                // Keep ownership in a detached reaper so it is collected when the kernel releases
                // it, without blocking the UI thread or leaking a zombie.
                let _ = std::thread::Builder::new()
                    .name("musheen-libarchive-reaper".into())
                    .spawn(move || {
                        let _ = child.wait();
                    });
            }
        }
        if let Some(reader) = self.reader.take()
            && reaped
        {
            let _ = reader.join();
        }
    }

    fn receive(&mut self, cancellation: &CancellationToken) -> Result<WorkerFrame, ArchiveError> {
        loop {
            if cancellation.check().is_err() {
                self.terminate();
                return Err(ArchiveError::Cancelled);
            }
            let elapsed = self.started.elapsed();
            if elapsed > self.limits.max_elapsed {
                self.terminate();
                return Err(elapsed_limit(elapsed, self.limits.max_elapsed));
            }
            let wait = self
                .limits
                .max_elapsed
                .saturating_sub(elapsed)
                .min(Duration::from_millis(10));
            let result = self
                .responses
                .as_ref()
                .ok_or(ArchiveError::InvalidArchive)?
                .recv_timeout(wait);
            match result {
                Ok(frame) => return Ok(frame),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(ArchiveError::InvalidArchive),
            }
        }
    }
}

impl Drop for LibarchiveScanner {
    fn drop(&mut self) {
        self.terminate();
    }
}

impl ArchiveScanner for LibarchiveScanner {
    fn next_entry(
        &mut self,
        cancellation: &CancellationToken,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        let frame = match self.receive(cancellation) {
            Ok(frame) => frame,
            Err(error) => {
                self.terminate();
                return Err(error);
            }
        };
        self.accept_frame(frame)
    }
}

impl LibarchiveScanner {
    fn accept_frame(
        &mut self,
        frame: WorkerFrame,
    ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
        match frame {
            WorkerFrame::End => {
                self.terminate();
                Ok(None)
            }
            WorkerFrame::Error(error) => {
                self.terminate();
                Err(error)
            }
            WorkerFrame::Entry {
                name,
                kind,
                size,
                bytes_read,
            } => {
                let result = (|| {
                    let delta = bytes_read.saturating_sub(self.reported_bytes);
                    self.reported_bytes = bytes_read;
                    self.counters.add_read_bytes(delta);
                    let allocation = self.counters.reserve(
                        name.len()
                            .saturating_mul(2)
                            .saturating_add(std::mem::size_of::<RawArchiveEntry>()),
                        self.limits.max_metadata_bytes,
                    )?;
                    let path = ArchivePath::normalize_bytes(&name, self.limits.max_path_bytes)?;
                    let ordinal = self.ordinal;
                    self.ordinal = self.ordinal.saturating_add(1);
                    Ok(Some(RawArchiveEntry {
                        provider: self.provider.clone(),
                        path,
                        kind,
                        size,
                        compressed_size: None,
                        ordinal,
                        _allocation: allocation,
                    }))
                })();
                if result.is_err() {
                    self.terminate();
                }
                result
            }
        }
    }
}

fn worker_path() -> Result<PathBuf, ArchiveError> {
    if let Some(path) = std::env::var_os("MUSHEEN_ARCHIVE_WORKER") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = option_env!("CARGO_BIN_EXE_musheen-archive-worker") {
        return Ok(PathBuf::from(path));
    }
    let executable = std::env::current_exe().map_err(|_| ArchiveError::Io)?;
    let directory = executable.parent().ok_or(ArchiveError::Io)?;
    let candidate = if directory.file_name().is_some_and(|name| name == "deps") {
        directory
            .parent()
            .ok_or(ArchiveError::Io)?
            .join("musheen-archive-worker")
    } else {
        directory.join("musheen-archive-worker")
    };
    candidate
        .exists()
        .then_some(candidate)
        .ok_or(ArchiveError::Io)
}

fn read_frame(reader: &mut impl Read) -> io::Result<WorkerFrame> {
    let mut tag = [0_u8; 1];
    reader.read_exact(&mut tag)?;
    match tag[0] {
        FRAME_END => Ok(WorkerFrame::End),
        FRAME_INVALID => Ok(WorkerFrame::Error(ArchiveError::InvalidArchive)),
        FRAME_TIME_LIMIT => read_limit(reader, "archive metadata milliseconds"),
        FRAME_EXPANDED_LIMIT => read_limit(reader, "expanded bytes"),
        FRAME_PATH_LIMIT => read_limit(reader, "archive path bytes"),
        FRAME_ENTRY => {
            let kind = read_u8(reader)?;
            let size = read_u64(reader)?;
            let bytes_read = read_u64(reader)?;
            let name_length = read_u32(reader)? as usize;
            let mut name = vec![0_u8; name_length];
            reader.read_exact(&mut name)?;
            Ok(WorkerFrame::Entry {
                name,
                kind: match kind {
                    1 => RawEntryKind::Directory,
                    2 => RawEntryKind::RegularFile,
                    3 => RawEntryKind::SymbolicLink,
                    4 => RawEntryKind::HardLink,
                    _ => RawEntryKind::Other,
                },
                size: (size != u64::MAX).then_some(size),
                bytes_read,
            })
        }
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid worker frame",
        )),
    }
}

fn read_u8(reader: &mut impl Read) -> io::Result<u8> {
    let mut bytes = [0_u8; 1];
    reader.read_exact(&mut bytes)?;
    Ok(bytes[0])
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0_u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0_u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_limit(reader: &mut impl Read, resource: &'static str) -> io::Result<WorkerFrame> {
    let value = read_u64(reader)?;
    let maximum = read_u64(reader)?;
    Ok(WorkerFrame::Error(ArchiveError::LimitExceeded {
        resource,
        value: usize::try_from(value).unwrap_or(usize::MAX),
        maximum: usize::try_from(maximum).unwrap_or(usize::MAX),
    }))
}

fn write_limit(output: &mut impl Write, tag: u8, value: u64, maximum: u64) -> io::Result<()> {
    output.write_all(&[tag])?;
    output.write_all(&value.to_le_bytes())?;
    output.write_all(&maximum.to_le_bytes())
}

const FAILURE_NONE: u8 = 0;
const FAILURE_TIME: u8 = 1;
const FAILURE_EXPANDED: u8 = 2;

struct GuardedSource {
    source: File,
    started: Instant,
    maximum_elapsed: Duration,
    maximum_bytes: u64,
    bytes_read: Arc<AtomicU64>,
    failure: Arc<AtomicU8>,
}

impl GuardedSource {
    fn check(&self) -> io::Result<()> {
        if self.started.elapsed() > self.maximum_elapsed {
            self.failure.store(FAILURE_TIME, Ordering::Release);
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "archive worker deadline",
            ))
        } else {
            Ok(())
        }
    }
}

impl Read for GuardedSource {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let count = self.source.read(buffer)?;
        let total = self.bytes_read.fetch_add(count as u64, Ordering::AcqRel) + count as u64;
        if total > self.maximum_bytes {
            self.failure.store(FAILURE_EXPANDED, Ordering::Release);
            return Err(io::Error::other("archive worker expanded-byte limit"));
        }
        self.check()?;
        Ok(count)
    }
}

impl Seek for GuardedSource {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        self.source.seek(position)
    }
}

#[doc(hidden)]
pub fn run_worker() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let max_path = arguments
        .next()
        .ok_or("missing path limit")?
        .parse::<usize>()?;
    let max_bytes = arguments
        .next()
        .ok_or("missing byte limit")?
        .parse::<u64>()?;
    let max_millis = arguments
        .next()
        .ok_or("missing time limit")?
        .parse::<u64>()?;
    if arguments.next().is_some() {
        return Err("unexpected worker argument".into());
    }
    let source = File::open("/proc/self/fd/0")?;
    let bytes_read = Arc::new(AtomicU64::new(0));
    let failure = Arc::new(AtomicU8::new(FAILURE_NONE));
    let guarded = GuardedSource {
        source,
        started: Instant::now(),
        maximum_elapsed: Duration::from_millis(max_millis.max(1)),
        maximum_bytes: max_bytes,
        bytes_read: Arc::clone(&bytes_read),
        failure: Arc::clone(&failure),
    };
    let mut output = io::stdout().lock();
    let mut iterator = match ArchiveIteratorBuilder::new(guarded)
        .mtree_format(false)
        .build()
    {
        Ok(iterator) => iterator,
        Err(_) => {
            match failure.load(Ordering::Acquire) {
                FAILURE_TIME => write_limit(
                    &mut output,
                    FRAME_TIME_LIMIT,
                    max_millis.saturating_add(1),
                    max_millis,
                )?,
                FAILURE_EXPANDED => write_limit(
                    &mut output,
                    FRAME_EXPANDED_LIMIT,
                    bytes_read.load(Ordering::Acquire),
                    max_bytes,
                )?,
                _ => output.write_all(&[FRAME_INVALID])?,
            }
            output.flush()?;
            return Ok(());
        }
    };
    let mut expanded_total = 0_u64;
    loop {
        match iterator.next_header() {
            Some(ArchiveContents::StartOfEntry(name, status)) => {
                if name.len() > max_path {
                    write_limit(
                        &mut output,
                        FRAME_PATH_LIMIT,
                        name.len() as u64,
                        max_path as u64,
                    )?;
                    output.flush()?;
                    return Ok(());
                }
                let file_type = status.st_mode & 0o170_000;
                let kind = match file_type {
                    0o040_000 => 1,
                    0o100_000 if status.st_nlink > 1 => 4,
                    0o100_000 => 2,
                    0o120_000 => 3,
                    _ => 0,
                };
                let size = if kind == 2 {
                    u64::try_from(status.st_size).unwrap_or(0)
                } else {
                    u64::MAX
                };
                if size != u64::MAX {
                    expanded_total = expanded_total.saturating_add(size);
                    if expanded_total > max_bytes {
                        write_limit(&mut output, FRAME_EXPANDED_LIMIT, expanded_total, max_bytes)?;
                        output.flush()?;
                        return Ok(());
                    }
                }
                output.write_all(&[FRAME_ENTRY, kind])?;
                output.write_all(&size.to_le_bytes())?;
                output.write_all(&bytes_read.load(Ordering::Acquire).to_le_bytes())?;
                output.write_all(&(name.len() as u32).to_le_bytes())?;
                output.write_all(name.as_bytes())?;
                output.flush()?;
            }
            None => {
                output.write_all(&[FRAME_END])?;
                output.flush()?;
                return Ok(());
            }
            Some(ArchiveContents::Err(_)) => {
                match failure.load(Ordering::Acquire) {
                    FAILURE_TIME => write_limit(
                        &mut output,
                        FRAME_TIME_LIMIT,
                        max_millis.saturating_add(1),
                        max_millis,
                    )?,
                    FAILURE_EXPANDED => write_limit(
                        &mut output,
                        FRAME_EXPANDED_LIMIT,
                        bytes_read.load(Ordering::Acquire),
                        max_bytes,
                    )?,
                    _ => output.write_all(&[FRAME_INVALID])?,
                }
                output.flush()?;
                return Ok(());
            }
            Some(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::index::{ArchiveScannerFactory, LazyArchiveIndex};
    use std::sync::atomic::{AtomicBool, Ordering};

    fn archive_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut builder = tar::Builder::new(&mut bytes);
        let contents = b"contents";
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, "entry.txt", contents.as_slice())
            .expect("entry writes");
        builder.finish().expect("archive closes");
        drop(builder);
        bytes
    }

    fn duplicate_archive_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut builder = tar::Builder::new(&mut bytes);
        for contents in [b"first".as_slice(), b"second".as_slice()] {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "duplicate.txt", contents)
                .expect("duplicate entry writes");
        }
        builder.finish().expect("archive closes");
        drop(builder);
        bytes
    }

    fn scanner(bytes: &[u8]) -> LibarchiveScanner {
        scanner_with_limits(bytes, ArchiveLimits::default(), DecodeCounterState::new())
    }

    fn scanner_with_limits(
        bytes: &[u8],
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
    ) -> LibarchiveScanner {
        scanner_for_provider(
            bytes,
            ProviderId::new("archive-libarchive-lifecycle").expect("provider ID"),
            limits,
            counters,
        )
    }

    fn scanner_for_provider(
        bytes: &[u8],
        provider: ProviderId,
        limits: ArchiveLimits,
        counters: Arc<DecodeCounterState>,
    ) -> LibarchiveScanner {
        let mut source = tempfile::tempfile().expect("temporary source");
        source.write_all(bytes).expect("source writes");
        source.seek(SeekFrom::Start(0)).expect("source rewinds");
        LibarchiveScanner::new(source, provider, limits, counters).expect("worker starts")
    }

    fn assert_terminated(scanner: &LibarchiveScanner) {
        assert!(scanner.child.is_none(), "terminal path must reap the child");
        assert!(
            scanner.responses.is_none(),
            "terminal path must drop the protocol receiver"
        );
        assert!(
            scanner.reader.is_none(),
            "terminal path must join or detach the completed reader"
        );
    }

    #[test]
    fn normal_completion_reaps_worker_and_reader_immediately() {
        let mut scanner = scanner(&archive_bytes());
        while scanner
            .next_entry(&CancellationToken::new())
            .expect("entry reads")
            .is_some()
        {}
        assert_terminated(&scanner);
    }

    #[test]
    fn worker_error_reaps_worker_and_reader_immediately() {
        let mut scanner = scanner(b"not an archive");
        assert!(scanner.next_entry(&CancellationToken::new()).is_err());
        assert_terminated(&scanner);
    }

    #[test]
    fn protocol_disconnect_reaps_worker_and_reader_immediately() {
        let mut scanner = scanner(&archive_bytes());
        let mut child = scanner.child.take().expect("live worker");
        child.kill().expect("worker kills");
        child.wait().expect("worker reaps");
        while let Ok(Some(_)) = scanner.next_entry(&CancellationToken::new()) {}
        assert_terminated(&scanner);
    }

    #[test]
    fn post_frame_path_normalization_error_reaps_worker_immediately() {
        let mut scanner = scanner(&archive_bytes());
        let result = scanner.accept_frame(WorkerFrame::Entry {
            name: b"../escape".to_vec(),
            kind: RawEntryKind::RegularFile,
            size: Some(1),
            bytes_read: 1,
        });
        assert!(matches!(result, Err(ArchiveError::UnsafePath(_))));
        assert_terminated(&scanner);
    }

    #[test]
    fn post_frame_allocation_error_reaps_worker_immediately() {
        let limits = ArchiveLimits {
            max_metadata_bytes: 0,
            ..ArchiveLimits::default()
        };
        let mut scanner = scanner_with_limits(&archive_bytes(), limits, DecodeCounterState::new());
        let result = scanner.accept_frame(WorkerFrame::Entry {
            name: b"entry.txt".to_vec(),
            kind: RawEntryKind::RegularFile,
            size: Some(1),
            bytes_read: 1,
        });
        assert!(matches!(
            result,
            Err(ArchiveError::LimitExceeded {
                resource: "metadata bytes",
                ..
            })
        ));
        assert_terminated(&scanner);
    }

    struct ObservedWorkerScanner {
        inner: Option<LibarchiveScanner>,
        drop_completed: Arc<AtomicBool>,
    }

    impl ArchiveScanner for ObservedWorkerScanner {
        fn next_entry(
            &mut self,
            cancellation: &CancellationToken,
        ) -> Result<Option<RawArchiveEntry>, ArchiveError> {
            self.inner
                .as_mut()
                .expect("worker remains live")
                .next_entry(cancellation)
        }
    }

    impl Drop for ObservedWorkerScanner {
        fn drop(&mut self) {
            drop(self.inner.take());
            self.drop_completed.store(true, Ordering::Release);
        }
    }

    #[test]
    fn sticky_index_rejection_reaps_libarchive_worker_before_returning() {
        let bytes = duplicate_archive_bytes();
        let provider = ProviderId::new("archive-libarchive-index-rejection").expect("provider ID");
        let limits = ArchiveLimits::default();
        let counters = DecodeCounterState::new();
        let worker = scanner_for_provider(
            &bytes,
            provider.clone(),
            limits.clone(),
            Arc::clone(&counters),
        );
        let drop_completed = Arc::new(AtomicBool::new(false));
        let observed = ObservedWorkerScanner {
            inner: Some(worker),
            drop_completed: Arc::clone(&drop_completed),
        };
        let factory: ArchiveScannerFactory = Arc::new(|| Err(ArchiveError::InvalidArchive));
        let mut index = LazyArchiveIndex::new(Box::new(observed), factory, bytes.len() as u64);

        let result = index.ensure_children(
            &ArchivePath::root(provider),
            2,
            &CancellationToken::new(),
            &limits,
            &counters,
        );
        assert_eq!(result, Err(ArchiveError::DuplicatePath));
        assert!(
            drop_completed.load(Ordering::Acquire),
            "the sticky index rejection must finish worker teardown before returning"
        );
    }
}
