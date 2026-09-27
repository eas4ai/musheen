mod support;

use musheen_core::{CancellationToken, StorePath};
use musheen_local::LocalStore;
use musheen_ops::{CopyProvider, CopyRequest, CopySession, EventGeneration, JobId};
use serde_json::json;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::Path;
use support::Sample;

const COPY_BYTES: u64 = 64 * 1024 * 1024;
const BLOCK_BYTES: usize = 1024 * 1024;
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

fn write_source(path: &Path) -> BenchResult<Vec<u8>> {
    let pattern: Vec<u8> = (0..BLOCK_BYTES).map(|index| index as u8).collect();
    let mut file = File::create(path)?;
    for _ in 0..COPY_BYTES / BLOCK_BYTES as u64 {
        file.write_all(&pattern)?;
    }
    file.sync_all()?;
    Ok(pattern)
}

fn verify_copy(path: &Path, expected: &[u8]) -> BenchResult<()> {
    assert_eq!(fs::metadata(path)?.len(), COPY_BYTES);
    let mut file = File::open(path)?;
    let mut block = vec![0_u8; expected.len()];
    for _ in 0..COPY_BYTES / BLOCK_BYTES as u64 {
        file.read_exact(&mut block)?;
        assert_eq!(block, expected, "copy changed file contents");
    }
    Ok(())
}

fn record(case: &str, strategy: &str, before: &Sample, after: &Sample) {
    println!(
        "{}",
        json!({
            "case": case,
            "bytes": COPY_BYTES,
            "strategy": strategy,
            "verified": true,
            "wall_ns": after.captured.duration_since(before.captured).as_nanos(),
            "cpu_ns": after.cpu_nanoseconds.saturating_sub(before.cpu_nanoseconds),
            "peak_rss_kib": after.peak_rss_kib,
            "open_fds": after.open_fds,
            "queued_work_max": 0,
            "retained_models_max": 0,
            "temporary_bytes": after.temporary_bytes,
        })
    );
}

fn run() -> BenchResult<()> {
    let temporary = tempfile::Builder::new().prefix("musheen-ops-").tempdir()?;
    let source = temporary.path().join("source.bin");
    let streamed = temporary.path().join("streamed.bin");
    let transactional = temporary.path().join("transactional.bin");
    let pattern = write_source(&source)?;
    let source_path = StorePath::from_unix_path(source.as_os_str());
    let mut provider = LocalStore::new();
    let cancellation = CancellationToken::new();

    let before_streamed = Sample::capture("musheen-ops-")?;
    let copied = provider.copy_streamed(
        &source_path,
        &StorePath::from_unix_path(streamed.as_os_str()),
        &cancellation,
    )?;
    let after_streamed = Sample::capture("musheen-ops-")?;
    assert_eq!(copied, COPY_BYTES);
    verify_copy(&streamed, &pattern)?;
    record(
        "large_copy_streamed",
        "streamed",
        &before_streamed,
        &after_streamed,
    );
    fs::remove_file(streamed)?;

    let request = CopyRequest::new(
        JobId::new(1).expect("nonzero benchmark job ID"),
        EventGeneration::new(0),
        source_path,
        StorePath::from_unix_path(transactional.as_os_str()),
    );
    let before_transaction = Sample::capture("musheen-ops-")?;
    let outcome = CopySession::default().execute(&mut provider, &request, &cancellation)?;
    let after_transaction = Sample::capture("musheen-ops-")?;
    assert_eq!(outcome.bytes_copied(), COPY_BYTES);
    verify_copy(&transactional, &pattern)?;
    let strategy = format!("{:?}", outcome.strategy()).to_ascii_lowercase();
    record(
        "large_copy_transaction",
        &strategy,
        &before_transaction,
        &after_transaction,
    );
    Ok(())
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("operations benchmark skipped; run with cargo bench --bench operations");
        return Ok(());
    }
    run()
}
