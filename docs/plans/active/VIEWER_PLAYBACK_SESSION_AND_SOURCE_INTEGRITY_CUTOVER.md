# Viewer Playback Session And Source Integrity Cutover

- Status: IMPLEMENTED — SESSION/INTEGRITY WORK RETAINED; PRESENTATION
  COORDINATION SUPERSEDED BY THE COMPLETED COMPOSED-SCHEDULER CUTOVER
- Planning requested by owner: 2026-07-31
- Last reviewed: 2026-08-01
- Supersedes the withdrawn temporal-playback implementation attempt retained
  in Git history.

This remains the implementation handoff for the fixed-LOD playback session,
bounded temporal residency, local source admission, lazy integrity, and the
explicit package-integrity audit. Its claims that temporal and spatial
*presentation* are fully independent are superseded by the
[composed presentation scheduler cutover](VIEWER_COMPOSED_PRESENTATION_SCHEDULER_CUTOVER.md).
Owner testing found that the predecessor whole-layout readiness gate could
pause playback during continuous spatial input. The composed-scheduler cutover
subsequently replaced that presentation authority and is now implemented,
automated-verified, and owner product-validated.

## Outcome

The completed cutover will provide:

1. one explicit playback session that selects a sustainable full-volume LOD
   during warmup and keeps that LOD fixed until Pause or Stop;
2. bounded rotating timepoint slots whose resource ownership is independent of
   camera rotation, translation, zoom, linked-plane interaction, and layout;
3. atomic, ready-only presentation of each recorded timepoint, with no coarse
   flash, blank surface, `Loading...` interval, or partial target group;
4. independent temporal and spatial progress: camera input cannot reset or
   rebase playback, and a temporal transition cannot restore an older camera;
5. fail-contained rendering that retains the last complete frame and cannot
   enter a repaint-rate error loop;
6. fast structural admission for ordinary local package opening, with
   per-object/per-brick integrity enforced as data is actually consumed;
7. no automatic whole-package SHA-256 or decoded scientific-content scan on
   ordinary open, idle, project I/O, analysis, or export;
8. an explicit, cancellable full integrity audit whose result is described as
   package/content self-consistency, never scientific authenticity; and
9. honest identity language that distinguishes a declared or computed content
   address, package integrity, and comparison with an independent external
   expected identity.

This plan changes no compression codec, brick or shard geometry, pyramid
semantics, rendering algorithm, temporal interpolation policy, or source TIFF.
It introduces no second renderer, reader, scheduler, cache, project model, or
compatibility path.

## Why The Existing Handoff Is Reopened

The previous playback handoff is not product-correct despite a green mapped
automation result. Owner observation and the current runtime log establish:

- standalone 3D playback repeatedly alternates between the intended playback
  scale and a much coarser navigation rung, while four-panel presentation does
  not show the same oscillation;
- camera movement during playback can create a renderer-global capacity demand
  just above the configured arena instead of reusing the admitted playback
  window;
- camera and temporal changes remain coupled through shared planning,
  publication, and requirement generations;
- an invalid 3D schedule/policy combination can precede
  `linked scope has no prepared render plan`;
- the missing-plan failure is retried on each repaint and freezes rendering
  rather than retaining the last valid frame; and
- the current tests prove some temporal and camera progress but do not prove a
  stable playback LOD, coherent immutable plan ownership, or failure recovery.

The source-verification path has a separate but related false claim and failure
containment defect:

- an automatic background scan resumes after the viewer becomes idle;
- several exact-object, directory-currentness, provisional-promotion, and
  bookkeeping failures collapse into `SourceChanged`;
- that result revokes the source and clears all presentations; and
- an independent read-only audit after the observed incident found the package
  fully consistent with its manifest, so the destructive `SourceChanged`
  conclusion was not supported.

These findings withdraw the earlier playback completion claim. They do not
withdraw the already product-validated spatial navigation work outside
temporal playback.

## Semantic Boundary: Identity Is Not Authenticity

Three concepts must remain distinct in types, UI, documentation, and project
records:

- **Content address:** a typed digest computed over canonical scientific
  values and metadata, or declared by a package. It names content under one
  algorithm; it does not say that the content is biologically correct.
- **Package integrity:** evidence that current encoded objects agree with the
  package's own manifest and checksums. This detects accidental corruption or
  drift; it does not authenticate the producer.
- **External identity match:** evidence that a computed content address equals
  an independently supplied expected address. Only this comparison has an
  external reference, and no such product trust source exists today.

The application must not use `verified scientific identity`, `certified
identity`, or equivalent language for package self-agreement. A package can be
created from the wrong input, by a buggy transformation, or with consistently
wrong self-declared records. Repeating the same package-owned computation
cannot disprove those cases.

The storage-independent `ScientificContentId` value remains a useful content
address and format field. This cut changes the authority claimed for it, not
its canonical bytes or the active experimental package profile.

## Non-Negotiable Invariants

### Playback quality and temporal truth

- Warmup admits one immutable `PlaybackSessionContract`: session generation,
  requested FPS, selected full-volume scale map, active layout target set,
  slot count, and exact CPU/GPU resource ceilings.
- The admitted playback scale map cannot change at a timepoint boundary,
  because data arrived, or because a spatial gesture started or ended.
- A playback timepoint is current only when its complete active-layout bundle
  is decoded, GPU resident, rendered for the latest spatial intent, and
  atomically presented.
- If the successor misses its deadline, the previous same-scale timepoint
  remains visible and the temporal clock applies backpressure. The application
  never publishes a coarser timepoint as a deadline substitute and never
  silently skips a recorded timepoint.
- Pause ends the session contract, retains the final presented timepoint, and
  permits ordinary stationary refinement toward the camera's ideal scale.
- Direct slider seeks are discontinuous and need no cadence guarantee, but
  they retain the prior complete frame until the requested frame is ready.

### Spatial and temporal independence

- `TemporalStreamState` owns the playback cursor, clock, fixed quality, and
  rotating slots. It owns no camera or linked-plane geometry.
- `Spatial3dIntent` owns the current 3D camera and mode-dependent render
  controls. `Linked2dIntent` owns the common XY/XZ/YZ geometry. Neither owns or
  rebases temporal slots.
- Rendering composes the newest applicable spatial intent with the current or
  ready temporal slot. It does not regenerate the slot's full-volume resource
  body when only spatial input changes.
- Single-3D layout creates no linked-plan requirements. Four-panel layout owns
  one coordinated target bundle containing 3D, XY, XZ, and YZ for the same
  timepoint.

### Atomic planning and presentation

- One `PreparedTemporalFrame` owns, as one immutable value, its timepoint,
  session generation, fixed scale map, active target set, requirements,
  renderer plans, schedules, publication policies, and readiness.
- A renderable/readiness flag cannot exist independently of the plan it
  describes. There is no map lookup that can observe `ready` while the plan is
  absent.
- Schedule and publication policy are constructed and validated together.
  Later global coordination may not rewrite one into an incompatible pair.
- Slot state follows one checked progression such as
  `Empty -> Loading -> Ready -> Presented -> Recyclable`. Cancellation may
  retire a non-presented slot, but no transition may partially publish it.
- The visible predecessor and its pins are released only after the successor's
  atomic presentation commit.

### Bounded ownership and capacity

- Playback owns a fixed rotating slot ring, not an ever-growing timepoint
  cache. Dataset temporal length cannot change steady-state memory use.
- Warmup transactionally proves the complete overlap of visible predecessor,
  ready successor, required runway, active layout, hidden renderer candidate,
  and global non-playback obligations.
- Camera input reuses those admitted timepoint bodies. It cannot add another
  full shifted playback union or consume capacity reserved for a later slot.
- Stop, seek, source replacement, layout change, or material display-policy
  change invalidates the session generation and releases only playback-owned
  slots. Shared current-view residency remains governed by the existing global
  owner.
- No fixed scale such as S1 is promised universally. The representative
  workload is expected to admit S1 at 24 FPS, but the general policy selects
  the finest sustainable scale during warmup and then freezes it.

### Failure containment

- Failure to prepare a successor retains the last complete presentation and
  reports one scoped problem. It cannot clear unrelated targets.
- A plan invariant failure cancels and rebuilds only the affected candidate;
  it cannot be retried once per repaint without state change.
- Capacity refusal during warmup may choose a coarser session scale before
  playback starts. Capacity refusal after Playing begins cannot change visible
  scale; it pauses advancement and reports the failed slot.
- Genuine source mutation stops new reads from the affected source generation
  but retains already presented pixels as explicitly stale/frozen evidence.
  It never replaces them with `Loading...`.

### Ordinary source integrity

- Normal open performs bounded metadata/schema/profile admission, safe path
  resolution, directory/object-type inspection, declared-length checks, and
  manifest-structure validation. It does not hash every object or decode every
  scale.
- Every consumed encoded brick/index retains its existing checksum and
  currentness checks. A failing object reports the exact path, phase, and
  reason and fails only consumers that require it.
- Packed min/max/validity facts from an unknown external package are
  acceleration hints until their corresponding payload has been decoded and
  checked. Unchecked hints cannot silently suppress a payload read or declare
  scientific emptiness.
- Fact validation is lazy and bounded within the one storage/runtime path.
  There is no second reader or unbounded permanent per-brick proof map.
- A package produced and validated by the active importer may transfer its
  publication capability within that session. That capability proves the
  publication transaction, not external scientific authenticity.

### Project and analysis behavior

- Rendering, project create/open/save/recovery, analysis, and export do not
  wait for an automatic whole-package scan.
- Projects store the typed content address, optional exact-package pin, and
  locator according to the existing project model, but no transient
  self-verification status masquerades as durable authenticity.
- Analysis validates every resource it actually consumes and fails atomically
  on a read/integrity fault. It does not require unrelated timepoints or LODs
  to be scanned first.
- An externally anchored identity comparison is a future separately approved
  capability. This cut does not add signatures, trust stores, publication
  services, or raw-TIFF revalidation.

## Target Architecture

### Playback session authority

Replace the current combination of transient cursor, independently rebuilt
demand scopes, preview selection, and late policy rewriting with one
application-owned `PlaybackSession`:

```text
Stopped
  -> Warming(PlaybackSessionContract, slot ring)
  -> Playing(PlaybackSessionContract, presented slot, ready/loading slots)
  -> Stopped(final presented timepoint)
```

Warmup evaluates complete candidate scale maps from fine to coarse against the
chosen FPS and exact rotating-window cost. It reserves a small nonzero startup
runway and a bounded refill window. The selected contract is installed once;
each slot is then populated with the same scale map for a different
timepoint.

The temporal clock advances only by presenting the immediate recorded
successor. It does not make a timepoint current before presentation, skip over
an unready slot, or renegotiate quality. The clock records missed deadlines
for diagnostics while applying readiness backpressure.

### Atomic target bundles

One slot prepares only the targets visible in the active layout:

- standalone 3D: one 3D target plan;
- four-panel: one coordinated 3D/XY/XZ/YZ target bundle; and
- linked-2D-only layouts, if later exposed: one linked XY/XZ/YZ bundle.

Layout membership is part of the immutable session contract. A layout change
ends the current session and performs a bounded new warmup; hidden targets are
never kept alive through shared booleans.

The frame coordinator accepts a complete prepared bundle rather than separate
renderability facts and plan-map entries. Atomic-refinement, direct, and
interactive schedules carry their compatible publication policy at creation.
No downstream code rewrites that policy to accommodate a temporal barrier.

### Latest spatial intent composition

Playback slots contain camera-independent complete volume bodies. At render
time, the coordinator uses the latest 3D camera and latest linked geometry.
Spatial input may replace a private render candidate, but it neither cancels
the temporal slot nor changes its requirements or LOD. A temporal successor
uses the latest spatial intent observed before its render cutoff.

This composition removes the current need to combine camera-local visibility
guards with future-timepoint bodies. Ordinary non-playback navigation keeps
its existing progressive spatial behavior.

### Integrity state and optional audit

Replace the current `SourceVerification` lifecycle with two separate concepts:

1. `SourceAdmissionState`, required for ordinary use and completed by fast
   structural admission; and
2. `PackageIntegrityAudit`, an explicit optional operation over the already
   admitted source.

The optional audit reports distinct stages rather than one synthetic
three-step percentage:

- exact encoded-object bytes checked against the package manifest;
- canonical S0 content address recomputed and compared with the declared
  content address; and
- optional acceleration-fact consistency checked for every pyramid brick.

Each stage reports real objects, encoded bytes, bricks, and decoded bytes. The
result may be `self-consistent`, `mismatch`, `cancelled`, or `read failed` with
an exact cause. It must not produce `scientifically verified`, authenticate a
producer, unlock ordinary product behavior, or alter visible presentation.

The automatic idle worker, interaction throttle, provisional-promotion race,
and generic `SourceChanged` invalidation path are deleted. Audit code may reuse
the existing bounded validators behind the explicit operation; the validators
do not remain an implicit application gate.

### Honest UI

Replace the single `scientific identity` verification line with concise,
separate facts:

- `content ID: declared` or `content ID: computed during this import`;
- `structure: admitted`;
- `read integrity: checked as used`; and
- `full integrity audit: not run / running / self-consistent / failed`.

The optional audit has Run and Cancel controls and truthful work progress. A
failure identifies the affected object or stage. No status-line height change
may move playback controls; those controls remain above status text.

## Authority Changes And Deleted Predecessors

The hard cut will:

- make `PlaybackSession` the sole temporal streaming, fixed-quality, and slot
  authority;
- replace independently mutable prepared-plan maps/readiness flags for
  temporal presentation with immutable `PreparedTemporalFrame` bundles;
- delete per-timepoint playback preview-profile selection;
- delete temporal reuse of camera navigation-fallback policy;
- delete post-construction schedule/publication-policy rewriting;
- delete any camera action that clears, recreates, pauses, or rebases playback
  demand;
- delete the automatic `CurrentSourceVerificationService` product lifecycle
  and its idle interaction throttle;
- replace `ScientificIdentityStatus::Verified/Unverified` product semantics
  with honest content-address and integrity-state semantics;
- rename capabilities/events whose `VerifiedScientific` wording claims more
  than publication or self-consistency proves;
- remove project, recovery, analysis, and export gates that wait for the
  automatic full scan;
- delete generic error mapping that collapses promotion bookkeeping,
  filesystem drift, digest mismatch, and missing objects into
  `SourceChanged`; and
- retain the existing hash/identity primitives for import, content addressing,
  format fixtures, explicit integrity audit, and future externally anchored
  comparisons.

No compatibility alias or dual verification path remains after the cut.

## Milestones

### C0 — Claim reset and failing integrated evidence

- Mark the former playback handoff superseded and remove its product-complete
  status from current authorities.
- Add a bounded integration fixture with visibly and semantically distinct
  timepoints.
- Freeze failures for single-3D LOD oscillation, simultaneous camera/playback
  coupling, invalid schedule, missing linked plan, and repaint error storms.
- Freeze that ordinary open followed by idle performs an automatic full scan
  and can destructively invalidate presentation.

Exit: the new tests fail for the observed reasons, not merely because a
controller counter differs.

### C1 — Identity and admission semantic cut

- Introduce separate content-address, admission, per-read-integrity, and
  optional-audit state.
- Cut project/application/UI wording and gating away from `verified scientific
  identity`.
- Preserve locally imported publication transfer without calling it external
  scientific certification.
- Make project recovery and ordinary analysis depend on admitted structure and
  integrity of consumed resources, not a global scan.

Exit: an admitted package can be viewed, saved in a project, recovered, and
analysed without starting a whole-package verifier.

### C2 — Lazy integrity and explicit audit

- Enforce checksum/currentness for every consumed object.
- Prevent unaudited packed facts from suppressing first-use payload validation.
- Add the explicit cancellable integrity audit with real progress and exact
  failure diagnostics.
- Delete automatic background verification, promotion, destructive source
  invalidation, and the idle-resume service.

Exit: idle performs zero audit object hashes and zero audit-only decodes;
accessed corruption fails its consumer precisely; an explicit audit catches
unaccessed corruption without disturbing the visible frame.

### C3 — Playback session and slot ownership

- Introduce the immutable session contract and bounded rotating slot ring.
- Select and freeze one admitted playback scale map during warmup.
- Reserve exact predecessor/successor/window/layout overlap transactionally.
- Refill and recycle slots without changing the selected scale or global
  ownership authority.

Exit: hundreds of temporal transitions retain one selected scale and bounded
steady-state resources independently of total timepoint count.

### C4 — Atomic presentation and spatial independence

- Replace split readiness/plan state with immutable active-layout bundles.
- Compose the latest spatial intent with each temporal slot.
- Construct schedule and publication policy atomically.
- Retain the predecessor until one complete successor bundle is presented.
- Make Pause and direct seek follow the same retained-frame lifecycle.

Exit: camera orbit, translation, zoom, linked interaction, and temporal
advancement proceed simultaneously with no reset, coarse flash, partial
layout, invalid schedule, or missing-plan state.

### C5 — Failure containment and cleanup

- Convert candidate failure into one scoped event and explicit recovery
  transition.
- Prevent unchanged failure state from requesting repaint-rate retries.
- Preserve the last complete target group across candidate, capacity, and
  source-read failures.
- Remove predecessor playback and source-verification types, services, gates,
  labels, and tests in the same cut.

Exit: injected failures retain current pixels, produce one actionable error,
  and either recover or remain stably stopped without an error storm.

### C6 — Product validation and authority closeout

- Run focused application, storage, project, analysis, UI, and renderer tests.
- Run the public PR gate once after focused checks are green.
- Exercise the normal release application on a representative time series and
  the designated mapped GPU/display.
- Update architecture, data-format, current-state, testing, and plan status
  from actual implemented and observed facts.

Exit: implementation, automated verification, and owner product observation
are reported separately and honestly.

## Useful Automated Checks

Automated evidence must observe actual renderer presentation and source work,
not GUI labels or generated input alone.

### Playback

- Record every atomic presentation commit with session generation, timepoint,
  active targets, selected scale map, spatial-intent identity, requirement-body
  identity, and texture revision.
- Require ordered distinct timepoint-bound GPU content across a long playback
  run in standalone 3D and four-panel layouts.
- Assert that every Playing presentation uses the session's frozen scale map;
  no terminal or intermediate fallback may appear at a frame boundary.
- Delay one successor deliberately and prove the prior same-scale frame
  remains visible until the immediate successor is ready.
- Inject independently clocked orbit and zoom throughout playback and require
  both spatial and temporal presentation progress, with no camera reset.
- Exercise MIP, DVR, ISO, voxel-exact, and smooth-linear correctness. Any
  smooth-linear performance limitation is reported separately and cannot
  weaken state-machine correctness.
- Assert a standalone session creates no linked requirements and a four-panel
  session cannot expose different timepoints across its four targets.
- Prove slot count, CPU bytes, GPU payload, requirements, and pending work are
  independent of total dataset length.
- Exercise exact capacity boundaries and prove camera movement cannot allocate
  a second full playback union.
- Enumerate every schedule/policy/target-role combination and reject invalid
  construction before renderer submission.
- Inject a missing/cancelled candidate and require one error event, no blank,
  and no repaint loop.

### Source integrity

- Open and idle an admitted package while recording object hashes, decoded
  bricks, and verifier-worker starts; all audit-only counters must remain zero.
- Save/open/recover a project and execute bounded analysis without a full
  audit.
- Corrupt an unaccessed object in a disposable fixture: ordinary open remains
  structurally admitted, while explicit audit reports the exact object.
- Access that corrupted object: per-read integrity fails the dependent request
  without clearing unrelated presentations.
- Mutate an object between pre-use and post-use currentness checks: retry or
  fail that read deterministically, retain the last frame, and report once.
- Supply a wrong declared content address: explicit content recomputation
  reports mismatch but never calls the package unauthentic or destroys viewer
  state.
- Verify cancellation at exact-hash and decoded-content stages, bounded memory,
  bounded descriptors, no stale completion, and no source mutation.
- Confirm that an unaudited packed fact cannot skip the first required payload
  validation and that a checked fact may use the existing fast path.

## Normal-Product Acceptance

On the representative multi-timepoint workload and designated workstation:

1. Open the package and leave it idle long enough to prove no automatic full
   audit begins and no idle source failure appears.
2. In standalone 3D, start 24 FPS playback from a settled fine view. Record the
   warmup duration and selected scale. Confirm that one scale remains visible
   for the entire playback session with no coarse flash.
3. Repeat at a lower FPS and confirm the independently selected session scale
   also remains fixed.
4. During playback, continuously rotate, translate, and zoom. Confirm temporal
   progression and camera response remain simultaneous and no view resets.
5. Repeat in four-panel mode and confirm all four panels share each presented
   timepoint while 3D and linked spatial controls remain independent.
6. Exercise MIP, DVR, ISO, and both sampling modes for correctness. Do not turn
   the known smooth-linear cost into a broader performance claim.
7. Pause and confirm playback-only resources are released, the final frame
   remains visible, and stationary refinement resumes without `Loading...`.
8. Seek directly to a distant timepoint and confirm the old frame remains until
   the requested complete frame replaces it.
9. Run and cancel the optional integrity audit, then run it to completion on a
   bounded fixture. Confirm truthful progress and zero effect on presentation.
10. Inspect the isolated runtime log: no capacity failure, invalid schedule,
    requirement mutation, missing prepared plan, repeated error, or destructive
    source-verification event is permitted.

Product acceptance is the owner's direct observation of the normal native
application. Automation supports that observation but cannot replace it.

## Risks And Controls

- **Lazy packed-fact validation may add first-use decode work.** Measure cold
  and warm delivery separately; retain only checked fast paths and do not
  restore global scanning to hide a performance regression.
- **A fixed playback LOD may miss cadence on an unusual frame.** Warmup selects
  from exact steady-state costs; exceptional lateness applies backpressure
  rather than visual quality oscillation or silent frame skipping.
- **Layout changes alter target cost.** End and re-warm the session under the
  new immutable target set rather than mutating a live contract.
- **Project identity wording is persisted.** Make one pre-alpha hard cut to
  honest semantics; add no compatibility reader or dual status.
- **An optional audit can still be large.** Keep it cancellable, byte-bounded,
  truthful about work, and user-initiated. It must not run as a hidden product
  prerequisite.
- **A green integrated test can still miss visible flicker.** Record atomic
  presented LOD/timepoint facts and require owner mapped observation before
  product validation is claimed.

## Explicit Non-Goals

- Temporal interpolation, cross-fading, frame synthesis, or playback frame
  skipping.
- Compression, chunk, shard, pyramid, or storage-profile redesign.
- Proving that raw acquisition, preprocessing, biological interpretation, or
  package authorship is correct.
- Digital signatures, remote trust services, release catalogs, or hostile
  filesystem/producer defense.
- Re-running the quarantined linked-S0 host-stress workflow.
- A universal FPS guarantee across all hardware and datasets.

## Completion Standard

The work is complete only when the predecessor playback and automatic source-
verification authorities are deleted, the new invariants are enforced by one
product path, focused and integrated checks pass, the public gate passes, and
the owner confirms the normal mapped product behavior. It is not complete if:

- playback changes LOD after Playing begins;
- a camera change rebuilds temporal residency;
- a ready flag can outlive its prepared plan;
- an error can blank the last complete frame or repeat every repaint;
- ordinary open or idle scans unrequested package payloads;
- project or analysis work waits for a global package scan; or
- package self-agreement is described as scientific certification.

## Implementation Closeout — 2026-07-31

The code cut is implemented. Playback owns one immutable session contract,
fixed per-layer scales, a bounded slot ring, one atomic active-layout target
set, exact CPU/GPU ceilings, retained-predecessor backpressure, and
generation-latched failure containment. Spatial intent remains independent and
is composed with each ready temporal slot instead of rebuilding temporal
residency.

Ordinary package open now performs structural admission and lazy integrity
checks only for consumed data. The automatic verifier and its destructive
source invalidation are removed. A user-requested, cancellable package
integrity audit reports encoded-object, content-address, and packed-fact self-
consistency without claiming scientific authenticity or changing viewer
authority. An admitted package's packed statistics cannot suppress inspection
of a physically present validity payload; the profile's omitted validity inner
remains the canonical all-invalid fill representation.

Focused storage and import-pipeline suites pass, including independent 2D and
3D sentinel-restoration oracles across chunk seams. The public PR gate passes
all policy phases, exact zero-warning Clippy, lane discovery, and all 1,303
applicable unit, contract, and UI tests with retries disabled.

Normal-product acceptance has deliberately not been claimed. No unattended
real-window workflow was used for closeout; the owner's direct exercise of the
normal native application remains the separate final product-observation step
listed above.

## Linked 2D Playback Correction — 2026-07-31

The owner's first normal-product exercise exposed a narrow remaining defect:
pan, zoom, or oblique input in a linked 2D panel could stop visible temporal
progress. No playback command was being issued. A temporal tick advanced the
linked-family identity while visible demand remained pinned to the active
gesture's older spatial revision; linked planning could then replace the
successor with a geometry-local body and strand presentation readiness.

The corrected model keeps the gesture revision only for sample/finish
settlement. Demand and presentation use the latest composed linked-family
revision. During playback, all three linked panels use the session contract's
fixed-scale, geometry-independent full-volume bodies, physically deduplicated
with the other visible targets. A linked spatial change can rebind an in-flight
temporal bundle to the newest geometry without cancelling or restarting the
bundle's resource preparation.

Regression coverage now exercises linked pan, zoom, and oblique sequences
during 24 FPS four-panel playback and requires real temporal advancement during
each input phase. Focused mailbox, demand-planning, in-flight rebase, linked-
wide, playback-wide, and application-wide suites pass. The public repository
gate also passes all policy phases, exact zero-warning Clippy, lane discovery,
and all 1,307 applicable unit, contract, and UI tests with retries disabled.
The owner's normal mapped recheck remains the final product evidence for this
correction and is not replaced by unattended real-window automation.

## Presentation Completion Withdrawal — 2026-07-31

The subsequent owner recheck disproved the completion claim above. Spatial
input no longer rebuilds the temporal slot ring, but every raw 3D or linked
sample still advances a composite display generation, and the playback clock
still waits for the newest required layout group to become current. In
four-panel layout, visible temporal progression can therefore stop throughout
an active gesture and resume only after spatial settlement.

The same recheck exposed two related presentation defects: ending playback can
route the valid visible playback front through a coarser navigation preview
before stationary exact output, and a four-panel temporal planner can validate
its newly prepared delta as if it were the full logical target set, producing
an erroneous incomplete-frame failure when other targets are validly reused.

These findings withdraw the automated and implementation completion claims
for spatial/temporal presentation independence and for failure-free atomic
four-panel assembly. They do not withdraw the implemented fixed playback
scale, bounded slot ring, geometry-independent temporal bodies, source-
integrity correction, or explicit package audit.

The
[Viewer Composed Presentation Scheduler Cutover](VIEWER_COMPOSED_PRESENTATION_SCHEDULER_CUTOVER.md)
is the sole target authority for the corrective presentation architecture,
hard-cut deletion list, meaningful evidence, and owner acceptance. No source
implementation for that corrective plan had started when this withdrawal was
recorded; the successor cutover completed on 2026-08-01. This withdrawal
remains the historical reason for the separate correction, not a current
product limitation.
