# Observability Specification

Status: DRAFT
Last updated: 2026-06-12

## Purpose

Define diagnostics, logging, and telemetry expected inside Mirante4D.

## Scope

Observability covers:

- logs
- hardware diagnostics
- runtime counters
- performance metrics
- preprocessing progress
- benchmark output

## Non-Goals

- Network telemetry by default.
- User tracking.
- Hidden analytics.

## Requirements

- Diagnostics must be local-first.
- GPU adapter/backend/features must be visible.
- Runtime cache and residency metrics must be inspectable.
- Preprocessing stages must report progress and timing.
- Benchmarks must capture hardware and dataset context.
- Logs should support both user bug reports and developer debugging.

Current implementation:

- Startup diagnostics are local-only and carry diagnostics format, app version, target OS, target architecture, target family, and GPU adapter status.
- Runtime diagnostics expose data-engine cache/request counters, worker queue activity, backend, GPU adapter, requested and supported GPU limit envelopes, GPU cache counters, display-resource cache counters, resident display path timing, and scene draw item counts.
- Import progress includes a deterministic storage estimate before payload conversion.
- Benchmark outputs are JSON and include hardware/dataset context for the implemented benchmark gates.

## Metrics

Important runtime metrics:

- frame time
- render pass time where available
- bytes read
- chunks/bricks read
- cache hits/misses
- CPU cache resident bytes
- GPU resident bytes or resource counts where available
- upload bytes/frame
- in-flight reads
- cancelled reads
- preprocessing stage duration

## Invariants

- Performance claims require recorded measurement context.
- Diagnostics must not require opening unsupported datasets.
- Logs must not contain unnecessary user data payloads.

## Failure Modes

- diagnostics unavailable on platform
- GPU timing unsupported
- log write failure
- benchmark fixture missing

## Testing Requirements

- Diagnostics object construction tests.
- Benchmark output schema tests.
- Log redaction/path policy tests if logs include user paths.
- Smoke test for GPU adapter diagnostics.

## Open Questions

- Log file location.
- Whether benchmark outputs also need CSV in addition to the current JSON reports.
- How much path information to include by default.
