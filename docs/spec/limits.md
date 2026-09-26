Prefix: LIMIT

# Resource limits

These are safe production defaults, not benchmark targets. Settings may
lower them. Advanced settings may raise a limit only within the validated
hard maximum recorded beside that setting; reaching a limit produces a
partial result or actionable error and cleans up app-owned temporary data.

[LIMIT-001] Each operation captures one validated resource-limit snapshot at start, so changing Settings affects new work but never changes an operation mid-run.
Falsifier: one operation observes two values for the same limit.
Mechanism: delayed-operation test that edits every limit while work runs.
Status: Draft

[LIMIT-002] Directory enumeration requests 512 items per page, prefetches at most two pages, retains at most 4,096 item models including selected and edited items, and renders at most three viewport heights of rows or tiles. Stable selected IDs may outlive their resident item models.
Falsifier: a large directory exceeds any default or loses selection when an item model leaves the resident window.
Mechanism: million-item paging test with model and component counters.
Status: Draft

[LIMIT-003] Search transfers batches of at most 256 matches through a 2,048-match channel, retains at most 4,096 off-screen result models, and stops at 100,000 displayed matches with a Refine Search message.
Falsifier: a default search exceeds a queue, model, or result bound or silently truncates results.
Mechanism: fast million-result producer with a stalled UI consumer.
Status: Draft

[LIMIT-004] Text preview initially reads 1 MiB; each explicit Load More reads at most 16 MiB, and the built-in preview refuses content beyond 64 MiB while still offering Open With.
Falsifier: selection alone reads more than 1 MiB or repeated preview loads hold more than 64 MiB.
Mechanism: sparse-file read-count and memory tests.
Status: Draft

[LIMIT-005] One thumbnail task reads at most 64 MiB of source, decodes at most 50 megapixels or 128 MiB, runs for at most 10 seconds, and the default worker pool runs at most four tasks.
Falsifier: a malformed or oversized image exceeds a default without a fail record.
Mechanism: oversized-header, decompression, slow-decoder, memory, and worker-count tests.
Status: Draft

[LIMIT-006] Archive browsing or extraction defaults to 100,000 entries, 20 GiB expanded bytes, 1,000:1 compression ratio, eight nested archives, 4,096 path bytes, 512 MiB memory, and the smaller of 10 GiB or half of currently free temporary space.
Falsifier: one archive exceeds any default without stopping and cleaning staging data.
Mechanism: independent and combined archive-bomb fixtures for every budget.
Status: Draft

[LIMIT-007] Each terminal defaults to 10,000 scrollback lines and 64 MiB of retained terminal state; the lower bound wins and truncation removes oldest complete lines. The PTY output queue holds at most 64 chunks of 32 KiB (2 MiB) and backpressures a child when the UI cannot consume output fast enough.
Falsifier: either bound is exceeded or truncation splits the active line.
Mechanism: long-line, wide-cell, escape-heavy, million-line, and stalled-consumer PTY tests.
Status: Draft

[LIMIT-008] Remote providers default to a 15-second connect timeout, 60-second idle timeout, four concurrent requests per connection, and eight connections per provider.
Falsifier: a default remote request or pool exceeds one limit without a visible timeout or queued state.
Mechanism: stalled DNS, connect, read, write, and saturated-pool tests.
Status: Draft

[LIMIT-009] The default scheduler runs at most two data mutations, four metadata jobs, and four hash or preview jobs concurrently, subject to stricter provider limits.
Falsifier: work starts beyond a default or provider concurrency ceiling.
Mechanism: scheduler tests with recording providers in every work class.
Status: Draft

[LIMIT-010] The status center retains at most 500 finished operation entries, in memory and in the persisted status document, dropping the oldest finished entries first and never a pending, running, paused, or interrupted one. The scheduler drops a job's record once its terminal state has been reported and keeps at most 4,096 recent events.
Falsifier: after more finished jobs than the bound, the status model, the persisted status document, or the scheduler holds more than its bound, or pruning drops an entry that is not finished.
Mechanism: limit-010
Rationale: docs/opus-audit-2.md B-M6: scheduler records and events, the status history, and the persisted status document grew without bound, and the whole model was rewritten on every state change.
Status: Agreed 2026-09-25
