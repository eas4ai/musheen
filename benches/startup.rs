mod support;

use serde_json::json;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};
use support::Sample;

const TEMPORARY_PREFIX: &str = "musheen-startup-";
const STARTUP_DEADLINE: Duration = Duration::from_secs(30);
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

struct AppChild(Child);

impl Drop for AppChild {
    fn drop(&mut self) {
        if let Err(error) = self.0.kill()
            && error.kind() != io::ErrorKind::InvalidInput
        {
            eprintln!("startup benchmark could not stop Musheen: {error}");
        }
        if let Err(error) = self.0.wait() {
            eprintln!("startup benchmark could not reap Musheen: {error}");
        }
    }
}

#[derive(Default)]
struct ProcessMetrics {
    cpu_ns: u64,
    peak_rss_kib: u64,
    open_fds: usize,
}

impl ProcessMetrics {
    fn sample(&mut self, pid: u32) -> io::Result<()> {
        let process = PathBuf::from(format!("/proc/{pid}"));
        let status = match fs::read_to_string(process.join("status")) {
            Ok(status) => status,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let cpu = fs::read_to_string(process.join("schedstat"))?;
        let descriptors = fs::read_dir(process.join("fd"))?.count();
        self.cpu_ns = self.cpu_ns.max(support::cpu_nanoseconds(&cpu)?);
        self.peak_rss_kib = self.peak_rss_kib.max(support::peak_rss_kib(&status)?);
        self.open_fds = self.open_fds.max(descriptors);
        Ok(())
    }
}

fn first_window_visible() -> io::Result<bool> {
    let output = Command::new("xwininfo")
        .args(["-name", "Musheen"])
        .stdin(Stdio::null())
        .output()?;
    Ok(output.status.success()
        && String::from_utf8_lossy(&output.stdout).contains("Map State: IsViewable"))
}

fn make_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn run() -> BenchResult<()> {
    let app = PathBuf::from(std::env::var_os("MUSHEEN_STARTUP_APP").ok_or_else(|| {
        io::Error::new(io::ErrorKind::NotFound, "MUSHEEN_STARTUP_APP is not set")
    })?);
    if !app.is_file() || std::env::var_os("DISPLAY").is_none() {
        return Err(io::Error::other("startup needs a built app and an X11 display").into());
    }
    let temporary = tempfile::Builder::new()
        .prefix(TEMPORARY_PREFIX)
        .tempdir()?;
    let root = temporary.path();
    for name in ["config", "cache", "data", "runtime", "initial-directory"] {
        make_private_directory(&root.join(name))?;
    }
    let before = Sample::capture(TEMPORARY_PREFIX)?;
    let mut child = AppChild(
        Command::new(&app)
            .arg(root.join("initial-directory"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_CACHE_HOME", root.join("cache"))
            .env("XDG_DATA_HOME", root.join("data"))
            .env("XDG_RUNTIME_DIR", root.join("runtime"))
            .env("XDG_SESSION_TYPE", "x11")
            .env_remove("WAYLAND_DISPLAY")
            .env_remove("MUSHEEN_THEME_PREVIEW")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
    );
    let deadline = Instant::now() + STARTUP_DEADLINE;
    let mut metrics = ProcessMetrics::default();
    loop {
        metrics.sample(child.0.id())?;
        if first_window_visible()? {
            break;
        }
        if let Some(status) = child.0.try_wait()? {
            return Err(io::Error::other(format!(
                "Musheen exited before opening its first window: {status}"
            ))
            .into());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "Musheen did not show its first window within 30 seconds",
            )
            .into());
        }
        thread::sleep(Duration::from_millis(10));
    }
    metrics.sample(child.0.id())?;
    let after = Sample::capture(TEMPORARY_PREFIX)?;
    if metrics.peak_rss_kib == 0 || metrics.open_fds == 0 {
        return Err(io::Error::other("Musheen process resources were not sampled").into());
    }
    println!(
        "{}",
        json!({
            "case": "first_window_startup",
            "window_visible": true,
            "display_backend": "x11",
            "wall_ns": after.captured.duration_since(before.captured).as_nanos(),
            "cpu_ns": metrics.cpu_ns,
            "peak_rss_kib": metrics.peak_rss_kib,
            "open_fds": metrics.open_fds,
            "temporary_bytes": after.temporary_bytes,
            "benchmark_cpu_ns": after.cpu_nanoseconds.saturating_sub(before.cpu_nanoseconds),
            "benchmark_peak_rss_kib": after.peak_rss_kib,
            "benchmark_open_fds": after.open_fds,
            "queued_work_max": null,
            "retained_models_max": null,
            "internal_counters_sampled": false,
        })
    );
    Ok(())
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("startup benchmark skipped; run with scripts/run-benchmarks.sh");
        return Ok(());
    }
    run()
}
