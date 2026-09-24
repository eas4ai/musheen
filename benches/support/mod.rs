use std::fs;
use std::io;
use std::path::Path;
use std::time::Instant;

pub struct Sample {
    pub captured: Instant,
    pub cpu_nanoseconds: u64,
    pub peak_rss_kib: u64,
    pub open_fds: usize,
    pub temporary_bytes: u64,
}

impl Sample {
    pub fn capture(temporary_prefix: &str) -> io::Result<Self> {
        let cpu = fs::read_to_string("/proc/self/schedstat")?;
        let status = fs::read_to_string("/proc/self/status")?;
        Ok(Self {
            captured: Instant::now(),
            cpu_nanoseconds: cpu_nanoseconds(&cpu)?,
            peak_rss_kib: peak_rss_kib(&status)?,
            open_fds: fs::read_dir("/proc/self/fd")?.count(),
            temporary_bytes: temporary_bytes(&std::env::temp_dir(), temporary_prefix)?,
        })
    }
}

pub fn cpu_nanoseconds(schedstat: &str) -> io::Result<u64> {
    first_u64(schedstat, "missing CPU time")
}

pub fn peak_rss_kib(status: &str) -> io::Result<u64> {
    let value = status
        .lines()
        .find_map(|line| line.strip_prefix("VmHWM:"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing peak RSS"))?;
    first_u64(value, "missing peak RSS")
}

fn first_u64(value: &str, missing: &'static str) -> io::Result<u64> {
    value
        .split_whitespace()
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, missing))?
        .parse()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub fn temporary_bytes(root: &Path, prefix: &str) -> io::Result<u64> {
    let mut bytes = 0_u64;
    let mut pending = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(prefix) && entry.file_type()?.is_dir() {
            pending.push(entry.path());
        }
    }
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                bytes = bytes
                    .checked_add(entry.metadata()?.len())
                    .ok_or_else(|| io::Error::other("temporary byte count overflow"))?;
            }
        }
    }
    Ok(bytes)
}
