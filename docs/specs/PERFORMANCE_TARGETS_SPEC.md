# Performance Targets Specification

Status: ACCEPTED
Last updated: 2026-06-18

## Purpose

Define initial performance targets and regression policy for Mirante4D.

These targets are starting engineering goals. They become hard gates only after benchmark hardware, datasets, tooling, and baseline measurements are recorded.

## Scope

This spec covers:

- responsiveness targets
- time-to-first-frame targets
- frame-rate targets
- memory/budget targets
- playback targets
- benchmark regression thresholds

## Non-Goals

- Promising that every dataset performs well on every machine.
- Creating separate laptop/workstation code paths.
- Making unsupported hardware appear supported.
- Claiming performance without benchmark evidence.

## Hardware Classes

Use the hardware classes in `docs/benchmarks/HARDWARE_MATRIX.md`:

- developer laptop
- lab workstation
- high-end workstation
- Apple Silicon Mac

Design should distinguish:

- opens correctly
- remains usable
- performs interactively

## Responsiveness Targets

UI responsiveness:

- normal UI interactions should respond within `100 ms`
- long operations must not block the UI event loop
- progress indicators should update at least every `250 ms` during active work when practical
- cancellation requests should be acknowledged within `500 ms`

## Time-To-First-Frame Targets

Synthetic fixture:

- `basic-u16-16cube.m4d`: first rendered frame within `2 s` in development builds on a normal developer machine

Small native dataset:

- first coarse or exact frame within `5 s`

Large native dataset:

- validation starts immediately
- first coarse visible frame target within `15 s` on lab workstation-class hardware, assuming local SSD storage and valid acceleration metadata

If a target cannot be met because hardware, storage, or dataset size is limiting, the app must show honest progress and diagnostics.

## Frame-Rate Targets

Interactive camera movement:

- preferred target: `60 FPS` where practical for the active working set
- frame time is diagnostic in the current runtime; normal LOD selection is based
  on view geometry, hard feasibility, residency/resource budgets, and
  loadability, not transient FPS samples
- the app must not lower LOD, reduce sampling, or hide the target LOD merely to
  meet a frame-rate target unless a later accepted policy explicitly changes
  the quality/LOD contract
- when the selected target is loading or genuinely blocked by budget/backend
  limits, the UI must report displayed versus target LOD and the typed reason
  truthfully
- UI must remain responsive even when renderer FPS drops

Small fixtures and simple scenes:

- target: monitor refresh rate where practical

Frame-rate targets apply to measured benchmark scenarios, not all possible
datasets. They are used to evaluate renderer efficiency and responsiveness,
not as an implicit runtime permission to sacrifice the current
highest-loadable-quality display policy.

## Playback Targets

Time-series playback should prioritize consistent interaction over fake completeness.

Targets:

- small/medium resident time series: `10 FPS` playback or better
- large streaming time series: adaptive playback with visible loading/incomplete state rather than silently skipping correctness
- playback prefetch starts with the next `2` timepoints and expands only when budgets allow

## Memory And Budget Targets

The app must respect configured budgets.

Targets:

- steady-state CPU decoded cache within configured budget plus `10%` accounting tolerance
- upload staging within configured budget plus `10%`
- renderer GPU residency within configured renderer budget where the backend exposes enough information

If budgets are too small for a minimum work unit, the app reports that directly.

## Preprocessing Targets

Do not define hard throughput targets before first real measurements.

Initial preprocessing benchmarks should record:

- source format
- source size
- output size
- chunk shape
- compression
- multiscale count
- total wall time
- CPU utilization where available
- peak memory
- disk read/write throughput

## Regression Policy

Once baselines exist:

- `>10%` slowdown in a stable benchmark requires explanation
- `>20%` slowdown in a stable benchmark blocks completion unless explicitly accepted
- memory growth `>10%` requires explanation
- correctness regressions always block completion

Benchmarks must record build identity, hardware, backend, dataset, and metric definitions.

## Invariants

- Performance claims require measurements.
- Low-resource and high-resource machines use the same architecture.
- Incomplete data is shown honestly.
- Lower LOD is shown honestly and must include a reason when it is below the target/source scale.
- Full source scale `s0` is preferred whenever it is screen-meaningful and sustainable.
- UI responsiveness is separate from render FPS.
- Benchmarks do not use hidden app-private data paths.

## Failure Modes

- benchmark measures the wrong thing
- benchmark runs on unknown hardware
- app blocks UI during I/O/decode
- frame-rate appears high by rendering incomplete data as empty
- memory budget is ignored
- regression accepted without baseline context

## Testing Requirements

- startup/first-frame benchmark for bootstrap fixture
- renderer frame benchmark for synthetic volumes
- data-engine cache pressure benchmark
- playback benchmark for generated time series
- preprocessing benchmark
- memory budget stress test

## Open Questions

- Official benchmark hardware availability.
- Which `MIRANTE4D_SAMPLE_DATA` experiments become standard local benchmark cases.

Concrete benchmark tooling is defined in `TESTING_TOOLING_SPEC.md`.

Baseline measurements that convert provisional targets into hard regression gates
remain to be recorded for named hardware/dataset combinations.
