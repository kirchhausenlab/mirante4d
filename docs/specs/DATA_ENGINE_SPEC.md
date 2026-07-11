# Data Engine Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the runtime data subsystem that validates native datasets and supplies renderable resources to the viewer.

## Scope

The data engine owns:

- dataset open and validation
- manifest and binary index parsing
- chunk/shard read scheduling
- decompression
- CPU cache budgets
- prefetch policy
- cancellation
- resource diagnostics
- handoff to GPU upload staging

## Non-Goals

- UI controls.
- Direct rendering.
- Raw import/preprocessing.
- Generic old-format compatibility.

## Requirements

- All datasets must be validated before viewer handoff.
- Reads must support datasets larger than memory.
- Chunk/shard reads must be cancellable.
- The engine must expose explicit cache budget configuration.
- Cache eviction must be deterministic enough to test.
- Prefetch must be prioritized by visible layer, active timepoint, playback direction, and camera/view state where available.
- Missing resident data must have honest state, not render as empty data unless the data is truly empty.
- Diagnostics must expose cache size, hit rate, in-flight reads, bytes read, and failure state.
- The same data path must support tiny datasets and very large datasets.
- Resource differences must be represented by budgets and policy, not by separate runtime implementations.
- The data engine must support out-of-core streaming and scale up to full CPU residency when budgets allow.
- Production dense intensity reads must resolve requested logical bricks to
  Zarr shard plus subchunk coordinates, cache shard indexes, and preserve
  `uint8`, `uint16`, and `float32` decoded resident payload identity.

## Canonical Viewer Data Path

The only normal viewer data path is:

```text
native dataset package on disk
  -> validation
  -> dataset handle
  -> bricked/multiscale reads
  -> CPU cache/decode/prefetch
  -> GPU upload handoff
  -> renderer GPU residency
```

There should not be separate product paths for disk streaming, full in-memory viewing, OPFS-style private storage, or workstation-only loading. A small dataset can become fully resident through cache policy, but it still uses the same data engine and resource contracts.

## Resource Model

The engine should expose typed handles for:

- dataset
- channel
- layer
- timepoint
- scale level
- spatial transform / geometry metadata
- dense intensity brick/page table
- track set
- histogram
- acceleration metadata

Renderer-facing handles should not expose raw file paths as the primary API.

## Cache Model

Expected cache tiers:

- manifest/index cache
- CPU decoded brick cache
- decompressed byte cache if useful
- upload staging queue
- renderer-owned GPU residency

The data engine owns CPU residency. The renderer owns GPU residency.

Concrete runtime worker, cache, budget, prefetch, cancellation, and loading-state policy is defined in `DATA_ENGINE_RUNTIME_POLICY_SPEC.md`.

Cache policy should adapt to available resources:

- low-resource machines use bounded caches and conservative prefetch
- high-resource machines may retain all decoded/prepared CPU data that fits
- all machines use the same cache keys, validation, and renderer handoff
- cache policy must never change correctness semantics

## Resource Policy

The runtime should expose or derive budgets for:

- CPU cache bytes
- decoded brick count/bytes
- in-flight read count
- decode/decompression concurrency
- prefetch distance
- GPU upload queue pressure

Renderer-side GPU residency is coordinated through renderer contracts, not owned by the data engine.

## Invariants

- Validation happens before reads are served.
- Cancellation must not poison cache state.
- Failed reads must not be cached as valid empty data.
- Cache keys must include dataset identity, layer, timepoint, scale, and representation.
- Cache/resource metadata must preserve layer spatial transform identity.
- Cache keys for dense intensity must preserve channel identity and stored/decoded representation.
- Data engine APIs must make scale level explicit.
- Data engine APIs must expose grid-to-world/world-to-grid metadata needed by renderer and analysis.
- Data engine APIs must preserve stored scientific values separately from display-normalized values.
- Cache policy must not create a second correctness path.
- Missing occupied data must be represented as loading/incomplete, not transparent or empty.

## Failure Modes

- invalid dataset handle
- read cancellation
- missing shard
- corrupt payload
- decompression failure
- budget too small for minimum read unit
- unsupported codec
- I/O permission failure
- budget too small to make forward progress
- stale task attempts to update a newer dataset/session

## Testing Requirements

- Cache hit/miss tests.
- Cancellation tests.
- Corrupt shard tests.
- Concurrent read dedupe tests.
- Budget enforcement tests.
- Scale-specific cache key tests.
- Dtype/value-mapping metadata handling tests.
- Active-channel subset read tests.
- Spatial transform metadata handoff tests.
- Prefetch ordering tests.
- Same-path tests proving small/full-resident and large/streaming modes use the same API contracts.
- Budget transition tests.

## Open Questions

- How renderer view-state feedback enters prefetch policy.
- Whether memory mapping is useful behind the same data-engine contract.
