#[cfg(feature = "archive-libarchive")]
fn main() {
    if musheen_desktop::run_libarchive_worker().is_err() {
        std::process::exit(1);
    }
}

#[cfg(not(feature = "archive-libarchive"))]
fn main() {
    std::process::exit(1);
}
