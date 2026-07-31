# Viewer Adaptive LOD And Global Capacity Plan

- Status: COMPLETE — PRODUCT-VALIDATED FOR ADAPTIVE CAPACITY AND 3D ZOOM
- Planning requested by owner: 2026-07-29
- Implementation authorized by owner: 2026-07-29
- Last reviewed: 2026-07-29

This document preserves the approved target and implementation/evidence
handoff. [Current State](../../CURRENT_STATE.md) remains the authority for
implemented product facts.

## Active Follow-On Correction

The mapped `zoom` and `combined` acceptance runs in this handoff drove the
independent 3D panel. The separate linked-oblique run validated S3 interaction
continuity, not linked-2D LOD transition or pixel fidelity. The broader
linked-2D inference is withdrawn after ordinary incremental linked zoom was
shown to retain an older scale while reporting exact settlement.

The
[linked-2D LOD truth and settlement plan](VIEWER_LINKED_2D_LOD_TRUTH_AND_SETTLEMENT.md)
owns that active evidence and currentness correction. This completed plan
retains its adaptive-capacity, raw-wheel-input, 3D zoom, and S3 oblique claims;
it does not claim linked-2D zoom reaches the selected scale.

## Implementation Result

A0 through A5 and the named hard cut are complete.

- Screen-derived ideal LOD, aggregate-feasible selected LOD, and complete
  displayed LOD are distinct transient facts.
- Generic catalog traversal starts from each layer's coarsest valid level,
  establishes one renderer-accounted navigation floor, and admits
  deterministic refinements only while the exact global retained union fits.
- The renderer's sole residency owner preflights the aggregate requirement
  union. Fixed equal panel quotas and ordinary ideal-LOD terminal latching are
  deleted.
- Camera, cross-section, resize, layout, and later intent continue to replace
  pending work. A stale worker result whose captured renderer-union base has
  changed is replanned rather than exposed as a hard capacity failure.
- Complete fronts remain visible until exact replacement; the UI truthfully
  reports shown, selected, ideal, refining, and adaptive-capacity states.
- Raw current-frame wheel events are the sole authoritative wheel input.
  Egui's multi-frame smoothing tail can no longer synthesize thousands of
  camera revisions after one physical wheel event.

On 2026-07-29, three 30-second mapped normal-app zoom sessions passed on the
representative four-panel workload and NVIDIA GeForce RTX 3070 Ti Laptop GPU.
Each observed both a feasible finer displayed boundary and an ideal boundary
constrained to a coarser selected/displayed level, then recovered to complete
S3 without a hard capacity error. Across the sessions the worst input,
window-receipt, main-loop, and externally visible-change gaps were 31.096 ms,
35.838 ms, 33.638 ms, and 54.763 ms; worst p99 input-to-visible latency was
28.945 ms.

The required five-minute combined normal-app exercise also passed. It
performed an exact real-window resize round trip, 120 real 3D orbit samples,
and 18,004 independently clocked wheel inputs. The app applied 17,910
authoritative camera samples; worst input, receipt, main-loop, and external
visible-change gaps were 32.714 ms, 39.150 ms, 34.181 ms, and 86.384 ms; p99
input-to-visible latency was 46.697 ms. Both LOD boundary classes were
observed, no ordinary hard capacity error occurred, and the final frame was
complete S3. A fresh 30-second linked-oblique regression also passed with a
44.303 ms worst visible gap and 26.595 ms worst p99 latency.

Focused formatting, `xtask`, UI, application, selector, residency, capacity,
currentness, and stale-result checks pass. The final broad repository gate
also passed zero-warning workspace Clippy, every policy phase, exact lane
discovery, and all 1,239 PR-lane tests; [Current State](../../CURRENT_STATE.md)
owns that implemented fact.

## Decision

Replace mandatory screen-ideal LOD admission and fixed per-panel capacity
shares with one general adaptive-LOD policy over the renderer's existing
global residency budget.

The screen-derived LOD is an ideal quality target, not an unconditional
scientific requirement. The normal viewer must select and truthfully report
the finest available LOD combination that fits. An unaffordable refinement is
not a product error: the affected view continues at a declared coarser LOD and
remains interactive.

This is a general policy over catalog-provided levels. Production code may not
special-case S2, S3, one transition, the representative dataset, four-panel
layout, or one GPU. The observed S3-to-S2 failure is only the regression that
exposed the incorrect policy.

## Blocking Product Observation

After the four-panel/S3 oblique-continuity correction passed, the owner used
the normal application and zoomed the 3D view. The screen projection crossed
from S3 to ideal S2. The viewer then stopped updating indefinitely while the
menus remained responsive and displayed:

> screen-selected scale s2 dataset demand cannot fit within 16384 resources,
> 191102976 decoded bytes, and 131072 candidates per visible layer; fidelity
> was not silently reduced

The user could not zoom back out because rendering no longer presented later
camera input. Closing the application was the only practical recovery.

The warning identifies an application demand-admission failure, not a GPU
hang, source mutation, or corrupt dataset. In four-panel layout the
application divides the renderer-global resource and payload capacities into
four equal shares before planning. The finer 3D demand exceeded its artificial
one-quarter byte share, so the planner rejected the ideal LOD, retained the
old image, classified the result as budget-limited, and latched the unchanged
failure.

The current wording is also misleading:

- the displayed byte number is a per-scope GPU payload allowance, not the
  actual demand and not a general decoded-RAM observation;
- it does not name which bound was exceeded or by how much; and
- it treats an ordinary adaptive display-quality choice as a terminal
  fidelity failure.

## Root Design Error

The product currently conflates three different facts:

1. **Ideal LOD** — the finest level justified by projected screen footprint.
2. **Selected LOD** — the finest level that can be rendered under current
   aggregate capacity and transition obligations.
3. **Displayed LOD** — the complete level actually represented by the
   presented frame.

Truthfulness requires that selected and displayed LOD be reported accurately
and that incomplete or stale data never masquerade as complete. It does not
require the ideal LOD to be admitted when it cannot fit.

The old “fidelity was not silently reduced” rule applied an analysis/export
principle to an adaptive interactive viewer. Choosing and visibly labelling a
valid S3 pyramid level is not silent scientific corruption. Labelling S3 as
S2, presenting incomplete S2 as complete, or using an undeclared alternate
representation would be corruption.

The fixed quarter shares are also inconsistent with the implemented ownership
model. `ResidencyOwner` owns one global payload arena, directory, exact
resident map, pins, and eviction policy. Pre-dividing that capacity into four
unborrowable planning quotas can reject a feasible asymmetric global union and
prevents the active view from using otherwise available capacity.

## Product Contract

For every mapped viewer state:

- input and durable camera/view state remain admissible independently of
  refinement success;
- the viewer renders the latest intent at the finest feasible declared LOD;
- ideal quality is attempted only when the complete aggregate transition fits;
- an unaffordable finer level leaves the view interactive at a coarser level;
- the UI reports the actual displayed LOD and may report an outstanding ideal
  refinement without presenting it as an error;
- old complete pixels remain visible until a complete frame for the latest
  intent is available;
- a finer frame replaces the old front atomically;
- repeated zoom, orbit, resize, layout, time, layer, and playback changes
  cannot trap the application in a failed render signature; and
- a red capacity failure is reserved for the exceptional case where no
  supported minimum navigation plan can fit at all.

This contract covers every available scale transition in both directions,
including non-adjacent transitions when projected footprint changes sharply.

## Generality Requirements

The selector operates on the actual ordered levels exposed by each visible
layer. It must support:

- any positive number of pyramid levels and nonuniform layer scale sets;
- independent per-layer projected levels for different physical grids;
- 3D, XY, XZ, and YZ targets and their synchronization groups;
- MIP, DVR, ISO, Mixed, sampling halos, validity, and pick requirements;
- single-3D and four-panel layouts;
- camera zoom/orbit/pan, cross-section translation/rotation/zoom, window and
  viewport resize, time changes, playback, and visibility changes;
- different dataset sizes, dtypes, transforms, and validity modes; and
- different renderer capacities and adapter limits.

No product branch may compare against a particular scale ordinal to decide
capacity behavior. Scale names may appear in focused regression fixtures and
human-readable evidence only.

## Retained Invariants

- Never mutate source microscopy data.
- Preserve valid zero, invalid/no-data, absent/loading, and complete data as
  distinct states.
- Preserve calibrated transforms, projected LOD mathematics, sampling halos,
  DVR physical distance, ISO gradients, picking, and complete-frame rules.
- Never label a coarser frame as a finer frame.
- Never present an incomplete brick mosaic as complete.
- Keep CPU memory, GPU payload, descriptors, queues, staging, candidate work,
  and in-flight transition overlap bounded and accounted.
- Keep one renderer, one CPU data plane, one intent mailbox, one global
  residency owner, and one frame coordinator.
- Capacity selection must not introduce a CPU renderer, dense path, legacy
  renderer, alternate residency map, or benchmark-only fast path.
- Large planning and refinement remain cancellable and latest-only.

## Target Authority And State

Durable project state continues to own cameras, layout, layers, time, and
render parameters. It does not persist an incidental hardware-selected LOD.

The transient mailbox continues to own the latest interaction intent. It must
accept and replace camera or cross-section samples without consulting a prior
render failure latch.

The demand planner remains a pure, cancellable worker over immutable intent
and scalar capacity facts. It produces candidate requirement bodies and their
exact resource/payload accounting; it does not allocate GPU slots.

The existing renderer `ResidencyOwner` remains the sole physical GPU-capacity
authority. It provides transactional feasibility for the exact aggregate
union, including current front pins, shared resources, replacement overlap,
and bounded in-flight work. The application may not mirror its resident map or
allocator.

The frame coordinator continues to own hidden replacement targets and atomic
publication. It records the selected and displayed LOD identities without
allocating another revision namespace.

One rendering-policy state records, per target and visible layer:

- ideal level;
- selected feasible level;
- displayed complete level;
- whether a finer refinement is pending; and
- the intent/frame identity to which those facts belong.

This is a projection of the existing authorities, not a new cache or second
mutable owner.

## Adaptive Selection Algorithm

For each latest intent:

1. Compute the ideal level independently for every visible target/layer using
   the existing projected-footprint rules.
2. Enumerate only catalog-provided candidates from that ideal toward coarser
   levels. Preserve all scientific sampling and validity requirements.
3. Charge the exact global union already obligated by unchanged visible
   fronts, their pins, shared resources, and in-flight replacement work.
4. Establish a minimum navigation configuration for every dirty target using
   the coarsest valid candidate that can render the latest intent.
5. If that minimum cannot fit, report one typed hard capacity failure while
   preserving input responsiveness and the last complete front.
6. Otherwise consider one-step refinements in deterministic priority order:
   the actively manipulated target first, its synchronization group next,
   other visible targets next, and nonvisible/background work last.
7. Admit a refinement only when the exact aggregate union, including the old
   front/new replacement overlap, passes renderer-global capacity preflight.
8. Continue until no higher-priority refinement fits or every target reaches
   its ideal level.
9. Render the selected latest-intent configuration and publish only complete
   synchronization groups.
10. Keep rejected finer candidates as quality opportunities, not terminal
    errors. Reconsider them after capacity, intent, layout, layer, time, or
    viewport changes.

This deterministic marginal-refinement procedure avoids a combinatorial
cross-product search while allowing asymmetric capacity use. Shared canonical
bricks count once in the aggregate union.

Candidate traversal, resource count, scratch memory, and payload capacity are
all feasibility dimensions. Hitting any of them at a finer level advances to
the next coarser candidate. Only failure of the minimum navigation
configuration is terminal.

## Navigation Floor And Continuity

The product maintains one complete, globally accounted navigation floor for
the current dataset/time/visible-layer set at a sufficiently coarse available
level. It is ordinary canonical residency inside `ResidencyOwner`, not a
second representation.

The navigation floor exists so a camera can continue moving while finer
view-specific demand is cold or unaffordable. Existing finer resources are
reused when they satisfy the latest view; otherwise the latest camera is
rendered from the navigation floor until a finer hidden target completes.

If even the coarsest catalog level cannot establish the navigation floor, the
viewer reports a genuine hard capacity limitation. The GUI and intent mailbox
still remain responsive, so changing layers, layout, viewport, or camera can
recover without restarting the process.

## LOD Stability

Selection must not oscillate around a screen or capacity boundary.

- Projected-footprint hysteresis remains generic over adjacent catalog levels.
- A refinement is not selected unless its complete transition overlap fits.
- The currently selected feasible level remains stable while intent changes
  stay within its reuse envelope.
- Capacity eviction or a larger aggregate union may lower selected quality by
  one or more catalog levels, but the change is explicit and atomic.
- Refinement resumes only after the responsible capacity/state epoch changes;
  there is no busy retry loop.

No timer, retry count, or scale-specific threshold may decide scientific
coverage.

## UI Semantics

Ordinary adaptive quality is neutral status, not a red failure. The UI should
concisely distinguish:

- the complete displayed level;
- a finer ideal level currently refining; and
- a finer ideal level currently constrained by aggregate capacity.

For example, a view may report `S3 · adaptive` or `S3 · refining toward S2`.
Exact wording is a UI decision made during implementation, but it must not
imply that S2 pixels are displayed before they are complete.

The current misleading “decoded bytes” warning must be removed from ordinary
LOD selection. A genuine minimum-plan capacity error must report the actual
failed dimension, required amount when known, global available amount, and
affected target/layer without exposing internal quota fiction.

## Hard Cut

The accepted implementation deletes in the same cut:

- fixed equal `demand_cohorts` capacity partitioning;
- per-panel `budget_share_*` admission as a product rule;
- ordinary-view tests that require ideal LOD capacity failure rather than
  adaptive selection;
- deterministic latching of an unaffordable ideal level as a terminal render
  failure;
- wording that calls the GPU payload allowance decoded-RAM capacity; and
- any currentness rule that prevents a later camera/viewport intent from
  selecting a feasible level.

There is no feature flag, old/new selector, alternate renderer, or permanent
compatibility path.

## Work Plan

### A0 — Faithful Boundary Reproducer

- Extend the existing real-window continuity machinery with one fixed 3D zoom
  workflow rather than creating a qualification framework.
- Use the normal release application, real display/GPU, representative
  dataset, four-panel layout, and actual pointer/wheel input.
- Begin at a feasible displayed level, repeatedly cross at least one
  finer-level capacity boundary, and return to the original zoom.
- Drive input independently of the app and observe real window receipt,
  main-loop progress, externally changed 3D pixels, actual displayed LOD, and
  final recovery.
- Require finite startup/run/shutdown deadlines and plain ignored local
  evidence. Add no schemas, receipts, hashes, provenance, replay, or
  publication machinery.
- The current product must reproduce the capacity warning and persistent
  visible stall before implementation starts.

### A1 — Separate Ideal, Selected, And Displayed LOD

- Introduce the three explicit facts without persisting hardware policy in the
  project model.
- Make actual frame identity carry truthful selected/displayed scale facts.
- Replace “ideal or error” with ordered generic catalog candidates.
- Retain all existing independent projected-LOD and scientific calculations.

### A2 — Global Aggregate Capacity Cutover

- Remove fixed per-target shares.
- Account unchanged fronts, dirty targets, shared resources, pins, and
  replacement overlap as one global proposed union.
- Add renderer-owned transactional capacity preflight without exporting
  physical slots or a resident map.
- Select candidates using active/synchronization priority and exact aggregate
  accounting.
- Delete the old partition owner in the same milestone.

### A3 — Continuous Interaction And Refinement

- Keep mailbox input admission independent of refinement outcome.
- Establish and reuse the complete navigation floor.
- Render every latest camera at the selected feasible level.
- Prepare finer work latest-only behind the complete front.
- Atomically promote completed refinements and abandon superseded or
  unaffordable ones without red failure.
- Ensure zoom-out, resize, layout, layer, and time changes always reopen
  selection and can recover.

### A4 — Status And Genuine Failure

- Present actual displayed/adaptive/refining LOD clearly.
- Reserve red capacity status for failure of the minimum navigation plan.
- Report actual required/available dimensions for genuine failures.
- Keep menus, viewer input, and recovery actions responsive even in that hard
  case.

### A5 — Product Acceptance And Cleanup

- Pass the real zoom-boundary scenario across repeated in/out direction
  changes without static pixels, indefinite stalls, or a red ordinary-LOD
  error.
- Exercise at least one boundary that fits and one ideal boundary forced not
  to fit while a coarser level does.
- Run focused scientific, complete-frame, currentness, residency, and capacity
  checks affected by the cutover.
- Perform one five-minute normal-app exercise combining zoom, orbit, resize,
  four-panel interaction, and return from finer to coarser views.
- Run the broad repository gate once at final integration.
- Update Current State and Current Work with measured results and remaining
  limits.

## Focused Automated Checks

Keep the automated additions small and independently falsifiable:

1. A pure selector test uses an arbitrary catalog level sequence and proves
   that the finest aggregate-feasible candidate is chosen without inspecting
   any particular scale ordinal.
2. An asymmetric four-target case proves that a globally feasible union is
   admitted even when one target needs more than one quarter of capacity.
3. A finer candidate forced over each independent bound—payload, resources,
   candidates, or scratch—selects the next valid coarser candidate and retains
   truthful scale identity.
4. Latest camera input remains replaceable after finer-level refusal, and
   zooming back out renders without restarting the runtime.
5. Hidden refinement retains the old complete front, then swaps only after the
   exact selected frame is complete.
6. Failure of every catalog candidate produces one typed hard error while GUI
   and intent state remain recoverable.

Existing numerical oracles, validity checks, source-safety checks, bounded
resource checks, and dedicated GPU-kernel checks remain. Internal publication
counts are not interaction-continuity evidence.

## Product Acceptance

On the representative real-display/GPU workflow:

- independently clocked active zoom has no input, main-loop, or externally
  changed-frame gap above 100 ms while a resident navigation level is
  available;
- p99 input-to-next-visible-change is at most 50 ms for resident navigation;
- crossing an unaffordable ideal boundary continues visibly at a declared
  coarser level;
- returning toward a coarser ideal always visibly recovers without restart;
- a feasible finer level prepares behind the old front and publishes
  atomically;
- no frame is blank, incomplete-as-complete, stale under a new identity, or
  labelled with the wrong LOD;
- repeated boundary crossings do not oscillate, leak, thrash, or emit
  repeating capacity/GPU errors; and
- the five-minute combined workflow terminates normally.

Cold refinement duration is reported separately from interaction continuity.
A slow cold finer load may keep the declared navigation LOD visible; it may
not stop camera input or turn loading into a frozen application.

## Scope Boundary

Included:

- general interactive LOD selection and hysteresis;
- aggregate visible-demand capacity selection;
- navigation-floor residency;
- camera/cross-section continuity across LOD boundaries;
- atomic finer refinement and truthful status;
- deletion of static per-panel quotas and wrong ideal-or-error tests.

Excluded:

- storage-format or physical-shard changes;
- changing the canonical logical brick edge;
- increasing the configured GPU budget as the solution;
- changing analysis/export resolution requirements;
- a CPU or legacy rendering path;
- public performance comparisons or release claims;
- unrelated viewer features; and
- a generalized benchmark, qualification, or provenance framework.

Any excluded change requires separate owner approval.

## Risks And Required Answers

- **Transition overlap:** old and new complete fronts may temporarily require
  more payload than either alone. Feasibility must charge their exact union.
- **Fragmentation:** byte totals alone may not prove arena allocatability.
  Renderer preflight must remain transactional and may use its sole bounded
  compaction path.
- **Thrashing:** repeated boundary motion can cause upload/eviction churn.
  Hysteresis, navigation-floor pins, latest-only work, and capacity epochs must
  suppress it.
- **Multiview fairness:** active priority must not destroy unchanged complete
  linked fronts. Their exact pins remain part of the aggregate obligation.
- **Multiple layers:** independently selected levels must preserve calibrated
  transforms and mode-specific sampling requirements.
- **Planning latency:** candidate selection must reuse ordered/coarser facts and
  remain cancellable rather than fully planning every cross-product.
- **Minimum-plan failure:** this remains possible on genuinely incapable
  hardware or malformed/extreme inputs. It must be typed, truthful, and
  recoverable through later user state changes.

## Authorization Boundary

The owner initially requested this written plan without implementation, then
explicitly authorized its full implementation on 2026-07-29. That
authorization covers A0 through A5 and the hard cuts named here. Excluded scope
still requires separate owner approval.
