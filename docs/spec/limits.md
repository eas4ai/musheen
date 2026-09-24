Status: Draft
Prefix: LIMIT

# Resource limits

These are safe production defaults, not benchmark targets. Settings may
lower them. Advanced settings may raise a limit only within the validated
hard maximum recorded beside that setting; reaching a limit produces a
partial result or actionable error and cleans up app-owned temporary data.

[LIMIT-001]
Status: Draft
Each operation captures one validated resource-limit snapshot at start, so
changing Settings affects new work but never changes an operation mid-run.
Falsifier: one operation observes two values for the same limit.
Mechanism: delayed-operation test that edits every limit while work runs.

[LIMIT-002]
Status: Draft
Directory enumeration requests 512 items per page, prefetches at most two
pages, retains at most 4,096 item models outside selected items, and renders
at most three viewport heights of rows or tiles.
Falsifier: a large directory exceeds any default without selection or edit
state accounting for the retained item.
Mechanism: million-item paging test with model and component counters.

[LIMIT-003]
Status: Draft
Search transfers batches of at most 256 matches through a 2,048-match
channel, retains at most 4,096 off-screen result models, and stops at
100,000 displayed matches with a Refine Search message.
Falsifier: a default search exceeds a queue, model, or result bound or
silently truncates results.
Mechanism: fast million-result producer with a stalled UI consumer.

[LIMIT-004]
Status: Draft
Text preview initially reads 1 MiB; each explicit Load More reads at most
16 MiB, and the built-in preview refuses content beyond 64 MiB while still
offering Open With.
Falsifier: selection alone reads more than 1 MiB or repeated preview loads
hold more than 64 MiB.
Mechanism: sparse-file read-count and memory tests.

[LIMIT-005]
Status: Draft
One thumbnail task reads at most 64 MiB of source, decodes at most 50
megapixels or 128 MiB, runs for at most 10 seconds, and the default worker
pool runs at most four tasks.
Falsifier: a malformed or oversized image exceeds a default without a fail record.
Mechanism: oversized-header, decompression, slow-decoder, memory, and worker-count tests.

[LIMIT-006]
Status: Draft
Archive browsing or extraction defaults to 100,000 entries, 20 GiB expanded
bytes, 1,000:1 compression ratio, eight nested archives, 4,096 path bytes,
512 MiB memory, and the smaller of 10 GiB or half of currently free temporary space.
Falsifier: one archive exceeds any default without stopping and cleaning staging data.
Mechanism: independent and combined archive-bomb fixtures for every budget.

[LIMIT-007]
Status: Draft
Each terminal defaults to 10,000 scrollback lines and 64 MiB of retained
terminal state; the lower bound wins and truncation removes oldest complete lines.
The PTY output queue holds at most 64 chunks of 32 KiB (2 MiB) and
backpressures a child when the UI cannot consume output fast enough.
Falsifier: either bound is exceeded or truncation splits the active line.
Mechanism: long-line, wide-cell, escape-heavy, million-line, and stalled-consumer
PTY tests.

[LIMIT-008]
Status: Draft
Remote providers default to a 15-second connect timeout, 60-second idle
timeout, four concurrent requests per connection, and eight connections per provider.
Falsifier: a default remote request or pool exceeds one limit without a
visible timeout or queued state.
Mechanism: stalled DNS, connect, read, write, and saturated-pool tests.

[LIMIT-009]
Status: Draft
The default scheduler runs at most two data mutations, four metadata jobs,
and four hash or preview jobs concurrently, subject to stricter provider limits.
Falsifier: work starts beyond a default or provider concurrency ceiling.
Mechanism: scheduler tests with recording providers in every work class.
