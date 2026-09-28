mod support;

use async_channel::{Receiver, Sender};
use musheen_core::{
    DisplayPath, ItemId, ItemKind, ProviderId, SEARCH_BATCH_RESULTS, SEARCH_CHANNEL_RESULTS,
    SEARCH_RESULT_LIMIT, SEARCH_RETAINED_RESULTS, SearchBatch, SearchQuery, SearchResult,
    StoreItem, StorePath,
};
use musheen_ui::search::{SearchResultModel, SearchState};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use support::Sample;

const POTENTIAL_MATCHES: usize = 1_000_000;
const QUEUED_BATCHES: usize = SEARCH_CHANNEL_RESULTS / SEARCH_BATCH_RESULTS;
const MAX_PRODUCED_AFTER_REFINEMENT: usize = SEARCH_RESULT_LIMIT.div_ceil(SEARCH_BATCH_RESULTS)
    * SEARCH_BATCH_RESULTS
    + SEARCH_CHANNEL_RESULTS
    + SEARCH_BATCH_RESULTS;
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

fn make_result(index: usize, provider: &ProviderId) -> SearchResult {
    let name = format!("item-{index}.txt");
    let item = StoreItem::new(
        ItemId::new(provider.clone(), index.to_be_bytes()).expect("unique result ID"),
        StorePath::from_unix_path(format!("/scope/{name}")),
        DisplayPath::new(name.as_str()),
        ItemKind::RegularFile,
        Some(index as u64),
    );
    SearchResult::new(item, Some("text/plain"))
}

fn produce(sender: Sender<SearchBatch>, generated: Arc<AtomicUsize>) {
    let provider = ProviderId::new("local").expect("valid provider ID");
    for start in (0..POTENTIAL_MATCHES).step_by(SEARCH_BATCH_RESULTS) {
        let end = (start + SEARCH_BATCH_RESULTS).min(POTENTIAL_MATCHES);
        let results = (start..end)
            .map(|index| make_result(index, &provider))
            .collect();
        let batch = SearchBatch::running(results, vec![]).expect("bounded search batch");
        generated.fetch_add(end - start, Ordering::SeqCst);
        if sender.send_blocking(batch).is_err() {
            return;
        }
    }
}

fn wait_for_full_queue(receiver: &Receiver<SearchBatch>) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while receiver.len() < QUEUED_BATCHES {
        assert!(
            Instant::now() < deadline,
            "search producer did not fill the queue"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn run() -> BenchResult<()> {
    let (sender, receiver) = async_channel::bounded(QUEUED_BATCHES);
    let generated = Arc::new(AtomicUsize::new(0));
    let before_saturation = Sample::capture("musheen-search-")?;
    let worker_generated = Arc::clone(&generated);
    let worker = thread::spawn(move || produce(sender, worker_generated));
    wait_for_full_queue(&receiver);
    let after_saturation = Sample::capture("musheen-search-")?;
    let produced_while_stalled = generated.load(Ordering::SeqCst);
    assert!(
        produced_while_stalled <= SEARCH_CHANNEL_RESULTS + SEARCH_BATCH_RESULTS,
        "producer exceeded the bounded queue and one in-flight batch"
    );
    println!(
        "{}",
        json!({
            "case": "search_backpressure_saturation",
            "potential_matches": POTENTIAL_MATCHES,
            "queued_matches_max": receiver.len() * SEARCH_BATCH_RESULTS,
            "producer_inflight_matches_max": SEARCH_BATCH_RESULTS,
            "produced_while_stalled": produced_while_stalled,
            "retained_models_max": 0,
            "wall_ns": after_saturation.captured.duration_since(before_saturation.captured).as_nanos(),
            "cpu_ns": after_saturation.cpu_nanoseconds.saturating_sub(before_saturation.cpu_nanoseconds),
            "peak_rss_kib": after_saturation.peak_rss_kib,
            "open_fds": after_saturation.open_fds,
            "temporary_bytes": after_saturation.temporary_bytes,
        })
    );

    let mut model = SearchResultModel::new(
        StorePath::from_unix_path("/scope"),
        SearchQuery::parse("name:item")?,
        SEARCH_RETAINED_RESULTS,
        SEARCH_RESULT_LIMIT,
    );
    let generation = model.begin();
    let before_drain = Sample::capture("musheen-search-")?;
    let mut accepted_batches = 0_usize;
    let mut max_retained = 0_usize;
    let mut max_queued = receiver.len() * SEARCH_BATCH_RESULTS;
    while model.state() == SearchState::Running {
        let batch = receiver.recv_blocking()?;
        assert!(model.apply(generation, batch));
        accepted_batches += 1;
        max_retained = max_retained.max(model.retained_results().len());
        max_queued = max_queued.max(receiver.len() * SEARCH_BATCH_RESULTS);
        assert!(max_retained <= SEARCH_RETAINED_RESULTS);
    }
    let after_drain = Sample::capture("musheen-search-")?;
    drop(receiver);
    worker.join().expect("search producer did not panic");
    let produced_matches = generated.load(Ordering::SeqCst);
    assert_eq!(model.total_results(), SEARCH_RESULT_LIMIT);
    assert_eq!(model.state(), SearchState::RefineRequired);
    assert!(produced_matches <= MAX_PRODUCED_AFTER_REFINEMENT);
    println!(
        "{}",
        json!({
            "case": "million_result_search_refinement",
            "potential_matches": POTENTIAL_MATCHES,
            "displayed_matches": model.total_results(),
            "produced_matches": produced_matches,
            "accepted_batches": accepted_batches,
            "queued_matches_max": max_queued,
            "retained_models_max": max_retained,
            "state": "refine_required",
            "wall_ns": after_drain.captured.duration_since(before_drain.captured).as_nanos(),
            "cpu_ns": after_drain.cpu_nanoseconds.saturating_sub(before_drain.cpu_nanoseconds),
            "peak_rss_kib": after_drain.peak_rss_kib,
            "open_fds": after_drain.open_fds,
            "temporary_bytes": after_drain.temporary_bytes,
        })
    );
    Ok(())
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("search benchmark skipped; run with cargo bench --bench search");
        return Ok(());
    }
    run()
}
