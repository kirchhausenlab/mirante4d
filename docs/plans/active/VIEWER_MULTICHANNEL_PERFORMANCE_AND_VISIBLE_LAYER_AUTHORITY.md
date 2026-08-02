# Viewer Multichannel Performance And Visible-Layer Authority Plan

- Status: CLOSED — IMPLEMENTED, AUTOMATED-VERIFIED, OWNER CLOSEOUT ACCEPTED; STATIC PERFORMANCE RECOVERY DEFERRED
- Planning requested by owner: 2026-08-02
- Full implementation authorized by owner: 2026-08-02
- Owner closeout accepted: 2026-08-02
- Planning baseline: `e124a13ecc22a4708a798464b92a4afe9e1bf371`
- Last reviewed: 2026-08-02

This is the authoritative corrective plan for static multichannel volume
rendering and for the structural failure exposed when the durable active layer
is hidden. It reopens a narrowly defined part of the spatial viewer after the
2026-07-31 performance closeout because the owner supplied new product
evidence and explicitly authorized the correction.

This document records target state and implementation decisions. Implemented
facts remain owned by [Current State](../../CURRENT_STATE.md) until a milestone
is completed and verified.

## Playback Boundary

Playback is explicitly excluded. This package will not change playback
retention, prefetch, runway, timepoint admission, temporal scheduling,
presentation handoff, memory policy, or quality policy. It will not use
playback measurements to justify static-viewer changes and will not make any
playback performance claim.

The ordinary and playback viewers currently share some layer and rendering
primitives. A shared correctness edit is permitted only when its contract is
independent of timepoint scheduling and focused regression tests prove the
existing playback result is unchanged. If clean separation is required, the
static path will be split from the playback branch rather than redesigning the
playback branch inside this package.

During implementation, owner product use exposed a violation of this
boundary: entering playback could reuse the newly installed static fair
ladder. Playback rejected its one-layer intermediate rungs but mistakenly
retained the common terminal rung as the whole ladder, which could freeze the
session contract at S5. The correction introduces no new playback policy.
Static and playback ladder baselines are now mode-local, a mode transition
rebuilds the requested policy while retaining the already presented front,
and playback presentation uses the exact pre-cutover neutral work formula and
observation identity. This incident and its regression coverage are part of
the implementation record below.

## New Product Evidence

The owner reports two distinct failures:

1. adding visible channels forces substantially coarser 3D presentation than
   a single channel, with perceived detail loss worse than the channel-count
   increase; and
2. hiding the first, durable active channel can break or severely degrade the
   viewer even while other channels remain visible.

The second failure is confirmed by source-level authority contradictions:

- render intent and projected per-layer scale maps contain visible layers
  only;
- demand, ladder, preview, and diagnostic paths nevertheless require the
  durable active layer to exist in those visible-only maps;
- visibility toggles do not change the durable active layer, nor should they;
- the dataset demand plan carries a scalar `scale` defined as the active
  layer's scale even though `layer_scales` is the multichannel authority; and
- an active-layer change invalidates retained resources and presentation more
  broadly than the rendered layer set requires.

The first failure has one proven policy mechanism and one still-unmeasured
runtime component. The native interaction estimator sums
`maximum_dimension × sampling_taps` for every visible layer under one fixed
work ceiling. With a factor-two pyramid, crossing that ceiling drops every
coherent layer by a whole level. One spatial level halves maximum ray steps
but removes roughly seven eighths of the voxels from a three-dimensional
body, so the visible quality cliff can exceed the numerical channel-count
increase even when GPU work scales linearly. Whether a current shader,
directory, CPU, upload, or storage path adds genuinely superlinear runtime
cost is not established and must be measured before it is redesigned.

## Decision

Cut static volume rendering over to visible-layer and per-layer-map authority,
then measure the renderer at fixed LOD before changing its algorithms. Use the
measurement to correct the first proven blocking boundary only. Do not infer a
GPU, CPU, cache, or storage bottleneck from the visible LOD chosen by a
conservative policy.

The implementation has four parts:

1. remove the hidden active layer as a render-demand prerequisite;
2. make quality, capacity, presentation, and diagnostics consume the same
   ordered visible-layer set and per-layer scale maps;
3. add a controlled warm-resident same-LOD multichannel timing matrix and the
   production diagnostics needed to explain a choice; and
4. refine the work model, capacity ordering, or shader traversal only where
   the new evidence identifies that boundary.

This ordering is mandatory. A faster renderer cannot repair contradictory
layer authority, and a looser LOD constant would trade detail for unbounded
frame cost without explaining the underlying work.

## Product Outcome

For an ordinary, non-playing dataset view:

- hiding any channel, including the durable active channel, leaves every
  remaining visible channel renderable;
- hiding all channels produces the existing intentional empty presentation,
  not a planning error, stale frame, or emergency-floor loop;
- changing the durable active layer without changing rendered state performs
  no new volume I/O, upload, residency replacement, or color render;
- ideal, capacity-selected, navigation-selected, and displayed fidelity remain
  distinct per-layer facts;
- candidate selection no longer derives rendering truth from one active-layer
  scalar;
- multichannel work diagnostics explain the selected candidate in terms of
  the exact visible layers and actual renderer kernel class; and
- compatible multichannel workloads retain the finest complete native-
  resolution body demonstrated safe by the accepted deterministic policy.

No outcome permits partial frames, reduced output resolution, silent sampling
changes, channel omission, altered transfer functions, or a second renderer.

## Non-Negotiable Invariants

### Scientific And Rendering Semantics

- Canonical source values, validity, scale construction, transforms,
  sampling, transfer, opacity, MIP, DVR, ISO, and authored Mixed composition
  do not change.
- Exact compatible DVR remains joint emission-absorption integration, not
  additive composition of completed channel images.
- Mixed mode retains authored layer order.
- A performance path must pass the existing independent numerical and
  order-semantics oracles before it can replace the current path.

### Presentation And Interaction

- The 3D output remains at the physical panel extent. No dynamic resolution or
  upscaling is introduced.
- Only complete frames become visible. The prior complete front remains
  visible until an atomic replacement is ready.
- One gesture freezes one selected per-layer scale map. Resource arrival and
  timing evidence cannot change visible LOD during that gesture.
- The geometry-bounded terminal body remains the unconditional emergency
  floor, and the bounded resident ladder remains one renderer-owned resource
  union.
- Ideal, selected, and displayed LOD remain separate. The fix must not make a
  coarse displayed frame masquerade as the ideal or selected target.

### Resource Ownership

- `ResidencyOwner` remains the sole GPU payload, placement, pin, eviction,
  directory, and upload authority.
- The current frame coordinator remains the sole publication authority.
- The canonical storage/cache path remains unchanged unless measurements show
  unavailable-brick physical I/O or decode is the first blocking boundary.
- Maximum supported layers, resources, catalog depth, and current GPU/CPU
  budgets do not increase.

### Boundedness And Failure

- Visible-layer collections remain bounded by the existing 64-layer product
  limit and requirement unions by the existing 65,536-resource limit.
- Empty visibility is an explicit successful state.
- Failure of an ordinary fine refinement remains recoverable; only failure of
  the valid minimum plan is a hard capacity failure.
- Diagnostics remain bounded and off the pixel-critical path.

## Target Authority Model

### Durable Analysis Selection

`ViewState::active_layer` remains the durable selection used by inspectors,
editing controls, crosshair or analysis actions, and keyboard/UI focus. It may
refer to a hidden layer. Hiding a layer must not silently rewrite that user
selection because visibility and analysis focus are different product facts.

The active layer is not a membership authority for rendering, demand,
residency, presentation, or fidelity.

### Ordered Visible Render Layers

One ordered visible-layer projection, derived from authored view-layer order,
owns membership and deterministic ties for rendering. It may be empty. Every
static 3D planning, workload, render-intent, residency, and presentation path
must consume either that projection or a per-layer map proven to contain
exactly that projection.

The ordered projection is deliberately derived rather than persisted. This
prevents a second mutable visibility authority from drifting away from
`ViewState`.

### Per-Layer Fidelity Maps

The following maps are authoritative:

- ideal projected scale by visible layer;
- capacity-selected target scale by visible layer;
- frozen navigation scale by visible layer; and
- displayed scale/coverage by visible layer.

A scalar summary may be exposed only as an explicitly named uniform value or
as `None` for empty/mixed configurations. It cannot drive resource identity,
candidate ordering, currentness, or rendering. The legacy active-layer
`DatasetDemandPlan::scale` and candidate `active_scale` fields are deleted,
not retained as compatibility aliases.

### Quality Ordering

Capacity refinement uses deterministic max-min progress across visible
layers. Starting from the complete floor, each layer has a finite catalog path
toward its own ideal. The next tentative refinement belongs to the layer with
the least normalized completed progress; authored order breaks exact ties.
If that refinement cannot fit the aggregate union, it is marked blocked for
that selection pass and another equally or less refined visible layer may
advance. Selection stops when no remaining refinement fits.

This replaces active-first greedy refinement. Active analysis selection is
allowed to be hidden and has no defensible claim to all available fidelity;
max-min progress avoids starving overlays with different shapes or catalog
depths. It also remains deterministic and bounded without an exponential
joint-map search.

The resident navigation ladder advances toward the selected map through the
same ordering. Each retained rung is still a complete full-volume per-layer
map and must fit the existing tail limits atomically.

## Performance Evidence Contract

### Controlled GPU Matrix

Add one ignored trusted-GPU diagnostic that renders warm, fully resident,
identically transformed channels at one fixed scale. It measures 1, 2, 4, and
8 visible channels without allowing demand planning to change LOD. At minimum
it covers homogeneous MIP, compatible exact DVR, and ISO under voxel-exact and
smooth-linear sampling; Mixed is included as a semantic/control case, not
assumed comparable to a homogeneous kernel.

For each case the report records:

- selected adapter and backend;
- physical output extent and volume geometry;
- channel count, sampling policy, render modes, and compatibility class;
- warmup count, measured sample count, median, p95, and maximum GPU color-pass
  time;
- CPU request/preparation and queue-submission time where already available;
- resource count, payload bytes, and whether upload or source work occurred
  during measured samples; and
- normalized ratios against one channel and ideal linear channel scaling.

The initial mapped workload is the current supported 1920×1080 workstation
envelope on the selected Vulkan adapter, with at least five discarded warmups
and thirty measured samples. A smaller fallback extent may be used only for
correctness, never for the performance conclusion.

### Interpretation Gate

A renderer traversal refactor is authorized for a homogeneous compatibility
class when p95 channel scaling exceeds ideal linear scaling by more than 20
percent at four or eight channels, or profiling attributes at least 15 percent
of p95 color-pass time to duplicated per-layer traversal mechanics that can be
shared without changing samples. The single-channel result must not regress by
more than 5 percent after the cut.

If the matrix is linear within that boundary, the source claim is narrower:
the disproportionate perceived loss comes from conservative estimation plus
discrete pyramid levels, not a proven superlinear GPU defect. No shader
rewrite is performed merely to make the plan look larger.

### Production Choice Diagnostics

For each considered navigation candidate, bounded diagnostics report:

- the full ordered layer-scale map, rendered as a compact uniform or mixed
  summary plus per-layer rows;
- analytical work components per layer and compatibility group;
- kernel class, sampling policy, projected pixel coverage, traversal bound,
  and sample-tap bound;
- completeness/residency, target eligibility, work-envelope result, and final
  disposition; and
- the selected map without calling any layer “active scale.”

These facts make future reports falsifiable: a coarse result can be assigned
to projection, work safety, aggregate capacity, missing residency, or genuine
minimum-plan failure.

## Deterministic Work-Model Cutover

The current `viewport_pixels × Σ(maximum_dimension × taps)` formula is replaced
by one renderer-schedule-aligned analytical estimate for static navigation.
For each layer it conservatively derives:

1. a screen-space rectangle containing the projected volume bounds, clipped
   to the native output extent;
2. a grid-space traversal bound derived from the camera ray and layer's
   world-to-grid transform rather than `max(x, y, z)` alone;
3. sampling taps, including smooth-linear interpolation and ISO gradient
   samples; and
4. fixed per-pixel/per-segment overhead represented separately from sample
   taps.

Compatible layers share only work that the selected shader actually shares.
For example, compatible exact DVR counts ray setup and segment traversal once
but still counts every layer's scalar samples and transfer/optics work. MIP or
ISO cannot receive shared-work credit until the renderer uses and verifies a
shared traversal. General affine DVR and authored Mixed retain their actual
general-path accounting.

The estimate remains an upper-bound policy, not a prediction trained to one
dataset. Page extrema skips and data-dependent early termination are reported
as observed savings but do not authorize a finer first frame because their
availability depends on content. Existing asynchronous timings may shrink
hidden refinement batches and certify an identical completed profile, but may
not raise the visible navigation envelope during a gesture.

The fixed 12 ms interaction guideline remains. Constants introduced by the
new estimate must be justified by shader operations and the controlled matrix,
not by tuning until one private dataset looks good.

## Conditional Renderer Cut

If the interpretation gate is met, replace duplicated traversal inside the
affected homogeneous kernel with compatibility-group traversal:

- compute the common world/grid ray once for co-registered layers;
- resolve the nearest common page-segment boundary once where geometry permits;
- retain one resource/page handle per layer and perform exactly the same
  per-layer samples, transfer, validity, extrema, opacity, and composition;
- keep a general affine path for genuinely incompatible transforms or scales;
  and
- select the path from the existing renderer intent/control builder, not from
  a benchmark-only flag.

This is an internal kernel specialization inside the one renderer. It is not
multi-pass channel rendering: separate full-frame channel passes would add
bandwidth, intermediate textures, synchronization, and ordering hazards for
exact DVR, ISO depth composition, and Mixed semantics.

No packed-channel storage or format change is included. Such a cut would
change upload, residency, physical accounting, bricking, and potentially
sampling precision; the present evidence does not authorize that blast
radius.

## Hard Cut And Deletions

The implementation is incomplete while any of these remain as ordinary static
rendering authority:

- `DatasetDemandPlan::scale` as the active layer's scalar;
- preview candidate and diagnostic `active_scale` fields;
- lookups of `view.active_layer()` in visible-only 3D layer-scale maps;
- active-layer membership checks that reject an otherwise valid visible set;
- active-first capacity refinement;
- whole-3D retained-resource or presentation invalidation caused solely by an
  analysis-active change; or
- a demand signature field that schedules identical static render work solely
  because analysis focus changed.

No compatibility accessor, fallback-to-first-active mutation, or hidden
auto-visibility rule remains after the cut.

## Work Sequence

### M0 — Baseline And Regressions

- Add focused failing tests for a hidden durable active layer with another
  visible layer, all layers hidden, active changes with identical visible
  render state, per-layer mixed scales, and reordered authored layers.
- Add the ignored same-LOD GPU timing matrix before optimizing a shader.
- Capture baseline results and identify whether GPU scaling is linear,
  superlinear, or hidden by CPU/upload work.

M0 changes no product policy.

### M1 — Visible-Layer Authority Hard Cut

- Introduce the ordered derived visible-layer projection and exact-map
  validation.
- Remove active-layer scalar fields and convert ordinary 3D callers, candidate
  facts, and UI diagnostics to per-layer maps.
- Make empty visibility short-circuit to the existing explicit empty display.
- Stop invalidating static 3D demand/residency/presentation for analysis-only
  active changes.
- Preserve active selection for inspector/edit semantics.

M1 is accepted only when hiding each possible channel yields the same result
for the same remaining visible set, independent of which hidden layer is
active.

### M2 — Fair Capacity And Ladder Ordering

- Replace active-first refinement with deterministic max-min visible-layer
  progress.
- Use the same ordering to construct coherent complete navigation rungs.
- Keep exact aggregate-union preflight and current tail limits.
- Add unequal-shape, unequal-catalog-depth, constrained-capacity, hidden-active,
  and authored-tie tests.

M2 does not claim renderer speed. It prevents an unrelated selection field
from monopolizing retained quality.

### M3 — Schedule-Aligned Static Work Model

- Implement conservative projected coverage and transform-aware traversal.
- Represent shared and per-layer work explicitly by actual kernel path.
- Use that estimate for direct safety, candidate selection, hidden strip
  sizing inputs, and diagnostics without merging ideal/selected/displayed
  state.
- Add independent geometry cases for orthographic, perspective, clipped,
  off-center, rotated, anisotropic, mixed-scale, and overflow boundaries.

M3 is accepted only if every benchmark-safe candidate rejected by the old
formula is explained by a changed conservative component, and known unsafe
cases remain rejected.

### M4 — Evidence-Gated Kernel Refactor

- Apply the conditional renderer cut only to compatibility classes that meet
  the gate.
- Preserve the general path and delete the replaced duplicated compatible
  path.
- Run independent pixel/validity comparisons across channel order, overlap,
  empty pages, missing samples, transfer functions, opacity, smooth sampling,
  and ISO shading as applicable.
- Rerun the exact M0 matrix and report absolute and normalized before/after
  timings.

If no class meets the gate, M4 closes with the measured no-change decision.

### M5 — First-Blocking-Boundary Audit

- During the normal static product workload, use existing bounded counters to
  distinguish planning, cache lookup, physical read/decode, upload, directory
  install, GPU color work, and presentation.
- Change CPU union construction, cache cohorting, or storage only if one is the
  measured first blocking boundary after M1–M4.
- Any storage-format, brick-edge, compression, or GPU-budget change requires a
  new owner-approved amendment and is not implied by this plan.

### M6 — Integration And Product Validation

- Run formatting, documentation, affected crate tests, workspace Clippy, the
  normal PR gate, trusted Vulkan correctness, and the same-LOD matrix.
- Exercise the normal mapped application with 1, 2, 4, and all available
  channels; hide every channel including the durable active one; orbit, zoom,
  resize, change modes and sampling, and settle exact presentation.
- Record selected/displayed per-layer maps and renderer validation output.
- Obtain owner-observed product validation before claiming the performance
  outcome complete.

## Focused Verification

At minimum the implementation must prove:

- visible-set permutation and hidden-active invariance;
- explicit all-hidden empty display;
- no new static request, lease drop, upload, render, or frame identity for an
  analysis-only active change;
- map equality across intent, demand, requirements, prepared body, workload,
  selected candidate, and displayed status;
- deterministic fair capacity selection at exact byte/resource boundaries;
- gesture-frozen mixed maps and one atomic exact promotion;
- numerical equivalence of every changed GPU path;
- no renderer validation errors or missing-resource reads; and
- unchanged playback-focused tests, without adding a playback claim.

Performance acceptance names the mapped GPU, backend, extent, geometry,
sampling/mode matrix, warmup/sample counts, p50/p95/max, chosen per-layer maps,
and comparison revision. A visually improved frame without those facts is
useful product feedback but not a universal performance proof.

## Risks And Responses

### A More Accurate Estimate Could Become Unsafe

Projected bounds and traversal math are conservative and independently tested.
Data-dependent skips may diagnose savings but cannot reduce the first-frame
bound. The terminal floor remains available if no candidate is safe.

### Fairness Could Reduce A User's Preferred Channel

The product has no explicit render-quality priority control. Active analysis
selection is not a substitute, especially when hidden. This package therefore
chooses deterministic fairness. A future explicit quality priority would be a
new product feature and persisted model decision.

### Mixed Scales Can Disable Compatible Fast Paths

The selector accounts the actual resulting kernel compatibility. A capacity
win that creates a slower render configuration cannot be called
interaction-safe merely because it fits memory. Exact DVR falls back to its
correct general integration when scales or transforms differ.

### Benchmarks Can Encourage Private-Dataset Tuning

The controlled matrix fixes geometry and LOD, and the analytical model remains
content-independent. The normal private workload validates usability but does
not define constants or enter checked-in evidence.

### Shared Code Could Accidentally Change Playback

Playback-specific quality, retention, runway, scheduling, admission,
presentation, and memory policy stay unchanged. Focused playback tests are
regression boundaries; shared static ladders and work estimates are rejected
at the mode boundary rather than extending this plan into temporal policy.

## Rejected Alternatives

- Automatically make another layer active when the active layer is hidden:
  this conflates analysis focus with render membership and moves the defect.
- Keep active scalars as convenient summaries: they have already become hidden
  authority and cannot truthfully represent mixed or empty maps.
- Raise `NATIVE_NAVIGATION_WORK_UNITS`: this is an unmeasured relaxation of the
  smoothness guarantee and remains equally inaccurate.
- Divide the work estimate by channel count: every channel still performs
  samples and composition, so this would hide rather than remove work.
- Render channels into separate full-frame passes: exact multichannel DVR,
  depth-ordered ISO, and authored Mixed semantics make that expensive and
  error-prone.
- Rewrite CPU demand, cache, storage, or package format preemptively: source
  inspection does not establish them as the first blocking boundary.
- Change playback retention or temporal scheduling: explicitly outside owner
  authorization for this package.

## Implementation And Evidence Record

The static implementation is present in the current working tree based on the
planning baseline above. The normal PR gate and the package's renderer GPU
equivalence/matrix checks pass. Normal mapped owner validation remains open;
this section does not call the product outcome validated.

### Authority Cut

- `DatasetDemandPlan::scale`, prepared-plan scalar aliases, selected
  active-level fields, and preview/diagnostic `active_scale` fields are
  deleted. Exact visible-layer maps cross planning, prepared bodies,
  residency scopes, workload profiles, presentation coverage, automation, and
  currentness checks.
- `ViewState::active_layer` remains analysis/editor focus. An active-only
  change preserves the installed requirement handles, retained leases,
  render-intent revisions, display generation, frame-fidelity state, decode
  submission count, and planning-call count.
- A hidden active layer plans the same 3D and linked-plane bodies as the same
  visible set with a visible active layer. All-hidden visibility installs an
  explicit empty scope, removes prepared 3D and navigation bodies, and
  publishes `RenderBackend::Empty` with `NoVisibleData`.
- Uniform scalar summaries are `Option<ScaleLevel>` and are `None` for empty
  or heterogeneous maps. They remain labels only. Exact-frame reuse,
  refinement settlement, coordinated automation, and retained-transaction
  reuse compare the complete presented layer-scale set, including absence of
  missing or extra layers.
- Backend and sampling summaries use the ordered visible set. Mixed modes have
  an explicit `GpuCameraMixed` diagnostic; linked-view summaries aggregate
  visible per-layer evidence instead of reading the active channel.

### Static Fairness And Playback Isolation

Static target selection starts at the complete per-layer terminal map and
uses exact integer max-min normalized catalog progress. One tentative layer
advances by one catalog step; authored visible order breaks equal ties. A
capacity-refused step is skipped for that bounded pass so another layer can
advance without an exponential joint-map search. The resident static ladder
uses the same one-layer ordering and re-adopts that exact adjacent structure
without rescanning semantic bodies.

Playback-specific quality, retention, runway, scheduling, admission,
presentation, and memory policy was not redesigned. The shared planner has an
explicit `playback_active` split: playback retains its prior active-first
greedy target order when the active layer is visible and its prior coherent
lockstep ladder; the new max-min and one-layer ladder apply only to ordinary
static viewing. Analysis focus is absent from static demand signatures but is
retained conditionally as playback's pre-cutover priority input, including its
existing rewarm behavior.

The initial implementation incorrectly allowed an installed static ladder to
cross that split. On static-to-playback transition, baseline adoption accepted
the common terminal rung, rejected the first mixed static rung, and returned a
one-rung result instead of rebuilding playback's lockstep ladder. Because a
baseline had nominally been adopted, the planner never generated the finer
playback rungs; the resulting fixed session contract could therefore be S5.
The application now reuses a navigation baseline only when the installed and
requested playback modes match. Planner validation independently rejects a
structurally incompatible ladder as a whole in either transition direction,
including the defensive fallback path. The prior neutral playback work
formula (`pixels × Σ(max_dimension × sample_taps + gradient_taps)`), 2.5
billion native envelope, 128 million initial calibration boundary, 1 million
minimum, and pre-cutover profile/family observation identity are isolated from
the schedule-aligned static model.

Regressions cover fresh static and playback plans, static-to-playback and
playback-to-static ladder transitions, the normal application command path on
the two-channel/three-timepoint fixture, an S0 playback contract after a
static mixed rung was installed, playback focus rewarm, and the legacy work
boundary. The ordinary playback-filtered application suite remains a
regression boundary, not a performance claim.

### Fixed-LOD GPU Measurement

The ignored diagnostic uses eight co-registered Float32 64x64x64 channels,
one resident full-volume resource and 1 MiB payload per channel, a 1920x1080
physical target, five discarded warmups, and thirty measured samples. It ran
on the NVIDIA GeForce RTX 3070 Ti Laptop GPU with the Vulkan backend and GPU
timestamp queries. Every measured sample reported zero visited resources,
zero uploaded resources, and zero residency queue submissions. Values below
are GPU color-pass p95 milliseconds; `norm` is the largest four- or
eight-channel p95 ratio divided by ideal linear channel scaling.

| Sampling | Kernel | 1 channel | 2 channels | 4 channels | 8 channels | max norm (4/8) |
|---|---:|---:|---:|---:|---:|---:|
| VoxelExact | MIP | 1.590272 | 3.070976 | 5.113856 | 10.278912 | 0.8080 |
| VoxelExact | compatible DVR | 3.223552 | 5.502976 | 10.269696 | 19.590144 | 0.7965 |
| VoxelExact | ISO | 7.575552 | 11.459584 | 20.486144 | 39.536640 | 0.6761 |
| VoxelExact | Mixed | 1.267712 MIP control | 4.741120 | 8.400896 | 17.150976 | 0.9044 |
| SmoothLinear | MIP | 6.878208 | 14.039040 | 28.290048 | 56.769536 | 1.0317 |
| SmoothLinear | general DVR | 10.402816 | 19.740672 | 38.460416 | 77.776896 | 0.9346 |
| SmoothLinear | ISO | 13.617152 | 24.479744 | 45.423616 | 80.719872 | 0.8339 |
| SmoothLinear | Mixed | 6.975488 MIP control | 19.866624 | 37.165056 | 74.224640 | 0.9354 |

The maximum homogeneous four/eight-channel normalized ratio was 1.0317 for
eight-channel SmoothLinear MIP, below the 1.20 interpretation threshold. CPU
renderer-planning p95 was at most 0.812712 ms and queue-submit p95 at most
0.309468 ms in this run. WGPU reported zero validation errors. This establishes
linear-within-gate warm-resident scaling for this named fixture and adapter;
it is not an all-hardware or arbitrary-volume claim.

The shader gate therefore did not trigger. No color shader, composition
algorithm, renderer ownership, cache, storage, upload, or package-format path
was changed. The disproportionate static detail loss is assigned to the old
conservative work estimate combined with discrete three-dimensional pyramid
steps, not to a measured superlinear GPU defect.

The standard PR gate passed zero-warning workspace Clippy, exact discovery,
and all 1,365 selected unit/contract/UI tests with retries disabled. The two
trusted-Vulkan renderer equivalence checks also pass. The ignored
application-level check
`unsafe_3d_profile_keeps_native_preview_visible_until_atomic_exact_strips_finish`
still fails because its harness expects an immediate UI refresh token after
every hidden-strip attempt. A detached clean worktree at the planning baseline
fails at the identical assertion, so this is recorded as pre-existing harness
debt rather than attributed to this package; it is not being weakened or used
as positive evidence here.

### Schedule-Aligned Work Model

Static navigation now uses quarter-voxel-MIP-step integer work units. The
model projects the eight renderer-quantized affine volume corners, clips a
conservative pixel rectangle to the native target, derives traversal from the
camera directions and uploaded world-to-grid rows, and saturates every
arithmetic boundary. Eye-plane ambiguity falls back to the full target.

The schedule separates full-output ray setup, grid/page traversal, per-layer
sampling and transfer/optics, and ISO gradient work. Compatible voxel-exact
DVR receives exactly the sharing present in its shader: one ray/traversal plus
per-layer optical work. General affine or smooth DVR uses one conservative
common-world interval with per-layer ray and sample work. MIP, ISO, and
authored Mixed receive no traversal-sharing credit their kernels do not own.

The fixed 2.5-billion-unit native envelope now preserves the measured safe
1080p boundaries: eight-layer voxel MIP, four-layer compatible voxel DVR, and
two-layer voxel ISO are admitted; eight-layer voxel DVR, four-layer voxel ISO,
two-layer smooth MIP/DVR, and one-layer smooth ISO are rejected. Independent
tests cover clipping, off-screen volumes, perspective eye-plane fallback,
anisotropic/sheared traversal, physically coincident mixed scales, kernel
classification, row-range accounting, and saturating overflow. Runtime and
automation diagnostics expose the complete scale map, kernel, projected and
scheduled pixels/steps, tap counts, shared work, per-layer components,
residency, eligibility, safety, and final disposition.

### Owner Closeout And Deferred Performance Recovery

On 2026-08-02 the owner explicitly accepted this correctness-first closeout
after normal product use while reporting that static viewer performance is
materially worse after the refactor. No controlled end-to-end before/after
measurement was captured, so this is qualitative negative product evidence,
not a quantified regression claim, and this package makes no performance-win
claim. The fixed-LOD matrix diagnoses current GPU scaling but does not measure
the refactor's product delta.

The owner waived the remaining exhaustive mapped matrix for this package and
deferred recovering static multichannel performance to separately approved
follow-up work. That follow-up must begin with a same-workload baseline/current
comparison and preserve visible-layer authority, complete per-layer fidelity
maps, scientific rendering semantics, native output, and atomic presentation.
Playback remains outside that follow-up unless separately authorized.

## Completion Boundary

The required authority cut, implementation, automated verification, and
fixed-LOD evidence are complete. The owner explicitly accepted closeout and
waived the remaining exhaustive mapped matrix after reporting materially worse
normal-product performance. This closes the correctness package without
asserting a performance improvement; static performance recovery remains a
separate unresolved follow-up. An evidence gate that does not trigger is
recorded as a deliberate no-change result, not left as ambiguous unfinished
work.

The owner authorized this complete bounded sequence on 2026-08-02. Any change
to playback, storage representation, brick geometry, GPU budget, scientific
rendering semantics, or output-resolution policy requires a separate explicit
amendment.
