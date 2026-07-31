# Viewer Rendering Pipeline Overhaul Handoff

- Status: COMPLETE — S3 OBLIQUE CONTINUITY PRODUCT-VALIDATED
- Planning authorization: OWNER REQUESTED 2026-07-28
- Implementation authorization: OWNER GRANTED 2026-07-28
- Measurement-boundary amendment: OWNER GRANTED 2026-07-29
- Real-interaction corrective amendment: OWNER APPROVED 2026-07-29
- Last reviewed: 2026-07-29

This is a target plan, not an implemented-state record. The current product
remains the architecture described by
[Current State](../../CURRENT_STATE.md) until an accepted milestone changes
it.

## Active Follow-On Correction

This handoff's accepted linked-panel product claim is continuous S3 oblique
interaction through the normal mapped application. It did not validate
incremental linked-2D LOD transitions or prove the scale represented by
close-view XY, XZ, and YZ pixels. The latter inference is withdrawn.

The
[linked-2D LOD truth and settlement plan](VIEWER_LINKED_2D_LOD_TRUTH_AND_SETTLEMENT.md)
owns the active test/report repair and resident-versus-exact settlement
correction. The rendering authority cut and accepted S3 continuity result
remain in force within their actual scope.

## Decision

Rebuild the viewer's rendering and data plane as one product-integrated
pipeline. Use the recovered work as a source of reviewed algorithms and GPU
mechanisms, not as a branch to merge and not as a second renderer.

This is a full pipeline overhaul. It is not a shader-only optimization, a
cross-section hotfix, or a revival of the former EP qualification program.
The work cuts over one authority layer at a time, runs the normal application
after every layer, and deletes the replaced owner in the same milestone.

The implementation anchors are:

- current `main` at planning revision `65250eb`;
- the reconstructed recovery tip `3dfe9de`; and
- historical diagnostic revision `ac38fce`.

Later commits may move code, so handoff work should find named types and
responsibilities rather than depend on recorded line numbers.

## Corrective Amendment — Real-Interaction Continuity

Status: COMPLETE

### Blocking Product Observation

On 2026-07-29 the owner exercised the normal application with the
representative dataset in four-panel mode at S3 and continuously dragged the
oblique-angle control. The viewer froze every one or two seconds and sometimes
took several seconds to resume. The owner reports that this is substantially
worse than the already-unsatisfactory predecessor behavior.

This observation fails the plan's primary product outcome. It overrides the
scripted command's internal currentness, submission, and structural-zero
counters. P0 through P5 remain implemented architecture and focused
correctness facts, but the rendering overhaul is not performance-accepted.

The former `representative_four_panel_three_sessions` result is withdrawn as
performance evidence because it:

- injected application commands rather than exercising the real oblique UI
  gesture through the window/input path;
- used limited, unrepresentative geometry and did not sweep the discontinuity
  the owner encountered;
- allowed application progress to pace command delivery instead of maintaining
  an independent input clock while the UI was blocked;
- measured application admission to internal currentness rather than physical
  input to visibly changed presented pixels; and
- used static pixel inspection, which can prove one frame is nonblank but
  cannot prove motion continuity.

Its retained reports are narrow diagnostics only. They cannot support a
performance, usability, storage-boundary, or completion claim.

### Correct Work Order

The corrective work is test-first, but it must not become another
qualification program:

1. replace the invalid performance boundary with one faithful finite freeze
   reproducer;
2. hard-cut misleading rendering-performance tests and proxies while
   preserving independent scientific and safety checks;
3. reproduce and profile the actual discontinuity;
4. fix the first blocking production authority rather than guessing at GPU,
   CPU, storage, or shader work; and
5. close with the same real gesture plus owner-observed use.

No further rendering-performance implementation begins until the new
reproducer fails reliably on the current product. Passing unit tests or
internal counters cannot waive that prerequisite.

### C0 — Evidence Reset

Status: COMPLETE

- Revoke the automated P6 performance and storage conclusions.
- Record the owner-observed regression in Current State and Current Work.
- Mark the old representative command as non-authoritative until its source is
  deleted or reduced to honestly named static correctness coverage.
- Do not delete independent numerical, scientific-validity, source-safety,
  bounded-resource, or persistence checks merely because the performance
  harness failed.

### C1 — Faithful Freeze Reproducer

Status: COMPLETE

Build one bounded, auto-closing normal-application scenario with these fixed
facts:

- the owner-provided representative dataset, four-panel layout, S3 LOD, the
  ordinary full-size viewport, and a real mapped display/GPU;
- sustained wide oblique-angle motion in both directions through the same UI
  control and geometry range used by the owner;
- input generated independently of the application's update loop at at least
  60 samples per second for 30 seconds;
- delivery through the real window/UI input boundary, never direct reducer or
  product-automation commands;
- no wait-for-current, request/response handshake, or per-sample settlement;
- finite startup and run deadlines, visible progress, automatic close, and a
  hard failure instead of an idle open viewer; and
- ignored local CSV/timeline and optional short video evidence, with no
  checked-in reports, schemas, receipts, provenance graph, or result hashing.

Observe four monotonic boundaries: independently generated input, window input
receipt, application/main-loop progress, and visibly changed presented
output. Internal renderer currentness and queue counters may be attached for
diagnosis but are not the visible-performance oracle.

The current implementation must reproduce at least one visible or main-loop
gap above 100 ms before C1 is accepted. If automation cannot reproduce the
owner's gesture faithfully, a human-driven recorded run remains the
development reproducer; direct command injection is not an acceptable
substitute.

Implemented as `cargo xtask viewer-oblique-continuity`. The command launches
the normal release viewer, clicks the actual `4 Panel` control, waits for
complete/current S3, holds Shift plus the primary pointer button, and drives a
wide bidirectional drag through X11 at an independent 60 Hz. A separate ffmpeg
process samples cursor-free linked-panel pixels at 60 Hz. Startup, capture,
input, and shutdown all have hard deadlines; evidence is plain ignored local
CSV/text.

The first 30-second run on the owner-provided dataset and RTX 3070 Ti/Vulkan
host failed faithfully:

- 1,800 generated input samples; producer maximum gap 39.506 ms;
- window-input receipt maximum gap 2,185.497 ms;
- application main-loop maximum gap 68.583 ms;
- 186 visibly changed frames from 1,801 captures;
- visibly changed-frame maximum gap 3,001.126 ms; and
- p99 generated-input-to-next-visible-change latency 2,891.171 ms.

The final frame remained complete/current S3. This proves a discontinuity in
the real input-to-visible-output path rather than startup failure, a static
blank frame, or an input-producer stall.

### C2 — Rendering-Test Hard Cut

Status: COMPLETE

- Delete the command-driven representative performance scenario and its
  publication-count-as-smoothness validators.
- Preserve a static renderer-mode or pixel check only if it has independent
  expected facts, and rename it so it cannot be mistaken for performance
  evidence.
- Audit application/renderer performance-related tests. Retain only tests that
  falsify an independent scientific result, a safety/resource bound, stale
  publication, or a concrete regression. Delete duplicate matrices,
  implementation-counter snapshots, and tests whose only assertion is that
  the current machinery called itself.
- Keep the edit loop focused: one named regression or affected-package check.
  Run the repository-wide PR gate only at integration, not after each
  performance edit.
- Keep exactly one persistent 30-second real-interaction performance scenario
  and one final five-minute product exercise. Do not grow another role,
  population, replay, receipt, or provenance framework around them.

The hard cut is incomplete while the obsolete scenario can still emit a
performance pass or the new scenario can bypass the actual UI input path.

The obsolete `representative_four_panel_three_sessions` scenario, aggregate
report/evidence validators, release-artifact binding, and representative-only
normal-app startup mode were deleted. The separate command-driven
`target_fixture_resident_navigation_no_readback` cadence proxy and its
publication-count/timing validator matrix were also deleted. It duplicated
focused resident-reuse checks and treated 301 app-paced internal publications
as continuity.

Retained checks cover independent render-mode pixels, numerical/scientific
facts, stale suppression, source nonmutation, bounded resources, resident
plane reuse/zero read-decode behavior, persistence, and real GPU mechanisms.
Focused compilation and the affected xtask and app automation test groups
passed after the cut.

### C3 — Discontinuity Localization

Status: COMPLETE

Run the failing reproducer with sampling profiles and a minimal timeline.
Attribute the first interval above 100 ms to the earliest blocking production
boundary, explicitly checking:

- resident plane-guard reuse versus exact plane replanning;
- latest-only worker cancellation, completion, and main-thread result install;
- CPU lease/request admission and foreground preemption;
- renderer residency offer, directory, allocation, upload, and eviction work;
- frame-coordinator recording, queue submission, device polling, and fence
  completion;
- egui texture update, redraw scheduling, surface presentation, and compositor
  visibility; and
- unexpected pipeline creation, readback, buffer mapping, lock contention, or
  blocking waits.

The output is a short human-readable timeline and profile identifying the
first blocking boundary. A list of plausible causes or cumulative overlapping
times does not complete C3.

The first blocking boundary was the application's linked-cross-section
authority, before storage, decode, upload, shader execution, or GPU completion.
The mailbox carried one geometry shared by XY, XZ, and YZ, but the runtime
applied transient demand and rendering only to the gesture's active panel and
treated foreground priority as effective exclusivity. The two passive linked
panels retained durable geometry. Guard and direction transitions therefore
entered exact planning and linked-currentness churn instead of presenting one
coherent resident cross-section transaction.

The failing real-window timeline ruled out the initially suspected GPU/CPU
handoff. Resident samples required no source read or decode, recorded GPU
plane work was already sub-frame, and diagnostic final cutoffs reached GPU
completion roughly 1–2 ms after button release. The production discontinuity
began earlier, where common linked geometry was split into one transient panel
and two durable panels.

Localization also found one measurement defect separate from the product
freeze. The initial harness captured through an undeclared teardown tail and
mistook a late raw-pixel change for multi-second settlement. Capture now begins
only after the real gesture is armed, has a fixed 500 ms lead-in, and evaluates
settlement only inside the declared one-second post-release observation. A
five-minute run later exposed the app trace's former 32,768-event limit; the
trace now records only the used boundaries, samples main-loop liveness at
10 ms, remains bounded at 131,072 events, and makes any dropped event an
explicit invalid run.

### C4 — Performance Correction

Status: COMPLETE

- Change the owner responsible for the measured first blocking boundary.
- Preserve one product path; do not add a CPU renderer, legacy fallback,
  alternate residency map, second coordinator, or benchmark-only fast path.
- Delete the replaced slow authority in the same cut.
- Rerun the 30-second reproducer after each meaningful change, using focused
  scientific/safety checks only where that change can affect them.
- Continue until the discontinuity is removed across guard, residency, and
  direction-reversal boundaries; do not optimize a steady-state region while
  a transition still freezes.

Storage or logical-brick changes remain unauthorized unless C3 proves that
required unavailable-brick physical I/O/decode is the first blocking boundary
for the failed visible interval.

The correction keeps one product path and changes the responsible authority:

- every active cross-section sample now derives effective XY, XZ, and YZ
  demand from the same mailbox geometry;
- all three resident plane guards are preflighted before they are atomically
  promoted, so a resident gesture performs no per-sample planner, dataset
  request, read, decode, upload, eviction, or residency rebuild;
- the gesture's active panel is scheduling priority, not an exclusion rule,
  and all three linked targets enter the coordinated color submission;
- a cold latest-only linked plan invalidates the old linked pixels at its
  atomic plan cutover rather than leaving semantically stale panels marked
  current;
- release atomically adopts all three already-rendered exact transient frames
  into durable state, preserving their frame, texture, and binding identities
  without replacement planning or GPU color work;
- a terminal empty cross-section clears all superseded linked presentations;
  and
- the speculative settlement prefetch and its duplicate work were deleted.

Foreground visible work uses a bounded 16 ms repaint cadence. The mapped
surface remains `AutoNoVsync` with one desired in-flight frame; a measured
FIFO experiment produced 145–185 ms visible gaps on this workstation and was
deleted rather than retained as speculative policy.

### C5 — Acceptance

Status: COMPLETE

Three fresh normal-application runs must each satisfy:

- 30 seconds of independently clocked continuous oblique motion at S3 in both
  directions;
- no window-input, main-loop, or visibly changed-frame gap above 100 ms while
  the input producer remains active;
- p99 latest-input-to-next-visibly-changed-frame latency at or below 50 ms;
- release-to-final-visible-settlement at or below 250 ms;
- no blank, stale, partial, snapped-back, or silently coarser settled panel;
- no apparent pass produced by static/repeated pixels while geometry changes;
  and
- finite automatic termination with no repeating WGPU or application error.

Then the owner or an explicitly delegated observer performs the same
five-minute normal-app workflow and accepts its visible continuity. Only after
that product result may focused correctness checks and one final
`cargo xtask verify-pr` close the implementation. The full suite cannot
outvote a visible freeze.

Three fresh 30-second normal-application runs passed on the owner-provided
dataset and RTX 3070 Ti/Vulkan mapped display. Each emitted 1,800 independently
clocked real Shift-drag inputs:

| Run | receipt max | main-loop max | worst linked visible gap | worst linked p99 input-to-visible | settlement |
| --- | ---: | ---: | ---: | ---: | ---: |
| 1 | 33.781 ms | 30.624 ms | 26.735 ms | 24.288 ms | 23.145 ms |
| 2 | 32.912 ms | 29.899 ms | 42.237 ms | 24.346 ms | 9.092 ms |
| 3 | 33.516 ms | 30.247 ms | 33.623 ms | 24.845 ms | 23.708 ms |

The delegated five-minute normal-app exercise then passed with 18,000 inputs,
a 24.033 ms receipt maximum, 24.106 ms main-loop maximum, 50.208 ms worst
linked visible gap, 27.271 ms worst linked p99 input-to-visible latency, and
8.171 ms final settlement. All three externally sampled panels remained
nonblank and continuously changing; the final app state was complete/current
at S3, the mapped window and presentation rectangles remained stable, and
neither the app nor WGPU emitted an error. Plain ignored evidence is under
`target/mirante4d/viewer-oblique-continuity/`; no report, receipt, hash, or
provenance artifact is checked in.

Focused app and real-Vulkan regression checks passed. The final
`cargo xtask verify-pr` then passed every policy phase, exact ignored-test lane
assignment, Clippy with zero warnings, and all 1,233 PR-lane unit, contract,
and UI cases.

### Corrective Authorization Boundary

Approval of this amendment authorizes C1 through C5 and deletion of the
obsolete rendering-performance harness. It does not authorize a storage
format, logical-brick edge, public benchmark claim, new product capability,
fallback path, general test framework, paid CI, or unrelated test-suite
rewrite. Any of those requires a separate concrete amendment.

## Outcome

The normal native application has one understandable path from view intent to
presented pixels:

```text
durable project state + bounded transient interaction
    -> one latest intent
    -> one demand and CPU-brick authority
    -> one renderer-owned GPU residency authority
    -> one frame coordinator
    -> dedicated mode kernels
    -> current presentation
```

The overhaul is complete only when:

- the representative large-data four-panel workflow meets the acceptance
  contract below and has no visible wheel freeze, panel starvation, or coarse
  settled result;
- cross-section work is proportional to the intersected plane rather than its
  enclosing three-dimensional brick volume;
- resident camera and plane interaction performs no dataset read, decode,
  upload, eviction, or residency rebuild;
- every mutable fact has one production owner and every render mode has one
  production kernel;
- scientific values, validity, calibration, LOD, picking, and complete-frame
  behavior remain correct;
- memory, VRAM, queues, descriptors, I/O, and temporary work remain bounded,
  cancellable, and stale-suppressing;
- the predecessor owners, temporary adapters, feature alternatives, and
  recovery-oriented names are deleted; and
- the result is exercised in the normal application on the representative
  workload and accepted by the owner.

Passing internal tests or improving one surrogate counter cannot complete the
overhaul without the visible product result.

## Why A Pipeline Overhaul Is Necessary

Historical normal-application evidence on a representative large dataset and
an RTX 3070 Ti Vulkan workstation showed that plane fragment work was already
small while orchestration was not:

- resident plane GPU timings were roughly `0.03–0.27 ms`;
- the old app-level pending-generation gap reached roughly `42–72 ms` at p95
  and `130–212 ms` at the maximum;
- one 240-sample gesture admitted 240 generations, superseded all of them,
  and published none as current during the gesture;
- individual exercises produced roughly `900–1,400` render passes,
  `1,800–2,900` queue submissions, hundreds of static rebuilds, and thousands
  of short-lived GPU-control objects; and
- individual plane passes in four-panel exercises were around
  `0.02–0.06 ms`.

Those measurements are engineering diagnostics, not a current performance
claim. Their old gap counter sampled a growing pending interval on main-loop
heartbeats, and their per-panel publication intervals were not operating-
system compositor presents. They must not be reused as cutover statistics.
The current tiny-fixture reports are also not comparable to the large
workload.

The recorded mismatch strongly implicates repeated planning, currentness
churn, residency/control rebuilds, and submission fan-out rather than plane
fragment cost. P0 must verify that this remains the blocking path on current
`main`; optimizing only the fragment shader cannot repair those ownership
failures.

The recovered successor contains mechanisms capable of removing those costs,
but it never became the normal product:

- `successor-product` was default-off;
- `MiranteWorkbenchApp` still constructed the predecessor runtime;
- the successor native target constructor had no product caller;
- no surviving normal-app four-panel result selects or qualifies it; and
- the recovered tip has a later unrelated compile defect even though focused
  renderer, directory, storage, and kernel tests passed.

The recovery branch therefore supports salvage, not a claim that its entire
implementation was faster or ready.

## Confidence And Open Decisions

The plan deliberately separates conclusions supported by evidence from
choices that still require measurement.

| Confidence | Decision |
| --- | --- |
| High | The historical failure and current leading hypothesis concern pipeline ownership and orchestration; the overhaul should begin with transient interaction, projected plane demand, shared residency, and coordinated frame ownership. P0 may falsify or reorder that diagnosis. |
| High | A whole-branch merge, permanent feature flag, dual cache, dual residency map, or old/new renderer fallback would reproduce the failed architecture. |
| High | The plane-demand geometry and several shader/residency algorithms are useful donors; the recovered app/session transaction graph is not. |
| Medium | One normal renderer queue submission containing color work will be the best frame policy. A measured active/passive two-stage policy may be better when heavy 3D work would delay the active plane. |
| Medium | The orthogonal 2D targets should normally share a currentness group while 3D remains independently presentable. Product measurement must confirm this. |
| Open | Logical `32³` versus `64³` renderer/cache bricks and any corresponding physical package profile, final directory sizing, buffer versus atlas representation, and full-screen versus tiled plane work. These are not architecture facts until measured. |

Choosing an evidence-gated decision point for the open items is part of the
robust design. Pretending to know them now would be less rigorous, not more.

## Retained Invariants

- Never mutate source microscopy data.
- Keep valid zero, invalid/no-data, absent/loading, and complete data
  semantically distinct.
- Preserve calibrated transforms, axis meaning, per-view projected LOD, DVR
  physical-distance opacity, ISO gradient-halo completeness, and independent
  numerical expectations.
- Never present an incomplete brick mosaic as complete or attach new intent
  to stale dataset, layout, layer, time, or render state.
- Bound and account CPU memory, VRAM, queues, in-flight work, descriptors,
  staging, I/O, and physical objects.
- Make large work cancellable and suppress stale display results without
  discarding reusable dataset-generation residency progress.
- Keep capacity, capability, corruption, and terminal GPU failures typed and
  visible. Do not select CPU rendering, an alternate scientific
  representation, a predecessor, or another product fallback. One bounded
  canonical directory rebuild/compaction path is allowed inside the sole GPU
  representation.
- Keep dataset storage sharded with a bounded object count.
- Keep one product authority for each mutable fact and delete its predecessor
  in the accepted cutover.
- Validate rendering and interaction in the normal mapped native application
  on relevant data and hardware.

## Target Authority Model

Pure geometry and kernel functions may be shared, but mutable ownership is
limited to four top-level authorities. The renderer contains two deliberately
non-overlapping subowners:

| Authority | Owns | Must not own |
| --- | --- | --- |
| Durable application/project state | Committed layout, cameras, layers, time, render parameters, project identity, dataset reference, and the sole dataset-generation allocator | Raw gesture samples, GPU pages, frame pins, or decode tickets |
| Transient interaction/intent mailbox | Latest camera or plane sample, target, gesture identity, base durable revision, dirty bits, and the sole monotonic `RenderIntentRevision` allocator for every render-affecting durable or transient intent | History, serialization, dataset work, or an unbounded event log |
| CPU data-plane actor/cache | Canonical brick interests and priorities, I/O and decode tickets, decoded payloads, CPU eviction, cancellation, and the latest revision it has observed | GPU slots, target textures, presentation pins, revision allocation, or a second resident map |
| Renderer root | Device/queue access, the sole renderer-device-generation allocator, and the two subowners below | Durable view state, decode scheduling, or app-level residency transactions |
| ↳ Residency subowner | Payload arena, directory, page records, physical GPU residency, residency pins, transfer work, transfer fences, and the sole bounded directory rebuild/compaction path | Target textures, color submission, durable view state, or decode scheduling |
| ↳ Frame coordinator | Dirty target set, synchronization groups, target priority, target texture allocation/revision, color-pass recording/submission, color completion fences, and the latest revision it has observed | A second cache/residency map, revision allocation, scientific state, or per-panel queue owners |

The demand planner is a pure, cancellable function over one immutable intent
snapshot. Dedicated mode kernels are renderer modules, not additional owners.
Downstream messages and tickets may copy the opaque `RenderIntentRevision`
value for comparison and stale rejection, but no downstream component may
allocate or reinterpret it.

The frame coordinator obtains one opaque resident-frame lease from the
residency subowner. It holds that lease through color completion and releases
it without mirroring the pin set or fence ledger. The renderer root mediates
queue access; residency transfer submission and coordinated color submission
remain distinct responsibilities. The application composition root connects
the top-level owners without duplicating their ledgers, pins, currentness
authority, or resident handles.

The production flow obeys these laws:

1. Raw 3D and cross-section input updates the constant-size transient mailbox
   in place. Only the settled latest value enters the durable reducer.
2. A gesture is bound to its target and starting durable revision. A source,
   layout, active-view, projection, or durable-camera change cancels or
   explicitly rebases it.
3. One canonical logical `BrickKey` identifies the same scientific brick at
   demand, CPU cache, scheduling, and GPU residency boundaries. Physical
   package chunks may retain their current 3D or true-2D geometry; their
   one-way mapping to logical bricks is stateless at the source boundary.
4. The CPU actor requests idempotent `ensure_resident` work and receives
   simple completion/failure events. It never owns renderer slots or a
   two-phase residency transaction.
5. Camera staleness suppresses stale drawing. It does not automatically roll
   back a completed upload that remains useful for the same dataset
   generation and brick key.
6. Presentation passes borrow an encoder/pass context and cannot submit or
   poll the queue. The renderer-owned frame coordinator is the sole
   color/presentation-submission owner.
7. Stable logical target IDs identify 3D, XY, XZ, and YZ. Durable application
   state allocates opaque dataset generations, the renderer root allocates
   opaque renderer-device generations, and the frame coordinator allocates
   renderer-local texture revisions. Downstream owners only copy and compare
   them; no process-lifetime target identity allocator is needed.
8. The active target records first, related 2D targets next, and passive heavy
   3D work last. The normal path uses one renderer queue submission containing
   color work. The precisely bounded active/passive exception in P3 is allowed
   only if its deciding product measurement shows that one stage violates the
   responsiveness gate.
9. Complete coverage is enforced per declared synchronization group. A
   lagging 3D target must not freeze an otherwise complete current 2D group.
10. Each render mode maps to exactly one kernel. Adding a mode without a
    canonical kernel is a build-time omission, not a runtime fallback.

At every merged milestone there is exactly one production authority for each
box in the target flow. Approval of this plan specifically permits one named
P1-to-P2 stateless `BrickKey -> LegacyRenderRequirements` projection while the
existing renderer learns the new CPU-data-plane authority. It may not cache,
schedule, upload, evict, submit, retry, or convert in both directions, and P2
must delete it. No other compatibility view is authorized. If that seam cannot
remain stateless and one-way, stop and redesign it.

## Recovery Classification

Salvage means port the smallest coherent algorithm or representation into the
target authority model. It does not mean preserving recovered type graphs or
names.

### Preserve From Current Main

- projected per-view LOD and hysteresis from `1123c75`;
- the useful 3D transient-camera behavior from `5f6fb53`, generalized to all
  cross-section interactions;
- atomic complete refinement from `08ea5ce`;
- DVR physical distance and ISO halo correctness from `1ea236b`;
- the first-cause terminal GPU latch from `fad4fd6`;
- source/currentness checks, validity semantics, bounded staging and payload
  accounting, typed capacity failures, and independent scientific oracles.

### Port Algorithms And Mechanisms

- exact projected arbitrary-plane demand from `4dbc02d` and `cc7e360`;
- canonical brick identity and one global interest union;
- the persistent directory, page-record, and payload-arena design from
  `8e44442`, with latest-only concepts from `66df63e`;
- coordinated target recording from `fdb7ba6`;
- dedicated plane, MIP, DVR, ISO, and pick kernel mathematics, including
  page-resolution caching and segment reuse from `056e87d` and `8d36e9f`,
  monotone transfer work from `6ed5ac4`, MIP termination from `c9693cc`, and
  joint DVR ordering from `81b07f3`;
- coalesced timing polling from `76a52b0`; if automation wake behavior is
  needed, use the final `0a164f0` semantics rather than `24640fd` alone.

### Rewrite

- cross-section pan, zoom, slice, and oblique-rotation interaction;
- `SuccessorProductSession`, runtime coordination, and native composition;
- the successor residency bridge and prepared/accepted/admitted transaction
  chains;
- demand mailbox, worker-service, pin, and lifecycle glue;
- app-owned GPU residency projections and target identity allocation;
- target resize/replacement and frame synchronization policy;
- mixed-mode shader structure;
- any compile-time brick-edge propagation through the product graph.

Port algorithms and invariants from successor frame control, global demand,
and brick-state code only where they fit the target authority model. Do not port
their entire type graph.

### Discard

- the EP-00/EP-01 roles, populations, selectors, receipts, replay, admission,
  schemas, provenance ledgers, and repeated hash unions;
- a default-off successor product, runtime old/new toggle, or permanent
  feature selector;
- simultaneous `32³` and `64³` generic product candidates;
- duplicate app state, cameras, LOD, presentation, cache, residency, or
  submission ownership;
- the recovered package/provenance machinery as a prerequisite for rendering;
- an isolated harness-first program that postpones normal-app integration;
- a wholesale cherry-pick of the roughly 200-commit recovery branch.

The final product and source tree use ordinary names such as `renderer`,
`render_runtime`, and `frame_coordinator`. Recovery remains a Git reference,
not a runtime concept.

## Scope

Included:

- 3D and cross-section transient interaction;
- intent snapshotting, plane demand, priority, deduplication, cancellation,
  and CPU brick/cache ownership;
- GPU residency, directory, page, payload, target, and submission ownership;
- plane, MIP, DVR, ISO, mixed, and pick kernel integration;
- currentness, complete-frame presentation, timing, and focused diagnostics;
- removal of replaced renderer/data-plane code and tests that only pin it.

Excluded unless a later decision explicitly admits them:

- segmentation or new analysis capabilities;
- unrelated product features;
- a public comparison or release performance claim;
- a permanent compatibility reader or migration layer;
- general multi-platform or 4K qualification;
- a storage/import format experiment or cutover before the P6 boundary and a
  separately owner-approved amendment.

Feature work on the predecessor rendering path is frozen during the overhaul.
A critical correctness or safety fix may be mirrored only on the unmerged
working branch, preferably through one shared pure implementation. Its
predecessor copy must be deleted at the active authority cutover; the exception
cannot justify two merged production implementations.

## Superseded Measurement And Acceptance Contract

The original P0–P6 contract below is retained only to explain the implemented
architecture and earlier decisions. The real-interaction corrective amendment
at the top of this plan supersedes its workload, timing oracle, performance
gates, and P6 acceptance conclusion. Internal admission/currentness and exact
publication counts remain optional diagnostics; they cannot pass C1 or C5.

### Workloads

Use only the workloads needed to decide this overhaul:

1. **Representative resident four-panel interaction:** the normal release app
   on one owner-provided large dataset, the owner's RTX 3070 Ti workstation,
   a real 60 Hz display, a 1920×1080 four-panel window, and fixed view/render
   settings.
2. **Representative cold four-panel use:** the same product configuration with
   warm operating-system cache but fresh application CPU/GPU residency. This
   isolates the rendering/data plane; full process launch and genuinely cold
   storage remain separately reported diagnostics.
3. **Representative volume modes:** MIP, DVR, ISO, mixed behavior, and picking
   on the same data after the shared runtime is active.
4. **Small independent correctness fixture:** exact expected samples,
   transforms, validity, LOD, DVR, ISO, and pick facts. It is not a performance
   proxy.

Each session:

1. opens the representative package in four-panel view and waits for
   exact/current target fidelity;
2. performs five seconds of ordinary axis-aligned slice translation inside a
   proven resident envelope;
3. performs five seconds of compound-angle plane rotation inside that
   envelope;
4. moves once beyond the resident envelope and waits for exact replacement;
   and
5. leaves the settled view idle for one second.

Capture three fresh sessions on current `main`, then the same three-session
workload on the candidate, with no retries. Report every session plus its
median and worst observation rather than pooling correlated publications into
a larger-looking population. No confidence interval or
scenario-by-role-by-hardware matrix is required. Add one owner-observed manual
real-input pass; the numeric clock may begin at semantic input admission.

Private paths, labels, geometry, and scientific identities stay outside the
repository.

### Timing Semantics

The compact measurement hook uses two clocks:

- A **current synchronization-group publication** occurs when every target in
  the active group is semantically complete/current for one admitted cutoff.
  Dirty targets are newly rendered and unchanged-valid targets may be reused.
  A changed but lagging target is stale and cannot count as unchanged-current.
- **Full-layout settlement** occurs when every visible synchronization group,
  including passive 3D, is complete/current for the final admitted cutoff.

The primary resident latency is:

`app intent admission -> current active-group publication`

It is not called click-to-photon unless operating-system input and compositor
presentation are actually measured. Do not sample a growing pending gap on
each main-loop heartbeat, and do not count per-panel app publications as
independent display frames.

Also record:

- **active-group publication interval:** time between current active-group
  publications during continuous motion;
- **active publication gap:** the maximum interval from gesture start or the
  previous current-group publication to the next current-group publication or
  gesture end. It resets only on a current-group publication, never on new
  input;
- **final settlement:** final input admission to full-layout settlement with
  foreground work idle;
- **cold coarse milestone:** every visible panel complete/current at its
  declared coarse scale after source and four-panel intent admission;
- **cold target milestone:** every visible layer/panel complete/current at its
  independently selected target scale; and
- **nonresident replacement:** target-intent admission to exact/current
  replacement while the prior complete frame remains visible.

Record admission latency only for cutoffs that actually become current.
Count superseded cutoffs separately. The current eframe hook records
publication intervals and gaps inside `App::ui`, before surface acquisition
and present, so those fields are pre-surface diagnostics rather than
compositor-cadence measurements. If real presentation feedback becomes
available later, bind the unchanged 20/50 ms cadence thresholds to that
measurement. Until then, make no click-to-photon or numeric compositor-cadence
claim.

For each run, report these fields where the owning counter already exists. In
P0, record an unavailable deep diagnostic as unknown and add it only when its
authority is replaced:

- primary latency and active-group publication-interval median, p95, maximum,
  and active publication gap;
- current, coalesced, superseded-before-work, and stale-after-work
  generations;
- durable commits per gesture;
- plane candidates and planner CPU time;
- dataset requests, reads, decodes, useful/fetched/decoded bytes, and
  cancellation waste;
- uploads, evictions, directory mutations, and residency rebuilds;
- coordinated display encoders and submissions;
- GPU time per target/mode; and
- bounded CPU-memory and VRAM high-water marks.

Evaluate cumulative counters as deltas around each phase and normalize
amplification by coordinated group/layout publications, not panels or raw
input. Collect GPU timestamps in a separate diagnostic run when their polling
changes the submission count. Do not require asynchronous stage times to add
to wall time.

### End-To-End Gates (Superseded)

Before the first authority replacement, record the instrumented predecessor
revision, exact Phase 0 baselines, display refresh period, cache conditions,
panel extents, and fixed camera/transfer-function/sampling/LOD/target settings
for each volume-mode comparison.

The round thresholds below are proposed owner-selected usability requirements,
not performance already demonstrated by the repository. Approval of this plan
adopts them. P0 may not add, remove, tighten, or weaken a blocking threshold
without further owner approval; diagnostics cannot silently become gates.

The minimum blocking contract is:

- in every candidate resident-gesture session, p95
  app-admission-to-current-active-group-publication is at most 20 ms;
- no app-admission-to-current-active-group-publication or resident final
  settlement exceeds 50 ms;
- every fixed 300-sample gesture produces exactly 301 current active-group
  publications and admission samples across 300 distinct app updates, with
  one color submission per publication and no same-update catch-up;
- pre-surface callback/publication intervals and active gaps remain complete
  reported diagnostics but do not claim or gate compositor cadence; visible
  cadence remains part of the owner-observed real-display exercise;
- a raw gesture produces one settled durable commit, not one commit per input
  sample;
- resident interaction produces zero dataset reads, decodes, uploads,
  reuploads, evictions, arena allocations, static control/page-layout rebuilds,
  buffer allocations, bind-group creations, or pipeline creations;
- a pure resident coordinated cutoff uses one renderer queue submission
  containing color work, or the single P3-selected active/passive two-stage
  policy;
- at most two logical coordinated cutoffs containing color work are in flight,
  excluding residency-transfer and timing-only submissions, and settled idle
  performs zero renderer submissions;
- no stale frame replaces a newer current frame, and no target labeled current
  is blank, partial, stale, or silently coarser;
- all-panel complete coarse coverage arrives within 250 ms of admitted
  four-panel demand, all-panel selected-target settlement within two seconds,
  and nonresident exact replacement within one second;
- the prior complete frame remains visible during nonresident replacement and
  overlapping resources are not reread, redecoded, or reuploaded;
- settled MIP, DVR, ISO, mixed, and pick results pass their focused independent
  correctness checks and visible product inspection;
- existing resource ceilings are not raised without a measured need and owner
  approval; and
- a five-minute normal-app visual/interaction exercise of the formerly failing
  workflow is accepted by the owner or their explicitly delegated observer.

Planner runs/time, candidate visits, cancellation, storage amplification,
cache hits, upload timing, encoder CPU time, GPU per-pass time, and resource
high-water marks are diagnostic unless a milestone explicitly makes their
structural outcome a gate. They locate the next authority to fix and cannot
outvote the blocking contract. Baseline-versus-candidate cold improvement and
per-mode GPU time are also diagnostic unless P0 proposes a numeric regression
threshold and the owner explicitly approves it.

### Proportionate Evidence

- The plan document is the sole handoff/checklist. Do not create EP-style
  subordinate plans, evidence populations, roles, receipts, schemas,
  provenance ledgers, or cryptographic source/test-result hashes.
- Existing data-integrity checksums remain data safety. Do not repurpose them
  into development-evidence provenance.
- Performance output is ephemeral human-readable text or CSV. Do not build a
  checked-in raw-run or artifact-signing pipeline.
- Use the normal app and a small direct counter/timing hook. Do not create a
  new xtask framework for this overhaul.
- One representative real workflow plus one small independent correctness
  fixture is sufficient until profiling identifies a materially distinct
  workload.
- Three repetitions are enough for an engineering decision. A broader
  population belongs only to a separately requested public claim.
- A new required check must name the unique failure it detects and should
  replace a weaker check where possible. Check count is not a quality metric.
- A regression check must drive the production entry point that can exhibit
  the defect, fail under the named broken behavior, and assert an independent
  pixel, currentness, wakeup, ordering, or resource consequence. A command
  that matches zero tests, a check that proves only that code ran, or an
  assertion over a diagnostic that the exercised path itself increments is
  not evidence and is not counted.
- Focused checks belong in the edit loop. The existing broad suite runs at
  integration/cutover boundaries, not after every small edit.
- Permanent measurement or qualification code may not grow faster than the
  product change it protects without explicit owner approval.

## Work Sequence

Authority milestones P1 through P5 change one layer at a time, exercise the
normal app, and delete the replaced owner before acceptance. P0 measures and
P6 accepts or identifies the separately authorized storage boundary. Work may
be staged on a branch, but no accepted milestone leaves two production
authorities.

### P0 — Trustworthy Normal-App Baseline

Status: COMPLETE — FIRST BLOCKING AUTHORITY LOCALIZED

The compact coordinator aggregate is implemented. It distinguishes raw
main-loop callback spacing from current-group publication, retains a bounded
complete sample set, and measures the final post-input cutoff without letting
idle heartbeats fabricate a growing interaction gap.

The useful interaction phase of the predecessor observation decisively
localized the first failure: 300 slice samples and 300 rotation samples each
entered durable application state, while only one current-group publication
occurred during each gesture. The resident phase performed no dataset read,
decode, request, or upload, yet still rebuilt and submitted GPU control work
repeatedly. Cold observations remained small relative to the multi-second
active publication gaps. Raw-input-to-durable-replanning churn is therefore
the first authority to replace; storage and fragment-kernel work are not the
first intervention.

The initial launch protocol also ran source verification before the gesture
and left the mapped viewer apparently idle for minutes. The owner stopped that
run. That preamble is discarded and must not return as a prerequisite for
interactive observation. At the owner's direction, repeated predecessor
qualification does not delay the now-localized implementation; the
three-session end-to-end comparison remains a P6 acceptance activity.

- Add only the compact counters and timing semantics in the measurement
  contract.
- Limit P0 to one fixed-capacity coordinator-level aggregate hook. Do not add a
  per-event trace, actor, schema, replay path, or new orchestration layer.
  Reuse existing phase counters; defer a missing deep diagnostic until its
  owning authority is replaced rather than delaying P1.
- Size the fixed-capacity aggregate to retain the complete measured phase.
  Overflow invalidates the run; a retained suffix cannot supply its p95.
- Restore coalesced timing polling if timing-enabled observation would
  otherwise perturb the workload.
- Run the resident, cold, and representative volume workflows in the normal
  release app for three repetitions.
- Inspect the mapped application and record the visible failure.
- Record the instrumented predecessor revision, baseline, exact workload
  settings, and any unknown diagnostic in this plan.

Exit:

- the baseline uses the real normal app and representative workload;
- one coordinated-layout observation cannot be double-counted by panels or
  heartbeats;
- the blocking behavior is localized sufficiently to choose the next authority
  without pretending asynchronous stage times are additive; and
- no receipt, replay, population, schema, or hashing framework was added.

Do not optimize against the current tiny-fixture report or the old
heartbeat-sampled gap statistic. If P0 cannot fit this direct-hook scope, stop
for owner review rather than constructing a measurement subsystem.
If P0 falsifies the ownership/orchestration hypothesis or locates a different
first blocking authority, amend the sequence and obtain owner approval rather
than entering P1 by inertia.

### P1 — Intent, Brick Identity, And CPU Data Plane

Status: COMPLETE — NORMAL MAPPED FOUR-PANEL PASS

Progress:

- **P1-A transient intent and live latest-body cutover — implemented and
  focused-verified.** One constant-size framework-neutral mailbox owns 3D and
  linked-2D raw gesture samples, target, gesture identity, base currentness,
  and monotonic intent revision. Egui no longer owns a second latest-camera or
  currentness state. Raw samples bypass the application reducer, project
  history, and durable `SetLayout`/camera commands. Resident camera movement
  reuses the installed body without planning; nonresident camera or active
  plane movement replaces one latest-only planner request. Drag finish or
  scroll settlement commits the latest value once, and product automation
  uses the same path.
- The production-composition check forces planner result N to block while a
  distinct plane body N+1 supersedes it. It proves that only N+1 installs,
  only the active XY body changes, passive XZ/YZ bodies retain identity, the
  actually missing active-plane key enters the real runtime first at
  foreground priority, an installed resident/empty body wakes display work,
  and finish commits once without replanning the already exact active panel.
  An outside-volume plane installs a terminal empty scope and does not retry.
  The narrower automation check proves exact three-sample camera accumulation
  before its single durable commit; it is not cited as pixel evidence.
- **P1-B canonical brick identity — implemented and focused-verified.** The
  former semantic resource-key name was hard-renamed at its definition and all
  516 Rust uses to the sole product `BrickKey`. Its identity, logical edge,
  exact region shape, equality, ordering, and hashing did not change. There is
  no alias, adapter, second key type, physical-chunk identity, or reverse
  mapping. The focused equality/hash contract and affected crate compilation
  pass.
- **P1-C projected plane demand — implemented and focused-verified.** The old
  depth-AABB scan is deleted. Physical pixel centers are projected through the
  affine transform and traversed in two dimensions with the declared sampling
  halo. Planner controls mirror the renderer's f32 plane and inverse-transform
  controls, carry a bounded outward shader-arithmetic radius, and reject an
  unrepresentable half-voxel contract rather than under-demand. Independent
  affine/oblique membership, shader-boundary, exact-footprint, and per-brick
  sliver-ranking checks exercise distinct failure modes.
- **P1-D CPU foreground and currentness cut — implemented and
  focused-verified.** The planner worker is owned by the dataset data plane and
  consumes mailbox revisions rather than allocating a second generation.
  Per-panel signatures let a transient plane replace only its active scope.
  Active linked demand is admitted before passive work at `CurrentView`.
  Incoming foreground work preempts an executing playback/analysis/prefetch
  decode, preserves its original ticket/waiters, and resumes the same logical
  job after the foreground request completes; the focused check verifies both
  resulting payloads byte-for-byte.
- **P1-E presentation and normal-product closure — implemented,
  focused-verified, and mapped-product exercised.** A cold coarse 3D
  publication now queues and wakes the successor refresh required to render
  its hidden exact target. Static-layout reuse is qualified by the layout
  actually installed on that exact presentation token, not merely by a
  semantically matching prepared scope; blank or recycled targets therefore
  receive a full layout. Exact cross-section finish adopts the already
  retained frame, and terminal empty intent clears the superseded image.
- The two presentation defects above were found by the strict normal mapped
  exercise, not by weakening a wait or accepting its status field. Each has a
  focused trusted-Vulkan regression that renders through the real WGPU path:
  the cold-refinement case asserts visible coarse pixels, a real repaint
  wakeup, a distinct exact hidden frame, atomic adoption, and cleared staging;
  the cross-section case asserts retained exact-frame adoption and subsequent
  empty clearing. Each exact invocation matched and passed one test.
- The normal mapped release product passed a 1280x720 four-panel exercise with
  an externally observed viewable X11 client. It reached exact/current initial
  3D and XY/XZ/YZ presentations, then admitted 300 time-distributed XY slice
  samples and 300 compound-angle rotation samples over five seconds each.
  Each gesture used the transient mailbox, coalesced 299 samples, made one
  durable commit, finished with a zero generation gap and a current final
  input, changed the requested geometry, and performed no new dataset request,
  read, decode, or upload. All panels remained distinct and nonblank, all
  three cross schedules were observed, and WGPU reported no validation error.
  This small fixture closes P1 protocol and product correctness only; it is
  not a representative performance claim.
- The same run reported 310 static rebuilds, zero dynamic updates, 620 page
  layout constructions, 1,260 bind-group creations, and 315 buffer
  allocations despite unchanged residency work. That is a P2 blocking
  diagnostic: resident view-only motion is still rebuilding GPU control state.

- Add the constant-size transient interaction mailbox for 3D and every
  cross-section gesture, bound to target and durable revision.
- Evolve or alias the current semantic resource key into one canonical logical
  `BrickKey` at the demand/I/O boundary, using the current logical product
  edge. Do not introduce a parallel product identity. The current-format
  reader performs the only stateless physical-chunk conversion.
- Replace the full 3D-AABB plane scan with the exact projected plane planner.
- Make one data-plane actor own global interests, priority, I/O/decode tickets,
  the decoded cache, cancellation, and latest-intent replacement across 3D,
  panels, playback, verification, and analysis.
- If unavoidable, let the existing renderer consume the new authoritative
  result through the specifically authorized stateless
  `BrickKey -> LegacyRenderRequirements` projection.
- Delete raw cross-section `SetLayout` dispatch per input sample, old plane
  demand generation, duplicate demand workers/waiters, duplicate decoded
  caches, and independent background wake/decode authority in the same
  milestone.

Exit:

- oblique-plane candidate work is bounded by crossed projected cells plus the
  declared sampling-support halo, not a three-dimensional candidate box;
- foreground interaction can preempt or pause lower-priority work through the
  one actor;
- each gesture commits only its final current value;
- the normal four-panel app is visibly exercised; and
- there is no second product brick key or stateful adapter.

If request/decode work is still disproportionate to the useful plane set, stop
before GPU work and repair this authority.

### P2 — Global GPU Residency Authority

Status: COMPLETE — GLOBAL OWNER AND CANONICAL AFFINE-CONTROL SEAM ACCEPTED

- Port the persistent directory, page-record, payload-arena, and latest-only
  mechanisms into the existing renderer authority.
- Make every active mode, including modes still using the monolithic shader,
  read the same global residency representation.
- Keep actual slots, pins, uploads, evictions, and transfer fences in the
  residency subowner. Target textures and color fences remain inside the
  renderer root and move wholly to its frame coordinator in P3; neither may
  escape to the app or CPU data plane.
- Replace cross-owner prepared transactions with idempotent residency requests
  and simple typed completion/failure events.
- Keep reusable same-dataset uploads when a camera frame becomes stale while
  suppressing its stale draw.
- Preserve the bounded mapped staging pool, exact byte accounting, validity
  behavior, and terminal GPU latch.
- Delete P1's `BrickKey -> LegacyRenderRequirements` projection as the
  renderer adopts the canonical key directly.
- Delete presentation-specific page tables, duplicate payload/resident maps,
  camera-layout rebuild ownership, app/session GPU pin bridges, and duplicate
  upload/eviction paths in the same milestone.

Exit:

- all modes use one GPU directory and payload authority;
- resident movement performs zero I/O, decode, upload, eviction, or directory
  rebuild;
- capacity failures remain typed and no fallback is introduced;
- the normal four-panel and volume-mode workflows still render correctly; and
- no CPU/app owner can claim an authoritative GPU slot, pin, target, or fence.

Accepted result:

- one private renderer `ResidencyOwner` now owns the exact-byte payload arena,
  global sparse directory, page records, physical residents, LRUs, bounded
  staging, pending decoded-lease queue, sequenced eviction events, and opaque
  resident-frame leases;
- presentation state holds only the opaque frame lease, while pin counts and
  immutable requirement bodies remain inside the residency owner;
- app/session resident-set mirrors, per-scope GPU recovery sets,
  presentation-specific page tables, legacy requirement cursors, and
  per-target lease-offer scheduling are deleted;
- one pure render-API helper now owns world-to-grid inversion and its exact f32
  shader-control projection; planner and renderer duplicates are deleted while
  their distinct typed error mappings remain;
- same-body execution selects through a per-frame-lease relevant-order index
  rather than scanning the 65,536-entry global offer queue, and installed
  camera-guard readiness advances monotone required/full-body cursors instead
  of rescanning large bodies on every raw sample;
- collision/tombstone/reupload behavior, union-scoped pins, lossless
  eviction/reoffer and event-log backpressure, cold hidden refinement, exact
  linked-target adoption/clearing, independent numerical output, and
  multichannel order independence passed focused checks on the Vulkan product
  device; and
- the mapped release resident-navigation scenario published two exact warm
  frames with zero new reads, decodes, dataset requests, uploaded resources,
  or uploaded bytes, then added zero submissions through 120 idle frames.
  The mapped MIP/DVR/ISO workflow also passed with no WGPU validation error.

If resident interaction still mutates residency, stop before kernel work.

### P3 — Coordinated Frame And Presentation Authority

Status: COMPLETE — ONE COORDINATED PRODUCT PATH

- **Complete — cold product admission prerequisite.** The existing-device
  product constructor returns after fixed resource/layout creation and starts
  one capacity-two compiler worker. The five dedicated color pipelines become
  available before Pick; readiness and the first failing creation
  operation/cause are polled once per UI turn without blocking, spinning,
  fallback, or a second renderer. Demand, decode, and residency offers remain
  live while all presentation/currentness claims are gated. Cold shutdown
  cancels and detaches from an uninterruptible driver call. GPU tests request
  the Vulkan device externally, enter this same constructor, and poll the same
  ordered readiness protocol; the blocking self-created-device constructors
  and ready-at-construction state are deleted.
- A fresh NVIDIA driver-cache mapped run admitted command zero in 46 ms,
  emitted progress throughout roughly 20 seconds of cold compilation, then
  produced current pixels and passed the resident-navigation workflow. Exact
  worker ordering/cancellation/first-failure checks and real-Vulkan readiness
  and resident-rendering checks pass.
- The duplicate application `RenderIntentRevision` value type is deleted:
  the mailbox now allocates the render API's opaque `FrameIdentity` directly.
  The fixed 3D/XY/XZ/YZ identity is likewise canonical
  `PresentationTarget` in the render API; the application name is a stateless
  re-export, not another allocator or conversion.
- One private renderer `FrameCoordinator` now owns the four fixed target
  fronts, a private eager 3D replacement, target texture revisions,
  presentation currentness, color retry, completion leases, recording order,
  and color submission. The app owns none of those ledgers and has no
  per-panel encoder, submit, target allocator, staging texture, or capture
  scheduler.
- A coordinated cutoff records the active target first, linked 2D targets
  next, and passive 3D last into one color encoder and one queue submission.
  At most two color cutoffs may remain in flight. Exact 3D replacement remains
  private until its complete post-submit atomic swap, while the prior front
  stays visible and pickable. Omitted targets and incomplete replacements
  sleep until a real offer or target-owned retry can change their outcome.
- Same-body resident movement rebinds an opaque lease and shared pin cohort in
  O(1), without entering transfer planning. It still records and publishes
  changed color targets through the coordinator and advances retained-frame
  accounting only after the color submission.
- The direct Vulkan four-target check produced four distinct nonblank images
  matching the independent CPU oracle, recorded XY/XZ/YZ/3D in the declared
  active-first order, used one color submission, changed no residency or
  allocator state, and added zero work for an identical idle cutoff. Focused
  product-path checks also exercised coarse-to-exact 3D wake/swap and exact
  linked-panel finish/empty clearing through real pixels and currentness.
- The named mapped release resident-navigation scenario passed with exactly
  two publications and two color submissions for its two warm camera changes.
  It added zero physical reads, decodes, dataset requests, uploads, evictions,
  arena allocator plans, directory/page writes, buffers, bind groups,
  pipelines, or staging allocations; timing was disabled and settled idle
  added zero submissions. The separate 103-command MIP/DVR/ISO mapped
  walkthrough also passed.
- The final hard-cut audit found no test-only direct presentation/execution
  path. Tests use the same coordinated layout, frame, capture, and pick entry
  points as the product. The remaining test constructor parameter changes only
  the bounded payload-segment limit on the same existing-device constructor.
  There is no per-panel submit owner or alternate target/currentness authority.

- Replace per-target frame submission with one frame coordinator for all
  visible dirty targets.
- Use stable logical target IDs, durable-state dataset generations,
  renderer-root device generations, and frame-coordinator texture revisions.
- Move target texture allocation/revision, color submission/completion fences,
  and release of the opaque resident-frame lease wholly into the frame
  coordinator. It must not mirror the residency pin or transfer-fence ledger.
- Give mode passes only borrowed recording contexts; they cannot submit,
  present, or poll.
- Record the active target first, related 2D targets next, and passive 3D last.
- Use one color-work queue submission by default. Test one fixed
  active/passive two-stage candidate only in the predeclared heavy-passive-3D
  scene, and select it only when one-stage violates the active-group
  latency/gap gate in at least two of three runs while two-stage passes the
  active-group, full-layout-settlement, correctness, and resource gates.
  Otherwise delete two-stage.
- Define and validate the final per-target synchronization groups.
- Coalesce timing polling without repaint polling or per-ticket wake fan-out.
- Delete per-panel encoders/submits, duplicate backpressure, app-owned target
  allocators, and competing presentation-currentness owners.

Exit:

- a resident coordinated cutoff follows the selected one- or two-stage
  color-submission budget;
- the active plane is not held behind stale or heavy passive work;
- each target/group remains complete and current according to its declared
  policy and final full-layout settlement remains within its gate;
- the normal app has no per-panel submit owner; and
- primary gates run with GPU timing disabled; a separate diagnostic run
  coalesces polling and explicitly labels/excludes timing-only submissions.

If heavy 3D work still starves an active plane, stop and repair the one
coordinator; do not restore independent panel queues.

### P4 — Dedicated Kernels

Status: COMPLETE — DEDICATED KERNELS AND MAPPED PRODUCT BOUNDARY ACCEPTED

- **Complete — Plane hard cut.** `ColorKernel` now exhaustively classifies
  Plane, MIP, DVR, ISO, and Mixed intent without a default arm, and every
  production color-recording site uses that one selector. Plane has its own
  pipeline composed from an ordinary shared binding/directory/payload/sampling
  source plus a 53-line additive plane module. The module contains no volume
  ray, page DDA, MIP, DVR, ISO, pick, alpha-termination, or dynamic view-kind
  branch. The cross-section function and branch are deleted from the temporary
  volume remainder.
- The exact Vulkan coordinator check rendered three rotated Plane targets plus
  MIP 3D as distinct nonblank images matching the independent CPU oracle,
  using one color submission with zero residency or allocator mutation and
  zero identical-idle work. Focused selector, module-isolation, and ordered
  first-pipeline-failure checks each matched and passed one test. P4 now moves
  between focused kernel subcuts without a mapped product rerun.
- **Complete — MIP hard cut.** Shared volume code now contains only ray and
  page-segment mechanics; one canonical MIP core owns raw maximum over valid
  samples, missing-coverage propagation, page-local segment sampling, and the
  single post-traversal transfer. The dedicated homogeneous MIP module cannot
  reach Plane, DVR, ISO, Mixed dispatch, pick, donor evidence, or terminal-
  maximum logic. The temporary Mixed path calls that same core but cannot
  redefine it, and the old standalone MIP fragment/entrypoints are deleted
  from the volume remainder.
- The focused Vulkan MIP check rendered a 65x65 float32 voxel-exact off-axis
  perspective frame and matched pixel `[48,32]` RGBA, coverage, and validity
  to the independent numerical oracle. The exact structural isolation check
  also passed.
- **Complete — DVR hard cut.** Homogeneous DVR now has its own module and
  pipeline. The dedicated source alone owns fused compatible-grid and
  common-world joint-medium integration; the temporary Mixed remainder can
  reach only the shared single-layer emission-absorption primitive. Physical
  world-step distance, deterministic CPU-canonical layer order, exact-only
  opacity termination, and page-segment reuse remain explicit. A focused CPU
  regression proves canonical reordering keeps every layer's scale,
  transform, transfer, and grid-cell fields in one aligned record.
- The focused off-axis Vulkan DVR check matched the independent numerical
  oracle exactly at pixel `[48,32]`: RGBA8 `[109, 0, 0, 151]`, coverage,
  validity, maximum-contribution pick value, world position, and physical ray
  distance all passed. Exact module-isolation, control-record, traversal, and
  ordered pipeline-construction checks also passed.
- **Complete — ISO and Mixed hard cut.** ISO owns its six-tap
  inverse-transpose world gradient, attached/detached lighting, first-hit
  traversal, physical world hit depth, depth-sorted homogeneous stack, and
  color/validation entries. Mixed alone owns explicit MIP/DVR/ISO per-layer
  dispatch and authored-order over-composition; an invalid mode fails closed
  with incomplete coverage, and Mixed cannot call joint DVR integration or
  the homogeneous ISO stack.
- The focused Vulkan ISO check matched affine attached and detached lighting
  to the independent CPU reference and proved that withholding one gradient
  support page yields Progressive color with covered=0/valid=1 and an
  Incomplete first-threshold pick, while restoring it yields Exact color with
  covered=1/valid=1 and an Exact pick. The independent off-axis oracle also
  matched ISO RGBA/facts, physical hit depth and pick distance, and reversed a
  deliberately far-first two-layer stack. The focused Mixed check matched the
  independent reference and fixed hand facts `[26, 69, 0, 194]` versus
  `[10, 115, 0, 194]` when authored MIP/ISO order was reversed.
- **Complete — Pick hard cut.** Pick compiles only the accepted
  binding/sampling source, volume ray/page mechanics, the scalar DVR optical
  conversion, the six-tap ISO gradient, and its compute program. It cannot
  compile a color kernel, fragment entry, volume compositor, or the deleted
  monolithic shader. Focused MIP, DVR, ISO, halo-completeness, typed hit,
  world-position, and physical-distance checks pass.
- The five color pipelines are Plane, MIP, DVR, ISO, and Mixed, followed by
  the separate Pick compute pipeline. The former general pipeline and
  monolithic `shader.wgsl` are deleted. P4 now has only the strengthened
  mapped render-mode/four-panel product exercise at its exit boundary.
- The finite mapped resident-navigation run completed all 31 normal-app
  commands. Each fixed 300-sample gesture used 300 distinct egui updates,
  produced 301 current linked-2D publications and 301 one-color queue
  submissions, committed durably once, and finished exact/current. Slice and
  rotation admission-to-current p95 were respectively 0.304 ms and 0.263 ms;
  maxima were 0.422 ms and 0.337 ms. Final full-layout settlement was
  0.227 ms and 0.151 ms. All 29 guarded resident-work counters stayed zero,
  and 120 settled callbacks added no work or submissions.
- The validator still failed its callback/publication-cadence checks. The
  observed roughly 33 ms / 1 ms pairs follow `App::ui` entry and publication,
  while eframe acquires and presents the FIFO surface only after `App::ui`
  returns. On the mapped X11/Vulkan surface the requested latency-one FIFO is
  clamped to a three-image swapchain. This pre-surface clock therefore cannot
  distinguish two queued images shown on consecutive refreshes from a visible
  missed refresh. It is valid semantic-issuance diagnostics, not compositor
  presentation evidence.
- On 2026-07-29 the owner approved the smallest honest measurement-boundary
  amendment. Admission p95 remains blocking at 20 ms; maximum admission and
  final settlement remain blocking at 50 ms; exact publication/submission,
  currentness, structural-zero, resource, and owner-observed visual gates
  remain blocking. The validator now requires exactly 301 current
  publications and admission samples per 300-sample gesture and retains the
  pre-surface callback/publication cadence only as an explicitly non-blocking
  diagnostic.
- The bounded mapped rerun passed after two measurement defects and one real
  fast-path defect were corrected rather than weakening the gate. Automation
  now waits for the final transient sample to become current before emitting
  the durable finish generation, and final generation-bound publication is
  counted even when it lands just after the active window closes. Passive
  linked-panel durable promotion now proves renderer residency before its
  O(1) promotability check, so an already-resident guard body does not enter
  the planner merely because its monotone readiness cursor had not yet been
  observed.
- The final 138-command mapped Plane/MIP/DVR/ISO/Mixed/four-panel exercise
  passed in the normal release application. P4 therefore exits with one
  canonical kernel per mode and no monolithic or fallback shader path.

- Split plane and MIP first, then DVR, ISO, mixed behavior, and picking.
- Delete each old mode branch, entry point, bind layout, and shader code in the
  same change that activates its dedicated kernel.
- Keep plane modules structurally unable to reach volume traversal code.
- Port page-resolution caching, retained page state across DDA segments,
  monotone transfer work, MIP early termination, and joint DVR order only
  where focused measurement or correctness supports them.
- Rewrite and profile mixed mode rather than blindly porting its large
  recovered shader.
- Run independent numerical and validity/coverage checks for each affected
  mode, followed by the normal-app exercise.

Implementation map, frozen at the accepted P3 exit:

- Keep the accepted P2 control ABI, global directory/page records and
  `ResidencyOwner`, plus the P3 coordinator and its one color-submit
  authority. Recover shader-local algorithms only; no donor runtime, session,
  target, transaction, residency, or storage owner crosses this boundary.
- Add one exhaustive private `ColorKernel` selection:
  cross-section is plane; homogeneous MIP, DVR, and ISO stacks select their
  matching kernels; only heterogeneous volume stacks select mixed. There is
  no default or fallback arm.
- Compose six ordinary, separate WGSL modules from shared source pieces:
  plane, MIP, DVR, ISO, mixed, and pick. Plane receives the binding,
  directory, payload, sampling, transfer, and plane-composition common source
  only. It must not contain or reach volume rays, page DDA, MIP, DVR, or ISO.
  Pick likewise does not concatenate the color monolith.
- Replace the general/MIP pipeline pair with five color pipelines sharing the
  accepted color bind-group layout. Color and validation entry points call
  the same mode implementation. Pick keeps the accepted extended pick layout.
- Port resolved-page retention and segment-local sampling first. Preserve the
  current variable logical-cell/page authority and adapt the recovered
  mechanism to it; do not restore the donor's fixed brick-edge DDA or ABI.
- Plane remains the current additive, authored-order-independent scientific
  implementation. MIP remains raw maximum over valid samples with transfer
  applied once. The previously measured sub-five-percent terminal-maximum
  optimization stays deleted unless a new direct timing result clears the
  retention threshold.
- DVR keeps common-world physical-distance integration, effective alpha to
  optical-depth conversion, deterministic joint-medium order, and exact-only
  opacity termination. Canonicalize homogeneous DVR layer order once on the
  CPU instead of sorting it per pixel.
- ISO keeps inverse-transpose normals, attached/detached lighting, physical
  hit depth, six-tap halo completeness, and depth-sorted all-ISO composition.
  Mixed is rewritten from the current authored-order semantic law and starts
  without the donor's large private arrays or multi-page set.
- Picking keeps the current public policy/result semantics while reusing the
  segment-local page mechanism and the P3 front/lease authority.
- Explicitly discard donor kernel manifests, SHA/Naga hashes, evidence modes,
  fidelity probes, descriptor counters, published source-order arrays, EP
  feature names, and every `publish_*_fidelity` path.
- Cut over and delete in this order: plane; MIP and `uses_mip_pipeline`; DVR;
  ISO plus mixed and the last general entry point; pick and the final
  monolithic source concatenation. Each subcut runs its one independent
  numerical/coverage check, then the final cut runs the mapped render-mode
  and four-panel product exercise. No broad suite is inserted between
  subcuts.

Exit:

- every render mode has one canonical kernel;
- plane work cannot compile a volume-mode call graph;
- missing/all-invalid data, calibration, DVR distance, ISO halo/pick, LOD, and
  atomic presentation remain correct;
- the monolithic shader is deleted when its last mode moves; and
- the normal-app product result remains correct while per-mode GPU timings are
  reported as diagnostics unless an owner-approved P0 threshold exists.

### P5 — Runtime Hard Cut

Status: COMPLETE — ONE PRODUCT RUNTIME PATH ACCEPTED

The final P5 read-only hard-cut audit found one ordinary runtime path:

- **Complete — P2 affine-control seam.** One pure render-API helper now owns
  world-to-grid inversion and its f32 shader-control projection. The planner
  and renderer duplicates are deleted with caller-specific typed error
  mapping retained.
- **Complete — P3 coordinated execution hard cut.** The sole public renderer
  exposes only fixed-target coordinated layout, frame, capture, and pick entry
  points. Every color target records into the frame coordinator's one encoder
  and one color submission. Test-only helpers alter fixture inputs or a
  bounded constructor parameter; none can allocate, render, submit, present,
  or claim currentness through another authority.
- **Complete — P4 constructor admission hard cut.** Tests now create the
  Vulkan device externally, call the sole existing-device constructor, and
  poll the real ordered background readiness protocol. The public
  self-created-device constructor, internal blocking construction mode,
  ready-at-construction pipeline state, and renderer-owned WGPU instance are
  deleted. The segmented-payload fixture uses only a test parameter on that
  same constructor path.

The first strengthened mapped render-mode run also exposed and fixed a
truthful-state bug: an exact full-coverage scale-0 presented frame was later
relabeled merely `Complete` by dataset status refresh. The bounded mapped
resident-navigation rerun retained exact/current final state for both
gestures; the exact gate remains in place.

The final mapped product reruns passed through the sole existing-device
constructor, global residency owner, frame coordinator, dedicated kernels,
and generation-bound presentation authority. No alternate constructor,
per-panel submit owner, target/currentness authority, monolithic shader, or
fallback render path was reintroduced.

- Confirm that P1's named compatibility projection and every mode-local
  adapter were already deleted at their required milestones. P5 is not allowed
  to become their first deletion point.
- Remove only unreachable recovery/predecessor naming, aliases, tests, and
  configuration debris. If a live alternate constructor, Cargo feature,
  renderer module, or production path remains, stop and reopen the milestone
  that should have deleted it.
- Tune bounded capacities for the current logical representation without
  raising existing ceilings silently.
- Rename the accepted path to ordinary product names.
- Run the focused checks, affected-boundary Vulkan checks, existing PR
  verification, and a normal real-display product exercise.
- Update Architecture and Current Work to describe the one implemented runtime
  path. Current State remains explicit about any P6 acceptance work still
  open.

Exit:

- exactly one renderer/session constructor exists in the normal app;
- no product code refers to `successor-product`, `predecessor`,
  `legacy_renderer`, a former presentation page-table owner, or the deleted
  monolithic shader;
- no render-mode fallback or hidden alternate path remains; and
- the normal native application is correct and usable enough to run the final
  acceptance workload.

### P6 — End-To-End Acceptance And Storage Boundary

Status: FAILED — OWNER-OBSERVED FOUR-PANEL OBLIQUE FREEZE

The former command-driven three-session result is a narrow diagnostic only.
Its internal currentness, publication/submission counts, structural-zero
counters, and static pixel inspection did not exercise or measure the actual
continuous oblique UI interaction. The owner's normal-app observation
therefore revokes both its performance acceptance and the storage conclusion
derived from it.

The current package format and `64³` logical renderer/cache brick edge remain
unchanged defaults. They are not a P6 performance result. Storage changes
remain unauthorized unless C3 identifies required unavailable-brick physical
I/O/decode as the first blocking boundary and the owner approves a separate
amendment.

C1 through C5 now own P6 completion. The broad repository verification result
remains useful integration evidence, but it cannot outvote the visible freeze
or substitute for the corrected real-interaction acceptance.

Successful exit:

- every end-to-end blocking gate passes on the one hard-cut runtime;
- the retained current storage/logical-edge decision is recorded honestly;
- the visible representative workflow is owner-accepted;
- Architecture, Current State, Testing if commands changed, Current Work,
  README, and `documentation-index.json` are updated consistently; and
- this completed active plan is deleted after Current State owns the
  implemented result. Git history remains the archive.

## Deletion Gates

These are source-tree and ownership outcomes, not requests for a new policy
test suite:

- **Brick identity:** one logical product `BrickKey`, one logical renderer/cache
  edge, stateless physical-package mapping only at source I/O, and no inverse
  product adapter.
- **CPU data plane:** background work expresses low-priority interest through
  the same actor and cannot independently open, decode, upload, or wake.
- **GPU residency:** one renderer directory/arena; no per-presentation page
  table, app residency bridge, or camera-layout rebuild owner.
- **Renderer subowners:** the frame coordinator holds only an opaque resident
  lease and cannot mirror residency pins/transfer fences; residency cannot
  mirror target revisions/color fences.
- **Frame submission:** presentation passes cannot access queue submission;
  the selected coordinated policy is the only color/presentation submit path.
- **Kernels:** one module/entry per mode and no “otherwise use old renderer”
  arm.
- **Composition:** one renderer constructor and no default-off alternate
  product feature.
- **Final tree:** no live product reference to recovery or predecessor naming.
- **Storage, if later approved:** one physical format/profile and logical edge,
  with no runtime candidate or compatibility reader.

Git history is the rollback and research archive. A runtime fallback is not.

## Useful Verification

Use the smallest check that can falsify each change:

- focused geometry tests compare projected plane enumeration with an
  independent brute-force oracle on small grids;
- focused state tests cover gesture revision invalidation, latest-intent
  replacement, priority, cancellation, and capacity failures;
- focused renderer tests cover directory lookup, collision/probe bounds,
  upload/eviction accounting, complete coverage, stale suppression, and
  terminal GPU failures;
- independent numerical tests cover sampling, validity, transforms, DVR
  physical distance, ISO gradient halo/picks, MIP, and mixed behavior;
- the trusted Vulkan lane runs when GPU-resource or shader boundaries change;
- the normal mapped native application is inspected after every milestone;
  and
- `cargo xtask verify-pr` runs at integration and final cutover, not on every
  edit.

No green test or counter can substitute for the exact visible workflow that
failed.

## Risks And Responses

| Risk | Response |
| --- | --- |
| The successor never proved itself in the normal app | Integrate every authority replacement into the normal app immediately; port mechanisms, not the recovered product shell. |
| Layered work accidentally creates two pipelines | Replace one authority at a time, delete its predecessor in the same milestone, and forbid stateful adapters or product toggles. |
| Recovered orchestration remains too complex | Use four top-level owners plus two non-overlapping renderer subowners; rewrite session, bridge, pin, transaction, and target lifecycle glue. |
| Heavy 3D work delays the active plane | Prioritize the active target and measure one coordinated submission against one bounded active/passive policy. |
| Global atomicity lets one panel freeze the others | Define complete per-target synchronization groups; test the 2D-group/independent-3D policy in the product. |
| `32³` might reduce overfetch but multiplies page/brick pressure | Keep logical renderer identity separate from physical package geometry; admit a separate experiment only if the hard-cut runtime misses a cold gate specifically while blocked on required-brick I/O/decode. |
| Performance work regresses scientific behavior | Preserve independent oracles and complete-frame/currentness semantics at every affected mode boundary. |
| Process grows faster than implementation | Use one plan, compact counters, three runs, focused checks, and the existing product-validation path. |
| The cutover target keeps moving | Freeze predecessor feature work; share any branch-local critical fix and delete its predecessor copy at the active milestone. |

## Hard Stop Conditions

Stop the current milestone and repair its design if:

- two caches, actors, GPU directories, resident maps, frame coordinators, or
  submit owners would remain after acceptance;
- a temporary adapter acquires state, caching, scheduling, retries, uploads,
  eviction, submission, or bidirectional conversion;
- a feature flag or fallback is proposed to keep both product paths;
- the normal app cannot be run and visibly inspected at the milestone;
- a phase begins generating receipt, hash, schema, role, or evidence-population
  machinery;
- P0 grows beyond one fixed-capacity aggregate hook or delays P1 merely because
  a deep diagnostic is unavailable;
- a structural gate fails and downstream shader tuning is proposed as
  compensation;
- a milestone introduces cross-owner prepare/accept/admit transactions,
  exposes internal slot/pin handles, mirrors currentness/generation/fence
  ledgers, or grows a session state machine instead of using idempotent
  requests and typed completion;
- stale/current correctness would require keeping the old implementation;
- the representative end-to-end result fails to improve materially; or
- storage candidate code or a format change is proposed without the P6
  diagnosis and owner-
  approved amendment.

If the architecture cannot satisfy one owner per fact, stop and redesign the
seam. Do not hide the conflict behind more tests.

## Rejected Alternatives

- **Small hotfixes only:** cannot remove the measured planning, residency,
  currentness, and submission fan-out.
- **Merge the recovery branch:** imports roughly 200 commits, unfinished
  product integration, duplicate authorities, and disproportionate
  qualification machinery.
- **Build a second successor beside the current renderer:** postpones product
  feedback and guarantees a dual cutover problem.
- **Rewrite storage first:** risks repeating the prior product-last program
  before proving that storage dominates.
- **Build speculative `32³`/`64³` candidates inside this overhaul:** recreates
  dual reader/writer/import work before the hard-cut runtime shows that
  required-brick I/O/decode blocks a failed product gate.
- **One-submit dogma:** confuses frame ownership with an exact queue count and
  may let heavy 3D work delay the active plane.
- **Thousands of tests as confidence:** cannot detect obvious visible freezes
  or prove the representative product workflow.
- **No tests or bounds:** would endanger scientific correctness, source/user
  data, and resource safety.

The middle ground is a deep product overhaul with simple ownership,
layer-by-layer deletion, focused independent correctness evidence, and early
normal-app measurement.

## Authorization Boundary And Handoff

Approval of this plan authorizes P0 through P5 and P6 acceptance/diagnosis
within the authority model and evidence class above. It does not authorize
storage candidate generation, temporary candidate readers/writers/importers,
or a storage/import format cutover; those require the measured P6
recommendation, a concrete amendment, and explicit owner approval.

One-versus-two color-work queue submissions and final synchronization groups
may be selected from the predeclared P3 product measurement without expanding
scope. Any new product capability, fallback, second architecture, public
performance claim, storage format, or materially broader evidence class
requires owner approval.

The implementing agent must:

1. read repository policy and the current checkpoint;
2. start only the next unblocked milestone;
3. keep the normal app runnable and inspect it at each exit;
4. update this plan and Current Work with decisions and accepted results;
5. report focused checks, product observations, skips, and remaining risk;
6. avoid starting a later layer to conceal a failed earlier gate; and
7. leave no completed plan as a second implemented-state authority.

This plan records the granted implementation authorization and the current
cutover checkpoint; implemented facts remain owned by Current State.
