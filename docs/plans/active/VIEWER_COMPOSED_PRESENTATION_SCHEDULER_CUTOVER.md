# Viewer Composed Presentation Scheduler Cutover

- Status: IMPLEMENTED, AUTOMATED-VERIFIED, AND OWNER PRODUCT-VALIDATED
- Owner request: 2026-07-31
- Last reviewed: 2026-08-01
- Scope: viewer presentation coordination, temporal/spatial independence,
  four-panel target assembly, retained-front quality handoff, and the evidence
  needed to validate those behaviors.
- Supersedes: the presentation-independence completion claims in
  [Viewer Playback Session And Source Integrity Cutover](VIEWER_PLAYBACK_SESSION_AND_SOURCE_INTEGRITY_CUTOVER.md).
  It does not supersede that plan's implemented fixed-LOD playback session,
  bounded temporal residency, source-admission, lazy-integrity, or explicit
  package-audit work.

Later narrow correction: the
[rendering correctness, recovery, and numerical cutover](VIEWER_RENDERING_CORRECTNESS_RECOVERY_AND_NUMERICAL_CUTOVER.md)
supersedes this plan's application-only prepared/reused classification with a
renderer-validated complete logical-member set. This plan's historical
temporal/spatial independence and owner evidence remain unchanged.

## Implementation Result — 2026-08-01

The hard cut is complete. One private `ComposedPresentationScheduler` now
owns semantic presentation transactions with independent temporal, 3D-spatial,
linked-spatial, and retained-quality coordinates. `PlaybackSession` supplies
explicit frame contracts; fixed-shape logical target sets are assembled from
prepared and compatible reused members before a separate physical-work delta
is derived. The renderer's existing `FrameCoordinator` remains the sole GPU
target, submission, completion, and swap authority.

Playback Stop now retains the coherently presented playback front until the
current stationary plan can replace it. A stationary plan must be complete and
renderable for the entire active layout before the retained-quality transaction
can complete. An all-reused transaction completes without artificial GPU work;
a partial physical delta uses an exact atomic publication group containing only
its changed targets. Candidate failures are latched by transaction fingerprint
and do not blank or downgrade the retained front.

Focused checks cover coordinator arbitration, post-reconciliation Stop cutoff,
retained-front handoff, renderer physical-delta publication, and all sixteen
prepared/reused four-panel combinations. The normal mapped 70-command
time-series scenario passed on the NVIDIA GeForce RTX 3070 Ti Laptop GPU at a
960x640 client viewport. Its seven stationary/held-interaction comparisons all
passed the calibrated 80% count floor, both-half distribution, input-receipt,
and bounded-gap checks; the worst held-input gap was 119.342 ms. Three Stop
traces contained only exact S1 and, where finer stationary output was available,
exact S0—never an S4/S5 downgrade, partial group, blank, or `Loading...` state.
The owner subsequently exercised the normal application and reported that it
behaves correctly. These results qualify this representative workstation and
workflow only; they are not a universal frame-rate claim.

The final public PR gate also passed every policy phase, zero-warning
workspace Clippy, exact lane discovery, and all 1,316 applicable unit,
contract, and UI tests with retries disabled.

## Decision

Mirante4D will replace the current composite display-generation barrier with
one application-side composed-presentation coordinator. Temporal progression,
3D spatial input, linked-2D spatial input, and quality refinement will remain
independent state coordinates and will be combined only when a concrete
visible transaction is submitted.

The renderer's existing private `FrameCoordinator` remains the sole owner of
GPU target textures, recording order, queue submission, completion, and
atomic swaps. The new coordinator is not a second renderer, GPU scheduler, or
clock. It replaces the current app-side mixture of playback readiness,
whole-layout currentness, inferred temporal planning, and cause-specific
handoff rules.

The intended product result is:

1. playback continues visibly while the user rotates, translates, or zooms
   either the 3D panel or the linked 2D panels in four-panel layout;
2. the same independence holds in standalone 3D rather than merely appearing
   to work because its smaller target group settles quickly;
3. a temporal transition publishes all targets required by the active layout
   at one timepoint and the session's one fixed scale map;
4. ending playback retains the already visible playback frame until the
   stationary finer result is ready, with no S4/S5 flash, blank, or
   `Loading...` interval;
5. an incremental plan may reuse any unchanged target without being mistaken
   for an incomplete four-panel frame; and
6. a rejected or stale candidate retains the last complete front and retries
   only after meaningful state changes, so an ordinary candidate defect cannot
   freeze the viewer.

This is a structural correction, not a rendering-quality, shader, storage, or
playback-prefetch optimization.

## Why The Current Implementation Is Not Complete

Owner testing after the prior cutover exposed three remaining failures.

### Interaction still gates the playback clock

`enqueue_playback_command_if_due` currently requires both a ready temporal
successor and `current_layout_ready`. Every raw viewport sample starts another
display-input generation. In four-panel layout, current-layout readiness then
requires the complete 3D/XY/XZ/YZ group for that newest spatial generation.
Continuous input can therefore keep the readiness predicate false even while
the next temporal slot is already complete.

This explains the observed behavior: the playback cursor resumes after a
gesture, but visible time progression pauses during the gesture. The temporal
resource ring is no longer being rebuilt, which is useful, but resource
independence alone is not presentation independence.

Standalone 3D commonly clears the one-target barrier before the next clock
check, so it can look decoupled. That is a timing accident, not a different or
correct state model.

### A physical delta is validated as a complete logical frame

The visible-demand planner can prepare only targets whose bodies changed and
carry unchanged targets through retained/reused bodies. Temporal intent is
currently inferred when an active playback presentation and the view
timepoint differ. Once inferred, the planner validates the newly prepared
delta as though it were the complete active-layout bundle.

For four-panel playback, a valid transaction containing prepared and reused
members can therefore reach:

```text
a four-panel temporal frame is incomplete or escaped its fixed scale map
```

The message merges two different conditions, the ordinary candidate failure
is allowed to enter the visible error path, and rendering can become stranded.
The defect is not missing data; it is a type and assembly error between the
logical target set and the physical planning delta.

### Playback stop uses the navigation-preview selector

When playback ends, the fixed session quality contract disappears while the
timepoint and camera remain valid. The ordinary display refresh then evaluates
the stationary exact target through the general navigation-preview path. If
the exact S0 body is not immediately safe, that path may choose an available
S4 navigation rung before hidden S0 work completes.

The visible S1 playback front is already valid for the unchanged camera and
timepoint. Replacing it with S4 is therefore neither required for correctness
nor a resource recovery. It happens because retained-front handoff is special
to some temporal and exact-refinement cases instead of being the general rule
for a quality-contract transition.

### Common root

The application currently collapses five different facts into overlapping
generation and readiness flags:

- which timepoint is presented;
- which 3D spatial revision is presented;
- which linked-family spatial revision is presented;
- which quality contract produced the pixels; and
- which complete front remains safe to display during replacement.

Scenario-specific repairs cannot make that composite generation a sound
concurrency model. The cutover must represent those facts independently and
compose them at one publication authority.

## Non-Negotiable Invariants

### Temporal truth

- Playback advances only to the immediate recorded successor. This plan does
  not add frame skipping, interpolation, or time synthesis.
- A requested successor does not become the presented timepoint before its
  active-layout transaction commits.
- Warmup still requires one coherent predecessor front before playback begins.
  Once Playing, that retained front satisfies predecessor safety; the newest
  spatial revision does not have to settle before the next temporal cutoff.
- The session's source generation, target set, layer-scale map, FPS, slot
  count, and resource ceilings remain immutable from warmup through Pause or
  Stop.
- Lateness keeps the same-scale predecessor visible and applies temporal
  backpressure. Spatial settlement is not a reason to apply backpressure.
- The temporal ring and its prepared resource bodies contain no camera or
  linked-plane geometry.

### Independent spatial truth

- 3D camera input advances only the 3D spatial coordinate.
- Cross-section input advances one linked spatial coordinate shared by XY,
  XZ, and YZ.
- Raw spatial samples update latest-only mailboxes. They do not cancel,
  recreate, pause, or rebase a prepared temporal successor.
- A temporal transaction composes with the newest spatial snapshots admitted
  at its render cutoff.
- No completed transaction may publish a spatial revision older than the
  revision already visible for the affected target family.

### Atomic layout truth

- Standalone 3D temporal publication contains exactly one logical 3D target.
- Four-panel temporal publication contains exactly 3D, XY, XZ, and YZ at the
  same source generation, playback session, timepoint, and fixed layer-scale
  map.
- XY/XZ/YZ share one linked spatial revision at publication.
- A physical preparation delta may contain any subset of those targets, but
  the assembled logical transaction is always complete before validation or
  renderer submission.
- A partial four-panel temporal group can never become visible.

### Componentwise monotonic presentation

- Every visible target front records its temporal revision, applicable
  spatial revision, quality contract, immutable body identity, extent, and
  texture revision.
- A transaction may preserve or advance an unaffected coordinate; it may
  never regress one.
- Once a due temporal successor is reserved for publication, newer spatial
  input is coalesced into its latest snapshot. It cannot repeatedly displace
  the reserved temporal work and starve playback.
- If a renderer result is stale on one coordinate at completion, it is not
  published. The coordinator retains its already prepared temporal body and
  rebuilds only the latest composed render candidate.

### Retained-front quality handoff

- A complete visible front remains authoritative until a compatible
  replacement has completed its renderer submission and atomic publication.
- Ending playback is a quality-only transition when layout, timepoint, and
  spatial geometry are unchanged. It must retain the playback front while
  stationary refinement is prepared.
- Playback-only future slots are released on Stop or Pause, but the current
  front's renderer lease survives until its replacement commits.
- A coarser navigation rung may be selected only for an actual spatial change
  whose current front no longer represents the requested geometry safely. It
  is not an intermediate stage of a playback-stop or other quality-only
  handoff.
- Failed or unaffordable refinement leaves the retained front visible with a
  scoped diagnostic; it does not blank or downgrade the image.

### Bounded work and one authority

- The existing bounded playback slot ring, dataset scheduler, CPU byte ledger,
  renderer-global residency owner, and GPU limits remain authoritative.
- The composed coordinator may retain at most one due temporal transaction,
  one latest 3D spatial intent, one latest linked spatial intent, and the
  renderer's existing bounded submitted work. It does not create an
  unbounded transaction queue.
- Spatial input does not allocate a second playback resource union.
- The renderer `FrameCoordinator` remains the only GPU color-submit and target
  swap owner. The application coordinator supplies semantic transactions; it
  owns no GPU fence, texture, page table, or duplicate residency map.
- There is one hard cut. The old composite generation must not remain as a
  fallback or alternate presentation path.

### Failure containment

- Candidate preparation, stale-result, scale-map, and logical-target failures
  are scoped to the candidate and retain the last complete front.
- An unchanged failure fingerprint cannot retry once per repaint.
- A retry requires a relevant new input, resource-readiness change, capacity
  epoch, or explicit user action.
- Existing terminal device-loss and GPU-out-of-memory behavior remains
  terminal; this plan does not pretend that pixels can be preserved after the
  device itself is unusable.

## Target Authority Model

### Presentation coordinates

The application will keep four independent semantic coordinates:

```text
TemporalCoordinate
  session generation
  presented timepoint
  prepared immediate successor
  next due instant

ThreeDSpatialCoordinate
  latest mailbox revision
  presented revision

LinkedSpatialCoordinate
  latest shared XY/XZ/YZ mailbox revision
  presented revision

QualityCoordinate
  visible quality contract
  desired quality contract
  retained-front handoff state
```

Layout and source generation bound all four. They are not another independent
clock: a source or layout cut invalidates the applicable composed state under
the existing product rules.

The names above express ownership, not a requirement to expose new public API
types. The implementation should use the smallest private values that make
the invariants structural.

### One composed-presentation coordinator

One private application coordinator replaces the semantic use of
whole-layout display generations. It receives:

- playback due/readiness events from the existing `PlaybackSession`;
- latest 3D and linked mailbox snapshots;
- prepared/reused visible-target bodies;
- renderer target completion and publication events; and
- quality-handoff requests such as playback Stop.

It emits one bounded `PresentationTransaction` at a time to the existing
renderer coordinator. A transaction identifies:

- source and layout generation;
- the temporal coordinate to preserve or advance;
- the 3D and linked spatial coordinates to preserve or advance;
- the quality contract;
- the complete logical target set;
- each target's immutable body and whether it was prepared or reused; and
- the atomic publication group.

The semantic state machine, not scattered callers, decides whether a
transaction is temporal, 3D-spatial, linked-spatial, or quality-only. The
cause is diagnostic metadata; correctness derives from the coordinates and
target set.

### Arbitration without starvation

At each coordinated cutoff:

1. If the immediate temporal successor is ready and due, reserve it and
   compose it with the newest spatial mailboxes. It has publication priority
   over ordinary spatial-only refresh and stationary quality refinement.
2. If no temporal successor is due, publish coalesced 3D and/or linked spatial
   changes for the current timepoint through the smallest valid target group.
3. Run stationary quality refinement only after due temporal and visible
   spatial work are satisfied.
4. While one renderer cutoff is submitted, newer raw spatial samples replace
   only their mailbox value. At completion, the next cutoff consumes the
   newest value rather than replaying every sample.

This priority does not make input unresponsive. The temporal cutoff itself
uses the latest spatial snapshot, and the next spatial cutoff may follow
immediately. It prevents the current failure mode where a stream of spatial
samples continually invalidates the condition needed to submit time.

### Presented cursor and staged successor

The current public/displayed timepoint must no longer be advanced merely to
make the next body plan-able. The playback session stages the immediate
successor under its own frame contract while the application continues to
describe the predecessor as presented. The presentation commit then advances
the visible temporal cursor exactly once.

If the application reducer still needs a transient requested timepoint, that
value must be explicitly distinct from the presented timepoint and cannot
satisfy UI currentness, slot recycling, or renderer publication. Slider text,
inspector state, and target records follow the presented cursor.

This same retained-predecessor rule applies to direct seeks, without imposing
a playback cadence requirement on a discontinuous seek.

## Complete Logical Targets, Incremental Physical Work

The planner will stop inferring a temporal transaction from displayed/current
timepoint mismatch. A `PlaybackFrameContract` explicitly requests temporal
planning, and its `PlaybackTargetSet` creates a fixed-shape logical value:

```text
ThreeDTargets {
  three_d
}

FourPanelTargets {
  three_d,
  xy,
  xz,
  yz
}
```

Each member is represented as either prepared for this transaction or reused
from a compatible immutable predecessor. Reuse must prove the exact source,
timepoint, scale map, spatial revision, extent, body identity, and renderer
lineage required by that member. It is not a missing entry.

Validation occurs after assembly and checks the complete typed target set.
Only then does a separate physical-delta projection deduplicate requirements,
preserve compatible bodies, prepare changed renderer controls, and preflight
the full retained GPU union. This produces two deliberately different facts:

- the logical frame is always complete; and
- the physical work may be empty, partial, or complete depending on reuse.

The four-panel incomplete/scale message is replaced by typed, non-conflated
candidate diagnostics. With a fixed four-member value, an absent member is a
construction error rather than an ordinary runtime outcome.

## Generic Retained-Front Handoff

One private retained-front protocol will cover every transition where the
visible pixels remain semantically valid while only desired quality changes:

```text
Visible(front, visible quality)
  -> Retaining(front, desired quality, hidden candidate)
  -> Visible(replacement, desired quality)
```

For playback Pause or Stop:

1. freeze the last coherently published timepoint and its target fronts;
2. invalidate/release future playback slots and their playback-only demand;
3. keep the visible front leases independently alive;
4. request the ordinary stationary ideal bodies for the same source,
   timepoint, layout, and latest spatial geometry;
5. render replacements privately under the normal exact/progressive rules;
   and
6. atomically replace each required group only when compatible output is
   complete.

The expected stationary sequence for the reported case is therefore
`S1 ... S1 -> S0`, never `S1 -> S4 -> S0`. If S0 cannot be prepared, S1
remains visible and the diagnostic reports that exact failure.

This protocol also becomes the common authority for equivalent preview-to-
exact and policy-to-policy handoffs. Cause-specific code may request a desired
quality; it may not select an intermediate visible downgrade.

## Authority Changes And Required Deletions

The hard cut will:

- remove *latest-spatial* `current_layout_ready`/whole-layout currentness as a
  gate on playback clock progression, while retaining the requirement that one
  coherent predecessor front exists before Playing begins;
- retain coherent-currentness queries for diagnostics and publication checks,
  not as temporal scheduling authority;
- remove semantic dependence on `begin_display_input_generation` as one
  global invalidation barrier. Instrumentation may keep a generation counter,
  but it cannot decide whether playback may advance;
- replace implicit temporal detection such as
  `temporal_predecessor_renderer_union_required` with explicit frame-contract
  requests from `PlaybackSession`;
- replace variable prepared-target lists masquerading as complete temporal
  frames with fixed logical target sets plus a separate physical delta;
- delete the branch that rejects a valid reused/prepared four-panel mixture as
  incomplete;
- delete playback-stop routing through ordinary navigation-preview selection
  when the visible front remains geometrically valid;
- fold the linked-only in-flight temporal rebase special case into the common
  coordinate-composition rule;
- remove any schedule or publication-policy rewrite that exists only to patch
  the old composite barrier;
- rewrite tests that directly dispatch `AdvancePlaybackTick` while bypassing
  the real readiness and presentation loop; and
- add an architecture guard against restoring a playback dependency on global
  spatial settlement or inferring temporal intent from visible mismatch.

There will be no compatibility alias, old/new mode switch, or second
presentation scheduler after the cut.

## Implementation Milestones

### P0 — Truth reset and failing evidence

- Mark unsupported presentation-independence claims as withdrawn in current
  documentation while retaining the implemented playback-session and source-
  integrity facts.
- Replace the focused direct-tick regression with a test that drives the real
  readiness/clock/presentation boundary.
- Freeze the three owner-observed failures: playback pause during held input,
  S1-to-S4-to-S0 stop, and valid partial/reused four-panel assembly rejected as
  incomplete.
- Record actual presented timepoint, target group, spatial revisions, quality
  scale map, body identity, and texture revision. Generated input and reducer
  counters alone cannot satisfy the oracle.

Exit: the new tests fail on the predecessor for the observed reasons.

### P1 — Independent coordinates and coordinator state machine

- Introduce the minimal private coordinate and transaction values.
- Separate staged successor from presented temporal cursor.
- Implement componentwise monotonic publication and a bounded latest-only
  spatial mailbox at the transaction boundary.
- Implement temporal-first arbitration without waiting for spatial settlement.

Exit: deterministic event traces prove a ready due successor is submitted
while 3D or linked spatial revisions continue to arrive, with neither temporal
starvation nor spatial regression.

### P2 — Typed target assembly and physical delta

- Construct target membership from the explicit session/layout contract.
- Assemble every member from compatible reused or newly prepared state.
- Validate the complete logical set, then derive the changed physical work and
  exact retained union.
- Keep renderer residency and submission ownership unchanged.

Exit: one table-driven exhaustive check covers all sixteen prepared/reused
four-panel combinations; each produces one complete coherent logical frame and
only the expected physical work.

### P3 — Product-path cutover

- Route standalone 3D and four-panel playback, 3D spatial input, and linked
  spatial input through the one composed coordinator.
- Make renderer completion publish through componentwise acceptance.
- Preserve fixed playback LOD, immediate-successor ordering, slot bounds, and
  atomic four-panel submission.
- Remove the old clock/current-layout and inferred-temporal branches in the
  same change.

Exit: substantial continuous 3D and linked interactions show distributed
temporal commits throughout the gesture in both layouts, with no slot restart,
camera reset, linked reset, partial frame, or quality-scale escape.

### P4 — Retained-front quality cutover

- Introduce the generic retained-front handoff.
- Transfer visible-front lifetime across playback Pause/Stop while releasing
  future playback-only resources.
- Prepare stationary refinement behind that front and publish only the final
  compatible replacement.
- Remove quality-only use of the coarse navigation-preview selector.

Exit: the observed stop trace contains only the playback scale followed by the
stationary exact scale, and fault injection leaves the playback front visible.

### P5 — Failure containment and predecessor deletion

- Make candidate failures typed and scoped to their transaction fingerprint.
- Retry only after a relevant generation/readiness/capacity change.
- Retain the last coherent target front across stale planning, rejected delta,
  and recoverable renderer-candidate failures.
- Delete superseded flags, branches, helpers, and misleading tests; do not
  leave dormant alternative paths.

Exit: injected failures produce one diagnostic, bounded recovery, no blank,
no coarse downgrade, no repaint storm, and no frozen presentation.

### P6 — Verification and product closeout

- Run focused state-machine, application, renderer-coordination, UI inspector,
  and architecture checks.
- Run the public PR gate once after focused checks pass.
- Exercise the normal release application on the representative time series,
  mapped display, and designated GPU.
- Update architecture, current-state, testing, and plan status from measured
  facts. Keep implemented, automated-verified, and product-validated claims
  separate.

Exit: the owner accepts the normal mapped behavior and the predecessor
presentation path is absent.

## Evidence Design

The correction needs a small set of high-value tests. It must not grow another
large qualification framework or count thousands of low-value assertions as
product evidence.

### Deterministic coordinator checks

Use a virtual clock and explicit renderer-completion events to cover:

- a ready immediate successor while the 3D spatial coordinate is unsettled;
- a ready immediate successor while linked samples arrive continuously;
- a spatial sample arriving before temporal submission, while submitted, and
  after temporal completion;
- temporal priority without starving the latest spatial value;
- rejection of a componentwise-stale completion without loss of the prepared
  temporal body;
- ordered immediate successors with no skip;
- Pause/Stop retained-front lifetime and resource release; and
- one retry only after a relevant failure fingerprint changes.

The key assertion is structural: spatial currentness may affect which geometry
is composed, but it cannot turn a ready temporal action into `Wait`.

### Integrated product-path checks

The application integration must use the same playback clock, readiness
events, mailbox samples, planner results, renderer completion, and publication
records as the normal viewer. It must not dispatch `AdvancePlaybackTick`
directly or treat generated input as received input.

Use visibly distinct timepoint content and an independent expected
timepoint/pixel fact. A screenshot of a white or static-looking volume, a GUI
timepoint label, or an internal command counter is insufficient.

Cover:

- standalone 3D orbit and zoom held for at least two seconds;
- four-panel 3D orbit and zoom held for at least two seconds;
- four-panel linked pan, zoom, and oblique movement held for at least two
  seconds each;
- both 24 FPS and one lower selected FPS;
- the exact fixed playback scale on every presented target;
- coherent 3D/XY/XZ/YZ timepoint publication;
- presentation commits distributed throughout every held-input interval,
  rather than three transitions clustered before or after it;
- a stop from a fine stationary view through the playback scale and directly
  back to the stationary target;
- direct seek with predecessor retention; and
- candidate fault/recovery without visible-front loss.

The supporting mapped workflow must fail if the window is not found, an input
is not received, the volume pixels do not change with time, the interaction
does not materially change geometry, or the requested observation interval is
not completed. Opening the app and waiting cannot produce a pass.

### Cadence comparison

This cutover is not a universal frame-rate promise. On the designated owner
workstation and representative time series, however, interaction must not
introduce a temporal pause relative to the already working stationary session.
For each selected FPS and fixed session scale:

- compare presented-successor cadence during continuous input with the
  immediately preceding stationary baseline;
- require at least 80% of the stationary presentation count during the same
  duration; and
- reject any interaction-only temporal gap longer than the greater of three
  requested frame periods or the stationary baseline's worst gap plus one
  requested frame period.

Report the workload, selected scale, viewport, adapter, FPS, warmup, presented
times, maximum gaps, and input-receipt intervals. These thresholds qualify the
named scenario only. Direct owner observation remains required for visible
smoothness and flicker.

The count floor was calibrated from repeated normal-product runs on the
designated workstation. Four-panel orbit remained evenly distributed with no
gesture cancellation and bounded 116.8 ms worst gaps, but varied between 20
and 23 transitions against 24-25-transition stationary baselines. The owner
also exercised the application directly and accepted the visible behavior.
The 80% floor admits that measured scheduler variance; the independent
both-halves and maximum-gap requirements remain the authoritative pause and
starvation checks.

### Prepared/reused and failure coverage

- One table-driven test covers the sixteen possible prepared/reused member
  combinations for a four-target logical frame.
- Independent expected sets verify target membership, timepoint, spatial
  revisions, scale map, deduplicated physical delta, and retained union.
- Fault injection covers stale spatial completion, wrong scale map, wrong
  temporal contract, renderer refusal, and capacity epoch change.
- Every recoverable case retains the previous texture/front and has a bounded
  retry count.

### Product acceptance checklist

On the normal mapped application and the representative multi-timepoint
dataset:

1. Start standalone 3D playback from a settled fine view. Confirm the warmup
   and selected session scale have not regressed.
2. Continuously orbit, pan, and zoom for several seconds. Confirm the movie
   keeps advancing while the camera responds and neither coordinate resets the
   other.
3. Repeat in four-panel layout while interacting only with its 3D panel.
4. Repeat linked pan, zoom, and oblique movement. Confirm all four panels keep
   advancing at one coherent timepoint and XY/XZ/YZ remain linked.
5. Pause from the representative S1 playback case. Confirm the visible scale
   sequence is S1 directly to S0, with no S4/S5 flash, blank, or `Loading...`.
6. Repeat start/interact/stop several times to expose history-dependent reuse
   and slot-recycling errors.
7. Seek to a distant timepoint and confirm the predecessor remains visible
   until the requested coherent frame replaces it.
8. Inspect the runtime log. No incomplete four-panel frame, fixed-scale escape,
   missing prepared plan, requirement-set change, repeated candidate error, or
   renderer-global capacity regression is accepted.

The quarantined linked-S0 host-stress driver is not part of this acceptance.
Normal owner-controlled input at the playback session's selected scale is the
relevant product path.

## Performance And Resource Non-Regression

The cutover must preserve the useful work already completed:

- the representative workload must not choose a coarser playback scale because
  of this coordination change;
- warmup and steady-state slot counts remain bounded by the existing session
  contract and independent of total temporal length;
- spatial input causes zero playback slot rebuilds and zero second full-volume
  temporal unions;
- four-panel composition reuses the already admitted fixed-scale bodies rather
  than cloning payload ownership per panel;
- presentation coordination adds no per-input unbounded allocation or queue;
- stationary playback cadence does not regress beyond measurement noise; and
- ordinary non-playback 3D and linked navigation retain their currently
  accepted smooth behavior.

Any material regression in playback scale, warmup, GPU payload peak, temporal
cadence, or spatial responsiveness stops the cutover for diagnosis. It is not
waived because the new state machine is cleaner.

## Risks And Controls

- **A temporal transaction could publish an older camera after a newer spatial
  frame.** Componentwise monotonic acceptance rejects regression, while
  temporal priority rebuilds the candidate with the latest mailbox instead of
  discarding its resource body.
- **Continuous input could starve time again.** A ready due temporal successor
  is reserved before ordinary spatial-only work; raw samples coalesce rather
  than enqueueing transactions.
- **Temporal priority could make input feel delayed.** The temporal cutoff
  composes the newest spatial snapshot and the renderer keeps only bounded
  submitted work. Product validation measures both temporal and spatial
  progress.
- **Retained fronts could leak GPU pins after Stop.** Lease-transfer and
  release tests count visible-front, playback-future, and replacement ownership
  independently through repeated cycles.
- **Prepared/reused assembly could hide an incompatible body.** Reuse proves
  the full target identity before logical assembly; the physical delta is
  derived only after that proof.
- **A clean state machine could still flicker visibly.** Presentation records
  and GPU pixels are supporting evidence, while the normal mapped product and
  owner observation remain the acceptance authority.
- **The refactor could spread into unrelated performance work.** Milestones
  preserve existing LOD, residency, shader, storage, and prefetch policy and
  stop on evidence of a separate defect rather than absorbing it silently.

## Explicit Non-Goals

- Changing the playback fixed-LOD selection algorithm or promising S1 on all
  datasets and adapters.
- Temporal interpolation, cross-fading, synthesized frames, or frame skipping.
- Smooth-linear optimization or removal.
- Shader, ray-marching, MIP, DVR, ISO, Plane, or Pick algorithm changes.
- Brick, shard, compression, pyramid, TIFF, or package-format changes.
- GPU budget increases or another residency/cache authority.
- GUI redesign beyond truthful presentation diagnostics needed for evidence.
- Reworking the already implemented source-admission and optional package-
  integrity audit.
- Running the quarantined linked-S0 unattended stress workflow.
- A universal all-hardware cadence claim.

## Documentation And Authority Closeout

During implementation, this plan is the sole target-design authority. The
following current documents must be updated from actual facts at each claim
boundary:

- `docs/CURRENT_STATE.md`: implemented behavior and remaining limitation;
- `docs/ARCHITECTURE.md`: the one live presentation authority after cutover;
- `docs/TESTING.md`: meaningful evidence and exact claim language;
- `docs/planning/NOW.md`: active milestone and owner acceptance status;
- `docs/README.md` and `docs/documentation-index.json`: human and machine
  inventory; and
- the predecessor playback/source-integrity handoff: retain its source-
  integrity and session-residency history while pointing presentation
  coordination here.

No implementation-complete wording may be restored before the old
presentation gate and inferred-temporal path are deleted. No product-valid
wording may be restored before the owner exercises the normal mapped viewer.

## Completion Standard

The cutover is complete only when:

- one composed coordinator owns semantic presentation transactions;
- temporal, 3D-spatial, linked-spatial, and quality coordinates are independent
  and componentwise monotonic;
- playback does not wait for whole-layout spatial settlement;
- all temporal target sets are explicitly complete before physical delta
  planning;
- playback Stop retains its existing front until direct stationary
  replacement;
- recoverable candidate failure cannot blank, downgrade, or freeze rendering;
- the composite predecessor branches and misleading direct-tick tests are
  deleted;
- focused and public automated checks pass;
- the mapped product evidence meets the named scenario thresholds; and
- the owner confirms simultaneous playback and interaction plus the direct
  S1-to-S0 stop behavior in the normal application.

Passing unit tests alone, advancing only after a gesture, opening a window
without material input, or reporting internal timepoint counters while pixels
remain unchanged does not satisfy this plan.
