# Viewer GPU Placeability And Recovery Plan

- Status: IMPLEMENTED, AUTOMATED-VERIFIED, AND OWNER PRODUCT-VALIDATED
- Planning requested by owner: 2026-07-30
- Implementation authorized by owner: 2026-07-30
- Last reviewed: 2026-07-31

Later narrow correction: the
[rendering correctness, recovery, and numerical cutover](VIEWER_RENDERING_CORRECTNESS_RECOVERY_AND_NUMERICAL_CUTOVER.md)
supersedes generic retryable-error projection and repeated checked identity
allocation with one typed attempt/wake authority and operation-scoped
exhaustion facts. This plan's physical-placeability and retained-front
recovery result remains unchanged.

This plan is the completed correctness handoff for the linked-2D physical
placeability incident. [Current State](../../CURRENT_STATE.md) remains the
authority for implemented facts.

## Original Blocking Product Observation

On the representative Cell dataset in four-panel layout, repeated linked zoom
does not have one stable outcome:

- one run retained shown S1 with no selected target, reported refining/stale,
  and never settled;
- another run later reached shown/selected/ideal S0 and exact/current; and
- the GUI simultaneously reported:

  `GPU capacity in PayloadResidency cannot satisfy 294912 bytes with
  38481956 bytes available`.

The image could continue moving while labelled stale, and Reset View did not
make later runs equivalent to earlier runs.

## Diagnosis

The renderer's exact-byte payload arena is split across up to four GPU storage
buffers. Allocation requires one contiguous range in one segment. The current
capacity error reports the sum of every free range across every segment, not
the largest allocatable range. It can therefore report tens of megabytes free
while failing to place one 294,912-byte payload.

The semantic planner admits a candidate from aggregate resource and byte
totals. Installation then asks the physical renderer allocator to preflight
the exact union. A fragmented arena can reject a candidate that passed the
planner's aggregate test. That failure is currently treated as deterministic,
so the same demand signature is not retried. The prepared linked plan is not
installed, leaving no selected target and retaining old paintable pixels.

The error lifecycle is independently wrong. A failed linked plan writes one
global plan/capacity string, but a later successful linked-only installation
does not clear it. Exact current S0 can therefore coexist with a historical
red warning.

Reset View resets view geometry. It does not reset GPU residency, allocation
layout, caches, retained fronts, or asynchronous work, so it is not a cold-run
boundary.

## Decision

Make physical placeability an ordinary adaptive-LOD feasibility dimension,
repair internal fragmentation inside the renderer's sole residency owner, and
preserve a current coarser display whenever a finer physical layout is not
currently feasible.

The correction has four parts:

1. **Truthful physical diagnostics.** Distinguish genuine aggregate
   exhaustion from placement fragmentation. Report requested allocation,
   aggregate free bytes, largest contiguous free range, segment state, and
   compaction/recovery counters.
2. **Bounded renderer-owned recovery.** When exact-union preflight fails only
   because free space is fragmented, compact existing payload allocations
   transactionally within the existing segmented arena, update the canonical
   page records, and retry the exact preflight. Compaction uses bounded
   renderer-owned scratch and does not export slots or create a second
   residency map.
3. **Adaptive residual fallback.** If the exact union remains physically
   unplaceable after compaction, reject that candidate as an ordinary quality
   opportunity and re-run generic catalog selection under a stricter bound.
   Continue until a physically feasible candidate is installed or every
   catalog-provided minimum candidate has genuinely failed. The current
   complete front remains paintable throughout.
4. **Scoped recovery lifecycle.** Fragmentation recovery is retryable and
   cannot enter a deterministic terminal latch. Successful linked planning or
   rendering clears the corresponding historical warning. A red capacity
   error is reserved for failure of every valid minimum candidate and names
   the affected target group and actual capacity dimension.

This is general over catalog levels, layers, targets, datasets, dtypes,
validity modes, layouts, and adapter segment limits. No production branch may
name S3, S2, S1, S0, the Cell dataset, or a fixed zoom count.

## Authority And Invariants

- `mirante4d-render-wgpu::ResidencyOwner` remains the sole physical allocation,
  compaction, page-record, resident-map, and GPU-capacity authority.
- The application planner continues to own semantic candidate selection and
  exact aggregate requirement bodies. It receives only typed feasibility and
  scalar diagnostic facts.
- The application may not mirror physical slots, free ranges, or GPU
  residency.
- The existing frame coordinator remains the sole presentation and color
  submission authority.
- Compaction changes physical placement only. It cannot change scientific
  values, validity, logical resource identity, scale identity, completeness,
  pins, or presentation currentness.
- Existing complete pixels remain visible until a complete selected
  replacement publishes atomically.
- Memory, VRAM, staging, compaction work, queue submissions, and retry count
  remain bounded.
- There is no CPU renderer, alternate GPU path, compatibility path, increased
  GPU budget, or storage-format change.

## Hard Cut

Delete or replace in the same cut:

- aggregate-free wording that implies those bytes form one allocatable block;
- deterministic latching of physical fragmentation;
- the single-shot planner/renderer mismatch that leaves selected LOD absent;
- stale linked plan/capacity messages that survive a successful replacement;
- any test assumption that Reset View creates a fresh renderer; and
- any performance conclusion drawn from a run whose allocator state is
  unknown.

## Work Plan

### C0 — Reproducer And Typed Allocator Facts

- Add deterministic allocator fixtures for aggregate exhaustion,
  fragmentation, compaction, and segmented placement.
- Add largest-contiguous and per-segment free-space facts.
- Introduce a typed placement failure distinct from genuine byte exhaustion.

Exit: the observed `requested < aggregate free` condition is independently
reproduced and cannot be formatted as ordinary total exhaustion.

### C1 — Bounded Physical Compaction

- Add one renderer-owned compaction scratch allocation inside the existing GPU
  budget.
- Compact resident payloads without changing logical residency or pins.
- Rewrite the moved resources' canonical page records before later color or
  pick work can consume them.
- Retry exact-union preflight once after compaction.
- Record moved resources/bytes, largest-range before/after, and submissions.

Exit: a fragmented arena with sufficient placeable capacity admits the same
exact union after one bounded recovery, with unchanged payload/page semantics.

### C2 — Adaptive Placeability Fallback

- Treat a residual physical-placement refusal as a rejected finer candidate,
  not a terminal render failure.
- Re-run the existing generic selector with a monotone bound that excludes the
  failed aggregate body.
- Preserve the last complete front while selection/replacement proceeds.
- Make later view, layout, layer, time, or capacity changes reopen selection.

Exit: a finer unplaceable candidate settles at a declared coarser feasible
level; the target never remains indefinitely at selected `none`.

### C3 — Error And Currentness Lifecycle

- Do not project recovered fragmentation as a red product error.
- Clear linked plan/capacity state after successful linked installation and
  clear renderer failure state after a successful replacement publication.
- Prefix genuine terminal capacity messages with the affected target group.
- Keep stale/provisional as semantic currentness terms, not synonyms for a
  physically frozen texture.

Exit: exact/current S0 cannot coexist with a historical linked capacity
warning, and a real minimum-plan failure remains visible and recoverable after
later input.

### C4 — Focused Verification And Handoff

- Keep allocator tests pure and deterministic.
- Add a small app-level selection/retry test proving finer refusal, coarser
  installation, later reopening, and error clearing.
- Extend existing diagnostics rather than adding a benchmark or evidence
  framework.
- Run formatting, focused renderer/app tests, documentation checks, and the
  smallest relevant broader checks.
- Do not launch the quarantined linked-S0 automation unattended.

Exit: implementation is automated-verified at the affected boundaries and
handed to the owner for normal-product observation before performance work
resumes.

## Acceptance

- Aggregate exhaustion and fragmentation have different typed outcomes and
  truthful messages.
- A 294,912-byte request is not described as impossible merely because 38 MB
  of noncontiguous space is reported as “available.”
- Recoverable fragmentation performs at most one compaction for one unchanged
  physical state before exact preflight is retried.
- Compaction neither evicts a logical resource nor changes its sampled bytes,
  validity, page identity, pin state, or current presentation.
- Residual placement refusal selects a feasible coarser catalog candidate
  without a red ordinary-LOD error or indefinite selected-none state.
- Failure of every minimum candidate remains typed, visible, and recoverable
  through later user state changes.
- Successful linked replacement clears its prior planning/capacity warning.
- Diagnostics identify cold process start versus retained residency and expose
  allocator state needed to compare runs.
- No test opens the interactive viewer or stresses linked S0 without explicit
  owner-controlled product validation.

## Scope Boundary

Included: renderer payload placeability, bounded compaction, adaptive
placeability fallback, retry/error lifecycle, allocator diagnostics, and
focused deterministic verification.

Excluded: shader or sampling optimization, resident-envelope tuning, storage
or brick-format changes, GPU-budget increases, broad test-suite expansion,
new qualification/provenance machinery, and performance claims.

The owner explicitly authorized this complete correctness implementation on
2026-07-30. Implementation, focused verification, and the required ordinary
product observation are now complete.

## Implementation Result

The authorized correctness cut is implemented:

- payload allocation now has separate typed outcomes for aggregate exhaustion
  and physical placement failure;
- diagnostics expose aggregate free bytes, largest contiguous range,
  per-segment free/largest ranges, placement refusals, compactions, and moved
  resource/byte counts;
- `ResidencyOwner` owns one fixed compaction scratch buffer carved from the
  existing transfer budget when that budget can still retain the bounded
  payload-upload envelope, packs allocations stably within each segment, rewrites
  canonical page records, submits the copy before later renderer work, and
  retries exact-union preflight once. Smaller diagnostic runtimes preserve
  their upload capacity and use adaptive fallback without compaction;
- recovery respects the existing in-flight submission bound. Saturated
  in-flight work defers recovery without a red error or quality downgrade;
- a residual placement refusal is converted into a strictly smaller aggregate
  payload bound for the existing generic catalog selector. Repeated refusals
  therefore make finite monotone progress without naming a scale;
- a later view, layout, layer, timepoint, dataset, or physical-capacity
  signature change discards that temporary bound and reopens normal
  selection;
- renderer-time placement refusal follows the same adaptive route if it
  escapes installation preflight;
- successful linked-only installation clears its matching historical plan and
  capacity warning, and successful presentation clears renderer failure state
  when no dataset-plan error remains; and
- only failure of the minimum valid candidate is projected as a red physical
  capacity error, with the affected target group and contiguous-placement
  facts.

The application retains semantic requirements and scalar feasibility only.
It does not mirror allocator ranges, physical slots, compaction moves, or GPU
residency.

## Automated Verification

The implementation has been checked without opening the native viewer:

- pure allocator tests distinguish fragmented placement from aggregate
  exhaustion and verify stable compaction plus validity-offset preservation;
- app tests cover monotone tightening/reopening, retry without a red error,
  non-latching recovery backpressure, and successful linked-only warning
  cleanup;
- `cargo test -p mirante4d-render-wgpu --lib` passes with 61 tests passed and
  12 hardware tests ignored;
- `cargo test -p mirante4d-app --lib` passes with 265 tests passed and 7 tests
  ignored;
- renderer-library Clippy passes with warnings denied;
- repository formatting and diff-whitespace checks pass; and
- `cargo xtask docs-check` passes.

The final repository PR gate also passed zero-warning workspace Clippy and all
1,298 unit, contract, and UI cases.

The quarantined linked-S0 automation was not launched.

## Product Validation Closeout

The owner completed the required normal-application observation on the
representative Cell dataset in four-panel layout and confirmed that the
history-dependent selected-`none` state and contradictory stale capacity
warning no longer recur. Ordinary linked interaction remains responsive and
settles truthfully across repeated zoom.

The quarantined linked-S0 host-stress workflow was deliberately not rerun and
is not a prerequisite for this correctness closeout. It remains available
only for a future owner-approved diagnosis with explicit host-risk
acknowledgement. No authorized implementation or validation work remains in
this plan.
