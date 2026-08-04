# Viewer Rendering Correctness, Recovery, And Numerical Cutover

Last reviewed: 2026-08-04

Status: Owner-authorized. P1 through P6 are implemented. P7 static
performance recovery and P8 product closeout remain open.

## Purpose

This plan owns only the unfinished closeout for the current viewer correction.
[Current state](../../CURRENT_STATE.md) owns implemented product facts.
[Testing](../../TESTING.md) owns current commands, thresholds, and evidence
language.

The correction must preserve scientific output while it closes two outcomes:

1. measure and, when evidence selects a safe change, recover static rendering
   performance; and
2. validate the corrected renderer in the clean trusted lane and normal mapped
   product, including a suitable temporal workload.

## Implemented Boundary

P1 through P6 made one hard cut. The current implementation has:

- complete logical transaction membership and renderer-owned reuse proof;
- one render-attempt, failure, wake, and composition authority;
- typed retryable and terminal outcomes that retain a valid predecessor;
- local color, Pick, hidden-worker, and device failure scopes;
- operation-scoped identity exhaustion;
- one binary32 affine, coordinate, camera, work, and page-traversal contract;
- shared page traversal for all volume modes and Pick; and
- explicit empty-surface publication that retires a stale renderer front
  before empty-union preflight.

The focused CPU checks and trusted GPU regressions for this boundary exist.
The mapped render-mode and native-navigation scenarios pass as automated
evidence. These results are not a performance-recovery claim or owner product
acceptance.

The pre-edit P0 campaign was not captured. Do not reconstruct it from later
runs or describe it as passed. The missing evidence remains a stated
limitation.

## P7: Matching-Work Static Performance Recovery

### Outcome

Close the static performance gap with comparable evidence, or leave it open
with the first measured blocker and no unsupported improvement claim.

### Comparison Set

Use three named revisions:

- A: 24f7da1531056c950cb5479098bd723c5fd8dc91;
- B: d5032c43525ddfa9d524d490e3391aa800c6d470; and
- C: the clean correction candidate.

A is eligible only when it can express the same scientific work as C. B
remains the required pre-correction comparison. If neither A nor B can
express a required case, that row is unevaluated and cannot support an
improvement claim.

### Evidence Overlay

Use one identical opt-in evidence overlay for A, B, and C. Record the source
revision, overlay patch or tree identity, and measured tree identity.

The overlay may add counters, timestamps, supported GPU queries, report
serialization, and harness commands. It must not change:

- request or publication order;
- branch decisions;
- shaders or control bytes;
- allocations, residency, LOD, or sampling;
- queue submission;
- output pixels; or
- default release behavior.

Prove overlay-disabled equivalence on B before admitting timing samples. If an
observation point cannot be applied consistently to all compared revisions,
exclude that metric. Do not substitute a proxy.

### Admission

Each compared worktree must be clean and use the same:

- pinned toolchain and release profile;
- Vulkan adapter, driver session, display, power, and thermal policy;
- physical viewport and renderer settings;
- package, visible layers, transfer settings, mode, and sampling;
- camera and input script; and
- requested, selected, navigation, and displayed per-layer maps.

Reject a run when work, pixels, quality, cache condition, or environment
differs. A different scientific workload is not a performance comparison.
Keep private paths, labels, geometry, and scientific identities outside the
repository.

### Workloads

Run all three controlled workloads:

1. The fixed-LOD 1920x1080 matrix for 1, 2, 4, and 8 co-registered Float32
   64x64x64 channels, both sampling modes, and the applicable MIP, DVR, ISO,
   and Mixed paths.
2. Three warm-resident 30-second static-navigation sessions per revision with
   matched input and map sequences.
3. Three fresh-process, warm-OS-cache settlements per revision for the same
   target body.

Before warm measurement, prove that every body used by the navigation
sequence is resident. During measurement, require zero physical reads,
decodes, dataset requests, uploads, and evictions. Settled idle must submit no
renderer work or immediate repaint.

Retain raw samples and the exact percentile rank in private evidence. Publish
only sanitized workload, environment, metric, and result facts.

### Attribution Order

Select the first measured boundary in this order:

1. A fixed-LOD GPU p95 regression with matching pixels and work selects the
   affected shared or kernel renderer path.
2. Unexpected warm data work selects reuse, residency, or currentness.
3. More than one direct color submission, excess hidden work, or settled-idle
   work selects attempt or presentation scheduling.
4. A semantic-demand or render-plan CPU regression greater than both 10
   percent and 0.5 ms selects the first measured planner stage that crosses
   the limit.
5. A measured egui or texture-composition delta selects the native UI handoff.
6. If no boundary qualifies, do not optimize. Collect a narrower
   owner-approved trace or leave P7 open.

Change one selected boundary at a time. After a retained correction passes its
component gate, rerun all three workloads. Delete a candidate that misses its
component gate or adds unmeasured complexity.

### Acceptance

For matching work on the designated workstation:

- fixed-LOD GPU p95 is at most 1.05 times the better valid baseline;
- warm data work remains exactly zero;
- settled idle submits no renderer work;
- one accepted direct color cutoff uses at most one color submission;
- warm UI-update p95 and exact settlement are at most the greater of 1.10
  times the better valid baseline or that baseline plus 1 ms; and
- independent facts confirm exact pixels, coverage, validity, per-layer maps,
  and currentness.

Correctness gates remain in force when a performance row fails or is
unevaluated. P7 stays open if no valid comparison can close a required row.

## P8: Integrated Product Closeout

### Temporal Workload

Select a time-series package whose second timepoint produces independently
verifiable changed pixels under the retained transfer window. The bundled
fixture clips both timepoints to the same RGB result and is not qualifying
evidence.

Record only a sanitized workload profile. Do not add private package paths,
labels, geometry, or scientific metadata to the repository.

### Required Checks

Run the checks that [Testing](../../TESTING.md) assigns to the changed
boundaries, including:

- focused render API, reference renderer, WGPU renderer, application, app, and
  UI tests;
- the clean trusted GPU correctness lane;
- public verification and documentation checks;
- target fixture render modes;
- representative native navigation;
- representative temporal playback with the suitable package; and
- the P7 static-performance campaign.

Do not run the quarantined linked-S0 host-stress workflow.

### Product Acceptance

Use the normal release application on the real mapped display. Confirm:

- no blank, partial, stale, silently coarser, mixed-timepoint, or wrong-body
  transaction;
- no repeated identical renderer error or immediate repaint loop;
- no product error for retryable backpressure;
- retained color operation after a local Pick failure;
- correct numerical and page-crossing pixels and picks;
- exact/current settlement with zero settled-idle submissions;
- correct empty-surface hide, idle, and restore behavior; and
- direct owner acceptance of the visible result.

Automation is supporting evidence. It does not replace owner observation.

## Invariants

This plan does not authorize:

- weaker correctness, capture, or performance gates;
- a GPU budget increase;
- reduced-resolution output or silent LOD substitution;
- a CPU or alternate renderer;
- a storage, shard, codec, chunk-shape, or TIFF change;
- a new device-recovery path;
- removal or approximation of smooth-linear sampling;
- a new rendering mode, transfer function, segmentation, or analysis;
- 4K or non-Linux qualification; or
- a monitor-visibility claim based only on internal publication or X11 client
  pixels.

Actual destructive device-loss or out-of-memory injection still needs
separate owner authorization.

## Completion

Delete this plan after all of these conditions hold:

1. P7 passes every required comparable row with matching scientific work.
2. P8 passes the clean trusted and normal mapped-product checks on the final
   revision.
3. A suitable temporal workflow proves the changed-pixel and transaction
   boundaries.
4. The owner accepts the normal visible product.
5. Current facts and remaining limits are accurate in the authority docs.
6. No predecessor behavior or parallel authority remains.

If a valid baseline cannot be established or no safe correction meets P7,
keep the plan and state the exact open result. Do not weaken a gate to close
the document.
