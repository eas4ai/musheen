#[path = "support/record.rs"]
mod benchmark_record;
mod support;

use benchmark_record::record;
use musheen_core::CancellationToken;
use musheen_desktop::{
    ThumbnailCache, ThumbnailError, ThumbnailLimits, ThumbnailLookup, ThumbnailMode,
    ThumbnailRequest, ThumbnailService, ThumbnailSize,
};
use serde_json::json;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use support::Sample;

const VALID_EDGE: u32 = 2048;
const OVERSIZED_WIDTH: u32 = 10_000;
const OVERSIZED_HEIGHT: u32 = 6_000;
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

fn write_valid_png(path: &Path) -> BenchResult<()> {
    let mut encoder = png::Encoder::new(File::create(path)?, VALID_EDGE, VALID_EDGE);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let pixels = vec![0x7f_u8; (VALID_EDGE as usize).pow(2) * 4];
    encoder.write_header()?.write_image_data(&pixels)?;
    Ok(())
}

fn write_oversized_header(path: &Path) -> BenchResult<()> {
    let encoder = png::Encoder::new(File::create(path)?, OVERSIZED_WIDTH, OVERSIZED_HEIGHT);
    drop(encoder.write_header()?);
    Ok(())
}

fn run_worker(worker: &Path, source: &Path, cache_root: &Path) -> BenchResult<u64> {
    let mut child = Command::new(worker)
        .arg("--source")
        .arg(source)
        .arg("--cache-root")
        .arg(cache_root)
        .args(["--mtime", "1", "--size", "normal"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let status_path = PathBuf::from(format!("/proc/{}/status", child.id()));
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut peak_rss = 0_u64;
    loop {
        match fs::read_to_string(&status_path) {
            Ok(status)
                if status
                    .lines()
                    .any(|line| line.starts_with("State:") && line.contains("Z (zombie)")) =>
            {
                // VmHWM disappears after exit, before try_wait reaps the worker.
            }
            Ok(status) => peak_rss = peak_rss.max(support::peak_rss_kib(&status)?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if let Some(status) = child.try_wait()? {
            assert!(status.success(), "thumbnail worker failed: {status}");
            return Ok(peak_rss);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            child.wait()?;
            return Err(
                io::Error::new(io::ErrorKind::TimedOut, "thumbnail worker timed out").into(),
            );
        }
        thread::sleep(Duration::from_millis(1));
    }
}

fn run() -> BenchResult<()> {
    let worker = PathBuf::from(std::env::var_os("MUSHEEN_THUMBNAIL_WORKER").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "MUSHEEN_THUMBNAIL_WORKER is not set",
        )
    })?);
    if !worker.is_file() {
        return Err(io::Error::new(io::ErrorKind::NotFound, "thumbnail worker is missing").into());
    }
    let temporary = tempfile::Builder::new()
        .prefix("musheen-thumb-")
        .tempdir()?;
    let cache = ThumbnailCache::new(temporary.path().join("cache"));
    let valid = temporary.path().join("valid.png");
    write_valid_png(&valid)?;
    let request = ThumbnailRequest::new(&valid, 1, ThumbnailSize::Normal)?;
    let before_decode = Sample::capture("musheen-thumb-")?;
    let worker_peak_rss = run_worker(&worker, &valid, cache.root())?;
    let after_decode = Sample::capture("musheen-thumb-")?;
    assert!(worker_peak_rss > 0, "worker RSS was not sampled");
    assert!(matches!(cache.lookup(&request)?, ThumbnailLookup::Hit(path) if path.is_file()));
    record(
        "thumbnail_worker_decode",
        &before_decode,
        &after_decode,
        json!({
            "decoded_pixels": VALID_EDGE * VALID_EDGE,
            "cache_hit": true,
            "worker_peak_rss_kib_sampled_max": worker_peak_rss,
            "queued_work_max": 0,
            "retained_models_max": 0,
        }),
    );

    let oversized = temporary.path().join("oversized.png");
    write_oversized_header(&oversized)?;
    let request = ThumbnailRequest::new(&oversized, 2, ThumbnailSize::Normal)?;
    let service = ThumbnailService::with_worker(cache.clone(), worker, ThumbnailLimits::default());
    let before_rejection = Sample::capture("musheen-thumb-")?;
    let result = service.resolve(&request, ThumbnailMode::Generate, CancellationToken::new());
    let after_rejection = Sample::capture("musheen-thumb-")?;
    assert!(
        matches!(result, Err(ThumbnailError::Worker(_))),
        "{result:?}"
    );
    assert!(matches!(
        cache.lookup(&request)?,
        ThumbnailLookup::Failed { .. }
    ));
    let pool_workers_max = service.max_observed_workers();
    assert!(pool_workers_max <= 4);
    record(
        "thumbnail_oversized_header_rejected",
        &before_rejection,
        &after_rejection,
        json!({
            "pixels": u64::from(OVERSIZED_WIDTH) * u64::from(OVERSIZED_HEIGHT),
            "failure_record": true,
            "pool_workers_max": pool_workers_max,
            "queued_work_max": 0,
            "retained_models_max": 0,
        }),
    );
    Ok(())
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("thumbnail benchmark skipped; run with cargo bench --bench thumbnail");
        return Ok(());
    }
    run()
}
