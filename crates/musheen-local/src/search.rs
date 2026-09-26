use crate::metadata::item_from_path;
use async_channel::{Receiver, Sender};
use futures_lite::future::{self, poll_fn};
use musheen_core::{
    BoxFuture, CancellationToken, ProviderId, SEARCH_BATCH_ERRORS, SEARCH_BATCH_RESULTS,
    SEARCH_CHANNEL_RESULTS, SEARCH_RESULT_LIMIT, SearchBatch, SearchCompletion, SearchQuery,
    SearchResult, SearchScopeError, SearchStream, StoreError, StorePath,
};
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::Path;
use std::task::Poll;
use walkdir::{DirEntry, WalkDir};

const CONTENT_CHUNK_BYTES: usize = 64 * 1_024;

pub(crate) fn start(
    provider: ProviderId,
    scope: &StorePath,
    query: SearchQuery,
    cancellation: CancellationToken,
) -> Result<Box<dyn SearchStream>, StoreError> {
    cancellation.check()?;
    let root = scope.as_unix_path().ok_or_else(|| {
        StoreError::unsupported("search", "the local provider accepts only Unix paths")
    })?;
    let (sender, receiver) = async_channel::bounded(
        SEARCH_CHANNEL_RESULTS
            .checked_div(SEARCH_BATCH_RESULTS)
            .expect("search batch size is nonzero"),
    );
    let root = root.to_path_buf();
    let worker_cancellation = cancellation.clone();
    let stream_cancellation = cancellation;
    std::thread::Builder::new()
        .name("musheen-local-search".into())
        .spawn(move || {
            SearchProducer {
                provider,
                root: &root,
                query: &query,
                cancellation: &worker_cancellation,
                sender: &sender,
                results: Vec::with_capacity(SEARCH_BATCH_RESULTS),
                errors: Vec::new(),
                total: 0,
            }
            .run();
        })
        .map_err(|error| {
            StoreError::Backend(format!("start local search worker: {error}").into())
        })?;
    Ok(Box::new(LocalSearchStream {
        receiver,
        cancellation: stream_cancellation,
    }))
}

struct LocalSearchStream {
    receiver: Receiver<SearchBatch>,
    cancellation: CancellationToken,
}

impl Drop for LocalSearchStream {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

impl SearchStream for LocalSearchStream {
    fn next_batch<'a>(
        &'a mut self,
        cancellation: CancellationToken,
    ) -> BoxFuture<'a, Result<Option<SearchBatch>, StoreError>> {
        Box::pin(async move {
            cancellation.check()?;
            let receive = async {
                match self.receiver.recv().await {
                    Ok(batch) => Ok(Some(batch)),
                    Err(_) => Ok(None),
                }
            };
            let cancelled = async {
                wait_for_cancellation(&cancellation).await;
                Err(StoreError::Cancelled)
            };
            future::race(receive, cancelled).await
        })
    }
}

struct SearchProducer<'a> {
    provider: ProviderId,
    root: &'a Path,
    query: &'a SearchQuery,
    cancellation: &'a CancellationToken,
    sender: &'a Sender<SearchBatch>,
    results: Vec<SearchResult>,
    errors: Vec<SearchScopeError>,
    total: usize,
}

impl SearchProducer<'_> {
    fn run(mut self) {
        let include_hidden = self.query.include_hidden();
        let walker = WalkDir::new(self.root)
            .follow_links(self.query.follow_links())
            .into_iter()
            .filter_entry(move |entry| include_hidden || !is_hidden(entry));
        for entry in walker {
            if self.cancellation.is_cancelled() {
                return;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    let path = error.path().unwrap_or(self.root);
                    self.record_error(path, error.to_string());
                    if !self.flush_if_needed() {
                        return;
                    }
                    continue;
                }
            };
            if !self.process_entry(&entry) {
                return;
            }
        }
        let _ = self.flush(SearchCompletion::Complete);
    }

    fn process_entry(&mut self, entry: &DirEntry) -> bool {
        if entry.depth() == 0 || !self.entry_matches(entry) {
            return true;
        }
        match item_from_path(&self.provider, entry.path()) {
            Ok(item) => {
                self.results
                    .push(SearchResult::new(item, Some(mime_for(entry.path()))));
                self.total += 1;
                if self.total == SEARCH_RESULT_LIMIT {
                    let _ = self.flush(SearchCompletion::RefineRequired);
                    return false;
                }
            }
            Err(error) => self.record_error(entry.path(), error.to_string()),
        }
        self.flush_if_needed()
    }

    fn entry_matches(&mut self, entry: &DirEntry) -> bool {
        if !matches_name(self.query, entry) || !matches_mime(self.query, entry.path()) {
            return false;
        }
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                self.record_error(entry.path(), error.to_string());
                return false;
            }
        };
        if !matches_metadata(self.query, &metadata) {
            return false;
        }
        if self.query.content_terms().is_empty() {
            return true;
        }
        if !metadata.is_file() {
            return false;
        }
        match file_contains_all(entry.path(), self.query.content_terms(), self.cancellation) {
            Ok(matches) => matches,
            Err(error) => {
                self.record_error(entry.path(), error.to_string());
                false
            }
        }
    }

    fn record_error(&mut self, path: &Path, message: String) {
        self.errors.push(SearchScopeError::new(
            StorePath::from_unix_path(path.as_os_str().to_os_string()),
            message,
            true,
        ));
    }

    fn flush_if_needed(&mut self) -> bool {
        if self.results.len() < SEARCH_BATCH_RESULTS && self.errors.len() < SEARCH_BATCH_ERRORS {
            return true;
        }
        self.flush(SearchCompletion::Running)
    }

    fn flush(&mut self, completion: SearchCompletion) -> bool {
        let batch = SearchBatch::new(
            std::mem::take(&mut self.results),
            std::mem::take(&mut self.errors),
            completion,
        )
        .expect("the producer enforces the search batch limit");
        let send = async { self.sender.send(batch).await.is_ok() };
        let cancelled = async {
            wait_for_cancellation(self.cancellation).await;
            false
        };
        future::block_on(future::race(send, cancelled))
    }
}

fn matches_name(query: &SearchQuery, entry: &DirEntry) -> bool {
    let name = entry.file_name().to_string_lossy().to_lowercase();
    query
        .name_terms()
        .iter()
        .all(|term| name.contains(&term.to_lowercase()))
        && query.glob().is_none_or(|pattern| {
            glob_matches(pattern.as_bytes(), entry.file_name().as_encoded_bytes())
        })
}

fn matches_mime(query: &SearchQuery, path: &Path) -> bool {
    query.mime().is_none_or(|expected| {
        let mime = mime_for(path);
        expected == mime
            || (!expected.contains('/')
                && mime.starts_with(expected)
                && mime.as_bytes().get(expected.len()).copied() == Some(b'/'))
    })
}

fn matches_metadata(query: &SearchQuery, metadata: &Metadata) -> bool {
    let kind = if metadata.is_dir() {
        musheen_core::ItemKind::Directory
    } else if metadata.is_file() {
        musheen_core::ItemKind::RegularFile
    } else if metadata.file_type().is_symlink() {
        musheen_core::ItemKind::SymbolicLink
    } else {
        musheen_core::ItemKind::Other
    };
    if query.kind().is_some_and(|expected| expected != kind)
        || query
            .size()
            .is_some_and(|range| !range.matches(&metadata.len()))
    {
        return false;
    }
    query.modified().is_none_or(|range| {
        metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|value| i64::try_from(value.as_secs()).ok())
            .is_some_and(|seconds| range.matches(&seconds))
    })
}

async fn wait_for_cancellation(cancellation: &CancellationToken) {
    poll_fn(|context| {
        if cancellation.is_cancelled() {
            Poll::Ready(())
        } else {
            cancellation.register_waker(context.waker());
            Poll::Pending
        }
    })
    .await;
}

fn is_hidden(entry: &DirEntry) -> bool {
    entry.depth() > 0 && entry.file_name().as_encoded_bytes().starts_with(b".")
}

fn file_contains_all(
    path: &Path,
    terms: &[Box<str>],
    cancellation: &CancellationToken,
) -> Result<bool, std::io::Error> {
    let needles = terms
        .iter()
        .map(|term| term.to_ascii_lowercase().into_bytes())
        .collect::<Vec<_>>();
    let overlap = needles
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(1)
        .saturating_sub(1);
    let mut found = vec![false; needles.len()];
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; CONTENT_CHUNK_BYTES];
    let mut tail = Vec::new();
    loop {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let mut haystack = std::mem::take(&mut tail);
        haystack.extend(buffer[..read].iter().map(u8::to_ascii_lowercase));
        for (index, needle) in needles.iter().enumerate() {
            found[index] |= haystack
                .windows(needle.len())
                .any(|window| window == needle);
        }
        if found.iter().all(|found| *found) {
            return Ok(true);
        }
        let keep = overlap.min(haystack.len());
        tail.extend_from_slice(&haystack[haystack.len() - keep..]);
    }
    Ok(found.iter().all(|found| *found))
}

fn mime_for(path: &Path) -> &'static str {
    if path.is_dir() {
        return "inode/directory";
    }
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("txt" | "md" | "log") => "text/plain",
        Some("rs") => "text/rust",
        Some("toml") => "application/toml",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    let (mut pattern_index, mut value_index) = (0, 0);
    let (mut star, mut star_value) = (None, 0);
    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star = Some(pattern_index);
            pattern_index += 1;
            star_value = value_index;
        } else if let Some(star_index) = star {
            pattern_index = star_index + 1;
            star_value += 1;
            value_index = star_value;
        } else {
            return false;
        }
    }
    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}
