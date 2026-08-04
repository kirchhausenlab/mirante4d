# Viewer GPU Testing Refactor

Last reviewed: 2026-08-04

Status: The refactor is implemented. Initial calibration, accepted relative
baseline, final candidate comparison, and owner product evidence remain open.

## Purpose

This plan owns the unfinished evidence lifecycle for GPU performance. The
implemented verification topology is current product infrastructure, not
future design.

[Testing](../../TESTING.md) owns commands, triggers, metrics, thresholds, and
claim language. [Current state](../../CURRENT_STATE.md) owns implemented facts
and limitations.

## Implemented Boundary

The repository now has three separate evidence authorities:

1. trusted GPU correctness;
2. component and product performance; and
3. mapped normal-product validation.

The correctness lane owns exactly 25 registered cases. It discovers,
reconciles, and runs them serially on the designated Vulkan adapter with zero
retries. Its clean corrected execution passes all 25.

The performance command owns three component functions and 39 measurements.
It uses release builds, 30 warmups, 120 measured frames, raw vectors, median,
nearest-rank p95, zero-work prerequisites, and validation status.

The mapped observer correlates:

- final swapchain present IDs;
- Vulkan present completion;
- an independent X11 image marker;
- swapchain generation and surface identity; and
- VK_EXT_present_timing first-pixel-out feedback.

First-pixel-out timing is the authority for exact scanout intervals. Present
wait times retain conservative visibility, visible-gap, input-response, and
settlement bounds. Neither is a photon or input-to-photon measurement.

Portable parser, registry, topology, statistics, matching-work, observer, and
counter checks run in ordinary verification. Hardware executions remain
explicit local work.

The former mixed trusted-GPU command, five-sample timing decision, and
universal 16.667 ms product requirement are gone.

## Current Evidence

- The clean trusted correctness lane passes 25 of 25 cases with zero retries.
- The mapped render-mode and native-navigation scenarios pass as automated
  product evidence.
- A current absolute product smoke passed all eight interaction and
  refinement limits.
- The accepted baseline is still pending initial calibration.
- One clean calibration activation failed the eight-channel voxel-exact ISO
  fixed-LOD case at 58.767 ms p95 against the 33.3 ms component limit.
- That failed activation accepted no calibration run and did not change a
  threshold or baseline.
- Direct owner observation and acceptance remain open.

The smoke result is not an accepted baseline, relative comparison,
performance-improvement claim, or owner product validation.

## Open Outcome 1: Initial Calibration

### Admission

Use the private configuration defined by
verification/gpu-performance-config.schema.json. It must identify clean
baseline and candidate revisions, separate target directories, workload,
adapter, driver, display, power, thermal, compositor, and quiescence facts.
The file remains mode 0600 and outside the public evidence.

Reject:

- a dirty revision;
- GitHub Actions or an unsupported display session;
- a changed adapter, driver, display, power, thermal, or quiescence fact;
- an automatic retry;
- mismatched scientific work, quality, cache state, or presentation identity;
  and
- any invalid or unevaluated required result.

### Component Blocker

Reproduce and localize the eight-channel voxel-exact ISO failure before
accepting calibration runs. Keep the 33.3 ms feasibility limit. Do not discard
the slow valid sample or replace the workload.

Use the static-recovery plan when the same renderer boundary is selected by
its A/B/C matching-work campaign. Do not create a separate speculative product
optimization.

### Ten-Run Calibration

After all absolute prerequisites pass:

1. run ten fresh accepted-baseline sessions under the fixed environment;
2. retain every valid run and record objective invalidations;
3. calculate the median, full range, and maximum proportional deviation for
   each run-level metric;
4. activate the 5 percent component gate only when maximum deviation is at
   most 2.5 percent;
5. activate the 10 percent product gate only when maximum deviation is at
   most 5 percent; and
6. leave an unstable metric inactive until the environment or measurement is
   stabilized.

Do not widen a threshold or drop a slow valid run to make calibration stable.

### Baseline Proposal

Generate one sanitized proposal under verification. It records:

- schema and measurement version;
- accepted source revision;
- public platform, adapter, backend, driver, and display profile;
- opaque workload profile;
- mode, sampling, layout, extent, and cache-condition identities;
- accepted metric values and calibration variation;
- active absolute and relative thresholds;
- date and explicit owner decision; and
- prior baseline identity when one exists.

Do not include private paths, raw samples, host identifiers, or unpublished
scientific facts.

The runner must not accept, replace, or rewrite its own baseline. The owner
must explicitly accept or reject the proposal.

## Open Outcome 2: Candidate Comparison

After an accepted baseline exists, compare it with the clean candidate in
fixed A/B, B/A, and A/B pair order.

Every pair must use the same session configuration and must prove:

- matching pixels and scientific work;
- matching quality and per-layer maps;
- matching cache and environment state;
- correct presentation identity;
- all absolute gates; and
- all active relative gates.

Use the median of the three valid pair ratios. Rerun only a complete pair with
an objective recorded invalidation. Never remove one because it is slow.

The current relative gates are:

- component GPU p95 at most 1.05 times the comparable baseline; and
- warm product p95 and refinement latency at most the greater of 1.10 times
  baseline or baseline plus 1 ms.

Correctness and declared zero-work prerequisites require exact agreement. A
candidate over an absolute limit fails even when it improves on the baseline.
A candidate with different work is incomparable, not faster.

## Open Outcome 3: Product And Owner Evidence

Run the representative mapped product on the real display after the final
candidate checks. Use the normal release application and actual input path.

The automated campaign must evaluate every applicable limit in
[Testing](../../TESTING.md), including:

- standalone and four-panel scanout intervals;
- resident input response and maximum visible gap;
- resident exact and prepared nonresident settlement;
- fresh-process coarse visibility and exact settlement;
- interruption, stale-suppression, and idle zero-work facts; and
- presentation identity, correctness, and validation prerequisites.

Report genuinely cold storage separately. It is not a GPU refinement gate.

The owner then observes continuous interaction, exact refinement, retained
front behavior, interruption, both sampling modes, relevant render modes, and
idle quiescence. Record acceptance or rejection without relabelling automated
evidence.

## Invariants

- Correctness, component performance, and product evidence remain separate.
- No required result with invalid or unevaluated status is serialized as pass.
- No automatic retry, flaky-pass policy, or quarantine can create green
  status.
- Internal publication, GPU completion, egui paint, or X11 surface change
  alone is not a presented-product result.
- The linked-S0 host-stress workflow remains quarantined.
- Public workflows gain no self-hosted runner, paid compute, cache, or
  artifact requirement.
- The product renderer, scientific output, LOD, viewport resolution, and
  ordinary WGPU configuration do not change for measurement.
- No CPU renderer, device recovery, storage redesign, or playback-policy
  change is authorized.

## Completion

Delete this plan after:

1. the component blocker is resolved without weakening its limit;
2. ten valid calibration runs establish stable metrics;
3. the owner accepts the sanitized initial baseline;
4. clean baseline/candidate pairs pass matching-work, absolute, and active
   relative gates;
5. the final mapped campaign evaluates and passes every applicable metric;
6. the owner accepts the normal visible product; and
7. current docs and verification metadata agree with the final evidence.

A green correctness lane or absolute smoke alone does not close this plan.
