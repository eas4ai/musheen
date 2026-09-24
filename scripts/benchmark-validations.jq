def nonnegative($value): ($value | type) == "number" and $value >= 0;
def measurements:
  nonnegative(.wall_ns)
  and nonnegative(.cpu_ns)
  and (.peak_rss_kib | type) == "number" and .peak_rss_kib > 0
  and nonnegative(.open_fds // .open_fds_sampled_max)
  and nonnegative(.temporary_bytes // .temporary_bytes_sampled_max);
def cases($names): (map(.case) | sort) == $names;

def directory:
  length == 3
  and cases(["first_directory_page", "million_item_directory_enumeration", "million_item_directory_scroll"])
  and all(.[]; measurements and nonnegative(.queued_pages_max)
    and .queued_pages_max <= 2 and nonnegative(.retained_models_max)
    and .retained_models_max <= 4096)
  and any(.[]; .case == "first_directory_page" and .items == 512
    and nonnegative(.temporary_bytes))
  and any(.[]; .case == "million_item_directory_enumeration"
    and .items == 1000000 and .pages == 1954
    and nonnegative(.temporary_bytes_sampled_max))
  and any(.[]; .case == "million_item_directory_scroll"
    and .items == 1000000 and .viewports >= 100
    and nonnegative(.temporary_bytes));

def search:
  length == 2
  and cases(["million_result_search_refinement", "search_backpressure_saturation"])
  and all(.[]; measurements and nonnegative(.queued_matches_max)
    and .queued_matches_max <= 2048 and nonnegative(.retained_models_max)
    and .retained_models_max <= 4096)
  and any(.[]; .case == "search_backpressure_saturation"
    and .potential_matches == 1000000
    and .queued_matches_max == 2048
    and .producer_inflight_matches_max == 256
    and .produced_while_stalled >= 2048
    and .produced_while_stalled <= 2304
    and .retained_models_max == 0)
  and any(.[]; .case == "million_result_search_refinement"
    and .potential_matches == 1000000
    and .displayed_matches == 100000
    and .produced_matches >= 100000
    and .produced_matches <= 102400
    and .accepted_batches == 391
    and .queued_matches_max == 2048
    and .retained_models_max == 4096
    and .state == "refine_required");

def operations:
  length == 2
  and cases(["large_copy_streamed", "large_copy_transaction"])
  and all(.[]; measurements and .bytes == 67108864 and .verified == true
    and .queued_work_max == 0 and .retained_models_max == 0
    and .temporary_bytes <= 134217728)
  and any(.[]; .case == "large_copy_streamed" and .strategy == "streamed")
  and any(.[]; .case == "large_copy_transaction"
    and (.strategy == "reflink" or .strategy == "sparse" or .strategy == "streamed"));

def thumbnail:
  length == 2
  and cases(["thumbnail_oversized_header_rejected", "thumbnail_worker_decode"])
  and all(.[]; measurements and .queued_work_max == 0
    and .retained_models_max == 0 and .temporary_bytes <= 67108864)
  and any(.[]; .case == "thumbnail_worker_decode"
    and .decoded_pixels == 4194304 and .cache_hit == true
    and (.worker_peak_rss_kib_sampled_max | type) == "number"
    and .worker_peak_rss_kib_sampled_max > 0)
  and any(.[]; .case == "thumbnail_oversized_header_rejected"
    and .pixels == 60000000 and .failure_record == true
    and .pool_workers_max >= 1 and .pool_workers_max <= 4);

def archive:
  length == 3
  and cases(["archive_compression_ratio_rejected", "archive_expanded_bytes_rejected", "archive_nesting_rejected"])
  and all(.[]; measurements and .rejected == true
    and .queued_work_max == 0 and .retained_models_max == 0
    and .temporary_bytes <= 67108864)
  and any(.[]; .case == "archive_expanded_bytes_rejected"
    and .resource == "expanded bytes")
  and any(.[]; .case == "archive_compression_ratio_rejected"
    and .resource == "compression ratio")
  and any(.[]; .case == "archive_nesting_rejected"
    and .resource == "archive nesting");

def terminal:
  length == 2
  and cases(["terminal_flood_backpressure", "terminal_million_line_scrollback"])
  and all(.[]; measurements and nonnegative(.queued_work_max)
    and .queued_work_max <= 64 and nonnegative(.retained_models_max)
    and .retained_models_max <= 10000 and .temporary_bytes == 0)
  and any(.[]; .case == "terminal_flood_backpressure"
    and .bytes == 4194304 and .received_bytes == .bytes
    and .queue_capacity == 64 and .queued_work_max == 64)
  and any(.[]; .case == "terminal_million_line_scrollback"
    and .lines == 1000000 and .scrollback_lines == 10000
    and .scrollback_bytes <= 67108864 and .retained_models_max == 10000);

if $benchmark == "directory" then directory
elif $benchmark == "search" then search
elif $benchmark == "operations" then operations
elif $benchmark == "thumbnail" then thumbnail
elif $benchmark == "archive" then archive
elif $benchmark == "terminal" then terminal
else false end
