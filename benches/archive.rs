mod support;

use musheen_core::{CapabilityMatrix, CapabilityState, ProviderId, ResourceLimits, StorePath};
use musheen_desktop::{
    ArchiveError, ArchiveOperationAccounting, ArchiveOperationError, ArchiveOperationLimits,
    ArchivePassword, ArchivePasswordProvider, FileJournalStorage, PasswordRequest,
    execute_scheduled_archive_operation_with_accounting,
};
use musheen_ops::{
    ArchiveCodec, ArchiveConflictPolicy, ArchiveOperationPlan, Journal, ProviderLimits,
    ProviderSnapshot, Scheduler,
};
use serde_json::json;
use std::fs;
use std::io::{self, Cursor, Write};
use std::path::Path;
use support::Sample;

const TEMPORARY_PREFIX: &str = "musheen-archive-";
const HUGE_DECLARED_BYTES: u64 = 20 * 1_024 * 1_024 * 1_024 + 1;
const RATIO_PAYLOAD_BYTES: usize = 8 * 1_024 * 1_024;
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

struct NoPasswords;

impl ArchivePasswordProvider for NoPasswords {
    fn request_password(
        &self,
        _request: &PasswordRequest,
    ) -> Result<Option<ArchivePassword>, ArchiveError> {
        Ok(None)
    }
}

fn local(path: &Path) -> StorePath {
    StorePath::from_unix_path(path.as_os_str())
}

fn provider() -> ProviderSnapshot {
    ProviderSnapshot::new(
        ProviderId::new("archive-benchmark").expect("static provider id"),
        CapabilityMatrix::new(|_| CapabilityState::Supported),
        ProviderLimits::unbounded(),
    )
}

fn run_case(
    root: &Path,
    case: &str,
    source: &Path,
    codec: ArchiveCodec,
    expected_resource: &str,
) -> BenchResult<()> {
    let destination = root.join(format!("{case}-output"));
    let plan = ArchiveOperationPlan::extract(
        local(source),
        local(&destination),
        codec,
        ArchiveConflictPolicy::Fail,
        false,
    )?;
    let storage = FileJournalStorage::at(root.join(format!("{case}-journal")))?;
    let mut journal = Journal::open(storage)?;
    let scheduler = Scheduler::new(&ResourceLimits::default());
    scheduler.enqueue_archive(plan, provider())?;
    let job = scheduler
        .start_ready()?
        .pop()
        .ok_or_else(|| io::Error::other("archive benchmark job did not start"))?;
    let accounting = ArchiveOperationAccounting::default();
    let before = Sample::capture(TEMPORARY_PREFIX)?;
    let result = execute_scheduled_archive_operation_with_accounting(
        &scheduler,
        &job,
        &ArchiveOperationLimits::default(),
        &NoPasswords,
        &mut journal,
        &accounting,
    );
    let after = Sample::capture(TEMPORARY_PREFIX)?;
    if !matches!(&result, Err(ArchiveOperationError::LimitExceeded { resource, .. }) if *resource == expected_resource)
    {
        return Err(io::Error::other(format!(
            "{case} did not reject {expected_resource}: {result:?}"
        ))
        .into());
    }
    if destination.exists() {
        return Err(io::Error::other(format!(
            "{case} published output after rejecting the archive"
        ))
        .into());
    }
    let counters = accounting.counters();
    println!(
        "{}",
        json!({
            "case": case,
            "resource": expected_resource,
            "rejected": true,
            "source_bytes": fs::metadata(source)?.len(),
            "wall_ns": after.captured.duration_since(before.captured).as_nanos(),
            "cpu_ns": after.cpu_nanoseconds.saturating_sub(before.cpu_nanoseconds),
            "peak_rss_kib": after.peak_rss_kib,
            "open_fds": after.open_fds,
            "queued_work_max": 0,
            "retained_models_max": 0,
            "temporary_bytes": after.temporary_bytes,
            "archive_entries": counters.entries,
            "archive_expanded_bytes": counters.expanded_bytes,
            "archive_compressed_bytes": counters.compressed_bytes,
            "archive_peak_memory_bytes": counters.peak_memory_bytes,
            "archive_max_nesting": counters.max_nesting,
        })
    );
    Ok(())
}

fn write_declared_huge_tar(path: &Path) -> BenchResult<()> {
    let mut header = tar::Header::new_gnu();
    header.set_path("huge.bin")?;
    header.set_entry_type(tar::EntryType::Regular);
    header.set_mode(0o600);
    header.set_size(HUGE_DECLARED_BYTES);
    header.set_cksum();
    fs::write(path, header.as_bytes())?;
    Ok(())
}

fn write_ratio_zip(path: &Path) -> BenchResult<()> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer.start_file(
        "zeros.bin",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated),
    )?;
    writer.write_all(&vec![0_u8; RATIO_PAYLOAD_BYTES])?;
    fs::write(path, writer.finish()?.into_inner())?;
    Ok(())
}

fn write_nested_zip(path: &Path) -> BenchResult<()> {
    let mut nested = b"leaf".to_vec();
    for depth in 0..=9 {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer.start_file(
            format!("opaque-{depth}.bin"),
            zip::write::SimpleFileOptions::default(),
        )?;
        writer.write_all(&nested)?;
        nested = writer.finish()?.into_inner();
    }
    fs::write(path, nested)?;
    Ok(())
}

fn run() -> BenchResult<()> {
    let temporary = tempfile::Builder::new()
        .prefix(TEMPORARY_PREFIX)
        .tempdir()?;
    let root = temporary.path();

    let huge = root.join("declared-huge.tar");
    write_declared_huge_tar(&huge)?;
    run_case(
        root,
        "archive_expanded_bytes_rejected",
        &huge,
        ArchiveCodec::Tar,
        "expanded bytes",
    )?;

    let ratio = root.join("ratio.zip");
    write_ratio_zip(&ratio)?;
    run_case(
        root,
        "archive_compression_ratio_rejected",
        &ratio,
        ArchiveCodec::Zip,
        "compression ratio",
    )?;

    let nested = root.join("nested.zip");
    write_nested_zip(&nested)?;
    run_case(
        root,
        "archive_nesting_rejected",
        &nested,
        ArchiveCodec::Zip,
        "archive nesting",
    )?;
    Ok(())
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("archive benchmark skipped; run with cargo bench --bench archive");
        return Ok(());
    }
    run()
}
