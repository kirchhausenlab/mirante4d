# Viewer Resident 3D Navigation Ladder Plan

- Status: IMPLEMENTED, VERIFIED, AND OWNER PRODUCT-VALIDATED
- Planning requested by owner: 2026-07-30
- Implementation authorized by owner: 2026-07-30
- Implementation completed: 2026-07-31
- Last reviewed: 2026-07-31

This document is the authoritative implementation and handoff plan for
replacing the binary 3D navigation choice—selected target or terminal
geometry—with one bounded resident ladder of complete, native-resolution
volume bodies.

The completed mapped Cell closeout retained S6, S5, S4, and S3 full-volume
rungs plus the exact S2 target. At the recorded 1280×720 profile the
controller selected S4, kept S6/S5 eligible, rejected resident S3/S2 as
interaction-unsafe, completed all 60 normal-product commands, and reported
zero renderer validation errors.

It extends the implemented
[native-resolution navigation plan](VIEWER_NATIVE_RESOLUTION_NAVIGATION.md)
and preserves the implemented
[GPU-memory and asynchronous-refinement plan](VIEWER_GPU_MEMORY_AND_ASYNC_REFINEMENT.md).
It does not restore reduced-resolution output, visible timing probes, a second
renderer, a second residency cache, or mixed-scale 3D frames.

## Problem

The current controller can rank several preview profiles, but the application
offers only two bodies:

1. the complete selected target, when it is resident and geometrically valid;
2. the dataset's complete terminal body otherwise.

For the representative Cell dataset the terminal body happens to be S6. A
camera gesture can therefore change a detailed exact image directly into S6
even when complete S3, S4, or S5 bodies would be inexpensive to retain and
safe to render. S6 is not a required interaction quality; it is only the
geometry-bounded emergency floor.

The previous gesture policy also records only “prefer target” rather than the
chosen scale configuration. If the target is unavailable, it selects candidate
zero even when a safe intermediate body exists.

## Outcome

For the active timepoint and visible layers, the viewer will:

1. retain the terminal full-volume configuration as the mandatory navigation
   floor;
2. retain a contiguous sequence of successively finer complete full-volume
   configurations while one explicit navigation-tail budget permits;
3. offer every complete GPU-resident configuration, plus a complete current
   target, to the one presentation controller;
4. reject navigation candidates finer than the current selected target;
5. choose the finest candidate inside the fixed native-resolution interaction
   work envelope;
6. freeze the chosen per-layer scale configuration for one gesture;
7. remain on that preview after release until the selected exact candidate is
   ready; and
8. atomically promote the exact candidate once, through the existing hidden
   row-batch path.

The terminal body remains available when no intermediate candidate is ready or
safe. It is no longer the normal consequence of an unfinished finer target.

## Non-Negotiable Invariants

### One Data And Residency Path

- Every ladder resource is ordinary canonical pyramid data.
- `ResidencyOwner` remains the sole GPU payload, allocation, pin, eviction,
  and directory authority.
- The ladder is one aggregate globally accounted requirement union, not an
  app-owned cache or a parallel renderer path.
- Source data, package format, scale generation, brick identity, validity,
  sampling, transfer, MIP, DVR, ISO, and Mixed semantics do not change.
- The terminal candidate is mandatory even when its own bytes exceed the
  optional-tail allowance.

### Bounded Retention

- The optional navigation tail receives at most one quarter of the configured
  logical GPU payload allowance and never more than 512 MiB.
- It receives at most one quarter of the global presentation-resource limit
  and never more than 16,384 resources.
- The terminal body is admitted first. Each finer rung is admitted atomically
  only when its deduplicated aggregate body fits both tail limits and the
  existing exact global requirement union.
- Rungs are coherent per-layer scale maps. Every visible layer advances by at
  most one catalog level between adjacent rungs; a body never mixes bricks
  from different scales within one layer.
- The terminal prefix is first useful. Optional finer rungs are admitted after
  visible required work and cannot delay the first responsive frame.
- Playback remains under the existing global budget and cannot make the
  active-timepoint ladder unbounded.

### Stable Native-Resolution Presentation

- Every preview extent equals the physical 3D panel extent.
- GPU memory fit determines which bodies may remain resident. It does not by
  itself certify interactive rendering cost.
- The existing deterministic work calculation decides whether a complete
  resident body is interaction-safe.
- A gesture records the selected per-layer scale configuration, not an array
  index or a target/floor boolean.
- Resource arrival and timing observations cannot upgrade or downgrade that
  choice during the same gesture.
- If a camera-local exact body ceases to cover the gesture, the controller may
  make one monotonic change to the finest complete safe full-volume rung. It
  cannot jump to the terminal body while a finer safe rung is available, and
  it cannot switch back during that gesture.
- Settlement preserves the exact visible preview until one exact atomic
  promotion. Partial hidden rows remain private.

### Truthful Diagnostics

- The normal inspector reports the shown preview level and whether it is the
  finest complete interaction-safe candidate.
- Runtime diagnostics report every bounded candidate's active-layer level,
  resource count, payload bytes, native work units, completeness/residency,
  work-envelope result, target-quality eligibility, and selection state.
- Diagnostics distinguish “not resident,” “finer than target,”
  “interaction-unsafe,” and “selected”; they do not describe every fallback as
  S6 or as a capacity failure.

## Architecture

### Planning And Residency

The demand worker constructs the ladder from the coarsest visible-layer scale
map. For each next rung, every layer that has a finer catalog level advances
one step. The worker plans the complete full-volume body, merges only new
logical resources into the aggregate tail, and stops at the first rung that
violates the optional-tail or global-union limit.

The aggregate requirement order is:

```text
complete terminal body
  -> new resources for next complete rung
  -> new resources for each later complete rung
```

The terminal body is the required first-useful prefix. Finer rungs are a
bounded resident prefetch suffix. Exact camera target resources retain their
existing contribution priority and optional visibility guard. The selected
target continues to retain the aggregate navigation tail through ordinary
global accounting, so exact promotion cannot discard the ladder.

Each rung owns an immutable exact selection body containing exactly one scale
per visible layer. Its renderer wrapper reuses the aggregate canonical
residency set but ranks that exact rung first and marks every other aggregate
resource as a permanently dormant prefetch suffix. Its explicit volume scale
chain contains only the selected rung. Consequently the sole residency owner
can upload the aggregate terminal-to-fine while no coarser or finer rung can
enter this wrapper's coverage, control, pixels, or prefetch promotion.

### Candidate Selection

The application supplies the complete candidate set in terminal-to-fine
order. For each candidate it builds the existing `VolumeWorkloadProfile` at
the current physical extent and camera. Ordinary selection requires exact
candidate-body GPU residency. Only candidate zero may bootstrap a cold
renderer when its exact terminal payload is retained on the CPU; it remains
reported nonresident and cannot present until upload completes. The
controller:

1. excludes incomplete or nonresident bodies;
2. excludes a rung finer than the current selected target for any visible
   layer;
3. evaluates the fixed native-navigation work envelope;
4. selects the finest remaining safe body;
5. uses the terminal body only if no finer candidate qualifies.

A complete target body participates as the final candidate. Equal scale
configurations are deduplicated in favor of the complete full-volume rung.

The first sample of a gesture stores the chosen layer-scale map. Later samples
rebuild the camera-dependent work profile for that same map. If that map is a
camera-local target and disappears, the controller stores the finest safe
resident full-volume replacement and does not upgrade again until the next
gesture.

### Diagnostics

The presentation controller retains one bounded latest candidate report. The
right-side fidelity display gives a compact summary; the runtime diagnostics
panel exposes one row per candidate with its rejection or selection reason.
Product automation serializes the same facts so a mapped run can prove that a
safe intermediate candidate was actually selected rather than infer it from a
scope scale.

## Hard Cut And Deleted Behavior

Implementation deletes:

- the two-entry preview candidate vector;
- the boolean “use navigation floor” request routing;
- gesture state that records only `prefer_target`;
- unconditional candidate-zero selection when an exact target is unavailable;
- comments and diagnostics that equate the navigation preview with the
  terminal body; and
- the unimplemented implication that a retained “coarse tail” already offers
  intermediate volume previews.

There is one ladder authority after the cut. No hidden binary fallback path
remains.

## Risks And Controls

- **Tail retention can crowd exact work.** The independent quarter-budget and
  512 MiB/16,384-resource caps bound this cost; every candidate also passes the
  existing global-union check.
- **A finer resident body can still be slow.** Residency and work
  classification remain separate; no VRAM-derived frame-rate assumption is
  introduced.
- **Resource arrival can cause visible LOD flicker.** The chosen scale map is
  frozen for the gesture and retained through settlement.
- **Multiple visible layers can diverge.** A rung is one coherent map advanced
  together and the work profile sums every visible layer.
- **Cold startup could wait for the whole tail.** The terminal wrapper ranks
  only its own exact body as first-useful. Optional rungs are a dormant
  lower-priority suffix; a CPU-retained terminal prefix alone may initiate the
  bounded upload path.
- **Existing exact promotion could release the ladder.** Target bodies retain
  the aggregate tail and promotion rebinds every rung to the promoted scope
  before releasing the predecessor.

## Work Packages

### L0 — Plan And Authority — Complete

- Record this approved plan before source changes.
- Link it from the documentation index.

### L1 — Bounded Ladder Planning — Complete

- Add coherent full-volume rung planning and aggregate deduplication.
- Apply the explicit byte/resource tail bounds and existing global-union
  admission.
- Preserve the terminal first-useful prefix and exact target/guard priority.
- Reuse an installed validated ladder as the next camera plan's baseline.

### L2 — Prepared Bodies And Residency — Complete

- Prepare one immutable render body per rung.
- Install every body against the one aggregate scope requirement handle.
- Preserve/rebind the ladder across exact staging and promotion.
- Delete the single navigation-floor render-plan field.

### L3 — Stable Candidate Selection — Complete

- Replace target/floor candidate construction with the complete bounded set.
- Select the finest complete target-eligible interaction-safe candidate.
- Freeze the per-layer scale map for a gesture and implement one-way loss of a
  camera-local target.
- Keep preview-to-exact settlement atomic.

### L4 — Diagnostics — Complete

- Add compact inspector summary and candidate-level runtime/product facts.
- Name rejection reasons truthfully.

### L5 — Focused Verification — Complete

- Prove ladder construction, byte/resource bounds, terminal-first ordering,
  coherent layer maps, and installed-baseline reuse.
- Prove S4/S5-like intermediate selection rather than terminal selection when
  the work envelope permits it.
- Prove unsafe and incomplete candidates are rejected, target-quality bounds
  are honored, and one gesture never upgrades after resource arrival.
- Prove exact promotion retains the ladder and one atomic exact transition.
- Run formatting, focused app/application/UI tests, documentation checks,
  trusted Vulkan checks affected by presentation-body ownership, and the
  repository PR gate.

### L6 — Product Validation — Complete

- Run the normal release `representative_native_navigation` scenario on the
  representative Cell dataset and selected Vulkan adapter.
- Exercise four-panel and standalone 3D, voxel-exact and smooth-linear MIP,
  DVR, and ISO, camera orbit/zoom, interrupted refinement, and settlement.
- Record candidate levels, payloads, work decisions, selected preview, native
  extent, validation errors, and final exact/current state.
- Do not run the quarantined linked-S0 workflow.

## Acceptance

Implementation is complete when:

- a dataset with eligible intermediate levels installs more than the terminal
  candidate within the declared bounds;
- the normal preview chooser receives the complete installed candidate set;
- the finest complete target-eligible safe body is selected;
- no gesture changes preview scale because a resource or timing observation
  arrives;
- loss of a camera-local exact body falls to the finest safe resident rung,
  not unconditionally to terminal;
- exact promotion preserves the aggregate ladder and performs one atomic
  preview-to-exact transition;
- diagnostics expose candidate admission and selection reasons;
- focused, trusted changed-boundary, documentation, and broad checks pass; and
- the normal mapped product is exercised, with the visible result reported
  separately from automated evidence.

Closeout evidence on 2026-07-31 includes the full 288-pass app library suite
with eight hardware/developer-local cases ignored, the focused trusted Vulkan
atomic-refinement test, documentation and repository gates, all 1,298
PR-lane unit/contract/UI cases, and the mapped Cell report described above.
The owner then exercised the normal application and reported that the
four-panel and 3D navigation behavior works as expected.

No universal hardware-performance claim follows. The supported product
boundary remains the current Vulkan workstation guideline and 1920x1080
maximum framebuffer.
