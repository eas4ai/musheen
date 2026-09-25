//! Small safe facade over zstd's pure workspace-sizing functions.

/// Returns the documented worst-case workspace for a single-threaded compression stream.
#[must_use]
pub fn compression_stream_bytes(level: i32) -> Option<usize> {
    // SAFETY: ZSTD_estimateCStreamSize is a pure sizing call with no pointer arguments.
    let result = unsafe { zstd_safe::zstd_sys::ZSTD_estimateCStreamSize(level) };
    valid_size(result)
}

/// Returns the documented worst-case workspace for a decompression stream with this window cap.
#[must_use]
pub fn decompression_stream_bytes(maximum_window_bytes: usize) -> Option<usize> {
    // SAFETY: ZSTD_estimateDStreamSize is a pure sizing call with no pointer arguments.
    let result = unsafe { zstd_safe::zstd_sys::ZSTD_estimateDStreamSize(maximum_window_bytes) };
    valid_size(result)
}

/// Returns the documented workspace for the zstd frame described by `frame_header`.
#[must_use]
pub fn decompression_stream_bytes_from_frame(frame_header: &[u8]) -> Option<usize> {
    // SAFETY: the pointer and length describe the live input slice. The zstd sizing function only
    // reads enough bytes to parse the frame header and does not retain the pointer.
    let result = unsafe {
        zstd_safe::zstd_sys::ZSTD_estimateDStreamSize_fromFrame(
            frame_header.as_ptr().cast(),
            frame_header.len(),
        )
    };
    valid_size(result)
}

fn valid_size(result: usize) -> Option<usize> {
    // SAFETY: ZSTD_isError only inspects the numeric result code.
    (unsafe { zstd_safe::zstd_sys::ZSTD_isError(result) } == 0 && result != 0).then_some(result)
}
