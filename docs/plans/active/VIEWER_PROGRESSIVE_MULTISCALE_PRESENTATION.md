# Viewer Progressive Multiscale Presentation Refactor

- Status: IMPLEMENTED, AUTOMATED-VERIFIED, AND OWNER PRODUCT-VALIDATED — 2026-07-30
- Planning requested by owner: 2026-07-30
- Implementation authorized by owner: 2026-07-30
- Last reviewed: 2026-07-30

This document is the authoritative implementation and handoff plan for the
progressive multiscale presentation refactor. It supersedes the finite
linked-plane guard as a presentation boundary while preserving the adaptive
LOD, global-capacity, placeability, single-residency, and dedicated-renderer
architecture recorded in [Current State](../../CURRENT_STATE.md).

## Corrective Amendment — Same-Intent Body Cutover

- Status: IMPLEMENTED, AUTOMATED-VERIFIED, AND OWNER PRODUCT-VALIDATED — 2026-07-30
- Trigger: owner-observed transient S5 fallback while changing viewer state
- Concrete fault evidence: the normal mapped viewer repeatedly logged
  `GPU display refresh failed` with
  `the requirement set changed within one frame generation`

### Outcome And Scope

When asynchronous planning finishes for an already accepted user-intent
revision, the renderer must be able to replace that revision's provisional
immutable requirement body with the latest prepared immutable body. The
replacement must render without waiting for another mouse, keyboard, camera,
layout, layer, timepoint, or settings change. This corrective cut is confined
to the coordinated renderer's requirement-body transition, its lease and
presentation regressions, and the owning documentation.

The safety fallback remains intentional: a changed view may render immediately
from its resident navigation floor while target data is unavailable. The bug
is an erroneous refusal at the subsequent provisional-to-prepared body
cutover, which can prolong that coarse display and emit a renderer error.

### Invariants And Authority

- `RenderIntentMailbox` remains the only allocator of monotonic
  `FrameIdentity`; asynchronous plan completion does not manufacture a second
  semantic input revision.
- Immutable requirement-body identity remains the resource-generation
  authority within one accepted intent revision. Coverage, exactness, dirty
  detection, pending-offer relevance, and rendered-body bookkeeping remain
  tied to that exact body.
- A replacement body is retained before its predecessor is released, so
  overlapping resources remain continuously pinned and no false
  unpin/re-pin transition is created.
- Older `FrameIdentity` values remain stale and are rejected.
- A body replacement cannot rebind coverage from another body, bypass catalog
  or lease validation, publish unavailable scientific data, or weaken the 3D
  exact-only candidate swap.
- No extra hash, provenance token, body-generation counter, renderer, cache,
  page table, queue, or application authority is added.

### Replaced Behavior And Deletion

Delete the renderer rule that treats a different immutable requirement body
or prefetch role under the same `FrameIdentity` as
`RequirementSetChanged`. Do not replace it with a second frame revision. The
existing body-aware lease replacement, coverage construction, presentation
dirty check, and last-rendered body identity become the single transition
path.

### Risks And Checks

- **Wrong-body coverage:** regressions must prove coverage is constructed for
  the successor body and never rebound across different bodies.
- **Pin discontinuity:** regressions must prove overlapping keys remain pinned
  and only true set differences change pin state during a same-frame cutover.
- **Suppressed repaint:** regressions must prove a different same-frame body is
  dirty and publishable once its first-useful coverage is available.
- **Stale publication:** existing older-frame rejection and latest-only
  application tests must continue to pass.
- **3D regression:** exact-only hidden-candidate checks and the ordinary smooth
  resident S3/S2 path remain unchanged.
- **Product evidence:** run focused renderer and application regressions,
  public Rust/policy gates, then a bounded normal-viewer exercise and inspect
  the new log interval for recurrence. Do not run the quarantined linked-S0
  desktop-driving host-stress workflow.

### Implementation And Verification Result

The renderer now accepts a prepared immutable-body replacement under the
mailbox-owned semantic frame revision. Presentation currentness and retained
frame adoption compare the exact requested body and prefetch role, and the
same-frame regression guard suppresses only regressions within that exact
body. The cutover therefore cannot be mistaken for already-presented
predecessor pixels or be suppressed merely because both bodies belong to the
same input revision.

Focused renderer and application suites passed, including a trusted Vulkan
regression that renders the provisional linked body, publishes the prepared
successor without changing the mailbox revision, proves all three linked
targets submit first-useful successor coverage, and retains the independent
fine-scale pixel-oracle comparison after settlement. The public PR gate also
passed with 1,260 unit, contract, and UI tests. The owner then repeated the
normal four-panel interaction and observed only a millisecond-scale S4-or-S5
safety fallback before the proper scale returned, rather than the former
indefinite coarse display. That mapped observation closes the reported
cutover defect.

Milestones M0 through M6 were implemented and automated-verified on
2026-07-30. [Current State](../../CURRENT_STATE.md) and
[Architecture](../../ARCHITECTURE.md) now own those implemented facts. This
plan remains the canonical implementation and evidence handoff. The owner's
normal mapped four-panel observations are the product-visible evidence;
automated results below are not reinterpreted as monitor evidence.

## Outcome

Interaction must remain visually responsive because every accepted camera or
linked-plane sample can render its **current geometry** from a safe resident
LOD. Finer data refines asynchronously and never blocks that first current
frame.

This is not permission to make target fidelity slower. If the selected target
is already complete and resident, the renderer uses it directly. Progressive
fallback is a rescue path for unavailable target data, not a mandatory coarse
flash, an intermediate-scale loading ladder, or a second renderer.

The required product result is:

- linked XY/XZ/YZ immediately render the latest common plane geometry from the
  finest resident valid scale available for each complete sample footprint;
- linked panels visibly refine target regions as target bricks arrive;
- 3D MIP, DVR, ISO, and Mixed remain coherent uniform-scale renderings and
  replace a coarser complete view only through one atomic whole-target swap;
- resident target data retains the existing direct rendering path, especially
  the already-smooth S3 and S2 3D cases;
- rapid target changes cancel or demote obsolete work and directly pursue the
  latest requested scale;
- crossing any number of linked-plane guard radii cannot freeze input, retain
  old geometry, or cause a later catch-up jump; and
- target completion, geometry currentness, provisional fallback, and exact
  settlement are reported as distinct facts.

## Non-Negotiable Invariants

### One Authority

- The existing renderer-global residency owner, sparse directory, payload
  arena, frame coordinator, and fixed presentation targets remain the only
  GPU authorities.
- There is no CPU product renderer, legacy fallback renderer, per-panel
  physical cache, duplicate page table, or compatibility path.
- Coarse fallback uses ordinary catalog bricks in the same canonical residency
  and shader sampling path as target bricks.

### Scientific Meaning

- Missing occupied data means unavailable/loading data. It is never converted
  to scientific zero or transparent valid data.
- Invalid/no-data at a finer scale is an authoritative scientific result and
  must not fall through to a coarser value.
- Voxel-exact sampling selects one scale for that sample.
- Smooth-linear sampling selects one scale for the entire interpolation
  footprint. If any required tap is missing at that scale, the whole footprint
  retries at a coarser scale; it may never mix taps from different levels.
- Exports, captures, picks, readouts, and analyses that require exact target
  fidelity must wait for exactness or explicitly report the actual scale.

### Bounded And Latest-Only

- Every visible layer retains one complete, globally charged navigation floor
  at its coarsest catalog scale. Fine work may not evict that floor.
- The navigation floor, target plane, target guard, current fronts, hidden 3D
  candidate, and transition overlap all remain inside the existing global
  resource and payload limits.
- If the catalog minimum itself cannot fit, the product reports the typed
  minimum-capacity failure; it does not pretend to guarantee continuous
  navigation.
- Planning, decoding, uploads, and publication remain bounded, cancellable,
  generation-aware, and stale-suppressing.
- A newer S3→S2→S1→S0 request does not wait for S2 or S1 to finish. Reusable
  resident work stays cached, but only the latest target may publish.
- Background refinement uses the existing bounded queue and upload envelope
  and cannot create an unbounded per-brick submission or repaint storm.

## Presentation Contract

### Linked 2D

Each installed linked-panel render body contains:

1. a complete full-volume coarsest-scale navigation floor for every visible
   layer, ranked as the complete first-useful prefix;
2. the directly selected target-plane resources, ranked next as refinement;
   and
3. an optional bounded fine-scale rolling guard, ranked last as dormant
   prefetch.

The scale chain in renderer control is the actual ordered catalog chain from
the selected target through every coarser level. Intermediate levels are
eligible when already resident, but they are not mandatory load dependencies
and cannot delay direct target loading.

For every plane pixel and visible layer:

1. try the selected target scale;
2. if the page or complete interpolation footprint is missing, try the next
   coarser catalog level;
3. continue to the retained navigation floor;
4. stop immediately on valid or invalid scientific data; and
5. report missing coverage only if no retained level can cover the footprint.

Because the complete navigation floor is the first-useful prefix, partial fine
residency can never create dark holes. Target bricks may replace fallback
regions in visible bounded batches as they arrive.

XY, XZ, and YZ continue to share one canonical cross-section geometry and one
latest interaction revision. Their pixel fidelity may be temporarily mixed by
region, but their geometry may not diverge.

### Linked Rolling Guard

The fine-scale guard is an optimization window, not a cage:

- comfortably contained motion reuses the installed fine body;
- near the guard edge, the latest-only planner prepares a new overlapping
  fine window while the current exact fine body remains renderable;
- crossing before the new window is ready immediately renders the latest
  geometry from the resident multiscale body, ultimately the navigation floor;
- useful fine pages remain in global residency and can be reused by the next
  body;
- an obsolete completed window cannot publish older geometry; and
- settling after sustained rotation loads and publishes only the latest exact
  target.

The old fixed approximately 0.45-radian envelope therefore remains a bounded
fine-prefetch unit, but no longer defines the maximum responsive rotation.

### Coherent 3D

3D keeps one uniform scale per visible layer and one coherent frame:

- current camera geometry renders immediately at the finest complete uniform
  scale already available;
- an incomplete finer target remains hidden;
- the whole required current-view target swaps atomically only after its
  visible resources and required sampling halos are complete;
- a complete intermediate uniform level may publish opportunistically, but
  no intermediate level is a prerequisite for the latest target; and
- target-ready input rebinds and renders the target body directly, without a
  coarse flash, additional decode/upload/allocation, or hidden staging pass.

Naive per-brick 3D mixing is forbidden. The stored pyramid is mean-reduced, so
mixing levels within rays would make MIP compare maxima of means with fine
samples, alter nonlinear DVR opacity and step semantics, create ISO
crossing/gradient discontinuities, and propagate all three defects into Mixed.

Screen-tiled coherent 3D refinement is outside this cut. It may be considered
later only if measured atomic whole-target latency remains unacceptable.

## Truthful Product State

The application and inspector must distinguish:

- current geometry versus stale geometry;
- selected target scale;
- finest and coarsest eligible fallback scales when one shown scalar cannot be
  proved;
- coarsest fallback/navigation scale;
- target resources available versus target resources required;
- provably uniform fallback, conservative fallback range, mixed
  target/fallback, and uniform target display;
- refinement pending;
- exact target settlement; and
- adaptive capacity selection versus temporary residency refinement.

For mixed linked output, a single `shown sN` value is false. The UI reports a
range such as `mixed s0–s3`, together with target completion. When no target
page is resident but shared global residency makes intermediate fallback
levels eligible, it reports a conservative range such as `fallback s1–s3`
instead of falsely claiming the S3 floor is the only level shown. A
progressive frame is current geometry with provisional fidelity, not stale
geometry.

3D continues to report one shown uniform scale because mixed-scale 3D
publication remains forbidden.

## Authority Changes

### Render API

- Add one immutable per-layer ordered scale-chain contract to prepared and
  frame-bound render requirements.
- Validate one target-first, strictly coarser chain for every visible layer.
- Carry explicit target and fallback coverage facts instead of inferring a
  single scale from an arbitrary first resource key.
- Keep frame coverage and exactness tied to the immutable requirement body.

### Demand Planner

- Generalize the existing full-volume navigation-floor planner so linked 2D
  can retain the same semantic floor without a second physical residency.
- Merge that floor before each panel's selected target and optional guard.
- Record separate first-useful and required-prefix boundaries.
- Remove transient selected-LOD anchoring; each worker request plans the
  latest projected target directly.
- Keep aggregate selection and placeability fallback generic across all
  catalog levels.

### Renderer

- Add plane-only multiscale control records after the existing fixed layer
  records. Volume control size and shader behavior remain unchanged.
- Add a dedicated plane sampling module that performs target-to-coarser lookup
  and whole-footprint fallback.
- Keep the global directory, page records, payload buffers, transfer path, and
  frame coordinator unchanged as physical authorities.
- Permit linked targets to publish every useful current-geometry frame after
  their complete navigation prefix is resident.
- Keep hidden 3D refinement exact-frame-only and atomically promoted.

### Application

- Treat navigation-floor renderability separately from exact installed demand
  identity.
- Mark current linked geometry renderable outside the fine guard while still
  submitting latest-only fine planning.
- Trigger overlapping fine-window planning near guard boundaries.
- Prefer a complete resident 3D target body over its navigation floor.
- Preserve exact-currentness and settlement rules: provisional fallback cannot
  satisfy target settlement.

## Replaced Behavior And Deletions

The cut removes these behaviors rather than leaving parallel branches:

- missing plane pages becoming dark/transparent holes;
- one-resource first-useful plane publication;
- exact-frame-only linked interaction whenever old settled pixels exist;
- the finite fine guard acting as the only renderability proof;
- transient linked LOD anchoring that delays direct pursuit of a newer target;
- inferring renderer scale from the first canonical key in a multiscale body;
- calling mixed-scale linked output one scalar shown scale; and
- unconditional 3D navigation-floor publication when a complete target body
  can be rebound directly.

The dedicated volume shaders, atomic hidden 3D candidate, adaptive selector,
global placeability recovery, and quarantined host-stress workflow are not
replaced.

## Milestones

### M0 — Plan And Contract

- Record this plan before source changes.
- Add focused contract tests for scale-chain validation, first-useful versus
  target completion, and truthful mixed fidelity.

### M1 — Linked Navigation Floor

- Build one complete coarsest full-volume floor per visible layer and merge it
  into every linked panel body.
- Rank floor, target primary, and guard without duplicates.
- Charge the exact final union and fail closed when the minimum cannot fit.

### M2 — Plane Multiscale Sampling

- Encode the ordered per-layer catalog chain in plane-only control.
- Implement nearest and whole-footprint smooth-linear fallback.
- Prove missing falls back, invalid does not, interpolation never crosses
  levels, and volume compilation units do not call plane fallback.

### M3 — Progressive Linked Publication

- Publish current geometry as soon as the complete floor prefix is resident.
- Refine as target resources arrive.
- Preserve exact target currentness only after the complete selected body is
  shown.
- Bound refresh and upload work through the existing coordinator.

### M4 — Unbounded Interaction And Rolling Fine Window

- Reuse exact fine data while comfortably inside the guard.
- Prepare an overlapping window near its boundary.
- Continue through boundary crossings with coarse/current geometry and no
  old-geometry catch-up.
- Keep only the latest window publishable.

### M5 — 3D Fast Path And Atomic Refinement

- Preserve uniform-scale volume shaders and exact hidden swaps.
- Prefer a complete resident selected body over the floor for new cameras.
- Add a regression proving ready S3/S2 reuse performs no residency transfer or
  coarse staging.
- Add a regression proving incomplete finer 3D data cannot publish a mixed
  volume.

### M6 — Product Truth And Verification

- Report linked uniform fallback, mixed refinement, and uniform exact target
  honestly.
- Format and run focused render-API, planner, renderer, application, and UI
  checks.
- Run the public PR verification gate once at integration.
- Do not run the quarantined mapped linked-S0 automation unattended.
- Product validation is the normal owner-controlled mapped viewer on the
  representative dataset and GPU. Automated checks cannot claim monitor
  continuity; the owner completed this observation on 2026-07-30.

## Acceptance Checks

The non-interactive regression set must independently establish:

- a plane sample with missing S0 data returns resident S1/S2/S3 data;
- a resident invalid S0 sample remains invalid and never exposes coarse data;
- a smooth-linear footprint with one missing fine tap retries wholly at one
  coarser scale;
- a complete floor plus zero target bricks yields current scalar fallback when
  only one fallback level is eligible, or a conservative fallback range when
  shared intermediate levels are eligible;
- partial target residency yields current mixed linked fidelity without
  missing coverage;
- complete target residency yields current exact target fidelity;
- repeated geometry samples beyond multiple fine-guard radii remain
  renderable and stale worker completions cannot publish;
- a near-boundary sample requests an overlapping latest fine window;
- rapid target changes leave only the latest selected target publishable;
- complete resident 3D target data takes the direct body path;
- incomplete 3D target data leaves the prior complete uniform frame visible;
  and
- MIP, DVR, ISO, Mixed, and Pick shader sources contain no plane multiscale
  fallback call.

The final mapped product check must exercise ordinary 4-panel interaction:

1. confirm the existing smooth S3 and S2 3D cases do not regress;
2. rotate, translate, and zoom linked planes at a fine selected scale;
3. observe current coarse geometry during unavailable fine regions and visible
   target refinement without dark holes;
4. cross the former guard boundary repeatedly without a freeze or catch-up
   jump;
5. stop and confirm exact target settlement; and
6. compare GUI target, mixed/fallback, exactness, and currentness to the visible
   result.

The mapped check is deliberately owner-controlled because the existing
linked-S0 desktop-driving diagnostic previously froze the host and cannot
observe compositor/monitor presentation reliably.

The owner completed the normal four-panel observation after the progressive
refactor and reported the view working as intended. After the same-intent
cutover correction, a repeated mapped check observed at most a
millisecond-scale S4-or-S5 safety image during change, followed immediately
by the proper scale, with no indefinite coarse display.

## Risks And Containment

- **Floor too large:** fail at the typed minimum-capacity boundary; never
  silently weaken fidelity or overcommit memory.
- **Control growth:** scale records are appended only for Plane; volume control
  and ready-target cost remain unchanged.
- **Shader cost:** plane lookup is bounded by the catalog's checked 64-scale
  maximum and normally terminates at the target. The common S3/S2 target-ready
  case performs one lookup.
- **Refinement storm:** residency additions remain batch-driven by the existing
  upload and frame coordinator envelopes.
- **Mixed scientific semantics:** only 2D display pixels may mix regions, never
  interpolation taps, rays, picks, exports, or exact analysis.
- **Stale publication:** immutable body identity and latest intent revision
  remain mandatory at every installation and presentation boundary.
- **Performance regression hidden by tests:** no automated result is described
  as product validation; the separate owner real-display observation remains
  explicit above.

## Completion Language

- **Implemented** means the hard-cut code and documentation exist.
- **Automated-verified** means the named non-interactive checks pass.
- **Product-validated** requires the normal mapped viewer exercise above, or an
  explicit owner waiver.

The plan may be marked complete only when all implemented facts are reflected
in Current State, obsolete behavior is deleted, focused and public checks
pass, and the remaining product-validation status is stated without
conflating it with automation.

As of 2026-07-30, the implementation, deletion, documentation, focused
backend-neutral tests, trusted headless Vulkan fallback fixture, and public
policy/Rust gates pass. The owner also completed the mapped four-panel
exercise and confirmed that the corrective S4-or-S5 fallback no longer
lingers. No authorized implementation or validation work remains in this
plan.
