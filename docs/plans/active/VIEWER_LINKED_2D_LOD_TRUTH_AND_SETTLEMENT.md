# Viewer Linked-2D LOD Truth And Settlement Plan

- Status: CLOSED — PRODUCT CORRECTION VALIDATED; HOST-STRESS EVIDENCE QUARANTINED
- Planning requested by owner: 2026-07-29
- Implementation authorized by owner: 2026-07-29
- Last reviewed: 2026-07-31

This document is the closed implementation and evidence handoff for the
linked-2D LOD and settlement correction. [Current State](../../CURRENT_STATE.md)
remains the authority for implemented facts and retained limitations.

## 2026-07-29 Evidence Correction And Approved Diagnostic Cutoff

The linked-2D product correction described below has substantially landed and
the owner has confirmed manually that ordinary linked zoom can now display S0.
That does **not** validate the continuity claims made by the first real-window
linked-wheel harness.

The owner ran that harness and observed the mapped viewer select four-panel
layout, place the pointer over XY, remain visibly static, and then close. An
audit found that the reported XY/XZ/YZ “visible changes” and
input-to-“visible” latency were synthesized from internal coordinated-target
publication events. They were not pixel observations. The retained PPM
artifacts were direct `XGetImage` reads of the X11 client/window surface. On
the current X11/NVIDIA/GNOME path those reads can change while the mapped
monitor image remains static, so they are useful client-surface artifacts but
not compositor or human-visible evidence.

Consequently:

- all linked-wheel visible-change counts, visible-gap values,
  input-to-visible latency values, and pass/fail conclusions derived from
  coordinated publication are withdrawn;
- no client-window readback may be described as monitor-visible or
  compositor-presented;
- the linked-wheel workflow may remain only as an endpoint correctness check
  for an ordinary S3-to-S0-to-S3 input path, with its internal publication and
  client-surface evidence labelled at those exact boundaries;
- internal target publication remains useful causal evidence, but it is not a
  visible-output oracle; and
- until a reliable compositor/monitor capture boundary is demonstrated on the
  actual host, the owner’s direct observation is the authority for
  human-visible continuity.

The owner has authorized only the following diagnostic work before another
review:

1. hard-cut the false linked-wheel visibility metrics and rename every retained
   artifact and metric to its actual observation boundary;
2. make the GUI report independent 3D and linked-2D LOD truth, including
   displayed, selected/target, ideal, provisional/refining, and per-panel
   divergence;
3. add a faithful, visibly moving real-window workload using the normal release
   application and a representative dataset: four-panel layout, exact warm
   S3, S1, and S0 phases, then bounded continuous real Shift-drag at 60 Hz for
   30 seconds per phase without deliberately crossing the resident reuse
   envelope; and
4. retain diagnostic timestamps and counters at the useful boundaries:
   generated and received input, UI update, demand planning, read/decode/upload,
   renderer CPU work, linked GPU pass, and window/surface scheduling. Any
   unobserved presentation boundary must be stated explicitly.

The continuous workload is diagnostic, not a performance acceptance gate. It
must visibly move while it runs, return the view to a settled state, close the
application, and produce plain evidence that distinguishes measurements from
owner-observed monitor continuity.

**Hard stop:** this authorization does not include renderer, scheduling,
resident-envelope, demand, upload, or presentation performance fixes. After
the four items above are implemented and exercised, work stops for an owner
report and independent confirmation. Performance repair resumes only after a
later explicit authorization.

### 2026-07-30 implementation and safety checkpoint

The approved diagnostic implementation now exists:

- the linked-wheel path emits endpoint-correctness evidence only; the former
  synthetic “visible” metrics and pass/fail conclusion are deleted, and its
  X11 artifacts are named and described as client-surface reads;
- the inspector reports independent `3D scale` and linked-2D fidelity. Linked
  panels report shown, selected, ideal, exact/provisional, refining, and
  display-current facts together, splitting into XY/XZ/YZ rows if they
  diverge;
- the new linked-LOD diagnostic generates a bounded, nonstationary 60 Hz
  Shift-drag workload at exact warm S3, S1, and S0, returns to S3, and records
  the approved causal boundaries without assigning a performance threshold;
  and
- the trace distinguishes generated input, egui receipt, UI-update duration,
  planning, queue/read/decode/upload counters, renderer CPU work, linked GPU
  timestamps, and egui texture-paint command queuing. Window-surface present
  and compositor/monitor visibility remain explicitly unobserved.

The first full live exercise is not complete. One normal mapped run completed
the generated S3 and S1 phases, then the desktop stopped making useful
progress while the workflow attempted to reach S0. The pointer remained
movable, but the rest of the session required a reboot. The abrupt reboot
left `app-trace.csv` empty, and the previous-boot journal contains no OOM kill,
NVIDIA Xid, GPU reset, or kernel-hang diagnosis. Therefore:

- no performance number, S0 diagnostic result, or freeze root cause may be
  inferred from that run;
- the only retained run facts are the generated S3/S1 input paths and the last
  live linked status, which still described exact S1;
- both real-window workflows that deliberately enter linked S0 are
  quarantined by default and require the explicit `--allow-host-stress`
  acknowledgement;
- scale selection now has a strict 16-input cap, requires receipt of each
  wheel event before another is sent, supervises live UI-turn progress during
  settlement, checks Shift-drag receipt progress twice per second, and releases
  held input on failure;
- the bounded app timeline is checkpointed on live input/UI heartbeats instead
  of being retained solely until graceful exit; and
- these safeguards are statically verified but have deliberately not been
  re-exercised on the real display after the reboot. They reduce unattended
  exposure; they do not prove that an acknowledged S0 run is safe.

At that checkpoint this was the required owner-report stop. The older design
and acceptance sections below remain the historical corrective handoff, but
this checkpoint supersedes any wording below that could be read as an active
performance claim or permission to continue implementation automatically.

## 2026-07-31 Closure

Subsequent approved placeability, progressive-presentation,
native-resolution, GPU-memory, and resident-navigation-ladder cuts completed
the product correction without relying on the quarantined diagnostic. The
owner exercised the normal mapped four-panel product repeatedly and confirmed
that linked panels reach the appropriate fine scale, remain smooth through
ordinary interaction, refine without dark holes, recover from transient
coarse presentation, and no longer retain the inconsistent selected-`none` or
historical capacity-warning state.

The original monitor-performance thresholds below are not accepted results:
their required observation boundary was shown to be invalid, and the
replacement host-stress workflow was intentionally quarantined after freezing
the desktop. Those withdrawn claims are an evidence limitation, not an
unfinished product defect or an obligation to rerun unsafe automation.

The correction is therefore closed on owner-observed normal-product behavior,
focused independent correctness evidence, trusted Vulkan checks, and the
ordinary repository gate. No authorized implementation or validation work
remains in this plan. Any future linked monitor-continuity measurement is a
separate, explicitly approved evidence task.

## Decision

Repair the evidence boundary before repairing the product defect.

The first accepted failing check must exercise incremental linked-2D zoom,
capture the actual XY, XZ, and YZ GPU output, and determine the represented
source scale independently of application currentness metadata. Only after
that check fails on the current product may implementation change the
resident-interaction and exact-demand authorities.

The production hard cut will separate:

1. **resident interaction coverage** — whether a complete previously installed
   body can be reprojected temporarily while input remains active; and
2. **settled exact currentness** — whether the complete displayed body matches
   the latest durable view and the adaptively selected feasible per-layer LOD.

Resident reuse may preserve responsiveness. It may not update or satisfy the
installed exact-demand identity when its LOD differs from the settled selected
target.

This is a general correction over all catalog scales, visible layers,
cross-section panels, affine grids, datasets, viewports, and hardware. No
production branch may special-case S3, S2, S1, S0, the representative local
dataset, or one zoom direction.

## Blocking Product Observation

The owner observed that the XY, XZ, and YZ panels in four-panel layout remain
visibly coarse regardless of how far the linked 2D view is zoomed in. The GUI
does not expose the linked-panel LOD clearly because its prominent fidelity
projection describes the independent 3D view.

A read-only investigation reproduced the defect through the normal
incremental cross-section interaction state machine:

- the final linked view was held identical between two runs;
- a direct durable `SetCrossSectionView` path selected S0;
- ten ordinary contained zoom samples followed by explicit gesture finish,
  coordinated settlement, and runtime idle remained at S3;
- direct output had dominant one-source-voxel runs of approximately 5–6
  screen pixels;
- incremental output had dominant runs of approximately 42–43 screen pixels,
  the expected eight-times-larger S3 footprint; and
- the incremental report called all three linked panels complete, exact,
  current, and settled at S3.

The same final view therefore has a functioning S0 source, demand, upload,
shader, and presentation path. The failure is specific to incremental resident
reuse followed by durable settlement. Capacity is not the blocker in the
reproduced view: the direct S0 plan selected only nine primary resources per
panel.

The earlier direct-set report was valid only for its synthetic durable command.
It did not establish what ordinary wheel interaction displays. Any broader
inference from that report is withdrawn.

## Root Cause

The current linked interaction optimization intentionally holds the installed
LOD during a contained gesture. This keeps a warm interaction off the demand
planner and is valid as a provisional display rule.

The defect is a currentness identity collision:

1. The installed linked body is S3.
2. Each small zoom sample remains geometrically contained by the installed
   plane reuse envelope.
3. The runtime reuses the S3 body and updates
   `VisibleDemandPlanningSignature.cross_sections` to the transient geometry.
4. The installed scope and prepared render body still contain S3 resources.
5. At durable finish, `prepare_resident_durable_cross_sections` independently
   calculates that the projected target differs and correctly refuses exact
   durable adoption.
6. The fallback calls `request_visible_bricks`.
7. `required_cross_section_plan` compares the already-updated common and panel
   geometry signatures, but not the installed-versus-selected layer-scale map.
8. It returns an empty cross-section plan mask, pumps the unchanged S3 body,
   and permits S3 to become current and settled for the new durable geometry.

Zooming in further shrinks the finite plane inside the same reuse envelope, so
the false-current state can persist indefinitely. No S0 request is submitted;
the renderer is not receiving mislabeled S0 payloads and is not switching
between CPU and GPU rendering.

The exact defect is distributed across the responsibilities currently named:

- `prepare_resident_cross_section_intent`;
- `prepare_resident_durable_cross_sections`;
- `VisibleDemandPlanningSignature`;
- `required_cross_section_plan`;
- `request_visible_bricks`; and
- linked presentation currentness and settlement projection.

Implementation must follow those responsibilities rather than recorded line
numbers, which may move.

## Evidence Defect

The product defect survived the previous correction because the evidence
boundary tested a different path and then allowed metadata to validate itself.

Known gaps are:

- the cited S0 scenario directly assigned the final durable view instead of
  traversing incremental wheel samples and gesture settlement;
- it did not retain a linked-panel pixel artifact;
- the generic automation screenshot and image-stat helpers currently read only
  the 3D target;
- linked-panel diagnostics derive expected and displayed scales from planner,
  scope, requirement, and presentation facts that share the same authority;
- `Current`, `Exact`, `settled`, and `runtime_idle` can all pass while the
  adaptively required replacement was never submitted;
- the closest state test uses one very large scale jump that exits the resident
  envelope and therefore misses repeated contained zoom samples; and
- existing real-window zoom qualification drives the independent 3D panel,
  not linked 2D zoom.

Metadata remains useful for localization. It is not an independent oracle for
the visible scale represented by the pixels.

## Product Contract

For every linked cross-section interaction:

- XY, XZ, and YZ use one canonical linked view and one coordinated durable
  commit; the active panel affects priority, not geometry ownership;
- 3D retains its independent camera, LOD, refinement, and presentation
  currentness;
- a complete resident linked body may be reprojected during active input when
  its geometric reuse proof remains valid;
- such a body is provisional whenever its installed scale map differs from the
  selected feasible target for the latest settled intent;
- provisional pixels remain labelled with their actual displayed scale and
  cannot satisfy exact target currentness or final settlement;
- gesture finish recomputes ideal LOD and performs the ordinary aggregate
  feasibility selection before declaring the selected target;
- exact currentness requires the latest durable geometry, selected per-panel
  and per-layer scale map, immutable requirement-body identity, complete GPU
  coverage, frame identity, surface generation, and extent;
- a mismatched resident body remains visible while the selected replacement is
  prepared, but the UI reports refining or adaptive status rather than
  `Current`/`Exact` settlement;
- the linked synchronization group publishes the complete selected
  replacement atomically;
- an adaptively selected coarser level may settle only when aggregate
  feasibility actually selected that level and the UI reports the finer ideal
  separately; and
- later zoom, pan, slice, rotation, resize, layout, layer, time, playback, and
  capacity changes always reopen exact selection when their target identity
  differs.

The contract applies in both zoom directions and across adjacent or
non-adjacent catalog levels.

## Authority Cut

### Installed exact demand

The installed exact-demand authority must describe what is actually installed,
not merely geometry that a resident body can temporarily cover.

Per linked panel and visible layer it must retain, directly or through one
canonical immutable body:

- selected scale;
- exact primary requirement-body identity;
- durable cross-section geometry and viewport identity for which it was
  planned; and
- the intent revision that installed it.

`VisibleDemandPlanningSignature` may remain the exact-plan signature, or it may
be replaced by one more precise type. It may not be overwritten when only a
resident guard was promoted.

### Resident interaction coverage

The mailbox remains the sole latest transient geometry owner. Plane reuse
envelopes remain immutable proofs attached to installed bodies.

If implementation needs a cached statement that the installed body covers the
current transient geometry, that statement must be explicitly resident and
provisional. It must not duplicate selected LOD, requirement membership,
renderer residency, or durable exact currentness.

Prefer deriving this fact from the mailbox geometry, immutable envelope, and
installed body rather than adding another mutable signature.

### Selected target

Ideal LOD remains a pure projected-footprint result. Selected LOD remains the
ordinary aggregate-feasible adaptive result. Displayed LOD remains a fact of
the complete presented frame.

At settlement the exact-demand decision must use the selected feasible target,
not blindly force ideal LOD. On the reproduced representative view S0 is
feasible and must be selected. On a genuinely constrained view a declared S1,
S2, or S3 target may be correct.

### Currentness

`required_cross_section_plan` must mark a panel dirty when any exact target
dimension differs, including the selected layer-scale map or immutable body
identity. Geometry equality alone is insufficient.

The no-work path may run only after independently checking that every linked
scope's installed scale map and exact body match the selected target.

### Presentation and settlement

Frame adoption and settlement must require the exact installed body that
authorized the color output. A complete older or coarser frame may remain the
visible navigation front but cannot be relabelled as the selected replacement.

`coordinated_presentation_settled` and `runtime_idle` must not pass while:

- a settled target decision has not run;
- selected and installed scales differ;
- selected resources are queued, decoding, uploading, or awaiting color
  publication;
- only a provisional resident frame represents the new geometry; or
- linked panels disagree about the synchronization generation.

Idle is an absence-of-work fact. Settlement is a target-satisfaction fact.
They must remain distinct even when both are used by one finite scenario.

## Test And Evidence Contract

### First principle

The final scale claim must be falsifiable without trusting the scale metadata
being tested.

Every linked-2D fidelity acceptance check must compare actual panel pixels with
independently derived expected pixels or source samples. Block size, edge
frequency, nonblank statistics, planner keys, currentness flags, and scope
scale fields are supporting diagnostics only.

### Audit scope

Before production repair, inventory every live test, benchmark, product
scenario, report, and validator that depends on one or more of:

- linked cross-section LOD or refinement;
- `SetCrossSectionView` as a proxy for interaction;
- `cross_section_zoom_sequence`;
- resident plane-guard promotion;
- `expected_scale_level` or `displayed_scale_level`;
- `Current`, `Exact`, coordinated settlement, or runtime idle;
- generic product screenshots or current display image statistics;
- four-panel smoothness or rendering-performance claims; or
- direct command timing presented as real interaction timing.

Classify each item as:

1. **retain** — it has an independent expected fact and its name states its
   narrow claim;
2. **rename/narrow** — it is useful for state or direct-command behavior but
   cannot support visible interaction or LOD claims;
3. **replace** — its claim is needed but its oracle is circular or its path is
   unrepresentative; or
4. **delete** — it duplicates stronger evidence or proves only that the
   implementation called itself.

The audit result is a short plan checklist or code-review table, not a new
registry, schema, receipt, hash, provenance graph, or evidence database.

### Per-panel capture

Hard-cut the generic capture boundary so a caller explicitly names `3D`, `XY`,
`XZ`, or `YZ`.

- Capture uses the normal renderer frame coordinator and asynchronous GPU
  readback path.
- A requested linked-panel artifact that is missing, stale, wrong-sized, or
  bound to another frame is a failure.
- Four-panel scenarios retain all requested panel artifacts with target,
  frame, extent, and command/sample stage.
- The old implicit-3D helper is deleted or renamed to an explicitly 3D-only
  helper; it cannot remain the default for a four-panel claim.
- Capture adds no second renderer or presentation path.

### Independent scale oracle

Use two complementary checks:

1. **Deterministic multiscale fixture.** Construct base data with enough
   spatial variation that independently generated catalog levels produce
   distinguishable rendered images. Generate expected panel pixels through
   the bounded CPU reference/oracle path, not from renderer diagnostics.
2. **Representative product observation.** For fixed, non-private geometry,
   compare bounded panel probes or image regions with samples decoded from the
   independently identified source scale. Do not publish source paths,
   scientific identities, or private geometry.

For every candidate catalog level, the oracle may render or sample the
expected result and identify which candidate matches the captured output
within the mode's declared numerical tolerance. The application-reported scale
must not choose the oracle candidate.

Do not use identical-neighbor run length as the acceptance oracle; real
microscopy values can legitimately repeat. Pixel-run measurements remain a
useful diagnostic for the reproduced eightfold discrepancy.

### Failing incremental regression

Before the production fix:

- open the ordinary four-panel product path;
- establish a complete coarse linked body;
- issue many small linked zoom samples that each remain within the resident
  envelope but cumulatively cross at least one projected LOD boundary;
- use the same mailbox sample and finish path as ordinary UI wheel input;
- prove the final durable view and panel extents;
- assert that the fixture's selected target is feasible;
- finish once and wait under finite deadlines;
- capture XY, XZ, and YZ;
- independently identify their represented scales; and
- fail because the current product remains at the old coarse level.

The direct durable setter is retained only as a control showing that the final
view itself can render the expected finer level.

### Focused state checks

After the fix, focused tests must prove:

- resident samples do not mutate the installed exact-demand identity;
- many contained samples can cross S3→S2→S1→S0 and submit exactly one settled
  linked replacement transaction;
- a large direct jump and many small steps converge to the same selected body
  and pixels;
- a provisional coarse body never satisfies exact currentness or settlement;
- zooming back out selects and truthfully presents the appropriate coarser
  target without restart;
- non-adjacent transitions work;
- XY, XZ, and YZ commit one linked generation while 3D remains independent;
- latest input cancels or supersedes stale exact planning without exposing a
  mixed linked generation;
- a capacity-constrained finer ideal settles only at the explicitly selected
  feasible coarser level;
- a feasible finer target is not mistaken for a capacity-constrained target;
- multiple visible layers and nonuniform scale catalogs retain independent
  selected maps; and
- resize, layout, slice, pan, rotation, time, and visibility changes reopen
  exact selection when required.

Avoid a large cross-product matrix. Use a small number of purpose-built cases
whose expected scale maps and source pixels are independently known.

### Product scenario

Extend the existing finite real-window continuity tool instead of creating a
new qualification framework.

Add one explicitly named linked-2D zoom workflow that:

- launches the normal release application on a real mapped display and GPU;
- selects four-panel layout through the actual UI;
- targets a 2D panel and delivers ordinary zoom input through the real window
  boundary at an independent clock;
- begins at the default complete coarse view;
- crosses feasible linked LOD boundaries through repeated small inputs;
- holds the final close view long enough to observe exact refinement;
- externally samples visibly changed XY, XZ, and YZ pixels;
- records raw input receipt and main-loop progress for continuity diagnosis;
- captures final linked panels for the independent scale oracle;
- confirms the 3D panel retained its independent camera and LOD; and
- terminates automatically under fixed startup, interaction, settlement, and
  shutdown deadlines.

Direct reducer commands, direct final-view assignment, internal publication
counts, and metadata-only scale checks cannot substitute for this workflow.

### Report semantics

For each linked panel and visible layer, a relevant report must distinguish:

- ideal scale;
- selected feasible scale;
- installed exact scale;
- displayed complete scale;
- independently observed pixel/source scale;
- provisional/refining/current state; and
- the frame and requirement-body identity to which those facts belong.

A report is invalid if a required artifact or independent observation is
absent. Missing evidence cannot be represented as an empty successful artifact
list.

The report may use a compact human-readable JSON extension already owned by
product automation. It must not introduce signed receipts, result hashes,
replay, roles, populations, or checked-in run artifacts.

### Benchmark validity

Correctness is a prerequisite for performance measurement.

A linked-2D benchmark may emit a performance result only after:

- the intended real interaction path was exercised;
- input changed the canonical linked view over a declared meaningful range;
- required panel captures exist;
- the independently observed final scale matches the selected target; and
- no panel is blank, stale, incomplete-as-complete, or falsely settled.

Measure separately:

1. resident interaction input-to-visible latency;
2. release-to-selected-target refinement latency; and
3. selected-target steady-state frame time.

Fast reprojection of stale S3 pixels is not S0 performance. A correctness
failure invalidates the benchmark rather than becoming a favorable timing.

Keep one short repeatable linked-zoom scenario and one final bounded product
exercise. Do not recreate the retired EP qualification program or run the
broad repository suite inside the edit loop.

## Hard Cut

The accepted implementation deletes or replaces in the same cut:

- the geometry-only cross-section exact-currentness decision;
- resident reuse mutation of the installed exact-demand signature;
- any no-work path that ignores selected-versus-installed scale mismatch;
- any ability for provisional coarse linked pixels to report exact selected
  settlement;
- the implicit 3D target in generic four-panel capture helpers;
- tests or reports that call direct final-view assignment interaction evidence;
- performance passes whose correctness prerequisite is metadata-only; and
- redundant implementation-counter tests superseded by the independent
  interaction and pixel checks.

There is no feature flag, old/new currentness path, scale-specific patch,
alternate renderer, CPU fallback, compatibility adapter, or benchmark-only
fix.

## Work Plan

### L0 — Claim Reset And Audit

- Record the linked-2D defect in Current State, Current Work, Architecture, and
  Testing.
- Amend the completed rendering and adaptive-LOD handoffs so their S3 oblique
  and 3D adaptive results are not misread as linked-2D LOD acceptance.
- Withdraw any linked-2D fidelity or performance claim based on direct view
  assignment, metadata-only currentness, or 3D-only captures.
- Inventory and classify the affected tests, scenarios, reports, and
  benchmarks under the audit scope above.

Exit: no active documentation or test name claims that ordinary linked zoom is
S0-validated or performance-accepted.

### L1 — Capture And Oracle Hard Cut

- Make product capture target-explicit.
- Retain actual XY, XZ, and YZ GPU artifacts.
- Add the deterministic multiscale fixture and independent CPU/source oracle.
- Make missing, stale, or wrong-target artifacts fail closed.
- Update report fields so metadata and independently observed scale cannot be
  conflated.
- Delete or narrow the misleading capture and image-stat helpers.

Exit: one direct-view control can prove actual linked-panel S0 pixels without
using reported scale as its oracle.

### L2 — Failing Incremental-Zoom Regression

- Implement the contained many-step zoom scenario.
- Prove it uses the mailbox sample and finish path.
- Prove the final view is identical to the direct control.
- Capture and independently classify all linked panels.
- Preserve the current failure as a deterministic red regression before
  production repair.

Exit: the test fails specifically because incremental settlement displays the
old coarse scale while the direct control displays the feasible finer scale.

### L3 — Exact-Demand Currentness Cutover

- Separate resident coverage from installed exact-demand identity.
- Stop resident guard reuse from overwriting exact-plan currentness.
- Include selected per-panel/per-layer scale maps and exact body identity in
  cross-section replan decisions.
- Make settled mismatch produce one latest-only linked exact plan.
- Keep the prior complete body visible and truthfully provisional.
- Delete the geometry-only predecessor decision in the same change.

Exit: the L2 regression reaches the same selected body and independent pixels
as the direct control, with one durable commit and no per-sample exact planner
churn.

### L4 — Presentation And Settlement Truth

- Bind linked frame adoption to the exact selected requirement body.
- Keep ideal, selected, installed, displayed, and observed scale facts
  distinct.
- Prevent provisional resident frames from satisfying selected-target
  settlement.
- Correct `coordinated_presentation_settled`, runtime-idle composition, and UI
  status.
- Preserve adaptive coarser settlement when global feasibility genuinely
  selects it.

Exit: reports and GUI cannot call an S3 navigation front complete S0, and a
feasible pending S0 replacement cannot be hidden behind `Current`/`Exact`.

### L5 — Focused Coverage And Test Deletion

- Add only the focused transition, stale-result, capacity, multilayer, and
  synchronization cases named above.
- Run the affected app, application, dataset-demand, UI, and renderer checks.
- Run the trusted Vulkan lane because per-target capture/currentness crosses
  the renderer boundary.
- Delete redundant metadata-only and implementation-counter cases identified
  by L0.
- Run the broad PR gate once after focused integration succeeds.

Exit: every retained affected test has a narrow honest name and an independent
fact or concrete state invariant; test volume has not grown through duplicate
matrices.

### L6 — Normal-Product Acceptance And Handoff

- Run the linked-2D real-window workflow repeatedly on the representative
  workload and designated Vulkan workstation.
- Inspect final XY, XZ, and YZ artifacts through the independent oracle.
- Exercise feasible finer selection, adaptive coarser selection, zoom-in,
  zoom-out, resize, oblique movement, and return to the default view.
- Perform one final bounded combined product exercise.
- Record workload, hardware, metrics, sampling method, thresholds, skips, and
  remaining risks.
- Update Current State, Current Work, Architecture, Testing, and the two
  predecessor handoffs with the accepted result.

Exit: the owner-visible linked panels reach the selected feasible scale,
remain interactive, report it truthfully, and terminate normally. Only then
may the linked-2D fidelity and performance claims be restored.

## Acceptance Matrix

| Boundary | Required independent fact |
| --- | --- |
| Direct final view | Captured linked pixels match the independently generated selected-scale result |
| Many-step zoom in | Same final selected body and pixels as the direct control |
| Zoom out | Captured pixels and reported displayed scale both move to the selected coarser target |
| During resident input | Old complete pixels may remain visible but are labelled at their actual scale and provisional state |
| Feasible refinement | Selected finer body is requested once, completes, and publishes atomically |
| Capacity-constrained refinement | Coarser selected body remains interactive and is labelled adaptive, with finer ideal separate |
| Linked synchronization | XY/XZ/YZ share one durable generation without a mixed-scale publication |
| 3D independence | Linked zoom does not change the 3D camera, target, or LOD authority |
| Latest-only replacement | Superseded fine work cannot publish after later input |
| Report validity | Metadata, artifacts, and independent observed scale agree; absence is failure |

## Product Acceptance

On the representative normal application, mapped real display, and designated
Vulkan adapter:

- active linked zoom has no input-receipt, main-loop, or externally visible
  panel gap above 100 ms while a complete resident navigation body exists;
- p99 input-to-next-visible-change is at most 50 ms for resident interaction;
- after release, an already-resident selected target settles within 250 ms;
- a cold selected target remains explicitly refining and reaches complete
  publication within the finite scenario deadline;
- the reproduced close view selects feasible S0 and independent XY/XZ/YZ
  pixels match S0;
- zooming back out visibly and truthfully selects the appropriate coarser
  target without restart;
- no required artifact is missing and no static or repeated image can create a
  false pass;
- no panel is blank, mixed-generation, incomplete-as-complete, stale under a
  new identity, or labelled with the wrong LOD;
- 3D remains independently presentable throughout; and
- every run closes automatically without repeating application or WGPU errors.

Cold source/decode/refinement duration is reported separately from resident
interaction continuity. It may exceed the warm settlement threshold, but it
may not be hidden by a false settled state.

## Retained Invariants

- Never mutate source microscopy data.
- Preserve calibrated transforms, axis meaning, validity, sampling, transfer,
  DVR, ISO, pick, and complete-frame semantics.
- Keep ideal, selected, installed, displayed, and observed scale distinct.
- Never label a coarse or incomplete frame as a finer complete frame.
- Preserve one mailbox, one CPU data plane, one renderer residency owner, one
  frame coordinator, and one presentation path.
- Keep planning, decoding, upload, transition overlap, queues, memory, and VRAM
  bounded, cancellable, latest-only, and stale-suppressing.
- Keep 3D and linked-2D interaction/LOD authorities independent.
- Preserve neutral adaptive quality and reserve hard capacity failure for
  failure of every valid minimum navigation plan.
- Keep verification proportionate to a one-person academic project.

## Scope Boundary

Included:

- audit and hard cut of misleading linked-2D tests, captures, reports, and
  benchmarks;
- explicit per-panel GPU capture;
- independent linked-panel scale/pixel oracles;
- incremental linked-2D zoom regression coverage;
- resident-versus-exact demand identity;
- selected/installed/displayed currentness and settlement;
- linked atomic refinement and truthful UI/reporting; and
- finite real-product validation.

Excluded:

- storage format, shard layout, logical brick edge, import pipeline, or source
  identity changes;
- shader performance optimization unrelated to captured-scale correctness;
- changing configured GPU capacity as the solution;
- a CPU renderer, fallback renderer, second residency map, or second frame
  coordinator;
- analysis/export resolution policy;
- public benchmark, comparison-viewer, release, Windows, macOS, or 4K claims;
- broad unrelated test-suite cleanup; and
- receipts, replay, result hashing, provenance graphs, evidence populations,
  roles, or a new qualification framework.

Any excluded scope requires separate owner approval.

## Risks And Responses

| Risk | Response |
| --- | --- |
| Fixing currentness causes exact planning on every wheel sample | Keep resident coverage transient and plan the selected exact target only at cold-envelope exit or the one settle boundary |
| A new scale field becomes another mutable authority | Bind selected scales to the immutable exact requirement body and derive desired targets from the latest view/capacity facts |
| Pixel oracle repeats the implementation | Generate expected fixture levels and panel pixels through the independent CPU/source path and never select the candidate from app metadata |
| Real data has repeated neighboring values | Compare bounded expected pixels or candidate images; retain run-length analysis only as diagnosis |
| Finer target is genuinely unaffordable | Preserve adaptive selection and require explicit selected/displayed coarser identity instead of forcing ideal |
| Linked atomicity delays active interaction | Keep the complete resident body visible during input and make active panel priority distinct from final linked publication |
| Test repair expands into another process framework | Reuse product automation, renderer capture, CPU oracle, and real-window tooling; delete weaker checks and forbid new schemas/receipts/provenance |
| Fix regresses 3D behavior | Assert 3D camera, LOD, and presentation identity remain unchanged throughout linked scenarios |
| Direct setters again substitute for interaction | Keep direct assignment only as a named control and require incremental plus real-window paths for interaction claims |

## Hard Stop Conditions

Stop and redesign the current milestone if:

- a provisional resident signature can still overwrite installed exact-demand
  identity;
- exact currentness can be satisfied without comparing selected and installed
  scale/body identity;
- implementation requires a second demand, residency, rendering, or
  presentation authority;
- a test chooses expected scale from the metadata it is validating;
- missing linked-panel artifacts can still produce a pass;
- a benchmark can publish timing after its pixel/scale correctness gate fails;
- the fix hardcodes one scale ordinal, dataset, panel, or transition;
- broad test/provenance machinery begins growing faster than the production
  correction;
- a storage, format, GPU-budget, or shader rewrite is proposed without evidence
  that this identified currentness defect has been removed; or
- the normal mapped application cannot be exercised at milestone exit.

## Authorization Boundary

The owner requested this written plan on 2026-07-29 and explicitly authorized
its full implementation on the same date. Implementation authorization covers
L0 through L6 and the named hard cuts within this evidence class. It does not
authorize any excluded scope.

The product correction is closed as described above. The unexecuted
host-stress measurement branch is withdrawn from acceptance rather than left
as an active implementation milestone.
