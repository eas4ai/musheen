use musheen_desktop::{ThumbnailCache, ThumbnailRequest, ThumbnailSize, generate_thumbnail};
use std::path::PathBuf;

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut source = None;
    let mut cache_root = None;
    let mut mtime = None;
    let mut size = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--source") => source = arguments.next().map(PathBuf::from),
            Some("--cache-root") => cache_root = arguments.next().map(PathBuf::from),
            Some("--mtime") => {
                mtime = arguments
                    .next()
                    .and_then(|value| value.to_str().and_then(|value| value.parse().ok()))
            }
            Some("--size") => {
                size = arguments
                    .next()
                    .and_then(|value| value.to_str().and_then(ThumbnailSize::parse))
            }
            _ => return Err("invalid thumbnail worker arguments".into()),
        }
    }
    let source = source.ok_or("missing --source")?;
    let request = ThumbnailRequest::new(
        &source,
        mtime.ok_or("missing or invalid --mtime")?,
        size.ok_or("missing or invalid --size")?,
    )?;
    let cache = ThumbnailCache::new(cache_root.ok_or("missing --cache-root")?);
    generate_thumbnail(&cache, &request)?;
    Ok(())
}
