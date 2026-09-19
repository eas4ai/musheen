use musheen_core::CancellationToken;
use musheen_desktop::{
    ThumbnailCache, ThumbnailError, ThumbnailLimits, ThumbnailLookup, ThumbnailMode,
    ThumbnailRequest, ThumbnailService, ThumbnailSize,
};
use std::fs::{self, File};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn request(path: &Path, mtime: u64) -> ThumbnailRequest {
    ThumbnailRequest::new(path, mtime, ThumbnailSize::Normal).unwrap()
}

#[test]
fn cache_hits_require_matching_uri_mtime_and_failure_state() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source.png");
    fs::write(&source, b"source").unwrap();
    let cache = ThumbnailCache::new(temporary.path().join("cache"));
    let first = request(&source, 10);
    cache.store_rgba(&first, 1, 1, &[0xff, 0, 0, 0xff]).unwrap();

    assert!(matches!(
        cache.lookup(&first).unwrap(),
        ThumbnailLookup::Hit(_)
    ));
    assert_eq!(
        cache.lookup(&request(&source, 11)).unwrap(),
        ThumbnailLookup::Miss
    );

    let failed = request(&temporary.path().join("failed.png"), 12);
    cache.record_failure(&failed, "malformed image").unwrap();
    assert!(matches!(
        cache.lookup(&failed).unwrap(),
        ThumbnailLookup::Failed { .. }
    ));
}

#[test]
fn cache_only_misses_never_start_a_worker() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source.png");
    fs::write(&source, b"not opened").unwrap();
    let service = ThumbnailService::with_worker(
        ThumbnailCache::new(temporary.path().join("cache")),
        temporary.path().join("worker-must-not-run"),
        ThumbnailLimits::default(),
    );

    assert_eq!(
        service
            .resolve(
                &request(&source, 1),
                ThumbnailMode::CacheOnly,
                CancellationToken::new(),
            )
            .unwrap(),
        ThumbnailLookup::Miss
    );
}

#[cfg(unix)]
fn worker_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;

    fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(unix)]
#[test]
fn worker_timeout_writes_a_failure_record() {
    let temporary = tempfile::tempdir().unwrap();
    let worker = temporary.path().join("slow-worker");
    worker_script(&worker, "sleep 1");
    let source = temporary.path().join("source.png");
    fs::write(&source, b"image").unwrap();
    let cache = ThumbnailCache::new(temporary.path().join("cache"));
    let service = ThumbnailService::with_worker(
        cache.clone(),
        worker,
        ThumbnailLimits::new(4, Duration::from_millis(50)).unwrap(),
    );
    let request = request(&source, 1);

    assert!(matches!(
        service.resolve(&request, ThumbnailMode::Generate, CancellationToken::new()),
        Err(ThumbnailError::Timeout)
    ));
    assert!(matches!(
        cache.lookup(&request).unwrap(),
        ThumbnailLookup::Failed { .. }
    ));
}

#[cfg(unix)]
#[test]
fn default_pool_runs_no_more_than_four_workers() {
    let temporary = tempfile::tempdir().unwrap();
    let worker = temporary.path().join("bounded-worker");
    worker_script(&worker, "sleep 0.1; exit 1");
    let service = Arc::new(ThumbnailService::with_worker(
        ThumbnailCache::new(temporary.path().join("cache")),
        worker,
        ThumbnailLimits::default(),
    ));
    let mut threads = Vec::new();
    for index in 0..8 {
        let service = Arc::clone(&service);
        let source = temporary.path().join(format!("source-{index}.png"));
        fs::write(&source, b"image").unwrap();
        threads.push(std::thread::spawn(move || {
            let _ = service.resolve(
                &request(&source, 1),
                ThumbnailMode::Generate,
                CancellationToken::new(),
            );
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }

    assert_eq!(service.max_observed_workers(), 4);
}

#[test]
fn isolated_worker_rejects_malformed_and_oversized_images() {
    let temporary = tempfile::tempdir().unwrap();
    let worker = env!("CARGO_BIN_EXE_musheen-thumbnail-worker");
    let cache = ThumbnailCache::new(temporary.path().join("cache"));
    let service = ThumbnailService::with_worker(cache.clone(), worker, ThumbnailLimits::default());

    let malformed = temporary.path().join("malformed.png");
    fs::write(&malformed, b"not an image").unwrap();
    assert!(
        service
            .resolve(
                &request(&malformed, 1),
                ThumbnailMode::Generate,
                CancellationToken::new()
            )
            .is_err()
    );

    let oversized = temporary.path().join("oversized.png");
    let encoder = png::Encoder::new(File::create(&oversized).unwrap(), 10_000, 6_000);
    let _header = encoder.write_header().unwrap();
    assert!(matches!(
        service.resolve(
            &request(&oversized, 1),
            ThumbnailMode::Generate,
            CancellationToken::new()
        ),
        Err(ThumbnailError::Worker(_))
    ));

    let valid = temporary.path().join("valid.png");
    let mut encoder = png::Encoder::new(File::create(&valid).unwrap(), 2, 1);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&[255, 0, 0, 255, 0, 0, 255, 255])
        .unwrap();
    assert!(matches!(
        service
            .resolve(
                &request(&valid, 2),
                ThumbnailMode::Generate,
                CancellationToken::new()
            )
            .unwrap(),
        ThumbnailLookup::Hit(path) if path.is_file()
    ));

    let too_large = temporary.path().join("too-large.png");
    File::create(&too_large)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert!(matches!(
        service.resolve(
            &request(&too_large, 3),
            ThumbnailMode::Generate,
            CancellationToken::new()
        ),
        Err(ThumbnailError::Worker(_))
    ));
}
