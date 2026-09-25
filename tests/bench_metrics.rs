use std::fs;

#[allow(dead_code)]
#[path = "../benches/support/mod.rs"]
mod support;

#[test]
fn linux_process_metrics_reject_missing_or_malformed_values() {
    assert_eq!(support::cpu_nanoseconds("12345 678 90\n").unwrap(), 12345);
    assert!(support::cpu_nanoseconds("not-a-number 678 90\n").is_err());
    assert!(support::cpu_nanoseconds("").is_err());

    let status = "Name:\tmusheen\nVmRSS:\t1024 kB\nVmHWM:\t2048 kB\n";
    assert_eq!(support::peak_rss_kib(status).unwrap(), 2048);
    assert!(support::peak_rss_kib("VmRSS:\t1024 kB\n").is_err());
    assert!(support::peak_rss_kib("VmHWM:\tbad kB\n").is_err());
}

#[test]
fn temporary_byte_measurement_only_counts_matching_directories() {
    let temporary = tempfile::tempdir().unwrap();
    let index = temporary.path().join("musheen-directory-owned");
    let other = temporary.path().join("another-app");
    fs::create_dir(&index).unwrap();
    fs::create_dir(&other).unwrap();
    fs::write(index.join("records"), [0_u8; 12]).unwrap();
    fs::write(other.join("records"), [0_u8; 40]).unwrap();

    assert_eq!(
        support::temporary_bytes(temporary.path(), "musheen-directory-").unwrap(),
        12
    );
}
