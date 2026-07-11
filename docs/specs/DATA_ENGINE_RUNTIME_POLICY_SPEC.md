# Data Engine Runtime Policy Specification

Status: ACCEPTED
Last updated: 2026-06-17

## Purpose

Define the concrete runtime policy for loading, caching, prefetching, cancellation, and progressive data availability.

## Scope

This spec covers:

- worker model
- request lifecycle
- cache policy
- budget defaults
- prefetch priority
- playback behavior
- loading state semantics
- memory mapping policy

## Non-Goals

- Renderer GPU residency internals.
- Preprocessing implementation.
- User-facing CLI or headless modes.
- Separate laptop/workstation correctness paths.

## Worker Model

The initial data engine should use dedicated bounded worker threads and message queues.

Do not adopt a general async runtime such as Tokio in the first data-engine milestone unless a concrete dependency review proves it is necessary.

Initial worker classes:

- I/O workers: blocking filesystem reads and Zarr store access
- decode workers: decompression, dtype decode, checksum verification when requested
- coordination thread/task: prioritization, cancellation, cache admission, diagnostics

The exact queue implementation can start with standard-library channels. Add a stronger queue dependency only when the implementation needs it.

## Request Lifecycle

Every read request carries:

- dataset generation ID
- dataset ID/fingerprint
- layer ID
- timepoint
- scale level
- chunk/brick coordinate
- requested representation
- priority class
- cancellation token or generation guard

Lifecycle states:

- `NotRequested`
- `Queued`
- `Reading`
- `Decoding`
- `ResidentCpu`
- `QueuedForUpload`
- `Failed`
- `Cancelled`
- `Evicted`

Renderer-side GPU residency is represented separately by renderer diagnostics.

Stale requests from older datasets, older sessions, or older camera generations must be discarded before mutating visible state.

## Cache Policy

Use one canonical data path for all dataset sizes.

CPU decoded brick cache:

- policy: weighted LRU with explicit pins for the current frame working set
- weight: decoded bytes plus fixed metadata overhead
- cache keys include dataset identity, layer, timepoint, scale, chunk coordinate, dtype/representation, and transform identity
- failed reads are not cached as empty data
- cancelled reads do not poison cache state

Manifest and index cache:

- kept for the open dataset
- invalidated when dataset identity changes

Accepted sharded-storage update:

- production dense Zarr shard indexes are runtime index data and must be cached
  by the data engine, not reparsed for every neighboring logical brick read
- diagnostics must distinguish logical decoded bytes from encoded shard bytes
  touched
- cache keys and byte accounting must distinguish `uint8`, `uint16`, and
  `float32` decoded resident payloads

Upload staging:

- bounded queue owned by the data/renderer handoff layer
- backpressure stops new low-priority prefetch before it blocks current-view work

Small datasets become fully resident only because the same cache policy admits all needed chunks.

## Budget Defaults

Budgets are policy, not separate implementations.

Default CPU decoded cache budget:

- if system RAM is known: `min(max(512 MiB, 20% of RAM), 32 GiB)`
- if system RAM is unknown: `2 GiB`

Default upload staging budget:

- `min(1 GiB, 25% of CPU decoded cache budget)`

Default in-flight limits:

- read requests: `min(32, 2 * logical_cpu_count)`
- decode requests: `max(1, logical_cpu_count - 2)`
- in-flight decoded bytes: `min(2 GiB, 25% of CPU decoded cache budget)`

If the minimum readable unit exceeds the configured budget, the app should report that the budget is too small to make progress instead of thrashing indefinitely.

Budgets are user-adjustable through desktop app settings.

Current implementation:

- `DataRuntimeConfig::default()` uses the unknown-RAM policy for the decoded brick cache: `2 GiB`.
- The separate whole-volume convenience cache defaults to `512 MiB`.
- Upload staging budget is derived as `min(1 GiB, 25% of decoded brick cache budget)`, currently `512 MiB` by default.
- In-flight decoded byte budget is derived as `min(2 GiB, 25% of decoded brick cache budget)`, currently `512 MiB` by default.
- Custom runtime configs can be supplied when opening a dataset through the data-engine API.
- `mirante4d-app` persists runtime budget preferences and applies them when opening datasets.
- On first launch without a preferences file, `mirante4d-app` uses system-RAM detection where available to seed the decoded brick cache budget from the documented system-RAM policy.
- The app reports configured cache/staging/in-flight budgets in Runtime Diagnostics.
- The app reports brick worker count and queue capacity in Runtime Diagnostics.

## Prefetch Priority

Priority order:

1. visible layers, active timepoint, current view, current scale needed for first frame
2. coarser scale chunks that improve time-to-first-frame
3. visible layers, active timepoint, refinement chunks for the current view
4. next timepoints in playback direction
5. neighboring chunks likely to enter view during camera motion
6. low-priority warm cache for recently viewed timepoints

Inactive or hidden layers should not be decoded or uploaded unless a future operation explicitly requests them.

When playing a time series, prefetch should focus on the next `2` timepoints first and expand only if cache and I/O budgets allow.

## Progressive Loading Semantics

The viewer must represent incomplete residency honestly.

Allowed visual/data states:

- loading coarse approximation
- loading refinement
- incomplete current view
- failed chunk/payload
- exact resident data

Not allowed:

- rendering missing occupied chunks as empty data
- reporting final analysis over incomplete renderer residency
- silently changing render mode to hide missing data

## Cancellation Policy

Cancellation is normal control flow, not an error.

Cancellation happens when:

- the user closes a dataset
- the user opens another dataset
- the camera/view changes enough to obsolete low-priority prefetch
- the user cancels a long operation
- budgets require dropping queued work

Cancelled work may finish internally, but it must not publish stale data if its generation token is obsolete.

## Memory Mapping Policy

Memory mapping is not a first implementation default.

Rationale:

- compressed/sharded Zarr data usually needs decode work anyway
- OS filesystem cache already provides useful read caching
- memory maps add platform-specific failure modes

Memory mapping may be added behind the same data-engine API only after benchmarks show a meaningful benefit.

## Diagnostics

Data-engine diagnostics should expose:

- CPU cache budget and used bytes
- decoded brick count
- cache hit/miss counts
- queued/read/decode request counts
- bytes read from disk
- bytes decoded
- cancellation counts
- failed payload counts
- current prefetch policy summary

Current implementation exposes:

- volume cache budget and used bytes
- decoded brick cache budget and used bytes
- upload staging byte budget
- in-flight decoded byte budget
- volume and brick cache hit/miss counters
- brick read/request queued/completed/cancelled/stale/failed counters
- encoded payload bytes requested for cache-missing reads, derived from required native brick `payload_bytes` records
- decoded `u16` bytes and decoded brick `u16` bytes
- current prefetch, warm-cache, and visible stream summaries through the app runtime diagnostics panel
- brick worker count and queue capacity through the app runtime diagnostics panel
- combined read/decode brick queue depth by priority class through the app runtime diagnostics panel
- validated per-brick occupancy/min/max/payload metadata without decoding payloads
- current-frame scheduling that avoids worker reads for metadata-empty visible bricks by materializing resident zero bricks
- prefetch and warm-cache scheduling that skips metadata-empty bricks before queue submission

Physical OS disk bytes are not currently observable through the Zarr store abstraction and OS filesystem cache. Runtime diagnostics therefore report encoded payload bytes requested from the native package, not guaranteed physical device reads.

The current worker implementation uses one bounded brick read/decode queue. Do not report a separate decode queue until the runtime actually splits read and decode scheduling.

## Invariants

- One data path serves tiny and huge datasets.
- Cache policy never changes correctness semantics.
- Missing data is not empty data.
- Metadata-empty data is empty data only when the validated native brick record says `occupied=false`.
- Cancellation is not an error.
- Stale tasks cannot mutate current session state.
- Inactive channels do not consume read/decode/upload work by default.

## Failure Modes

- budget too small for minimum chunk
- filesystem permission error
- corrupt payload
- unsupported codec
- request starvation under camera motion
- stale request publishes into a newer session
- cache thrash due to bad priority policy

## Testing Requirements

- deterministic cache eviction tests
- cache key identity tests
- cancellation/stale-generation tests
- prefetch ordering tests
- budget-too-small tests
- inactive-channel no-read tests
- playback prefetch tests
- corrupt payload failure-state tests
- small fully resident and large streaming same-path tests

## Open Questions

- Exact queue crate only if the runtime later splits read/decode scheduling or needs stronger priority/cancellation behavior than the current bounded worker queue.
