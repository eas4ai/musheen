//! One broker session for each elevated window (SYS-034).
//!
//! Open as Administrator authorizes once. The broker then stays with its
//! window and answers the window's folder listings under the granted root,
//! each checked like any request, until the window closes, Musheen exits, or
//! no request comes for [`ELEVATED_SESSION_IDLE`].

use std::io::{BufRead as _, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use musheen_core::CancellationToken;

use super::{
    AuditSink, Authorizer, BROKER_RESPONSE_FRAME, Broker, BrokerDirectoryEntry, BrokerError,
    BrokerOperation, BrokerOutput, BrokerRequest, BrokerResponse, Clock, ElevatedRootReference,
    OperationRunner, decode_broker_response, encode_broker_request, encode_broker_response,
};

/// The line Musheen sends when an elevated window closes; the broker ends.
pub const BROKER_END_FRAME: &str = "MUSHEEN_END";

/// How long a session broker waits for its window's next request.
pub const ELEVATED_SESSION_IDLE: Duration = Duration::from_secs(15 * 60);

/// How often a waiting session broker checks its idle time.
const IDLE_CHECK: Duration = Duration::from_secs(1);

/// Time since boot, including time the machine was suspended. A session's
/// idle time is measured on it: the monotonic clock behind `Instant` stops
/// during a suspend, which would let a session outlive its idle limit.
#[must_use]
pub fn boot_clock() -> Duration {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Boottime);
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or(0),
        u32::try_from(now.tv_nsec).unwrap_or(0),
    )
}

/// The largest listing a session carries, before framing.
pub const MAX_SESSION_LISTING_BYTES: usize = 64 * 1024 * 1024;

/// A request line's upper bound. A request names at most two paths of at
/// most 4,096 bytes each.
pub const MAX_REQUEST_LINE_BYTES: usize = 1024 * 1024;

/// A response line's upper bound: the largest listing in base64, with room
/// for the frame's other fields.
const MAX_RESPONSE_LINE_BYTES: usize = MAX_SESSION_LISTING_BYTES / 3 * 4 + 64 * 1024;

/// The broker's request lines, read on their own thread so the broker can
/// stop waiting when its window goes idle.
pub struct RequestLines {
    lines: mpsc::Receiver<Result<String, BrokerError>>,
    /// Set while a session runs. The input thread then ends the process as
    /// soon as the end line or the end of input arrives, even while a listing
    /// runs: the window is gone, so nothing is left to answer.
    session: Arc<AtomicBool>,
}

/// What waiting for the next request produced.
enum NextRequest {
    Line(Result<String, BrokerError>),
    Waiting,
    Closed,
}

impl RequestLines {
    pub fn spawn(input: impl Read + Send + 'static) -> Result<Self, BrokerError> {
        let (sender, lines) = mpsc::sync_channel(1);
        let session = Arc::new(AtomicBool::new(false));
        let in_session = Arc::clone(&session);
        let end_with_input = move || {
            if in_session.load(Ordering::SeqCst) {
                std::process::exit(0);
            }
        };
        std::thread::Builder::new()
            .name("musheen-broker-input".to_owned())
            .spawn(move || {
                let mut input = std::io::BufReader::new(input);
                loop {
                    let mut line = Vec::new();
                    let line = match (&mut input)
                        .take(MAX_REQUEST_LINE_BYTES as u64 + 1)
                        .read_until(b'\n', &mut line)
                    {
                        Ok(0) | Err(_) => {
                            end_with_input();
                            return;
                        }
                        Ok(_) if line.len() > MAX_REQUEST_LINE_BYTES => {
                            Err(BrokerError::InvalidRequest)
                        }
                        Ok(_) => String::from_utf8(line).map_err(|_| BrokerError::InvalidRequest),
                    };
                    if line
                        .as_ref()
                        .is_ok_and(|line| line.trim() == BROKER_END_FRAME)
                    {
                        end_with_input();
                    }
                    // An unreadable line ends the input: nothing after it
                    // can be framed reliably.
                    let last = line.is_err();
                    if sender.send(line).is_err() || last {
                        return;
                    }
                }
            })
            .map_err(|_| BrokerError::BrokerCrashed)?;
        Ok(Self { lines, session })
    }

    /// The first request, however long it takes to arrive.
    #[must_use]
    pub fn first(&self) -> Option<Result<String, BrokerError>> {
        self.lines.recv().ok()
    }

    /// The next request, waiting at most `wait`.
    fn next(&self, wait: Duration) -> NextRequest {
        match self.lines.recv_timeout(wait) {
            Ok(line) => NextRequest::Line(line),
            Err(mpsc::RecvTimeoutError::Timeout) => NextRequest::Waiting,
            Err(mpsc::RecvTimeoutError::Disconnected) => NextRequest::Closed,
        }
    }
}

/// Writes one response line. A listing over [`MAX_SESSION_LISTING_BYTES`]
/// becomes an error response, so the window gets a typed answer.
pub fn write_response(output: &mut dyn Write, response: &BrokerResponse) -> std::io::Result<()> {
    let mut frame =
        encode_broker_response(response).map_err(|_| std::io::Error::other("response"))?;
    if frame.len() > MAX_RESPONSE_LINE_BYTES {
        frame = encode_broker_response(&BrokerResponse::failure(&BrokerError::Io))
            .map_err(|_| std::io::Error::other("response"))?;
    }
    output.write_all(frame.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

/// Answers the listings of the window that `root` was granted to, after the
/// broker answered that window's Open as Administrator request. A request
/// must list a folder under that same root; `bind` ties it to the invoking
/// process again, and the broker checks it like any request. The session
/// ends when the input closes or brings the end line, when `idle` passes
/// without a request on `clock` (production passes [`boot_clock`]), or when
/// the output closes. From here on, the end line or the end of input ends
/// the process at once, even during a listing.
pub fn serve_session<A, R, S, C>(
    broker: &Broker<A, R, S, C>,
    root: &ElevatedRootReference,
    requests: &RequestLines,
    output: &mut dyn Write,
    bind: &dyn Fn(&mut BrokerRequest) -> Result<(), BrokerError>,
    idle: Duration,
    clock: &dyn Fn() -> Duration,
) where
    A: Authorizer,
    R: OperationRunner,
    S: AuditSink,
    C: Clock,
{
    // Holding the granted folder keeps its inode allocated, so a folder
    // created again at its path cannot take its inode number and pass a later
    // request's identity check.
    let Ok(_granted) = root.hold() else {
        return;
    };
    requests.session.store(true, Ordering::SeqCst);
    let mut last_request = clock();
    loop {
        let waited = clock().saturating_sub(last_request);
        if waited >= idle {
            return;
        }
        let line = match requests.next((idle - waited).min(IDLE_CHECK)) {
            NextRequest::Line(line) => line,
            NextRequest::Waiting => continue,
            NextRequest::Closed => return,
        };
        // A request that arrives after the idle limit, such as one sent
        // just after the machine woke from a suspend, is not served.
        if clock().saturating_sub(last_request) >= idle {
            return;
        }
        last_request = clock();
        if line
            .as_ref()
            .is_ok_and(|line| line.trim() == BROKER_END_FRAME)
        {
            return;
        }
        let answer = line
            .and_then(|line| super::decode_broker_request(line.trim()))
            .and_then(|mut request| {
                match request.operation() {
                    BrokerOperation::ReadDirectory {
                        root: requested, ..
                    } if requested == root => {}
                    _ => return Err(BrokerError::ScopeEscape),
                }
                bind(&mut request)?;
                broker.handle(request)
            });
        let response = answer.map_or_else(
            |error| BrokerResponse::failure(&error),
            BrokerResponse::success,
        );
        if write_response(output, &response).is_err() {
            return;
        }
    }
}

/// Makes the sudo broker's terminal pass bytes through unchanged: no echo,
/// no line editing and no 4,095-byte line limit, so a long request arrives
/// whole and is not echoed back. It also makes the terminal refuse any
/// further open by an unprivileged process, so no other process of the user
/// can write into the session. This matters when sudo gives the broker a
/// terminal of its own (`use_pty`).
pub fn prepare_sudo_terminal() -> Result<(), BrokerError> {
    let input = std::io::stdin();
    if !rustix::termios::isatty(&input) {
        return Ok(());
    }
    let mut terminal =
        rustix::termios::tcgetattr(&input).map_err(|_| BrokerError::BrokerCrashed)?;
    terminal.make_raw();
    rustix::termios::tcsetattr(&input, rustix::termios::OptionalActions::Now, &terminal)
        .map_err(|_| BrokerError::BrokerCrashed)?;
    rustix::termios::ioctl_tiocexcl(&input).map_err(|_| BrokerError::BrokerCrashed)
}

/// Why a broker's output produced no response line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChannelError {
    Cancelled,
    TimedOut,
    /// The broker's output closed: it exited.
    Ended,
    Oversized,
}

/// Stops a broker once Musheen is done with it. After the end line and a
/// closed input a session broker exits on its own; with `graceful` false,
/// or when it runs on, `end` stops it. Either way it reaps it.
pub(crate) trait BrokerProcess: Send + 'static {
    fn end(self: Box<Self>, graceful: bool);
}

/// Musheen's end of a running broker: requests go in through `writer`, and
/// the broker's output arrives in chunks from a reader thread, so a large
/// response never blocks the broker on its pipe.
pub(crate) struct BrokerChannel {
    writer: Option<Box<dyn Write + Send>>,
    chunks: mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    scanned: usize,
    process: Option<Box<dyn BrokerProcess>>,
    /// Set when the broker is to be stopped rather than asked to end.
    stopped: bool,
}

impl BrokerChannel {
    pub(crate) fn new(
        writer: Box<dyn Write + Send>,
        chunks: mpsc::Receiver<Vec<u8>>,
        buffer: Vec<u8>,
        process: Box<dyn BrokerProcess>,
    ) -> Self {
        Self {
            writer: Some(writer),
            chunks,
            buffer,
            scanned: 0,
            process: Some(process),
            stopped: false,
        }
    }

    pub(crate) fn send(&mut self, line: &str) -> std::io::Result<()> {
        self.send_bytes(line.as_bytes())
    }

    /// Sends `bytes` and a newline.
    pub(crate) fn send_bytes(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| std::io::Error::other("closed"))?;
        writer.write_all(bytes)?;
        writer.write_all(b"\n")?;
        writer.flush()
    }

    /// Stops the broker when the channel drops, without the end line.
    pub(crate) fn stop_now(&mut self) {
        self.stopped = true;
    }

    /// The broker's next response line. Other output is skipped.
    pub(crate) fn next_response(
        &mut self,
        deadline: Instant,
        cancellation: &CancellationToken,
    ) -> Result<String, ChannelError> {
        loop {
            if let Some(offset) = self.buffer[self.scanned..]
                .iter()
                .position(|byte| *byte == b'\n')
            {
                let line = self
                    .buffer
                    .drain(..=self.scanned + offset)
                    .collect::<Vec<_>>();
                self.scanned = 0;
                let line = String::from_utf8_lossy(&line);
                let line = line.trim_matches(['\r', '\n']);
                if line.starts_with(BROKER_RESPONSE_FRAME) {
                    return Ok(line.to_owned());
                }
                continue;
            }
            self.scanned = self.buffer.len();
            if self.buffer.len() > MAX_RESPONSE_LINE_BYTES {
                return Err(ChannelError::Oversized);
            }
            if cancellation.is_cancelled() {
                return Err(ChannelError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(ChannelError::TimedOut);
            }
            match self
                .chunks
                .recv_timeout((deadline - now).min(Duration::from_millis(20)))
            {
                Ok(chunk) => self.buffer.extend_from_slice(&chunk),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(ChannelError::Ended),
            }
        }
    }

    /// Output received but not yet read as a response line.
    pub(crate) fn pending_output(&self) -> &[u8] {
        &self.buffer
    }

    /// Takes in the output received so far; true once the broker's output
    /// closed, which means the broker ended.
    pub(crate) fn closed(&mut self) -> bool {
        loop {
            match self.chunks.try_recv() {
                Ok(chunk) => self.buffer.extend_from_slice(&chunk),
                Err(mpsc::TryRecvError::Empty) => return false,
                Err(mpsc::TryRecvError::Disconnected) => return true,
            }
        }
    }

    /// The next chunk of output, for a transport that reads its own
    /// handshake before any response.
    pub(crate) fn receive_chunk(&mut self, wait: Duration) -> Result<(), ChannelError> {
        match self.chunks.recv_timeout(wait) {
            Ok(chunk) => {
                self.buffer.extend_from_slice(&chunk);
                Ok(())
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ChannelError::Ended),
        }
    }

    pub(crate) fn clear_output(&mut self) {
        self.buffer.clear();
        self.scanned = 0;
    }

    /// Removes and returns the complete lines received so far.
    pub(crate) fn take_lines(&mut self) -> Vec<String> {
        let mut lines = Vec::new();
        while let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=newline).collect::<Vec<_>>();
            lines.push(
                String::from_utf8_lossy(&line)
                    .trim_matches(['\r', '\n'])
                    .to_owned(),
            );
        }
        self.scanned = 0;
        lines
    }
}

impl Drop for BrokerChannel {
    fn drop(&mut self) {
        // The end line and the closed input end a session broker. The
        // process is then stopped and reaped on its own thread, so a window
        // closes at once.
        let graceful = !self.stopped;
        if graceful {
            let _ = self.send(BROKER_END_FRAME);
        }
        drop(self.writer.take());
        if let Some(process) = self.process.take() {
            let _ = std::thread::Builder::new()
                .name("musheen-broker-end".to_owned())
                .spawn(move || process.end(graceful));
        }
    }
}

/// Reads a broker's output in chunks until it closes. The channel has no
/// bound, so the broker never waits on Musheen: the answer to a cancelled
/// listing is kept until the next request reads past it. At most one such
/// answer waits, because a session sends a request only after every earlier
/// answer was read.
pub(crate) fn spawn_output_reader(
    mut output: impl Read + Send + 'static,
    chunk_bytes: usize,
) -> Result<mpsc::Receiver<Vec<u8>>, BrokerError> {
    let (chunks, received) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("musheen-broker-output".to_owned())
        .spawn(move || {
            let mut chunk = vec![0_u8; chunk_bytes];
            loop {
                match output.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(length) if chunks.send(chunk[..length].to_vec()).is_err() => break,
                    Ok(_) => {}
                }
            }
        })
        .map_err(|_| BrokerError::BrokerCrashed)?;
    Ok(received)
}

/// A running broker that serves one elevated window. It was authorized by
/// the window's Open as Administrator request and answers the window's
/// listings under that request's root. Dropping it ends the broker.
pub struct BrokerSession {
    root: ElevatedRootReference,
    state: Mutex<SessionState>,
    timeout: Duration,
}

struct SessionState {
    /// `None` once the session ended; its broker was then told to stop.
    channel: Option<BrokerChannel>,
    /// Requests whose callers stopped waiting; their answers come first.
    unanswered: usize,
}

impl std::fmt::Debug for BrokerSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrokerSession")
            .field("root", &self.root)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl BrokerSession {
    pub(crate) fn new(
        root: ElevatedRootReference,
        channel: BrokerChannel,
        timeout: Duration,
    ) -> Self {
        Self {
            root,
            state: Mutex::new(SessionState {
                channel: Some(channel),
                unanswered: 0,
            }),
            timeout,
        }
    }

    /// The root the session was granted.
    #[must_use]
    pub const fn root_reference(&self) -> &ElevatedRootReference {
        &self.root
    }

    /// Whether the session still serves listings. It notices a broker that
    /// ended on its own, such as after its idle limit, without a request.
    pub fn is_open(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let state = &mut *state;
        let Some(channel) = state.channel.as_mut() else {
            return false;
        };
        if channel.closed() {
            end_session(state, ChannelError::Ended);
            return false;
        }
        true
    }

    /// Lists `relative` under `root` through the session's broker, which
    /// refuses any root but its own. After the session ended, because its
    /// broker ended or a request failed, every request fails with
    /// [`BrokerError::AuthorizationExpired`].
    pub fn read_directory(
        &self,
        root: ElevatedRootReference,
        relative: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<BrokerDirectoryEntry>, BrokerError> {
        let frame = encode_broker_request(&BrokerRequest::read_directory(root, relative)?)?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let deadline = Instant::now() + self.timeout;
        let response = exchange(&mut state, &frame, deadline, cancellation)?;
        match decode_broker_response(&response)? {
            BrokerOutput::DirectoryEntries(entries) => Ok(entries),
            _ => Err(BrokerError::BrokerCrashed),
        }
    }
}

/// Reads past the answers of cancelled requests, sends `frame`, and returns
/// its answer. A failure other than a cancel ends the session.
fn exchange(
    state: &mut SessionState,
    frame: &str,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> Result<String, BrokerError> {
    let Some(channel) = state.channel.as_mut() else {
        return Err(BrokerError::AuthorizationExpired);
    };
    let mut failure = None;
    while failure.is_none() && state.unanswered > 0 {
        match channel.next_response(deadline, cancellation) {
            Ok(_) => state.unanswered -= 1,
            Err(ChannelError::Cancelled) => return Err(BrokerError::AuthorizationCancelled),
            Err(error) => failure = Some(error),
        }
    }
    let failure = match failure {
        Some(error) => error,
        None => match channel.send(frame) {
            Err(_) => ChannelError::Ended,
            Ok(()) => match channel.next_response(deadline, cancellation) {
                Ok(response) => return Ok(response),
                Err(ChannelError::Cancelled) => {
                    state.unanswered += 1;
                    return Err(BrokerError::AuthorizationCancelled);
                }
                Err(error) => error,
            },
        },
    };
    Err(end_session(state, failure))
}

/// Ends the session after `error` and names why for the caller. The broker
/// is told to stop at once: its input closes, which ends a session broker
/// even during a listing, and a sudo broker's terminal hangs up.
fn end_session(state: &mut SessionState, error: ChannelError) -> BrokerError {
    if let Some(mut channel) = state.channel.take() {
        channel.stop_now();
    }
    state.unanswered = 0;
    match error {
        ChannelError::Ended => BrokerError::AuthorizationExpired,
        ChannelError::TimedOut => BrokerError::ExecutionTimedOut,
        ChannelError::Oversized | ChannelError::Cancelled => BrokerError::BrokerCrashed,
    }
}
