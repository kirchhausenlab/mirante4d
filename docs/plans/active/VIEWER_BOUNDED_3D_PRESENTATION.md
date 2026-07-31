# Viewer Bounded 3D Presentation Plan

- Status: SUPERSEDED BY `VIEWER_NATIVE_RESOLUTION_NAVIGATION.md`
- Planning requested by owner: 2026-07-30
- Implementation authorized by owner: 2026-07-30
- Last reviewed: 2026-07-30

This document records the implemented predecessor to the
[native-resolution navigation cut](VIEWER_NATIVE_RESOLUTION_NAVIGATION.md).
Its reduced-output preview, visible timing probes, and same-camera preview
upgrade policy are no longer current architecture and must not be restored.
The successor retains only its complete-preview settlement, hidden exact
screen strips, atomic promotion, latest-only cancellation, and one
renderer/residency authority.

## Adaptive Cost And Preview-Quality Correction

The owner product-validated the primary bounded-presentation outcome: 3D
interaction remains smooth. The first mapped follow-up also exposed two
quality/settlement defects in the initial controller:

- MIP is permanently charged one nominal unit per sample tap while DVR and ISO
  are permanently charged two, so the unmeasured guess directly creates
  approximately twice as many DVR/ISO screen tiles; and
- an unsafe navigation-floor preview can truthfully use S3 data while rendering
  so few internal pixels that it looks like S5 or S6 after enlargement.

The values one and two were conservative shader-cost guesses, not mapped
measurements. MIP usually performs a sample and maximum update before one
post-ray transfer. DVR performs transfer, optical, exponential, and
compositing work per contributing sample. ISO performs transfer and threshold
work per traversed sample and may perform six gradient samples at a hit.
Those facts justify different *initial uncertainty*, but not a permanent 2:1
runtime ratio. Early termination, page rejection, data values, camera
geometry, sampling, and GPU behavior can reverse or greatly change the ratio.

The reduced-resolution preview remains required. A coarser dataset level
reduces ray steps but not the number of screen rays, so even a complete
navigation floor can exceed the interaction duration at a large output
extent. The correction changes who chooses preview quality; it does not
restore an unbounded native-size first pass.

### Corrective Outcome

The one bounded 3D presentation controller will:

- count neutral sample-tap work without a permanent MIP/DVR/ISO multiplier;
- use the former relative mode costs only as conservative first-observation
  priors;
- learn safe work throughput from asynchronous GPU timestamps per bounded
  workload family rather than through one dataset-global ratchet;
- contract promptly after slow work and recover upward gradually after
  repeated fast work;
- transfer calibrated throughput across camera positions and extents through
  normalized work while keeping projection, layer mode composition, sampling,
  and ISO shading distinct;
- derive direct, preview, and hidden-tile sizes from that measured authority;
- keep one hidden candidate's tile geometry stable so learning cannot discard
  already rendered work;
- compare every complete uniform preview body already available to the
  presenter—the selected target when complete and the installed navigation
  floor—at each body's safe internal extent;
- select the candidate with the best combined dataset-detail and screen-detail
  score, preferring screen resolution when effective detail ties;
- treat a one-half linear internal-resolution ratio as the preferred perceptual
  floor, never as permission to exceed the measured interaction bound; and
- replace a pessimistic current-camera preview with a materially better
  calibrated preview before exact tiling, rather than leaving the first small
  image visible until final promotion.

If no available complete body can satisfy the preferred perceptual floor, the
smaller bounded preview remains the correct last resort. The inspector must
continue to report both its actual uniform dataset scale and actual internal
extent. No preview may be described as exact.

### Corrective Invariants And Scope

- GPU timestamps remain asynchronous. The UI thread never waits for a query.
- Slow observations reduce future work immediately. Upward learning is
  rate-limited, requires repeated fast evidence, retains safety headroom, and
  never exceeds the full logical output.
- Calibration state and pending timing associations remain fixed-capacity and
  reset on dataset retirement. One render mode or sampling family cannot
  permanently lower another family's quality.
- Missing timing support retains conservative bounded priors and functional
  preview/exact refinement.
- Preview selection may use only a complete prepared uniform body already
  covered by the ordinary global residency authority. It cannot manufacture
  an LOD, sample spatially mixed volume scales, or create another residency
  cache.
- A finer selected body that is incomplete cannot become a preview candidate.
  The navigation floor remains the first current-camera image in that case.
- Preview upgrades retain the same frame/camera meaning but are real new
  presentations with truthful texture and internal-extent revisions.
- Exact refinement remains native-size, hidden, uniform-scale, and atomic.
- Linked Plane presentation, scientific shader semantics, storage, payload
  capacity, and the quarantined linked-S0 workflow remain outside this
  correction.

### Authority Changes And Deleted Behavior

`VolumePresentationController` remains the only 3D frame-work authority, but
its state changes from global interaction/refinement limits to a bounded
work-family calibration cache. A work family contains projection and ordered
visible-layer mode/sampling/shading classes; camera, output extent, and catalog
scale contribute normalized work rather than creating permanent isolated
calibrations.

The application remains the only selector of prepared render bodies. It will
construct the available floor/target preview candidates, ask the controller
for their safe extents and quality ordering, and issue exactly one selected
request. Renderer residency remains authoritative for completeness.

This hard cut deletes:

- permanent per-ray-step MIP `1` versus DVR/ISO `2` multiplication as the
  steady-state scheduling authority;
- one dataset-global monotone-only interaction work limit;
- one dataset-global monotone-only refinement work limit;
- cross-mode poisoning by a slow unrelated profile;
- restarting a same-camera hidden candidate merely because learned future
  tile size changed; and
- treating the first pessimistic preview extent as final until exact
  settlement.

There is no compatibility controller or second preview policy.

### Corrective Milestones

#### C0 — Plan And Truth

- Record this correction before source changes.
- Keep the existing mapped smoothness result separate from the new quality and
  settlement claims.

#### C1 — Neutral Work And Family Calibration

- Separate neutral sample-tap work from first-observation mode priors.
- Add a bounded per-family calibration cache.
- Apply immediate safe contraction and rate-limited bidirectional recovery.
- Feed direct, preview, and tile timestamps into the same normalized family
  authority.

#### C2 — Stable Exact Refinement

- Freeze the chosen tile height for one exact candidate identity.
- Allow new calibration to affect the next candidate without resetting the
  current candidate.
- Preserve exact-profile certification from a complete accumulated tile
  observation.

#### C3 — Perceptual Preview Selection

- Calculate safe extents for every currently complete uniform candidate.
- Rank candidates by combined physical data detail and internal screen detail,
  with deterministic screen-resolution and finer-data tie breaks.
- Prefer candidates at or above the perceptual floor when available.
- Schedule a materially larger same-camera preview after fast evidence before
  continuing exact tiles.

#### C4 — Truthful Product State

- Preserve separate logical output, shown uniform scale, internal preview
  extent, selected/ideal scale, exact tile progress, and currentness facts.
- Add bounded controller diagnostics only where they help falsify candidate
  selection or adaptation; do not create another evidence framework.

#### C5 — Focused Verification And Product Exercise

- Prove mode priors affect only unmeasured initialization and measured families
  converge independently.
- Prove slow contraction, fast recovery, growth bounds, cache bounds, and
  dataset retirement.
- Prove candidate ranking favors a higher-resolution coarser body when it has
  better effective detail and favors a finer body when calibrated resolution
  makes it superior.
- Prove preview upgrades are material, bounded, finite, and do not reset exact
  candidate progress.
- Re-run existing hidden/atomic/tiled-equivalence checks.
- Exercise MIP, DVR, and ISO in four-panel and standalone 3D through the normal
  mapped product. Inspect actual internal extent, visible preview quality,
  interaction continuity, tile settlement, and exact atomic promotion.
- Do not run the quarantined linked-S0 host-stress workflow.

### Corrective Completion

Implementation is complete only when the old global monotone controller and
permanent mode multiplication are absent, measured families recover in both
directions, the best available safe preview is selected and can visibly
upgrade, exact work remains atomic without learned-size restart, focused and
broad automated checks pass, and mapped product-validation status is reported
without being inferred from those checks.

## Implementation Result

B0 through B6 and C0 through C5 were completed on 2026-07-30. There is no
remaining source-refactor or automated-integration milestone in this plan.
Owner observation in the normal mapped application on the representative Cell
workload remains the final perceptual-quality acceptance check; it is not
silently inferred from fixture automation.

The implemented hard cut now:

- classifies the exact camera/output/layer/scale/mode workload before choosing
  a native direct frame;
- counts neutral ray sample taps and ISO hit-gradient taps, while using the
  former MIP `1` and DVR/ISO `2` values only to initialize an unmeasured
  family's conservative envelope;
- maintains a fixed-capacity calibration per projection and ordered
  mode/sampling/shading family instead of one global monotone work limit;
- contracts future work immediately after an over-budget timestamp and grows
  it through bounded, separately timed probes after fast observations;
- compares every already-complete selected-target/navigation-floor body at
  the internal extent allowed by its own calibrated work;
- chooses the best combined data-detail/screen-detail preview, with a
  half-linear-resolution preference when that remains inside the safe
  envelope;
- upgrades a pessimistic current-camera preview only after a material
  improvement, before beginning hidden exact tiles;
- retains full-size preview pixels as provisional and, once their timing
  certifies the matching direct profile, forces exactly one real direct pass
  and exact validation capture instead of stalling or relabeling preview
  pixels as exact;
- renders the selected exact body into the existing private full-size 3D
  candidate one bounded horizontal tile per coordinated cutoff, with a tile
  height frozen for that candidate;
- keeps every partial candidate hidden from presentation, picks, and
  validation capture;
- cancels mismatched hidden progress and restarts from the latest camera;
- atomically publishes only the completed exact candidate;
- suppresses settled-idle work only when the current exact frame is at the
  selected LOD, no staged refinement exists, and the renderer reports no dirty
  execution; and
- reports logical output, actual uniform data LOD, internal preview extent and
  linear-resolution percentage, direct/preview state, and hidden tile progress
  separately.

An unmeasured MIP family starts from 128,000,000 neutral work units. An
unmeasured DVR or ISO family starts conservatively at half that neutral work
because of its former `2` prior; that ratio has no authority after timing
evidence. Twelve milliseconds is the contraction boundary, observations at or
below six milliseconds may propose at most a twofold larger separately timed
preview, eight milliseconds certifies the exact matching direct profile, and
hidden-tile work is derived from the family's measured throughput for a
three-millisecond target. These are scheduling bounds, not universal
performance claims.

Final automated evidence establishes:

- all ordinary app and UI suites pass;
- focused policy checks cover neutral work, initial-only mode priors,
  independent family recovery and contraction, bounded cache retirement,
  perceptual candidate ordering, material preview upgrades, and stable
  per-candidate tile height;
- the focused real-GPU renderer contract keeps preview/candidate work hidden,
  cancels an obsolete candidate, publishes exactly once, and proves tiled
  MIP, DVR, ISO, and Mixed RGBA/coverage/validity exactly match ordinary direct
  rendering; the same contract proves a full-size preview requires and then
  settles through one real direct exact promotion;
- the focused real-GPU application contract keeps a bounded preview visible
  through hidden tiles, performs one exact atomic promotion, and performs no
  redundant settled submission;
- `cargo xtask verify-pr` passes exact discovery, policy, formatting,
  documentation, dependency, zero-warning Clippy, and 1,273/1,273
  unit/contract/UI checks; and
- the normal release application's 141-command
  `target_fixture_render_modes` scenario passes on a real mapped display with
  the Vulkan NVIDIA GeForce RTX 3070 Ti Laptop GPU, including nonblank exact
  MIP, DVR, and attached/detached-light ISO captures at the requested extents.

The mapped scenario is internal native-window automation and therefore
establishes product integration, not black-box interaction smoothness or
representative Cell preview quality. The owner exercise listed in B6/C5
remains the authority for that perceptual claim, but no implementation work is
held open behind it.

## Original Product Observation

The owner confirmed that linked XY/XZ/YZ interaction now remains smooth while
fine resources refine asynchronously. The independent 3D panel and standalone
3D view do not yet have the same property. Resident S3 is smooth on the
representative workload, but resident S2 can slow camera rotation, translation,
or zoom substantially.

This was not a new loading or residency regression. The diagnosed pre-cut
renderer treated a complete resident S2 body as a direct fast path and
recorded one native-size full-panel ray pass for every camera sample. “Fast”
meant that the path avoided planning, decoding, transfer, allocation, and
directory mutation. It did not mean that its fragment and ray work fit an
interaction frame.

The resource-oriented `FrameBudget` bounded resource visits, uploads, bytes,
command buffers, and submissions. By itself, it did not bound output pixels,
ray samples, shader iterations, or GPU duration. A single accepted S2 MIP,
DVR, ISO, or Mixed pass could therefore monopolize the GPU even while every
resource budget was satisfied.

## Outcome

Every accepted 3D camera sample must be able to publish current camera geometry
without waiting behind an unbounded fine full-panel ray pass.

The target product behavior is:

- use the complete resident selected target directly when that exact workload
  is conservatively within the interaction budget;
- otherwise render a complete uniform preview at a safe coarser LOD and, when
  needed, a reduced internal resolution that egui scales to the panel;
- keep that complete preview visible while the exact uniform target renders
  into the existing private 3D candidate in bounded screen-space tiles;
- expose no partial candidate and atomically swap the exact full target only
  after every tile for the same camera, body, extent, and rendering controls is
  complete;
- cancel an obsolete candidate at the next bounded checkpoint and prioritize
  the newest camera preview;
- learn from asynchronous GPU timestamps when a formerly conservative
  full-frame profile is safely direct, while immediately demoting a profile
  that exceeds the interaction budget;
- retain existing smooth S3/S2 behavior without a compulsory coarse flash when
  the direct target is actually safe; and
- report preview resolution, displayed LOD, selected LOD, exact refinement,
  and display currentness as separate facts.

This work makes no universal monitor-frame-rate claim. It establishes bounded
renderer work and graceful fidelity reduction inside the supported Vulkan and
1920×1080 product envelope. Product smoothness remains a mapped observation on
the relevant GPU, display, dataset, layout, modes, and input path.

## Non-Negotiable Invariants

### One Renderer And One Residency Authority

- `mirante4d-render-wgpu` remains the sole product renderer.
- The existing global residency owner, sparse directory, payload arena, frame
  coordinator, four public fronts, and one private 3D candidate remain the only
  GPU authorities.
- Preview and exact rendering use the same dedicated MIP, DVR, ISO, and Mixed
  shaders, canonical resources, controls, and residency.
- No CPU display path, second renderer, duplicate volume cache, per-mode
  residency, compatibility path, or storage-format change is added.

### Scientific And Visual Coherence

- A displayed 3D frame uses one uniform catalog scale per visible layer.
- Missing data is never converted to scientific zero or transparent valid
  data.
- Mean-reduced pyramid levels are never mixed spatially along a displayed ray.
- Preview fidelity is explicitly provisional and cannot satisfy exact capture,
  pick, export, analysis, or settled-presentation claims.
- Every exact candidate pixel is produced by the ordinary target shader with
  the exact target body and controls.
- Screen tiling changes only scheduling. Disjoint pixels are evaluated with
  the same camera and uniform body as a normal full-frame render.
- The exact candidate becomes visible and pickable only after its final tile
  submission has been accepted and the existing atomic front/candidate swap
  occurs.

### Bounded, Latest-Only Work

- Interactive preview work is bounded by a conservative ray-sample/tap
  envelope derived from physical output pixels, selected scale shapes, layer
  modes, and sampling policies.
- Preview resolution may decrease until that envelope fits. It may never
  increase work beyond the full output extent.
- Exact candidate work is split into bounded horizontal screen tiles. At most
  one next tile per candidate is recorded in one coordinated cutoff.
- A tile submission retains the same in-flight color and residency limits as
  every other coordinated pass.
- The first tile clears the hidden target; later tiles load the retained hidden
  result and write only their scissored rows.
- New camera, body, extent, layer, mode, sampling, transfer, timepoint, layout,
  or dataset input invalidates an unfinished candidate. Completed old work may
  retire safely but cannot publish.
- Tiling and timestamp state are fixed-capacity and cannot create unbounded
  maps, ticket queues, repaint loops, or submission fan-out.

## Presentation Contract

### Direct Exact Frame

The selected complete resident target is rendered directly at native panel
resolution only when one of these conservative authorities accepts it:

1. its calculated worst-case work fits the current bounded interaction work
   envelope; or
2. a matching workload profile has a complete asynchronous GPU observation
   comfortably below the interaction duration threshold.

The workload identity includes physical extent, visible layer identity,
uniform selected scale, render mode and parameters, and sampling policy.
Timing reuse is bounded and exact-profile-only. A slow direct observation
demotes the profile. Unknown or timestamp-unsupported cases remain
conservative.

Direct certification is an optimization, not the correctness authority.
Current camera geometry can always fall back to the safe preview path.

### Interactive Preview

When the selected target is not certified direct:

1. bind the complete full-volume navigation-floor body for the newest camera;
2. calculate a bounded internal extent that preserves the panel aspect ratio;
3. render the complete uniform preview in one bounded full pass at that
   internal extent; and
4. present the smaller texture over the full logical panel rectangle.

The camera's presentation viewport remains the full panel in logical points.
Only its physical internal sampling extent changes, so the preview covers the
same camera geometry rather than cropping or zooming it.

The preview frame records the full logical output extent for application
currentness and the smaller internal extent for fidelity truth. A preview can
be current geometry and complete at its shown LOD while remaining provisional
relative to the selected target.

### Atomic Exact Refinement

Once interaction settles, or whenever a current preview exists for a durable
camera:

1. bind the complete selected target body to the private 3D candidate;
2. allocate the candidate at the full native output extent;
3. choose a tile height whose conservative work fits the refinement envelope;
4. render one horizontal tile per coordinated cutoff;
5. keep the complete preview front visible and pickable throughout;
6. schedule the next tile only while the exact candidate still matches the
   latest request; and
7. after the final tile, publish one exact frame and atomically exchange the
   candidate and front.

Partial tile submissions are real GPU work but are not presentation events.
They do not update displayed pixels, exact currentness, product captures, or
picks. Validation capture is issued only for the completed candidate.

### Rapid Input And Cancellation

For S3→S2→S1→S0 or any other rapid input sequence:

- each input revision replaces the prior pending camera intent;
- a safe current-geometry preview is recorded first;
- an unfinished exact candidate for an older camera is discarded before any
  new candidate work is recorded;
- already-resident useful resources remain in the one global cache;
- loading and tiling pursue only the latest selected target; and
- the maximum delay before the newest preview is one already-submitted bounded
  tile, never an entire old fine frame.

## Cost Authority

The initial cost model counts a conservative upper bound over:

- internal output pixels;
- the maximum ray steps implied by each selected catalog shape;
- every visible layer;
- voxel-exact versus smooth-linear sample taps; and
- additional mode-specific work such as ISO gradient sampling.

Two distinct envelopes are required:

- an interaction envelope for a complete current-geometry preview;
- a smaller per-tile envelope for hidden exact refinement.

Asynchronous GPU timestamps are enabled for adaptive 3D color work when the
adapter supports them. They never block the UI thread. The application uses
them to:

- tighten the safe work envelope after an over-budget preview or tile;
- retain a bounded worst observation for each exact workload profile;
- certify a direct full frame only with safety headroom; and
- revoke that certification immediately after an over-budget direct sample.

Timestamp support is not mandatory. Without it, the conservative work model
remains the authority and exact rendering still proceeds through bounded
tiles.

The initial numeric work and duration envelopes are implementation constants,
not public performance claims. Their final values must be recorded in current
architecture documentation and may be adjusted only with focused mapped
evidence.

## Truthful State And Picking

The inspector must distinguish:

- logical panel extent;
- current internal 3D render extent;
- displayed uniform LOD;
- selected and ideal LOD;
- direct exact versus provisional preview;
- hidden exact tiles completed and total;
- target refinement pending;
- current versus stale camera geometry; and
- capacity adaptation versus frame-work adaptation.

Examples of truthful states include:

- `shown s3 · direct 3D 900×500`;
- `shown s3 preview 450×250 / selected s2`;
- `shown s3 preview 450×250 / selected s2 · exact tiles 4/12`; and
- `shown s2 · exact 3D 900×500`.

Picking continues to target the visible front. A partial hidden candidate can
never become pick authority. A preview pick, exact capture, or other
fidelity-sensitive operation must either report its actual preview fidelity or
wait for the exact swap; it must not be mislabeled exact.

## Authority Changes

### Render API

- Add one explicit coordinated 3D color schedule: direct full frame,
  interactive preview, or atomic exact tiles.
- Keep the logical output extent separate from the internal render extent for
  preview requests.
- Report bounded atomic-tile progress independently from presentation.
- Bind timing tickets to the actual timed target and submitted tile/full-frame
  work.

### Renderer

- Extend the existing private 3D candidate with fixed tile-progress state.
- Add scissored color-pass recording with clear-on-first-tile and
  load-on-successor semantics.
- Treat a partial tile as hidden work, not a presented frame.
- Preserve completion leases until every submitted tile finishes.
- Cancel mismatched candidate state through the existing private-presentation
  deactivation path.
- Capture and atomically promote only the final exact candidate.
- Keep Plane rendering and its visible per-brick refinement unchanged.

### Application

- Add one bounded 3D presentation-quality controller.
- Select direct target versus navigation-floor preview for each active camera.
- Calculate preview extent and exact tile height from the one cost authority.
- First publish current durable camera geometry as a preview when the exact
  target is not direct-safe, then drive the hidden exact candidate.
- Associate asynchronous 3D timing tickets with bounded workload observations.
- Keep dataset selected LOD and residency refinement independent from
  presentation-resolution adaptation.

### UI

- Scale the renderer's internal preview texture to the normal logical panel.
- Expose preview, internal extent, and exact tile progress without calling
  hidden work visible.
- Preserve independent linked-2D fidelity reporting.

## Hard Cut And Deleted Behavior

The implementation deletes these behaviors rather than retaining parallel
branches:

- treating every resident S2/S1/S0 body as automatically interaction-safe;
- issuing one unbounded native-size fine ray pass for every active camera
  sample;
- equating upload/resource budgets with a render-time budget;
- making a full exact 3D render the indivisible unit of hidden refinement;
- reporting a reduced-resolution preview as native-resolution exact output;
- allowing an obsolete hidden fine render to delay newer camera input; and
- running timing only in external diagnostics while product scheduling needs
  the result.

The global residency design, adaptive capacity selector, volume shader
semantics, linked-2D progressive path, storage format, and quarantined
linked-S0 host-stress workflow are not replaced.

## Implementation Milestones

### B0 — Plan And Contracts

- Record this plan and link it from the documentation authority.
- Add focused pure checks for workload bounds, aspect-preserving preview
  extent, tile coverage, direct certification, demotion, and bounded state.

### B1 — Explicit 3D Scheduling Contract

- Add direct, preview, and atomic-tile scheduling to coordinated requests.
- Separate logical output extent from internal render extent.
- Reject preview/tile schedules on Plane targets or malformed extents.
- Report hidden tile progress without claiming presentation.

### B2 — Renderer Atomic Tiling

- Add candidate tile state and scissored recording.
- Clear once, load thereafter, cover every output row exactly once, and submit
  no work after completion.
- Keep partial candidates hidden and picks on the front.
- Cancel an obsolete candidate and preserve bounded in-flight leases.
- Atomically swap only the completed exact candidate.

### B3 — Bounded Preview And Cost Controller

- Compute mode/layer/scale-aware conservative work.
- Select a safe navigation-floor preview extent during unsafe interaction.
- Enable nonblocking adaptive 3D timestamps where supported.
- Learn exact-profile direct safety with headroom and demote slow profiles.
- Keep every cache, accumulator, and pending timing queue bounded.

### B4 — Application Cutover

- Make active 3D samples choose certified direct target or bounded preview.
- Publish the latest durable camera preview before hidden exact tiling when
  needed.
- Cancel obsolete exact work during continued input.
- Preserve dataset staged-refinement promotion only after exact atomic swap.
- Remove the unconditional resident-target preference.

### B5 — Product Truth And Focused Evidence

- Add preview/internal-extent/tile facts to the inspector.
- Prove pure tile coverage and latest-only cancellation.
- Prove a partial Vulkan candidate does not replace the visible front.
- Prove the final tiled Vulkan output matches the existing independent exact
  pixel oracle.
- Prove a certified safe direct S3/S2 request still uses one native full pass
  without coarse staging.
- Prove an unsafe target issues bounded preview/tile work rather than one fine
  full pass during active input.

### B6 — Integration And Product Validation

- Format and run focused render API, renderer, application, and UI checks.
- Run the public PR gate once after focused checks pass.
- Run the bounded trusted local Vulkan checks affected by scheduling.
- Exercise the normal mapped application in four-panel and standalone 3D:
  S3 and S2 rotation, translation, and zoom; a finer unsafe target; interruption
  of an in-progress exact refinement; MIP, DVR, ISO, and Mixed; and inspector
  truth.
- Do not run the quarantined linked-S0 desktop-driving workflow.

## Focused Acceptance

Automated evidence must establish:

- preview extents preserve aspect ratio and never exceed the output extent;
- conservative preview and tile work never exceed their declared envelopes
  except for the unavoidable one-pixel minimum;
- tile rectangles cover every output pixel once without gaps or overlap;
- a partial candidate records GPU work but publishes no frame or texture
  revision;
- the final tile publishes exactly once and swaps atomically;
- new input retires old tile progress and starts from the newest camera;
- partial candidates are never used for picks or validation capture;
- exact tiled MIP/DVR/ISO/Mixed pixels use the ordinary shaders and match an
  independently rendered exact result;
- slow measurements tighten preview work and revoke direct certification;
- matching safely timed profiles retain the direct native-resolution path;
- timing-unavailable operation remains bounded and functional; and
- settled idle submits no additional tile or color work.

Product validation must directly observe:

1. ordinary four-panel S3 3D interaction remains as smooth as before;
2. a previously smooth S2 case does not gain a noticeable preview when its
   measured direct cost is within budget;
3. an expensive S2 or finer case moves continuously through a complete
   preview rather than freezing;
4. exact quality refines after interaction and swaps coherently;
5. interrupting refinement immediately prioritizes the new camera;
6. standalone 3D follows the same policy at its larger extent;
7. MIP, DVR, ISO, and Mixed do not show partial-volume artifacts; and
8. the inspector agrees with visible preview/exact behavior.

Automation, client-surface capture, internal currentness, and GPU timestamps
remain supporting evidence. They do not establish monitor-visible continuity.

## Risks And Containment

- **Preview is unnecessarily coarse:** exact-profile timing can certify the
  direct path; conservative profiles are not permanently hardcoded to S3.
- **First unknown profile is expensive:** unknown work starts from the bounded
  model, not an optimistic fine full pass.
- **Tile still takes too long:** the per-tile work envelope is smaller than the
  interaction envelope and tightens after an over-budget timestamp.
- **Partial 3D semantics leak:** only screen tiles in a hidden uniform-scale
  target are mixed during construction; the front remains one complete frame.
- **Quality flicker during input:** one interaction chooses a stable preview
  policy with hysteresis; direct certification changes only at controlled
  request boundaries.
- **Timing overhead regresses hot paths:** timestamps are asynchronous, bounded,
  and requested for adaptive 3D work; no synchronous GPU wait is introduced.
- **Candidate work starves input:** one bounded tile is the maximum old work
  ahead of the next preview, and active target ordering remains authoritative.
- **Memory increases:** the existing fifth private allocation is reused; no
  sixth target or duplicate residency is introduced.
- **Tests overclaim:** all automated reports state their actual boundary, and
  owner-visible product observation remains required.

## Completion Language

- **Implemented** means the hard-cut source and owning documentation exist.
- **Automated-verified** means the focused checks and relevant integration
  gates pass at the stated revision.
- **Product-validated** means the normal native application was exercised on a
  real mapped display with the representative data, GPU, layouts, modes, LODs,
  and actual camera input described above.

The plan is complete only when obsolete unbounded resident-target behavior is
deleted, the exact candidate is screen-tiled and atomic, the UI is truthful,
focused and integration checks pass, and mapped product-validation status is
reported without conflating it with automation.

At the 2026-07-30 checkpoint, the implementation and automated-verification
conditions are complete. The only open condition is the explicitly separate
mapped product-validation observation; it is not an unfinished code backlog.
