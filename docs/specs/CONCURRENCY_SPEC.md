# Concurrency Specification

Status: DRAFT
Last updated: 2026-06-10

## Purpose

Define broad concurrency, scheduling, cancellation, and progress-reporting policy.

## Scope

This spec covers UI thread rules, CPU worker pools, async I/O, preprocessing jobs, renderer upload scheduling, cancellation, and progress reporting.

## Non-Goals

- Choosing exact crates before implementation.
- Blocking the UI thread during long work.
- Creating unbounded task queues.

## Requirements

- UI thread must remain responsive.
- Long-running work must be cancellable.
- Preprocessing jobs must report progress by stage.
- I/O concurrency must be bounded.
- CPU compute concurrency must be bounded.
- GPU upload work must be scheduled explicitly.
- Cancellation must leave caches and outputs in valid states.
- Shared mutable state must be minimized and owned clearly.

## Concurrency Domains

- UI/event loop.
- Renderer frame loop.
- Data-engine read/decode tasks.
- Preprocessing CPU tasks.
- GPU upload staging.
- Benchmark/stress harnesses.

## Cancellation Policy

Cancellation should be explicit and propagated through:

- preprocessing stages
- chunk/shard reads
- decompression
- cache insertion
- GPU upload scheduling
- e2e workflow operations

Cancellation is not an error. It should not poison cache state.

## Invariants

- No unbounded background queues.
- No blocking file I/O on the UI thread.
- No GPU resource mutation from uncontrolled threads.
- No stale task may update current viewer state without generation/session checks.
- Progress reporting must not require polling huge shared structures.

## Failure Modes

- race on timepoint change
- cancelled task writes stale cache entry
- preprocessing cancellation leaves valid-looking partial output
- GPU upload for old dataset reaches current renderer
- worker panic kills runtime silently

## Testing Requirements

- Cancellation propagation tests.
- Bounded-concurrency tests.
- Stale-generation tests.
- Cache consistency under cancellation.
- Repeated open/close stress tests.

## Open Questions

- Whether to use `rayon` for CPU work.
- Exact cancellation token type.
- How renderer commands cross thread boundaries.

The initial data-engine worker/runtime policy is defined in `DATA_ENGINE_RUNTIME_POLICY_SPEC.md`.
