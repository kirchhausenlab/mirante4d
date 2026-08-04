# Viewer GPU Correctness Campaign Follow-Up

- Status: IMPLEMENTED — CLEAN TRUSTED CORRECTNESS PASSES; PERFORMANCE PREFLIGHT AND OWNER ACCEPTANCE OPEN
- Investigation requested by owner: 2026-08-03
- Flexible-panel publication policy selected by owner: 2026-08-03
- Full implementation authorized by owner: 2026-08-03
- Failing campaign commit: `a6ef07aea1ea04bddeb77efd7ad9cbd0f97a236d`
- Failing campaign tree: `e63dd568ae8a7f315301cf18ad8e7db60f63c3e1`
- Last reviewed: 2026-08-03
- Scope: the six failures exposed by the first complete trusted-GPU
  correctness execution, the shared asynchronous test-driving defect they
  exposed, exact failed-run attribution, and a future-safe active-panel
  publication model.

This plan recorded the agreed solution before production or test code changed.
It is a corrective follow-up to the implemented
[GPU testing refactor](VIEWER_GPU_TESTING_REFACTOR.md), not a replacement for
that plan's correctness/performance separation, hardware policy, thresholds,
or baseline lifecycle.

The plan also makes one narrow forward-looking architectural cut. The current
product still exposes only standalone 3D and the fixed four-panel layout, but
the presentation model introduced here must be able to represent any bounded
visible subset of 3D, XY, XZ, and YZ. This does **not** implement panel
open/close controls, arbitrary layout editing, or a new UI. It prevents the
correctness repair from hard-coding another fixed trio or four-panel
assumption that would immediately become a predecessor when flexible layouts
arrive.

## Evidence Baseline

The trusted command ran from a clean immutable checkout on the designated
NVIDIA GeForce RTX 3070 Ti Laptop GPU through Vulkan, with exact inventory
reconciliation, serial execution, and zero retries. The native Nextest summary
reported 25 selected cases, 19 passing cases, and the following six failures.
The lane as a whole failed and therefore creates no trusted correctness pass.

| ID | Failing case | Observed boundary | Classification |
| --- | --- | --- | --- |
| C1 | `exact_transient_cross_section_updates_all_linked_panels_before_finish_and_empty_clears_them` | The test expects durable gesture completion to allocate a newer frame even though the implemented contract preserves the final transient frame identity. | Stale test contract; production behavior is correct. |
| C2 | `incremental_linked_zoom_pixels_match_direct_fine_cpu_oracle_after_one_settlement_plan` | The test makes one direct render call while all required shader work envelopes are still asynchronously pending, so it observes no successor submission. The current structure also does not make out-of-order linked-envelope readiness a sufficient proof of one atomic visible linked publication. | Test phase defect plus a real publication-policy gap. |
| C3 | `coordinated_four_target_resident_cutoff_has_real_pixels_one_submit_and_idle_zero` | The test supplies three physical requests while declaring a four-member atomic group and expects preparation without publication. The renderer now correctly rejects the malformed group. | Stale test contract; renderer validation is correct. |
| C4 | `coordinated_target_pins_are_union_scoped_and_layout_retirement_releases_them` | A recently rebound requirement body retains its new age only on its pin cohort. Replacement preflight ranks the outgoing payload by its older materialized resource age and evicts the just-revisited payload. | Production residency/LRU authority defect. |
| C5 | `rays_beyond_the_removed_16384_sample_cap_reach_the_far_voxel` | The work-envelope proof charges an opposite volume boundary that is provably unreachable in the ray direction, exceeds the half-voxel error budget, and rejects a renderable ray before GPU submission. | Production numerical-admission defect. |
| C6 | `resident_coordinated_volume_is_exact_zero_work_and_idle_when_resident` | Two setup color cutoffs can still occupy both legal in-flight slots. The test immediately requires a third cutoff to present instead of accepting typed backpressure and waiting for its exact completion event. Isolated reruns alternate between pass and fail with GPU completion timing. | Nondeterministic test phase/race defect; production backpressure is correct. |

The run exposed one additional evidence defect outside those six product/test
cases. After the native process returned nonzero, the wrapper report retained
the selected inventory but lost per-case execution outcomes and conservatively
marked all 25 cases unevaluated. That fail-closed result is safer than inventing
passes, but it is not adequate attribution. H1 below repairs the report without
changing the native process as the authoritative pass/fail result.

The diagnoses above were confirmed before implementation by focused reruns
and, where needed, instrumented observation of the actual
application/renderer gates. The implementation record below owns their current
status.

## Decision

The follow-up will make one hard cut with six coordinated outcomes:

1. repair C1 and C3 by changing obsolete assertions, not correct production
   behavior;
2. replace fixed `ThreeD`/`FourPanel` logical publication shapes with one
   bounded active-layout and affected-target authority;
3. publish a shared change atomically across the currently visible panels
   whose pixels depend on that change, while doing no panel-specific work for
   hidden or unaffected panels;
4. give application and renderer correctness tests event-driven drivers that
   advance the real asynchronous phases and wait on causal events rather than
   assuming one function call means publication or GPU completion;
5. make pin-cohort use age and reachable-ray geometry authoritative at their
   respective production boundaries; and
6. retain exact per-case outcomes in a failed trusted-lane report without
   weakening the native nonzero result.

The central panel rule is:

```text
publication targets = active layout targets ∩ targets affected by the change
```

Those targets form one semantic publication group when they represent one
shared state change. The renderer may still derive a smaller physical GPU
delta when a member is already current or is a terminal no-work result, but
the application does not expose a partially advanced semantic group.

The 3D panel is not part of a linked cross-section change merely because it is
visible in the same window. It does not rerender, wait, allocate another
texture, acquire another body, or block XY/XZ/YZ unless a future feature makes
that exact change alter the 3D pixels as well.

## Classification Of Required Changes

| ID | Product behavior changes? | Test/harness changes? | Primary authority |
| --- | --- | --- | --- |
| C1 | No | Yes | Application gesture/presentation test |
| C2 | Yes: atomic publication for the active affected set | Yes | Application composed-presentation and render-attempt coordination |
| C3 | No | Yes | Renderer logical-set and physical-group tests |
| C4 | Yes: replacement age | Yes | Renderer `ResidencyOwner` |
| C5 | Yes: numerical admission proof | Yes | Render API shader work envelope |
| C6 | No | Yes | Renderer event/completion test driver |
| H1 | No product change | Yes | `xtask` trusted-lane evidence capture |

This classification is binding. C1, C3, and C6 must not be used as reasons to
alter correct product behavior. C2, C4, and C5 must not be hidden by weakening
their failing cases.

## Product Outcome

After the cut:

- a shared linked change cannot leave two visible cross-section panels on
  different semantic revisions;
- an invisible cross-section panel cannot delay or consume panel-specific
  rendering resources for the visible linked panels;
- standalone 3D performs no XY/XZ/YZ envelope, requirement-body, texture,
  pin, or color work;
- a layout with one visible affected panel naturally publishes a group of
  one, while layouts with two or three visible affected planes publish those
  exact members together;
- opening a panel prepares it from the latest semantic state rather than
  replaying changes that occurred while it was hidden;
- closing, hiding, opening, or resizing changes layout generation so work for
  the old membership or extent cannot publish into the new layout;
- recently used resident payload is not selected as oldest merely because its
  use age is still stored on a live pin cohort;
- a ray is rejected only for the binary32 coordinate and sample work it can
  actually reach, while uncertain or unprovable bounds still fail closed;
- tests distinguish preparation, admission, submission, GPU completion,
  publication, and currentness; and
- the trusted-lane report names the cases that executed, passed, failed,
  skipped, or remained unevaluated even when the overall process fails.

For linked cross-section changes, the intended behavior by active layout is:

| Active panels | Atomic linked publication group | Panels that do no linked work |
| --- | --- | --- |
| 3D, XY, XZ, YZ | XY, XZ, YZ | 3D |
| 3D, XY, XZ | XY, XZ | 3D and hidden YZ |
| XY, YZ | XY, YZ | hidden XZ and 3D |
| XZ | XZ | XY, YZ, and 3D |
| 3D | Empty | XY, XZ, and YZ |

The same rule applies to other changes through their own dependency set. A
3D camera change affects 3D only. A shared source, timepoint, visible-layer,
transfer, or layout change may affect every active panel whose pixels depend
on it. Playback retains its separately defined session membership and atomic
temporal policy, expressed through the same bounded target-set model.

## Non-Negotiable Invariants

### Scientific And Presentation Truth

- MIP, DVR, ISO, Mixed, Plane, Pick, transfer, sampling, validity, coverage,
  scale, and authored layer-order semantics do not change.
- The strict shader coordinate error budget remains **less than** half a voxel.
- Grid ends and sample counts remain accepted only through the existing exact
  binary32 half-step ceiling of `2^23`.
- No coordinate, sample count, extent, or LOD is silently clamped or reduced
  to make a case pass.
- A previous complete front remains authoritative throughout a recoverable
  wait. A successor group becomes visible only when its required members are
  ready and one matching publication succeeds.
- A physical submission count or an internal `Current`/`Exact` flag cannot
  substitute for the required independent pixel or resource fact.

### One Authority Per Decision

- The application semantic scheduler owns which active panels are affected by
  one change and whether they form one atomic publication group.
- The renderer `FrameCoordinator` remains the sole owner of physical target
  validation, GPU recording, queue submission, completion, texture revision,
  and swap.
- `ResidencyOwner` remains the sole owner of physical payload, pins, eviction,
  and replacement age.
- The render API remains the sole owner of binary32 shader-work admission.
- The application and renderer test drivers observe these production
  authorities; they do not duplicate their decisions in a mock state machine.
- Native Nextest process status remains the trusted-lane result authority. A
  parsed event report improves attribution but can never turn nonzero into
  pass.

### Boundedness

- `PresentationTarget` remains the closed universe `{ThreeD, Xy, Xz, Yz}`.
- An active or affected target set contains at most four unique targets in
  canonical order. No unbounded target collection or public target allocator
  is introduced.
- Same-body resident rebind remains O(1) in requirement count. It may advance
  one cohort scalar but may not touch every payload resource on each frame.
- Per-resource age is materialized only at an existing bounded body-release,
  replacement, or retirement traversal.
- Numerical proof remains bounded analytically over the viewport and layer
  set. It may not enumerate pixels or ray samples on the CPU.
- Test drivers use finite deadlines, bounded event queues, and zero automatic
  retries. They do not use an unbounded spin or arbitrary sleeps.

### Hidden And Inactive Panels

- Hidden and closed panels are both absent from the active rendering set.
- Global semantic state continues to advance while a panel is hidden.
- A hidden panel owns no required current image, panel-specific requirement
  body, shader work envelope, output texture, front pin, color command, or
  publication obligation.
- A previously produced hidden-panel image may survive only as ordinary
  disposable cache state. It is stale after semantic state changes, cannot
  block active work, and cannot be presented as current when the panel opens.
- This cut introduces no special always-warm hidden-panel policy. Any future
  cache-retention optimization requires measurement and remains independent
  of correctness.

### Failure And Progress Honesty

- A pending shader envelope, pipeline, residency item, or GPU completion is a
  typed wait, not a failure and not a successful publication.
- If one member of an atomic visible affected group fails deterministically,
  all predecessor members remain visible. The product does not publish the
  successful subset and call the group current.
- Recoverable candidate or capacity rejection replans the affected group under
  the existing monotone policy; it does not silently drop one visible member.
- A layout-generation change supersedes the old group. It does not mutate the
  membership of an in-flight group after preparation has begun.
- Typed color backpressure is handled by its exact causal completion event.
  It is not bypassed by increasing the in-flight limit or by retrying on a
  timer.

## Active Layout And Affected-Target Authority

### Active Layout Snapshot

The application will own one immutable active-layout snapshot containing:

- a monotone layout generation;
- the unique active targets from the fixed four-target universe;
- the exact target extent and surface generation for each active target; and
- deterministic scheduling order, kept separate from membership and
  atomicity.

The current standalone-3D mode projects `{ThreeD}`. The current four-panel
mode projects `{ThreeD, Xy, Xz, Yz}`. The model must also validate every other
bounded subset even though the current UI does not yet expose it.

Layout membership and extent are immutable for one transaction. A panel open,
close, hide, show, or resize creates a new layout generation. Results prepared
against an older generation may finish and clean up, but they cannot publish
into the newer layout.

### Change Dependency Set

Every semantic change supplies one target dependency set:

- linked cross-section geometry, linked Plane pan/zoom, and linked Plane
  rotation: `Xy`, `Xz`, and `Yz`;
- 3D camera/projection-only change: `ThreeD`;
- target-local tool or surface change: the named target;
- source, timepoint, visible-layer, transfer, sampling, or render-mode change:
  every target whose rendered pixels consume that fact;
- playback successor: the targets frozen in the active playback session; and
- layout change: newly active and resized targets, plus explicit retirement
  of removed targets.

This table is semantic rather than a hard-coded assumption that all visible
panels always belong together. A future feature that makes the 3D image show
the linked section must explicitly add 3D to that feature's dependency set.
Until then, linked input cannot acquire 3D work merely through co-location.

### Semantic Group Versus Physical Delta

The application first computes the active affected set and waits until every
required member has a complete immutable logical request or an explicit
terminal no-work result. It then hands that complete bounded set to the
renderer.

Inside one observation, the renderer classifies members as current/reused,
terminal no-work, or requiring physical execution. Only the last category
enters the color command list. This retains incremental GPU work without
letting a physical delta masquerade as the complete semantic transaction.

For a nonempty color delta, every executing member is preflighted before the
first pass records, all passes use one encoder/submission under the existing
coordinator, and publication is atomic. For an empty delta, the renderer still
validates current fronts or terminal results and returns a complete zero-work
report.

Nonaffected active panels are not logical members of that change. Their
current fronts remain untouched and cannot make the affected group wait. This
is the required distinction that keeps visible 3D independent of linked Plane
interaction.

### Layout Transitions

The following transition rules are required:

1. Closing or hiding a panel publishes its deactivation for the new layout,
   releases its front/pin/target-specific obligations, and supersedes older
   work for that target.
2. Remaining panels are replanned under the new immutable membership; an
   in-flight old group is never shortened in place.
3. Opening or showing a panel builds its first request from the latest source,
   timepoint, cross-section, camera, layer, transfer, sampling, and layout
   facts. Intermediate hidden-time revisions are not replayed.
4. Resizing records the new exact extents in a new layout generation. A result
   at an old extent cannot satisfy or publish into it.
5. A panel that remains active but is unaffected by the semantic change keeps
   its existing current front without joining that change's wait or physical
   delta.

These rules must be covered at the bounded semantic coordinator before the
future flexible UI is built.

## C1 — Durable Linked Gesture Identity Test Correction

### C1 Diagnosis

The authoritative mailbox contract allocates one frame identity for transient
gesture samples and commits durable state using the final sample identity.
Allocating another identity at gesture completion would manufacture a second
render intent and can cause duplicate GPU work. The failing test still expects
the removed behavior.

### C1 Required Change

Keep production behavior unchanged. Correct the test to require:

- the durable linked frame identity equals the final exact transient frame;
- the durable reducer commit occurs once;
- matching XY/XZ/YZ presentations retain their frame, texture revision,
  texture binding, requirement body, and currentness identities;
- finish creates zero additional renderer submission, upload, residency,
  allocation, or binding work; and
- terminal empty input clears every active affected linked target without
  reviving a hidden or predecessor image.

The existing non-GPU camera gesture regression that already requires identity
equality remains a cross-check. The linked GPU test must keep real presentation
facts rather than reducing to mailbox metadata.

### C1 Forbidden Fix

Do not allocate a new durable frame, force a rerender, weaken the equality
contract, or delete the empty-clearing assertions.

## C2 — Atomic Visible-Affected Linked Publication

### C2 Diagnosis

The immediate failure occurs because the test calls display rendering once,
before the latest-only shader-envelope workers have produced any of the three
required results. In the normal UI, envelope completion produces a wake and a
later turn retries the request.

Merely adding that missing wait would make the existing case pass, but it
would not close the deeper policy gap. XY/XZ/YZ envelope results may complete
on different turns. The visible linked views represent one shared geometry,
so the application must not allow the first ready panel to publish while
another visible affected panel retains the predecessor.

### C2 Required Product Change

- Compute the active affected target set from the immutable layout and linked
  dependency set.
- Start or retain bounded latest-only preparation for every member.
- Keep the predecessor group visible while any required member lacks a body,
  envelope, residency, or other publication prerequisite.
- Accept envelope completions in any order without publishing a subset.
- When all required members are ready, call the renderer once with the
  complete semantic group.
- Derive the physical delta in the renderer and publish the group atomically.
- Leave 3D and hidden Plane targets outside the request, submission, and wait.
- If a member fails deterministically, retain the whole predecessor visible
  group and project one scoped failure. A later relevant semantic/layout
  change creates a new fingerprint and can try again.

For the current four-panel mode this means XY/XZ/YZ. For a future layout with
only XY/XZ visible it means XY/XZ. For a single visible Plane it naturally
means a one-member group.

### C2 Required Test Change

Drive the real asynchronous envelope and renderer-event phases. The trusted
test must deliberately make member prerequisites observable out of order and
require:

- no linked target advances before the last active affected member is ready;
- exactly one successor renderer color submission when all three current
  four-panel linked members require color;
- all active affected frame/texture/currentness facts advance together;
- changed GPU pixels match the independent fine CPU oracle;
- 3D frame, texture, binding, residency, and submission facts are unchanged;
- a hidden member in a bounded two-Plane test performs no envelope, body,
  residency, texture, or color work; and
- the identical settled observation performs zero work.

### C2 Forbidden Fix

Do not make envelope computation synchronous, poll it with sleeps, publish
panels independently, hard-code an always-three Plane group, include 3D as a
blocking bookkeeping member, or render a hidden Plane to simplify the batch.

## C3 — Complete Logical Set And Malformed Physical Group Tests

### C3 Diagnosis

The renderer correctly rejects a request list that omits a declared member of
its atomic physical publication group. The failing test expects that malformed
call to act as a private preparation stage, contradicting the current
complete-logical-set contract.

### C3 Required Change

Split the two meanings:

1. a portable/focused validation case passes an incomplete declared physical
   group and requires `InvalidCoordinatedPublicationGroup` before recording or
   mutation; and
2. the trusted real-pixel case uses the ordinary complete logical target-set
   entry, lets the renderer derive the physical delta, and requires correct
   pixels, deterministic target order, one submission, atomic publication,
   and a later identical zero-work cutoff.

The trusted case may be renamed from its fixed-four-panel wording to an
active-target name only with an exact verification-registry replacement. The
25-case trusted minimum may not silently lose this evidence.

### C3 Forbidden Fix

Do not weaken renderer group validation, restore a partial-group preparation
path, manually predict the renderer's physical delta in the application, or
remove the one-submit/idle-zero assertions.

## C4 — One Effective Residency Use-Age Authority

### C4 Diagnosis

Same-body rebind correctly avoids walking every resource and advances only
`ResidentBodyPinCohort.latest_used_frame`. Physical resources retain their old
`last_used_frame` until body release. Replacement preflight currently ranks
outgoing candidates from that stale physical value, before the cohort age is
materialized, so its victim choice disagrees with the true use history.

### C4 Required Product Change

Define one effective use age for replacement decisions:

```text
effective payload age = max(
    materialized resident-resource age,
    latest-use age of every live/releasing pin cohort covering that payload
)
```

The implementation must satisfy all of the following:

- same-body rebind advances one cohort scalar in O(1);
- replacement preflight uses the effective age for every candidate released
  by the outgoing body/cohorts;
- actual release or retirement materializes that same effective age into the
  physical resource while performing the already-required bounded body walk;
- preflight simulation and committed mutation use identical age/tie-break
  rules;
- a payload still pinned by another live cohort is not made evictable;
- overlapping cohorts use the maximum relevant age, not whichever cohort is
  visited last;
- rejected or rolled-back preflight does not partially mutate physical ages,
  pins, or LRU order; and
- existing deterministic tie order remains unchanged after age comparison.

### C4 Required Evidence

- A portable deterministic LRU case admits eight payloads, revisits the oldest
  through same-body rebind, admits a ninth, and requires eviction of the truly
  untouched second-oldest payload.
- A shared-cohort case proves overlapping target use takes the maximum age and
  that retirement of one target cannot age or unpin the survivor incorrectly.
- A rollback case proves refused replacement leaves the age/pin/LRU state
  byte-for-byte equivalent to the preflight input.
- The existing trusted union-pin/layout-retirement case retains its real GPU
  residency, pin, reuse, and release assertions.
- Structural counters prove same-body rebind still performs zero requirement
  walk, allocator plan, directory mutation, upload, or residency submission.

### C4 Forbidden Fix

Do not touch every body resource on every rebind, add a second app-owned LRU,
special-case the fixture's page identities, or simply change the expected
victim.

## C5 — Reachability-Aware Volume Work Envelope

### C5 Diagnosis

The current volume envelope bounds ray parameter and coordinate error from the
maximum distance to either end of the complete grid. For a ray starting near
`x = 16383.5` and pointing toward `x = 0` in a one-million-voxel X dimension,
the opposite boundary near one million is behind the ray. Charging it expands
the coordinate error to roughly 2.84 voxels and falsely rejects the view.

The GPU has not produced a wrong pixel in this case because the request is
rejected before submission. The bug is that the authoritative CPU admission
proof does not describe the work reachable by the shader.

### C5 Required Product Change

Replace whole-dimension distance with an outward-rounded, reachability-aware
ray interval proof that matches the quantized shader controls and shared slab
classification:

1. derive bounded ray-origin and direction facts across the complete viewport
   from the exact binary32 controls sent to the shader;
2. classify each axis with the same positive, negative, stationary-normal,
   and admitted/rejected-subnormal rules used by volume traversal;
3. for a provably positive direction, exclude the boundary behind the ray; for
   a provably negative direction, exclude the opposite boundary; for a
   stationary axis, prove that its bounded coordinate remains inside the
   applicable slab;
4. compute outward entry and exit intervals, clip them to the finite camera
   ray interval, and prove their order and finiteness;
5. derive maximum reachable `|t|`, coordinate magnitude, layer span, and
   sample-count bounds only from the surviving interval;
6. use the same reachable interval for general-affine DVR layer-entry/count
   facts and every other volume schedule that consumes it;
7. preserve the existing strict half-voxel and `2^23` limits; and
8. fail closed with the existing typed stage/axis/error facts whenever sign,
   slab membership, ordering, or finiteness cannot be proved.

The proof must be analytical and bounded over the viewport. It cannot trace
every pixel or enumerate samples. Where direction sign genuinely varies over
the viewport, it may conservatively retain both reachable branches or reject
with a typed bound; it may not assume the center ray represents all pixels.

### C5 Required Evidence

- A nonignored render-API regression reproduces the one-million-voxel fixture
  and proves the reachable interval stays near the actual ray segment.
- A mirrored positive-direction case proves the branch is not hard-coded to
  one sign.
- An empty/intersection-miss case and an uncertain-sign boundary case prove
  the proof remains fail-closed.
- Exact `2^23`/`2^23 + 1` and half-voxel boundary regressions remain green.
- Independent f64 or hand-authored interval facts are kept outside the
  production helper being tested.
- The existing trusted GPU case keeps the large fixture and requires the ray
  to reach the intended voxel beyond the former 16,384-sample cap with the
  expected color/coverage/validity facts and zero validation error.

### C5 Forbidden Fix

Do not shrink the million-voxel fixture, raise the half-voxel budget, restore
the old 16,384 cap, clamp the far coordinate, skip work-envelope validation,
or add a test-only admission bypass.

## C6 — Completion-Aware Resident Correctness Test

### C6 Diagnosis

The renderer intentionally permits at most two in-flight color cutoffs. The
correctness branch creates two setup submissions and immediately attempts the
measured successor. Whether one setup submission has completed by then is a
hardware scheduling race. Typed backpressure is correct, but the test treats
it as a permanent failure.

### C6 Required Change

- Install the renderer event sink before setup work.
- Wait for the exact setup submission-completion events needed to establish
  the test's declared idle starting state.
- Submit the measured resident request only after that state is proved.
- If typed backpressure is deliberately encountered in a separate branch,
  retain and retry the exact same request only after its declared causal
  completion event.
- Require eventual presentation before a finite deadline, exactly one color
  submission for the measured changed frame, zero residency/upload/allocator
  work, correct current facts, and zero identical settled work.
- Add a deterministic backpressure subtest that fills the legal in-flight
  capacity, proves the predecessor remains authoritative, releases one exact
  completion, and observes one eligible retry without a duplicate submission.

### C6 Forbidden Fix

Do not increase the production in-flight limit, sleep for an arbitrary
duration, enable Nextest retries, accept a non-presented report, or remove the
zero-work assertions.

## Shared Phase-Aware Test Drivers

C2 and C6 are two manifestations of one testing mistake: a helper call is
being treated as though it synchronously crossed several asynchronous product
phases. One small shared testing vocabulary will replace that assumption.

### Required Phase Vocabulary

Tests and diagnostics must distinguish:

1. **planned** — semantic work and target membership are known;
2. **prepared** — immutable bodies and work envelopes exist;
3. **admitted** — the render-attempt coordinator permits execution;
4. **submitted** — the renderer queued the exact GPU cutoff;
5. **completed** — the matching GPU submission completed;
6. **published** — the renderer swapped/returned the matching front; and
7. **current** — application source, layout, surface, frame, body, quality,
   and target facts still match after publication.

An assertion must name the phase it requires. `prepared` cannot satisfy
`submitted`; `completed` cannot satisfy `published`; an old `published` front
cannot satisfy current semantic state.

### Application Driver

The app-side test driver must execute the same relevant turn prologue and
service boundaries as the normal workbench:

- begin the renderer/UI wake turn;
- consume completed shader-envelope wake state;
- consume renderer and submission-completion events;
- advance bounded demand/planner results;
- ask the one render-attempt coordinator whether the fingerprint is eligible;
- execute at most the product-authorized attempt;
- apply the renderer report and semantic currentness checks; and
- stop only at a named predicate, deterministic failure, or finite deadline.

The driver may expose deterministic test hooks at existing event seams to
control completion order. It may not create a second scheduler, manufacture
currentness, call private mutation paths unavailable to the product, or use a
mock renderer as proof of the real GPU handoff.

### Renderer Driver

The renderer-side driver must:

- observe exact submission identities through the existing event sink;
- drive the WGPU poll/completion boundary without blocking indefinitely;
- wait for a named submission or causal capacity event;
- preserve the exact request across typed backpressure;
- reject unrelated or stale completion as progress for that request; and
- terminate with a bounded diagnostic naming the last observed phase.

No driver uses wall-clock sleeping as progress. Time is only a finite hang
deadline.

## H1 — Failed-Lane Per-Case Attribution

### H1 Required Change

The trusted correctness runner must retain a structured Nextest event or
equivalent machine-readable per-case result stream in a unique local run
directory and parse it after process exit regardless of exit status. It must:

- reconcile selected, started, passed, failed, skipped, and not-started names
  against the exact registered inventory;
- preserve the native exit status as authoritative;
- list a panic, timeout, abort, signal, or test failure under its exact case
  where the structured evidence permits;
- mark only genuinely unstarted or unattributable cases unevaluated;
- fail closed if the structured stream is absent, truncated, corrupt, or
  disagrees with inventory; and
- keep bounded output under ignored local `target/` storage without publishing
  private paths or host facts.

### H1 Required Evidence

Portable runner tests must cover an all-pass stream, a mixed 19-pass/6-fail
stream with nonzero native status, fail-fast or killed execution with
not-started cases, corrupt/truncated output, unexpected names, and a native
nonzero status paired with an apparently all-pass stream. None of those cases
may serialize the lane as pass unless the complete native and inventory
contract agrees.

### H1 Forbidden Fix

Do not scrape human console prose when a structured result is available, infer
passes from the absence of failure text, or let the report override process
status.

## Authority Changes And Required Deletions

Implementation is incomplete until these authority changes and predecessor
removals are both present:

- Replace app/render boundary enums that can express only standalone 3D or a
  fixed four-panel logical transaction with one bounded active/affected target
  set. Current UI projection remains two modes, but the semantic type must not.
- Remove any application path that constructs a renderer physical delta as
  though it were the complete logical group.
- Remove any linked publication decision based on whichever panel envelope
  becomes ready first.
- Remove any requirement that hidden YZ or unchanged 3D prepare, allocate,
  render, or complete before visible XY/XZ publication.
- Keep renderer rejection of an incomplete declared physical group and remove
  the test-only expectation that such a group prepares successfully.
- Replace replacement-preflight use of stale physical age with the one
  effective cohort-aware age; remove any later contradictory age update.
- Replace whole-grid opposite-boundary distance as the volume sample/coordinate
  authority with reachable intervals; remove duplicate caller-side relaxations.
- Remove one-shot asynchronous test helpers whose names or assertions imply
  publication/completion after only preparation or submission.
- Remove failed-process report behavior that discards every available per-case
  outcome.

No compatibility enum, alias, dual logical-set path, hidden fixed-trio helper,
or fallback numerical proof remains after the cut. Git history is the archive.

## Required Regression Inventory

The names below are target names. A materially different name requires a plan
amendment or an explicit one-for-one traceability update before deletion of the
named predecessor.

| Owner | Required case | Mandatory assertion |
| --- | --- | --- |
| `mirante4d-app` trusted GPU | `exact_transient_cross_section_updates_all_linked_panels_before_finish_and_empty_clears_them` | Final transient and durable frame identities are equal; no finish GPU work; active linked targets clear coherently on terminal empty. |
| `mirante4d-app` | `active_affected_target_set_covers_every_bounded_layout_combination` | Every subset of the four-target universe produces exactly `active ∩ affected`, with no duplicates and canonical order. |
| `mirante4d-app` | `hidden_linked_target_tracks_semantic_state_without_render_obligation` | Hidden target is absent from envelope/body/residency/texture/publication work and opens at the latest semantic state. |
| `mirante4d-app` | `layout_generation_change_suppresses_old_group_without_shrinking_it` | Old membership/extent cannot publish after close/open/resize; the replacement group uses the new immutable snapshot. |
| `mirante4d-app` | `out_of_order_linked_prerequisites_publish_the_visible_affected_group_once` | No subset publication; final readiness causes one attempt and one semantic publication; unrelated 3D remains untouched. |
| `mirante4d-app` | `linked_member_failure_retains_the_complete_visible_predecessor` | One deterministic member failure publishes none of the successor group and becomes quiescent until a relevant change. |
| `mirante4d-app` trusted GPU | `incremental_linked_zoom_pixels_match_direct_fine_cpu_oracle_after_one_settlement_plan` | Actual asynchronous phases complete; current four-panel linked members publish once and match independent pixels; 3D and idle work remain unchanged/zero. |
| `mirante4d-render-wgpu` | `incomplete_declared_physical_group_is_rejected_before_recording` | Exact typed error, zero target mutation, zero submission. |
| `mirante4d-render-wgpu` trusted GPU | `coordinated_active_target_set_has_real_pixels_one_submit_and_idle_zero` | Complete logical active set; renderer-derived delta; correct pixels; one atomic submission; settled zero. |
| `mirante4d-render-wgpu` | `recent_cohort_rebind_age_prevents_oldest_payload_eviction` | Recently rebound oldest payload survives; truly untouched payload is evicted. |
| `mirante4d-render-wgpu` | `overlapping_cohorts_use_maximum_effective_age_and_preserve_survivor_pins` | Maximum age and remaining pins survive partial retirement. |
| `mirante4d-render-wgpu` | `replacement_preflight_rollback_preserves_age_pin_and_lru_state` | Failed plan has no partial mutation. |
| `mirante4d-render-wgpu` trusted GPU | `coordinated_target_pins_are_union_scoped_and_layout_retirement_releases_them` | Full real-GPU union/rebind/retirement behavior uses the corrected victim order. |
| `mirante4d-render-api` | `volume_envelope_excludes_proven_unreachable_opposite_boundary` | Million-voxel fixture is admitted from reachable interval while strict error budget remains intact. |
| `mirante4d-render-api` | `volume_envelope_reachability_is_symmetric_and_fails_closed_when_uncertain` | Positive/negative branches agree under mirroring; uncertain sign/order does not receive an optimistic bound. |
| `mirante4d-render-wgpu` trusted GPU | `rays_beyond_the_removed_16384_sample_cap_reach_the_far_voxel` | Large fixture reaches intended voxel and matches independent GPU output facts. |
| `mirante4d-render-wgpu` | `color_backpressure_waits_for_exact_completion_then_retries_once` | Filled capacity retains predecessor; named completion makes one retry eligible; no duplicate submit. |
| `mirante4d-render-wgpu` trusted GPU | `resident_coordinated_volume_is_exact_zero_work_and_idle_when_resident` | Setup completion is established; measured resident change presents once with zero residency work; settled idle is zero. |
| `xtask` | `failed_trusted_gpu_run_retains_exact_per_case_attribution` | Nonzero 19/6 fixture remains failed and reports all exact outcomes. |
| `xtask` | `truncated_or_contradictory_trusted_gpu_results_fail_closed` | Missing/corrupt/contradictory structured evidence cannot become pass. |

The exact trusted inventory must remain reconciled. A rename updates the
registry and generated selector in the same change; a split may increase the
count but cannot remove one of the six observed boundaries without named
replacement evidence.

## Work Sequence

### P0 — Freeze The Failing Contracts

Outcome: preserve every diagnosis before production changes.

Required work:

- retain the clean campaign commit/tree and six exact names in this plan;
- add portable failing cases for dynamic active-set membership, out-of-order
  linked prerequisites, effective residency age, reachable-ray admission,
  completion-aware backpressure, and failed-run attribution;
- prove C1 and C3 fail only because their assertions describe superseded
  behavior;
- keep the million-voxel and eight-plus-one residency fixtures unchanged; and
- record initial focused failure output without claiming the 19 native passes
  as a qualifying lane result.

No production behavior changes in P0.

Exit: every product defect fails for its diagnosed reason, every stale test is
separately identified, and the native campaign remains red.

### P1 — Bounded Active/Affected Target Cutover

Outcome: establish the future-safe semantic publication authority.

Required work:

- introduce one canonical bounded active-layout snapshot and target-set type;
- project current standalone-3D and four-panel layouts through it;
- express dependency sets for linked, 3D, shared, playback, surface, and
  layout changes;
- replace fixed one/four logical transaction shapes at app and renderer
  boundaries;
- preserve fixed four-slot renderer capacity while allocating/retaining
  resources only for active targets;
- implement layout-generation stale suppression and explicit target
  deactivation; and
- delete the predecessor fixed-shape semantic path.

Exit: all bounded layout-combination tests pass, current UI behavior is
unchanged, and 3D-only creates no 2D panel work.

### P2 — Linked Cohort Readiness And Phase-Aware Drivers

Outcome: close C1, C2, C3, and C6 without synchronous shortcuts.

Required work:

- implement the application and renderer event-driven test drivers;
- make linked active affected members wait as one semantic cohort;
- handle out-of-order envelope completion and member failure;
- correct durable identity and malformed-group assertions;
- wait for exact setup completion in resident correctness tests;
- retain explicit backpressure coverage; and
- remove one-shot/sleep/retry assumptions.

Exit: focused phase/order/failure tests pass deterministically across repeated
local executions, and no correct production backpressure or frame-identity
contract was weakened.

### P3 — Effective Residency Age Cutover

Outcome: close C4 with one age authority and retain O(1) rebind.

Required work:

- compute cohort-aware effective age in replacement preflight;
- materialize the same age during bounded release/retirement;
- preserve overlapping pins and deterministic ties;
- keep simulation/commit/rollback consistent; and
- delete stale-physical-only victim ranking.

Exit: portable victim/overlap/rollback cases pass and structural counters show
no per-frame body walk.

### P4 — Reachable Volume Envelope Cutover

Outcome: close C5 without numerical relaxation.

Required work:

- implement viewport-wide outward reachable ray intervals;
- share shader direction/slab classification facts;
- derive coordinate and sample bounds from finite reachable segments;
- keep general-DVR and other volume schedule counts consistent;
- retain exact typed rejections when proof is uncertain; and
- delete whole-grid unreachable-boundary authority.

Exit: portable independent interval/boundary tests pass, the million-voxel
fixture is admitted, and existing numerical rejection boundaries remain
unchanged.

### P5 — Trusted Evidence Attribution And Six-Case Closure

Outcome: make the lane itself report a useful red or green result.

Required work:

- capture and reconcile structured per-case results on success and failure;
- update exact registry names for any deliberate case rename;
- run the six repaired cases directly during iteration on the designated
  Vulkan adapter;
- run the complete clean 25-case-or-larger trusted correctness lane once all
  focused checks pass; and
- require zero unexpected skip, timeout, validation error, or unattributed
  selected case.

Exit: the complete trusted lane is green and its report names every exact
outcome. A direct dirty-tree pass does not satisfy this exit.

### P6 — Integrated Verification And Product Closeout

Outcome: establish the correction through current product modes without
claiming the future flexible UI exists.

Required work:

- run formatting, focused crate tests, Clippy/public verification,
  documentation checks, and exact verification synchronization;
- run current standalone-3D and four-panel normal-product scenarios;
- exercise linked pan/zoom/rotation, exact finish, terminal empty, resize,
  interruption, settlement, and idle;
- confirm linked changes advance visible XY/XZ/YZ together and do not alter
  3D frame/texture/refinement state;
- confirm standalone 3D performs no panel-specific 2D work;
- inspect errors, displayed fidelity, target generations, and captures on the
  real mapped Vulkan product; and
- run the performance evidence required by the existing GPU testing trigger
  policy for the scheduler, residency, and numerical-work changes, without
  changing thresholds or tuning behavior inside this plan.

The current UI cannot product-validate two- or three-panel combinations. Their
bounded semantic behavior is automated-verified here; visible flexible-layout
validation belongs to the future UI feature that exposes those layouts.

Exit: the current product is owner-accepted for the affected workflows, the
trusted correctness lane passes, applicable performance evidence is reported
under its existing authority, and no flexible-layout product claim is made.

## Verification Commands

Exact implementation-time commands may narrow during iteration, but closeout
must include at least:

```bash
cargo fmt --all
cargo test -p mirante4d-render-api
cargo test -p mirante4d-render-wgpu --lib
cargo test -p mirante4d-app --lib
cargo test -p xtask
cargo xtask verification-sync --check
cargo xtask docs-check
cargo xtask verify-pr
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu-correctness
```

Renderer, presentation, residency, and numerical-work changes also trigger the
applicable existing `gpu-performance` and mapped-product requirements from the
GPU testing refactor. This plan does not redefine their workload, sampling,
30/60 Hz policy, stall/refinement limits, or baseline acceptance. A performance
failure is reported and discussed before optimization; it cannot be repaired
by weakening correctness.

Product validation uses the normal native release application on the real
mapped display and designated Vulkan adapter. Internal GPU readback and
currentness records remain supporting evidence.

## Performance And Resource Non-Regression

This is a correctness plan, but three production changes can alter work:

- dynamic active-target assembly changes planning/submission membership;
- effective-age selection changes future eviction/reupload choices; and
- reachability-aware admission changes CPU proof work and admits a formerly
  rejected long ray.

The following structural constraints therefore apply before numeric campaign
results:

- hidden/unaffected targets add zero panel-specific envelopes, bodies, target
  textures, pins, color passes, or submissions;
- a linked changed group uses one color submission, not one per panel;
- same-body rebind remains O(1) and zero residency work;
- identical settled groups submit zero work;
- numerical admission is analytical rather than per-pixel/per-sample; and
- no extra readback, timestamp, or diagnostic work is enabled in the normal
  product merely to satisfy a test.

The existing performance plan decides absolute and relative results. This plan
does not establish a new threshold, accepted baseline, or performance claim.

## Risks And Controls

### Future Flexibility Expands The Present Scope

Control: implement only the bounded semantic target/layout model and current
mode projections. Panel controls, arbitrary layout editing, drag UX, persisted
layout, and flexible-layout product validation remain outside this cut.

### Atomic Publication Waits For The Slowest Panel

Control: wait only for visible affected panels. Hidden panels and unrelated 3D
are excluded. Existing causal waits, failure projection, retained predecessor,
and stall/refinement limits remain authoritative.

### A Hidden Panel Reopens With Stale Pixels

Control: hidden output has no currentness authority. Reopen uses a new layout
generation and latest semantic state before publication; optional cached bytes
are disposable only.

### Layout Churn Publishes Obsolete Work

Control: immutable layout generation, target extents, and membership are part
of the attempt fingerprint before and after renderer execution. Old work may
clean up but cannot publish.

### Dynamic Sets Reintroduce Unbounded Or Duplicate Work

Control: the universe is exactly four targets, represented by one validated
bounded set with uniqueness and canonical order tests.

### Effective Age Makes Rebind O(N)

Control: update one cohort scalar on rebind and materialize only during an
existing body traversal. Structural tests reject per-resource rebind work.

### Effective Age And Pins Disagree Under Overlap

Control: use the maximum age of every relevant cohort and preserve any
surviving pin. Simulation, commit, rollback, and retirement share one rule.

### Reachability Proof Becomes Too Optimistic

Control: outward rounding, viewport-wide bounds, shader-matching sign/slab
classification, mirrored cases, and fail-closed uncertainty. Independent
facts and final GPU pixels remain required.

### Reachability Proof Remains Needlessly Conservative

Control: regression fixtures isolate forward/backward reachable boundaries and
typed failure stage. Do not relax the error budget; improve the interval proof
when a valid case remains unprovable.

### Test Drivers Become A Parallel Product Scheduler

Control: drivers only invoke and observe existing production phases and events.
They own deadlines and diagnostics, not semantic eligibility or currentness.

### Failed-Lane Parsing Misreports Success

Control: native status remains authoritative, exact inventory reconciliation
is mandatory, contradictions fail closed, and parser fixtures cover nonzero,
killed, and corrupt runs.

### Correctness Work Regresses Performance

Control: preserve structural zero-work/O(1) gates and run the existing
performance campaign when its trigger applies. A numeric regression stops
tuning for owner discussion; it cannot change scientific or publication
truth.

## Hard Stop Conditions

Stop and request owner direction if implementation requires:

- implementing the flexible panel UI, persisted layout format, or arbitrary
  layout editor to complete this correction;
- keeping a hidden panel in the required render/publication set for
  convenience;
- making 3D block a linked Plane update that does not change 3D pixels;
- a second scheduler, renderer, residency/LRU authority, or numerical policy;
- relaxing the half-voxel or `2^23` numerical boundary;
- changing sampling, transfer, layer order, output resolution, selected LOD,
  or scientific pixels to make a test pass;
- increasing GPU budget or in-flight submission capacity;
- a storage, package-format, brick, shard, or compression change;
- wall-clock sleeps or automatic test retries as the only way to stabilize a
  correctness case;
- running the quarantined linked-S0 host-stress workflow;
- accepting a partial or unattributed trusted run as green;
- a GitHub/self-hosted runner, paid hosted resource, or public private-data
  upload; or
- performance tuning or threshold changes before a newly observed regression
  is presented to the owner.

## Explicit Non-Goals

- Implementing panel open/close/hide/resize controls or persisted flexible
  layouts.
- Keeping every hidden panel pre-rendered for instant reopen.
- Changing current four-panel visual layout or standalone-3D UX.
- Changing renderer scientific semantics, sampling, transfer, LOD, or output
  resolution.
- Performance optimization, baseline replacement, or threshold revision.
- Device recovery, CPU rendering fallback, another backend, or broader
  hardware/platform qualification.
- Reopening the linked-S0 stress diagnostic.
- Closing the separate static R11/P7 performance-recovery campaign.
- Adding GitHub GPU execution, retries, flaky-pass policy, or quarantine-based
  green status.

## Implementation Record

C1 through C6 and H1 are implemented at their diagnosed boundaries:

- C1 retains the final transient identity through durable completion and proves
  terminal empty currentness without inventing finish work.
- C2 replaces the fixed one/four semantic shape with immutable bounded
  `ActivePresentationLayout` snapshots and `active ∩ affected` publication.
  Hidden and unrelated targets own no obligation; terminal no-work members can
  complete semantics without a fabricated GPU body; the phase-aware trusted
  driver compares repeated GPU captures with an independent CPU oracle.
- C3 preserves renderer rejection of an incomplete declared group and tests
  the complete logical active set before the renderer derives its physical
  delta.
- C4 uses the maximum of materialized resource age and every owning pin-cohort
  age in simulation, commit, retirement, and rollback.
- C5 uses an outward-rounded forward-reachable ray interval, including mirrored
  and fail-closed uncertain-direction coverage, while retaining the existing
  numerical limits.
- C6 waits for exact public submission completion before retrying typed
  backpressure, without sleeps, retries, or private currentness mutation.
- H1 consumes a structured per-case result stream after success or failure,
  reconciles it with exact discovery and inventory, and cannot override a
  nonzero native result or make incomplete evidence green.

The fixed-shape semantic predecessor and obsolete one-shot helpers are deleted.
Portable target-set, failure, residency, numerical, driver, parser, registry,
and crate suites pass. The clean immutable trusted execution used commit
`efb745221ba3a78d00a20fe14b843d07f5272d06`, tree
`acbcce58fe91ac7e1dd7c1623998a3ab2b22bf11`, the expected Vulkan backend, and
the NVIDIA GeForce RTX 3070 Ti Laptop GPU. Exact discovery and reconciliation
passed; all 25 selected cases started, executed, and passed serially with zero
retries, skips, not-started cases, validation errors, or unattributed outcomes.
The structured status was `complete_native_success`.

On the same clean revision, normal release-product automation passed all 141
render-mode commands and all 60 representative native-navigation commands.
Those runs exercise the current standalone-3D and four-panel product modes;
they are automated mapped evidence, not owner visual acceptance, and they do
not claim that a two- or three-panel UI exists.

The applicable performance campaign was activated with an owner-selected
package that passed production multi-layer preflight. Its controlled X11
presentation probe initially stopped before measurement. Subsequent live
replay identified stale per-second command attribution, a lost in-pass wake,
and self-minimize/self-restore deadlock. The current-tree harness repairs those
defects; its 29-command product validation now passes unattended at the final
1920x1080 extent with a nonblank GPU capture.

The independent presentation receipt remains `unevaluated`, however, because
the NVIDIA/Vulkan/X11 stack produced zero X11 Present completion events. It
contains zero accepted performance runs and left the pending baseline
untouched. No threshold was changed and no performance result is inferred.
Selection of a trustworthy presentation authority, accepted calibration and
comparison evidence, and explicit owner product acceptance therefore remain
open completion items.

## Documentation And Authority Closeout

Implemented facts are synchronized in the same cut:

- `docs/ARCHITECTURE.md` for bounded active-layout, affected-target,
  publication, residency-age, and numerical-envelope authorities;
- `docs/CURRENT_STATE.md` for implemented behavior and remaining flexible-UI
  limitation;
- `docs/TESTING.md` for repaired trusted cases, phase-aware evidence, and the
  current campaign result;
- `docs/planning/NOW.md` for milestone status;
- the GPU testing refactor for exact trusted inventory/evidence status;
- `verification/registry.json` and generated selectors for any exact trusted
  rename/split; and
- this plan's status and implementation record.

No target wording is copied into current architecture before implementation.
No automated-verified wording appears before the relevant checks pass on the
same revision. No product-validated wording appears before the normal mapped
application is exercised and accepted by the owner.

## Completion Standard

This plan is complete only when:

1. C1 through C6 and H1 are closed at their diagnosed boundaries;
2. the fixed one/four semantic transaction predecessor is deleted and one
   bounded active/affected target authority remains;
3. every visible affected linked group publishes atomically while hidden and
   unrelated panels perform no required work;
4. same-body rebind remains O(1) and replacement uses the correct effective
   age;
5. the million-voxel ray passes through a reachability proof that retains all
   strict numerical limits;
6. the phase-aware drivers are deterministic without sleeps or retries;
7. a failed trusted run retains exact useful attribution and can never become
   green through reporting;
8. the exact complete trusted-GPU correctness inventory passes on a clean
   revision with zero unexpected skip, timeout, validation error, or
   unattributed selected case;
9. focused, public, documentation, and verification-sync checks pass;
10. the current standalone-3D and four-panel normal product are exercised and
    accepted for the affected workflows;
11. applicable existing performance evidence is run and honestly reported
    without a threshold or scientific-semantic change; and
12. documentation distinguishes implemented current modes from future
    flexible-layout capability.

A corrected assertion alone does not close C2, C4, or C5. A direct dirty-tree
GPU pass does not close the trusted lane. Portable dynamic-layout tests do not
claim that the future UI exists. A performance pass cannot override wrong or
partially published pixels.
