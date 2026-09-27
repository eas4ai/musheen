#[path = "support/record.rs"]
mod benchmark_record;
mod support;

use benchmark_record::record;
use musheen_core::StorePath;
use musheen_desktop::{
    PtyEvent, TerminalError, TerminalExit, TerminalModel, TerminalProfile, TerminalSession,
    TerminalSize,
};
use serde_json::json;
use std::io;
use std::time::{Duration, Instant};
use support::Sample;

const OUTPUT_BYTES: usize = 4 * 1_024 * 1_024;
const QUEUE_CAPACITY: usize = 64;
const SCROLLBACK_LINES: u32 = 1_000_000;
const TEMPORARY_PREFIX: &str = "musheen-terminal-";
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

fn flood_pty() -> BenchResult<()> {
    let profile = TerminalProfile::new(
        "flood",
        "/bin/sh",
        ["-c", "exec /usr/bin/head -c 4194304 /dev/zero"],
    )?;
    let cwd = StorePath::from_unix_path(std::env::temp_dir().as_os_str());
    let before = Sample::capture(TEMPORARY_PREFIX)?;
    let session = TerminalSession::spawn(profile, &cwd, TerminalSize::default())?;
    let events = session.events();
    if events.capacity() != Some(QUEUE_CAPACITY) {
        return Err(io::Error::other("terminal event queue capacity changed").into());
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    while events.len() < QUEUE_CAPACITY && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    if events.len() != QUEUE_CAPACITY {
        return Err(io::Error::other("terminal flood did not saturate the queue").into());
    }

    let mut received_bytes = 0_usize;
    let mut exited = false;
    let mut queued_work_max = events.len();
    while Instant::now() < deadline {
        queued_work_max = queued_work_max.max(events.len());
        match session.recv_timeout(Duration::from_millis(100)) {
            Ok(PtyEvent::Output(bytes)) => received_bytes += bytes.len(),
            Ok(PtyEvent::Exited(TerminalExit::Code(0))) => exited = true,
            Ok(PtyEvent::Exited(status)) => {
                return Err(io::Error::other(format!(
                    "terminal flood exited unexpectedly: {status:?}"
                ))
                .into());
            }
            Ok(PtyEvent::ReadFailed) => {
                return Err(io::Error::other("PTY reader failed during flood").into());
            }
            Err(TerminalError::NotRunning) => break,
            Err(TerminalError::Io(message))
                if message.as_ref() == "terminal event receive timed out" => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !exited || received_bytes != OUTPUT_BYTES {
        return Err(io::Error::other(format!(
            "terminal flood lost output: received {received_bytes} of {OUTPUT_BYTES} bytes, exited={exited}"
        ))
        .into());
    }
    let after = Sample::capture(TEMPORARY_PREFIX)?;
    record(
        "terminal_flood_backpressure",
        &before,
        &after,
        json!({
            "bytes": OUTPUT_BYTES,
            "received_bytes": received_bytes,
            "queue_capacity": QUEUE_CAPACITY,
            "queued_work_max": queued_work_max,
            "retained_models_max": 0,
        }),
    );
    Ok(())
}

fn million_line_scrollback() -> BenchResult<()> {
    let mut terminal = TerminalModel::new(TerminalSize::default());
    let before = Sample::capture(TEMPORARY_PREFIX)?;
    for line in 0..SCROLLBACK_LINES {
        terminal.feed(format!("line-{line}\r\n").as_bytes());
    }
    let after = Sample::capture(TEMPORARY_PREFIX)?;
    let retained = terminal.scrollback_line_count();
    let bytes = terminal.scrollback_bytes();
    if retained != 10_000 || bytes > 64 * 1_024 * 1_024 {
        return Err(io::Error::other(format!(
            "terminal scrollback exceeded its budget: {retained} lines, {bytes} bytes"
        ))
        .into());
    }
    record(
        "terminal_million_line_scrollback",
        &before,
        &after,
        json!({
            "lines": SCROLLBACK_LINES,
            "scrollback_lines": retained,
            "scrollback_bytes": bytes,
            "queued_work_max": 0,
            "retained_models_max": retained,
        }),
    );
    Ok(())
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("terminal benchmark skipped; run with cargo bench --bench terminal");
        return Ok(());
    }
    flood_pty()?;
    million_line_scrollback()
}
