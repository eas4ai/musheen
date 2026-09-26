mod support;

use futures_lite::future::block_on;
use musheen_core::{ResourceLimits, Store, StorePath};
use musheen_test_support::{MillionItemFixture, RecordingStore};
use musheen_ui::{ApplyPageResult, DirectoryLoad, DirectoryModel};
use serde_json::json;
use support::Sample;

const ITEM_COUNT: usize = 1_000_000;
const TEMP_PREFIX: &str = "musheen-directory-";
type BenchResult<T> = Result<T, Box<dyn std::error::Error>>;

struct DirectoryBenchmark {
    limits: ResourceLimits,
    store: RecordingStore,
    model: DirectoryModel,
    load: DirectoryLoad,
}

impl DirectoryBenchmark {
    fn new() -> BenchResult<Self> {
        let limits = ResourceLimits::default();
        let fixture = MillionItemFixture::new(ITEM_COUNT)?.with_delay_yields(0);
        let store = RecordingStore::read_only(fixture);
        let mut model = DirectoryModel::new(limits.clone());
        let load = model.begin_navigation(StorePath::from_unix_path("/fixture"));
        Ok(Self {
            limits,
            store,
            model,
            load,
        })
    }

    fn load_next_page(&mut self) -> BenchResult<bool> {
        let Some((_, request)) = self.model.begin_page() else {
            return Ok(false);
        };
        assert!(
            self.model.begin_page().is_none(),
            "only one page may be in flight"
        );
        let page = block_on(self.store.read_directory(
            self.load.location(),
            request,
            self.load.cancellation().clone(),
        ))?;
        assert_eq!(
            self.model.apply_page(&self.load, page),
            ApplyPageResult::Applied
        );
        Ok(true)
    }

    fn first_page(&mut self) -> BenchResult<Sample> {
        let before = Sample::capture(TEMP_PREFIX)?;
        assert!(self.load_next_page()?);
        let after = Sample::capture(TEMP_PREFIX)?;
        assert_eq!(self.model.items().len(), self.limits.directory_page_items());
        println!(
            "{}",
            json!({
                "case": "first_directory_page",
                "items": self.model.items().len(),
                "wall_ns": after.captured.duration_since(before.captured).as_nanos(),
                "cpu_ns": after.cpu_nanoseconds.saturating_sub(before.cpu_nanoseconds),
                "peak_rss_kib": after.peak_rss_kib,
                "open_fds": after.open_fds,
                "queued_pages_max": 1,
                "retained_models_max": self.model.items().len(),
                "temporary_bytes": after.temporary_bytes,
                "temporary_root": std::env::temp_dir().display().to_string(),
            })
        );
        Ok(after)
    }

    fn enumerate_remaining(&mut self, before: Sample) -> BenchResult<usize> {
        let mut pages = 1_usize;
        let mut max_retained = self.model.items().len();
        let mut max_fds = before.open_fds;
        let mut max_temporary_bytes = before.temporary_bytes;
        while self.load_next_page()? {
            pages += 1;
            max_retained = max_retained.max(self.model.items().len());
            assert!(max_retained <= self.limits.directory_retained_items());
            if pages.is_multiple_of(128) {
                let sample = Sample::capture(TEMP_PREFIX)?;
                max_fds = max_fds.max(sample.open_fds);
                max_temporary_bytes = max_temporary_bytes.max(sample.temporary_bytes);
            }
        }
        assert_eq!(self.model.indexed_count(), ITEM_COUNT);
        assert_eq!(self.model.visible_count(), ITEM_COUNT);
        let metrics = self.store.metrics();
        assert_eq!(metrics.generated_items(), ITEM_COUNT);
        assert!(metrics.largest_page() <= self.limits.directory_page_items());
        let after = Sample::capture(TEMP_PREFIX)?;
        max_fds = max_fds.max(after.open_fds);
        max_temporary_bytes = max_temporary_bytes.max(after.temporary_bytes);
        println!(
            "{}",
            json!({
                "case": "million_item_directory_enumeration",
                "items": self.model.indexed_count(),
                "pages": pages,
                "wall_ns": after.captured.duration_since(before.captured).as_nanos(),
                "cpu_ns": after.cpu_nanoseconds.saturating_sub(before.cpu_nanoseconds),
                "peak_rss_kib": after.peak_rss_kib,
                "open_fds_sampled_max": max_fds,
                "queued_pages_max": 1,
                "retained_models_max": max_retained,
                "temporary_bytes_sampled_max": max_temporary_bytes,
            })
        );
        Ok(max_retained)
    }

    fn scroll(&mut self, max_retained: usize) -> BenchResult<()> {
        let before = Sample::capture(TEMP_PREFIX)?;
        let mut viewports = 0_usize;
        for first in (0..ITEM_COUNT).step_by(10_000) {
            let last = (first + 24).min(ITEM_COUNT);
            assert_eq!(
                self.model.indexed_range(first..last)?.unwrap().len(),
                last - first
            );
            viewports += 1;
        }
        assert_eq!(
            self.model
                .indexed_range(ITEM_COUNT - 24..ITEM_COUNT)?
                .unwrap()
                .len(),
            24
        );
        let after = Sample::capture(TEMP_PREFIX)?;
        println!(
            "{}",
            json!({
                "case": "million_item_directory_scroll",
                "items": self.model.visible_count(),
                "viewports": viewports + 1,
                "wall_ns": after.captured.duration_since(before.captured).as_nanos(),
                "cpu_ns": after.cpu_nanoseconds.saturating_sub(before.cpu_nanoseconds),
                "peak_rss_kib": after.peak_rss_kib,
                "open_fds": after.open_fds,
                "queued_pages_max": 0,
                "retained_models_max": max_retained,
                "temporary_bytes": after.temporary_bytes,
            })
        );
        Ok(())
    }
}

fn main() -> BenchResult<()> {
    if !std::env::args().any(|argument| argument == "--bench") {
        eprintln!("directory benchmark skipped; run with cargo bench --bench directory");
        return Ok(());
    }
    let mut benchmark = DirectoryBenchmark::new()?;
    let first_page = benchmark.first_page()?;
    let max_retained = benchmark.enumerate_remaining(first_page)?;
    benchmark.scroll(max_retained)
}
