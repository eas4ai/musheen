use crate::support::Sample;
use serde_json::{Value, json};

pub fn record(case: &str, before: &Sample, after: &Sample, details: Value) {
    let mut result = json!({
        "case": case,
        "wall_ns": after.captured.duration_since(before.captured).as_nanos(),
        "cpu_ns": after.cpu_nanoseconds.saturating_sub(before.cpu_nanoseconds),
        "peak_rss_kib": after.peak_rss_kib,
        "open_fds": after.open_fds,
        "temporary_bytes": after.temporary_bytes,
    });
    result
        .as_object_mut()
        .expect("benchmark record is an object")
        .extend(
            details
                .as_object()
                .expect("benchmark details are an object")
                .clone(),
        );
    println!("{result}");
}
