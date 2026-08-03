# Viewer Rendering Correctness, Recovery, And Numerical Cutover

- Status: OWNER-AUTHORIZED — CORRECTNESS CUT IMPLEMENTED; P0/P7 AND P8 CLOSEOUT OPEN
- Planning requested by owner: 2026-08-02
- Full implementation authorized by owner: 2026-08-02
- Planning baseline: `d5032c43525ddfa9d524d490e3391aa800c6d470`
- Last reviewed: 2026-08-02
- Scope: composed-presentation reuse, renderer attempt and wake authority,
  retryable and terminal failure projection, hidden refinement, pipeline
  capability isolation, pick retirement, affine and camera admissibility,
  volume page traversal, shader coordinate precision, and controlled static
  performance recovery.

This document is the sole target-design authority for the corrective package
described below. The owner authorized full implementation on 2026-08-02.

## Owner-Authorized Empty-Surface Correction, 2026-08-02

The owner subsequently reported that hiding the final visible channel leaves
the mapped window displaying its previous pixels until another channel is
shown. The first correction incorrectly assumed that demand planning had
already installed the semantic empty presentation and that only a later egui
composition turn was missing. A mapped native-product reproduction disproved
that assumption: both channel visibility commands reached canonical state,
but the first replacement was still holding the old renderer front and the
last-hide transaction remained pending after the renderer rejected a proposed
requirement union with `RequirementSetChanged`. The stale color front therefore
remained both renderer-owned and visible. Synthetic tests that installed empty
demand before refresh could not exercise this ordering and are not qualifying
evidence for the product defect.

The corrected cut owns both halves of the transition: canonical empty display
publication must not wait for asynchronous demand planning, and the published
renderer front must be retired before the empty renderer requirement union is
preflighted. The implementation will correct that contract as follows:

1. `refresh_frame` and `refresh_texture_only` will stop reporting the presence
   of a new GPU color submission as a proxy for a visible presentation change.
   Their single return authority will be a typed, fixed-target composition
   change: unchanged, texture-published, surface-cleared, or the bounded mixed
   case where different members of one coordinated layout did both.
2. The change will be derived across the complete active presentation target
   set, including presented-frame identity, native texture-binding identity,
   and the target's background/empty projection. A transition from an old
   frame to an intentional current empty surface therefore requires a
   composition turn even though it submits no synthetic black GPU frame.
3. UI callers will request exactly one later composition turn for every typed
   non-unchanged result. Re-observing the same empty state must return
   unchanged, schedule no hot loop, and perform no renderer submission.
4. Empty publication will remain a successful current presentation: the old
   color frame, validation-capture authority, pick currentness, and obsolete
   native binding are retired through their existing owners. Cached dataset
   residency is not forcibly evicted, and empty state is never projected as
   loading, failure, or a fallback renderer.
5. The rule applies to the standalone 3D layout and every member of the linked
   four-panel layout. Showing a channel again follows the ordinary demand and
   publication path with no empty-state latch or special recovery path.
6. `rerender_coordinated_display_state` will recognize the canonical
   zero-visible-layer view before consuming or submitting a visible-demand
   worker result. It will first request the empty renderer layout, which
   atomically retires fixed-target fronts and their frame leases, and then let
   the existing latest-only planner install the zero-body dataset transaction.
   Until that transaction is current, every refresh remains on the explicit
   empty surface and cannot reconstruct or repaint the predecessor from stale
   installed scopes.
7. Superseded nonempty planner results remain stale-suppressed by the mailbox
   revision. A deterministic failure from an intermediate visibility set must
   not latch against the newer zero-visible-layer signature. Once the front is
   retired, empty-union preflight must succeed without weakening the
   renderer's rule that a genuinely published nonempty front remains part of
   the global residency obligation.

The old boolean/color-submission interpretation is deleted at the cut; no
parallel repaint predicate may remain. The implementation must preserve the
lifetime of texture paints already queued earlier in the same UI turn, must
classify mixed four-target changes without variable-size state, and must not
disturb retained-front behavior for a nonempty recoverable successor. Focused
tests will cover old-frame-to-empty before demand installation, sequential
multichannel hides with an intermediate replacement pending, repeated-empty
quiescence, empty-to-frame, single-3D and linked layouts, stale
presentation/pick retirement, renderer-front retirement before empty-union
preflight, and the post-paint repaint decision. Product validation will
exercise the hide-last, empty composition, idle, and unhide lifecycle in the
mapped native application on the real display before the normal repository
verification lane is rerun. A launched process, an automation sidecar, or a
headless/internal frame alone is explicitly not evidence that this lifecycle
passed.

Corrective implementation record, 2026-08-02: canonical empty publication now
precedes visible-demand result consumption, the empty coordinated renderer
layout retires published fronts before aggregate-union preflight, and omitted
native binding identities move to a fixed four-target deferred-free set until
the next UI turn. Empty fidelity clears prior scale, refinement, timing, and
failure projection and reports that rendering is not required. The focused app
library suite passes 348 tests with 9 explicitly ignored GPU/developer-local
cases, and the UI package passes all 41 tests. The rebuilt release application
completed a 30-command native RTX/Vulkan lifecycle covering sequential
two-channel hide, settled empty idle, unhide, four-panel hide, four-panel empty
idle, and four-panel unhide; both empty assertions in each layout and the final
no-render-error assertion passed, with no final planning or renderer error.
The mapped 960x720 native window was also captured with both channels hidden
and the old volume absent, then with one real checkbox restored and the exact
GPU volume visible again. This evidence supersedes the failed reproduction
that timed out with `RequirementSetChanged`; it does not close the unrelated
P0, P7/R11, temporal-workload, clean-revision, or owner-acceptance items below.

Implementation record, 2026-08-02: the P1–P6 production hard cut and named
focused regressions are present in the current working tree. The affected six-
package library suite passes 657 tests with 31 explicitly ignored cases. Four
shared-traversal cases, the hidden exact-handoff case, and the direct temporal
pixel-replacement case pass when invoked directly on the designated RTX/Vulkan
adapter. The repaired fixed-LOD matrix also passes its local `1.20` guard with
a worst homogeneous linear normalized ratio of `1.0181`, zero uploads during
samples, and zero validation errors. That matrix is not the P7 cross-revision
`1.05` gate.

Normal real-display automation passes all 141 `target_fixture_render_modes`
commands and all 60 `representative_native_navigation` commands. The attempted
`representative_temporal_playback` run reached an Exact/current timepoint-1
presentation, then correctly refused its second capture because the bundled
fixture produced no intermediate RGB values under its retained timepoint-0
window. That workload therefore cannot establish the required temporal pixel
evidence; the temporal exercise remains unevaluated rather than being weakened
or counted as a renderer failure.

The required P0 A/B EvidenceOverlay and baseline campaign were not captured
before the production edits. That ordering violation is recorded rather than
retroactively relabelled as a P0 pass. The immutable A/B/C campaign
orchestrator, raw validator, threshold evaluator, and sanitized publisher are
implemented, but the private campaign has not run. The full trusted-GPU lane
also cannot produce qualifying evidence from this dirty implementation tree;
its clean-revision guard rejects the run as designed. P7/R11 performance
evidence, a suitable temporal product workload, clean-revision trusted-lane
evidence, and owner mapped acceptance therefore remain open. This plan is not
complete, and no performance-recovery or owner product-validation claim
follows yet.

The plan corrects gaps in the completed rendering handoffs without discarding
their valid architecture or historical evidence:

- the complete-logical-frame and retained-front design from
  [Viewer Composed Presentation Scheduler Cutover](VIEWER_COMPOSED_PRESENTATION_SCHEDULER_CUTOVER.md)
  remains, but its current transaction-level reuse proof is incomplete;
- the single `FrameCoordinator`, single `ResidencyOwner`, dedicated color
  kernels, and first-cause device latch from
  [Viewer Rendering Pipeline Overhaul](VIEWER_RENDERING_PIPELINE_OVERHAUL.md)
  remain;
- renderer-owned asynchronous exact refinement from
  [Viewer GPU Memory And Asynchronous Refinement](VIEWER_GPU_MEMORY_AND_ASYNC_REFINEMENT.md)
  remains, but its failures become typed and schedulable;
- adaptive placeability and retained-front recovery from
  [Viewer GPU Placeability And Recovery](VIEWER_GPU_PLACEABILITY_AND_RECOVERY.md)
  remain, but transient outcomes stop entering the product failure projection;
  and
- this plan takes ownership of the explicitly deferred static performance
  recovery from
  [Viewer Multichannel Performance And Visible-Layer Authority](VIEWER_MULTICHANNEL_PERFORMANCE_AND_VISIBLE_LAYER_AUTHORITY.md).

Historical implementation and product-validation claims in those plans remain
attached to their named revisions and workloads. They do not establish the
new edge cases or target state defined here.

## Decision

Mirante4D will make one hard corrective cut with four coordinated outcomes:

1. every composed Exact transaction hands the renderer a complete typed
   logical target set whose members bind the exact semantic body and quality
   contract;
2. one application-side render-attempt coordinator owns execution eligibility,
   retry state, failure latching, and repaint scheduling, using typed renderer
   progress facts rather than independent booleans;
3. one render-API numerical contract validates the actual binary32 controls,
   coordinate range, and ray-sample range used by every volume path; and
4. static performance is measured and corrected only after those correctness
   cuts, through a deterministic attribution protocol that permits exactly one
   evidence-selected optimization branch at a time.

The prior complete front remains visible throughout every recoverable wait or
failed successor. No change may create a CPU renderer, a fallback backend, a
second residency map, a second presentation coordinator, reduced-resolution
3D output, silent LOD substitution, silent coordinate clamping, or a device-
recovery path.

Path coverage is explicit:

| Rendering path | P1 complete-set/reuse | P2/P3 state and failure | P5 numerical envelope | P6 traversal |
| --- | --- | --- | --- | --- |
| Composed Exact 3D/four-panel transaction | Required | Required | Required | Volume members |
| Standalone interactive 3D preview | Not transaction-reusable | Required | Required | Required |
| Provisional Plane navigation floor | Cannot satisfy an Exact member | Required | Required | Not applicable |
| Hidden Exact refinement | Publication remains renderer-atomic | Required | Uses parent 3D envelope | Required |
| Pick | No color presentation | Capability/ticket contract | Uses exact presented 3D envelope | Required |
| Validation capture/GPU timing | No color presentation | Auxiliary-only terminality/wake | Observes named presentation | Not applicable |

P1 therefore changes composed transaction authority, not preview scientific
semantics. P3 routes both transaction and nontransaction color paths through
the one attempt coordinator; no preview/floor path may retain the old generic
error or independent repaint predicate.

## Audited Failures And Required Closure

The following identifiers are stable within this plan. A milestone cannot be
closed while its corresponding row remains reproducible.

| ID | Severity | Current failure | Required closure |
| --- | --- | --- | --- |
| R1 | High | Transaction reuse checks frame/time/extent/exactness/scale but not immutable body, prefetch role, or renderer lineage. | P1 |
| R2 | High | A deterministic execution latch blocks GPU execution while the UI still sees renderer work and requests immediate repaint forever. | P3 |
| R3 | High | Several retryable backpressure outcomes enter the generic error projection and relabel the retained display `Incomplete`. | P3 |
| R4 | High | Hidden refinement collapses timeout, device failure, and worker panic into one cause-free permanent failure. | P2 |
| R5 | High | Pick-pipeline failure poisons already-ready color capability; pending pick tickets can remain live indefinitely. | P4 |
| R6 | High | Ray intersection and page exit disagree for direction magnitudes between `1e-7` and `1e-6`, so volume and pick kernels can retain the wrong page. | P6 |
| R7 | Medium | Absolute determinant thresholds reject well-conditioned transforms solely because of world units. | P5 |
| R8 | Medium | Volume metadata accepts grid ends and sample indices beyond binary32's exact half-step range, so boundary values or adjacent sample centers can alias silently. | P5 |
| R9 | Medium | Orthographic planning reconstructs a validated camera basis from `target - eye` and rejects small or cancellation-prone positive distances. | P5 |
| R10 | Medium | A terminal GPU failure can leave raw residency or capture state owning a permanent 50 ms background wake. | P2/P3 |
| R11 | Evidence gap | Static normal-product rendering is owner-reported materially slower, but no controlled baseline/current attribution exists. The existing fixed-LOD matrix also prints its 1.20 comparison with inverted `gate_met` polarity and does not enforce that declared gate. | P0/P7 |
| R12 | High | Hidden-worker spawn/panic/identity failure is not represented as permanent hidden-only capability loss, so independent direct/Plane color can be failed or later fingerprints can retry a dead worker. | P2 |
| R13 | Medium | Private-presentation and texture-revision exhaustion have no permanent operation-scoped preflight fact, so later fingerprints can re-enter a checked allocator that cannot recover or over-disable unrelated work. | P3 |

R11 deliberately remains an evidence gap rather than a guessed code defect.
P7 cannot select an optimization until its attribution gate identifies a
specific first blocking boundary.

## Product Outcome

After completion:

- a same-frame successor body, changed prefetch role, changed source, changed
  renderer device, replaced target allocation, or stale texture/front can
  never satisfy a composed transaction through reuse;
- an unchanged exact predecessor can still be reused in O(1), and a four-panel
  transaction may still submit zero, one, two, three, or four physical color
  targets without losing its complete logical identity;
- retryable pipeline, residency, placement, eviction, and queue pressure keeps
  the predecessor visible, does not create a product error, and wakes exactly
  from the event capable of making progress;
- deterministic request failure executes once per exact fingerprint and then
  becomes quiescent until a fingerprint component changes;
- terminal GPU failure is projected once, retires non-executable renderer
  background work, and creates no fallback or recovery loop;
- hidden-refinement timeout, worker failure, cancellation, and device failure
  are different typed outcomes with different retry and cleanup behavior;
- spawn/panic/identity failure of the hidden worker disables only hidden
  refinement; it neither kills direct/Plane color nor silently substitutes an
  over-budget direct pass;
- a Pick-only validation or worker failure disables picking without disabling
  an already-ready color capability, while device-level causes still terminate
  the whole renderer;
- every submitted pick ticket reaches one terminal application result;
- uniformly tiny or large but well-conditioned affines render when their
  quantized controls satisfy the actual shader error envelope;
- singular, ill-conditioned, nonrepresentable, or insufficiently precise
  controls fail before GPU work with one typed explanation;
- orthographic basis orientation comes from the canonical camera axes rather
  than a subtract-and-renormalize reconstruction;
- every admitted normal nonzero binary32 grid-direction component participates
  consistently in intersection and page traversal, while a subnormal is
  canonical stationary only after a viewport-wide irrelevance proof;
- coordinates or sample counts outside the exact half-step binary32 envelope
  fail visibly instead of rendering scientifically ambiguous pixels; and
- the reported static slowdown is either corrected to the declared threshold
  or remains explicitly unclosed with measurements naming the first boundary.

## Non-Negotiable Invariants

### Scientific And Presentation Truth

- Valid zero, invalid/no-data, missing residency, and outside-volume remain
  distinct.
- MIP raw maximum, DVR physical-distance opacity, ISO first threshold and
  inverse-transpose gradient, Mixed authored-order composition, Plane
  whole-footprint fallback, and the three pick policies do not change except
  where P6 corrects page traversal shared by all volume paths.
- Voxel-exact and SmoothLinear remain supported. No milestone may silently
  change sampling, transfer functions, channel order, projection, or scale.
- A composed temporal or retained-quality transaction is Exact-only. A Plane
  navigation-floor presentation may remain provisionally current outside such
  a transaction, but it cannot be reused as an Exact transaction member.
- The prior complete front remains the displayed and pick authority until one
  matching successor publishes. Attempt status must never rewrite the
  fidelity facts of those already presented pixels.
- Four-panel temporal publication remains exactly 3D, XY, XZ, and YZ at one
  source generation, timepoint, fixed playback scale map, and atomic cutoff.

### One Authority Per Decision

- `ComposedPresentationScheduler` remains the sole semantic transaction
  authority.
- `FrameCoordinator` remains the sole GPU front, candidate, texture,
  recording-order, submission, completion, and swap authority.
- `ResidencyOwner` remains the sole physical-residency, pin, page, directory,
  allocator, eviction, and compaction authority.
- The application render-attempt coordinator introduced in P3 is the sole
  owner of attempt fingerprint, wait reason, deterministic failure latch, and
  repaint decision. UI and playback helpers may query its decision but may not
  reconstruct it from renderer booleans.
- The render API owns one canonical binary32 affine/coordinate admissibility
  result. Application planning and WGPU control construction consume that
  result and may not perform independent determinant or precision policy.
- The global GPU failure latch remains the sole device-loss/OOM/backend-
  internal/backend-validation terminal authority.

### Boundedness And Event-Driven Progress

- Complete logical target assembly uses fixed one-target or four-target
  storage. It cannot allocate a variable unbounded target collection.
- Immutable requirement bodies are shared by their existing O(1) allocation
  identity. No transaction, fingerprint, or reuse check may hash, clone, sort,
  or compare up to 65,536 `BrickKey` values.
- A retry must name the event that can change its result. An unchanged timer
  is not a retry event.
- Immediate repaint is legal only when one renderer execution can make
  progress now or when newly published pixels require one composition turn.
- Timed repaint is legal only for an external operation that has no wake
  callback. Each such use must name its interval and termination condition.
- No failure path may add an unbounded queue, unbounded retry counter, or
  repeated GPU submission for the same unchanged failed fingerprint.

### Failure Honesty

- Retryable wait is not success and not failure.
- A deterministic successor failure does not make predecessor pixels
  incomplete. It makes the requested successor failed and non-current.
- Only failure of every valid minimum navigation candidate is a hard adaptive
  capacity failure. Intermediate placement or refinement refusal remains a
  retained-front wait/replan.
- Auxiliary capture, timing, and pick failures do not mutate color fidelity.
- Device-level terminal causes stop later unsafe WGPU work and are projected
  once. Cleanup remains available; recovery and fallback remain absent.

### Numerical Honesty

- Durable `GridToWorld` continues to preserve any finite affine. Rendering is
  the boundary that requires invertibility and binary32 admissibility.
- No absolute determinant magnitude decides invertibility.
- No shader coordinate or sample count is silently clamped to make it fit.
- A transform is accepted only from the controls that will actually be sent
  to the shader, not from an unquantized f64 predecessor alone.
- CPU semantic planning, GPU controls, independent reference expectations, and
  picks use the same accepted coordinate convention.

## Target Presentation Authority

### Complete Logical Members

The application introduces one fixed-shape semantic model and projects it into
one fixed-shape renderer request. The split is required because application
source/surface generations must not become renderer-owned types.

```text
PresentationTransactionMember {       // mirante4d-app
  target,
  source_generation,
  timepoint,
  spatial_frame,
  surface_generation,
  quality,
  prepared_request
}

PresentationTransactionTargets =      // mirante4d-app
  ThreeD { three_d }
  | FourPanel { three_d, xy, xz, yz }

CoordinatedLogicalTargetRequest {      // mirante4d-render-wgpu boundary
  target,
  frame,
  timepoint,
  output_extent,
  required_completeness,
  layer_scale_map,
  immutable_render_intent,
  immutable_requirement_body,
  prefetch_role,
  render_schedule
}

CoordinatedLogicalTargetSet =          // mirante4d-render-wgpu boundary
  ThreeD { three_d }
  | FourPanel { three_d, xy, xz, yz }

TargetRenderSchedule =
  VolumeDirect
  | VolumeAtomicRefinement
  | PlaneExact
```

The application member fields mean exactly:

- `target`: one existing `PresentationTarget`;
- `source_generation`: the current application source session;
- `timepoint`: the semantic frame timepoint;
- `spatial_frame`: 3D mailbox identity for 3D or linked mailbox identity for
  XY/XZ/YZ;
- `surface_generation`: the current app-owned surface generation;
- `quality`: required Exact completeness plus the ordered per-layer scale map;
  and
- `prepared_request`: the immutable render intent/body and target-specific
  schedule projected below.

The renderer request fields mean exactly:

- `target`, `frame`, and `timepoint`: the semantic target identity already
  validated against the application transaction;
- `output_extent`: the exact physical renderer extent;
- `required_completeness`: Exact for every transaction in this plan;
- `layer_scale_map`: exact ordered visible-layer-to-scale mapping, not a
  scalar summary;
- `immutable_render_intent`: the existing immutable target controls;
- `immutable_requirement_body`: the existing shared `RenderRequirements`
  allocation produced for this target;
- `prefetch_role`: the existing promoted/unpromoted body role that affects
  residency semantics; and
- `render_schedule`: `VolumeDirect` or `VolumeAtomicRefinement` for 3D and
  `PlaneExact` for XY/XZ/YZ. `InteractivePreview` is not eligible to satisfy an
  Exact transaction member.

`immutable_render_intent` is not compared by allocation identity. The existing
mailbox invariant remains authoritative: one `FrameIdentity` is the opaque
`RenderIntentRevision`, and any source/time/view/presentation/layer/transfer
change allocates the appropriate family revision before planning. Receiving
different render-intent semantics under one frame is `FrameContractMismatch`.
Requirement bodies are deliberately separate because residency/prefetch
planning may replace a body under the same semantic frame; that is why body
allocation identity and role appear explicitly in reuse and fingerprint
checks.

The transaction cannot enter renderer validation until every member exists.
An unavailable member means `WaitingForLogicalMember`; it does not produce a
shorter vector, a partial transaction, or a renderer call.

Immediately before projection, the application validates source generation,
layout, timepoint, spatial frame, surface generation, and quality against the
current transaction. The renderer set then borrows the existing immutable
intent and body allocations; it does not copy them. `mirante4d-render-wgpu`
does not import application source or surface types.

After the renderer returns and before semantic completion, the application
rechecks source generation, layout, mailbox identities, surface generations,
and requested quality against the same fingerprint. A change discards only
the stale semantic completion and recomputes the latest set; it never applies
an old report to a new transaction.

### Renderer-Owned Reuse Classification

The application no longer decides `prepared` versus `reused`. It hands the
complete `CoordinatedLogicalTargetSet` to one renderer operation. Under one
`FrameCoordinator` observation, the renderer classifies every member as:

```text
MemberDisposition =
  Reused(PresentedMemberFacts)
  | RequiresExecution
```

`Reused` is legal only when the current renderer front simultaneously proves:

1. the same fixed target;
2. the same current renderer device generation;
3. the target slot's current private front allocation and target texture
   revision remain registered throughout the observation/report;
4. the same frame identity and timepoint;
5. the same output extent;
6. the same exact immutable requirement-body allocation;
7. the same prefetch role;
8. the same target-specific render schedule/Exact promotion state;
9. Exact progress with the same ordered layer-scale map;
10. current required-residency freshness; and
11. no pending body, target-texture, or hidden-candidate replacement that
    invalidates those pixels.

Items 10 and 11 use existing renderer facts, not a new generation guess.
`rendered_residency_freshness` must be `Current`; the front must have no
relevant pending offer or full-coverage dirty-color work; its displayed extent
must equal the slot's current desired extent; and the target's current texture
registration must still name that front's `TargetTextureRevision`. Merely
having a private hidden candidate does **not** invalidate a matching front.
The candidate matters only when clause 8 says the transaction requires an
Exact promotion the front has not published. This preserves the current
preview-adoption ordering and avoids making front and candidate wait on each
other.

Source and surface generation are checked by the application immediately
before the renderer call. Body identity and renderer lineage are checked only
inside the renderer; the application does not mirror private presentation
tokens or residency facts.

The renderer derives the physical delta from `RequiresExecution` members:

- zero members: validate all fronts, return a zero-submission current report,
  and permit semantic transaction completion;
- one to three members: derive one exact atomic publication group containing
  precisely those changed targets; or
- all members: execute the complete group.

For a nonempty delta, preflight succeeds for every changed member before the
first color pass is recorded. The changed group keeps the existing atomic
publication rule: if any member fails before cutoff, none of its changed
members publish; reused fronts are neither rerecorded nor withdrawn.

The all-reused path is therefore GPU-idle but not validation-free. A stale
front discovered during renderer validation returns a typed stale/wait
outcome; the application cannot complete the transaction from an earlier
reuse observation.

The execution report covers every logical member and explicitly labels reused
and executed targets. `recorded_targets` remains the physical color order and
may be empty. Semantic completion requires a current report for every logical
member, never merely equality between a request vector and `recorded_targets`.

`PresentedMemberFacts` returns target, frame, timepoint, extent, completeness,
ordered layer-scale map, requirement-body identity, prefetch role, render
schedule, private front identity, texture revision, device generation, and
residency freshness from that same locked observation. The application uses
the report to complete the transaction; it never stores private identities as
a second reuse cache. The report's device generation must equal the attempt
fingerprint captured for the call; otherwise the report is stale and cannot
complete the transaction.

### Transaction Fingerprint

One `RenderAttemptFingerprint` scopes waiting and deterministic failure. It
contains:

- source generation and layout;
- the fixed logical target set;
- for every member: target, frame, timepoint, surface generation, extent,
  exact layer-scale map, immutable requirement-body allocation identity,
  prefetch role, and target render schedule;
- dataset runtime epoch;
- renderer capacity epoch; and
- renderer device generation.

Changing any field clears a deterministic request latch. Equality of an
immutable body is O(1) shared-allocation identity plus prefetch role, matching
the renderer's existing `shares_resources_with` contract. There is no body
hash, body-generation allocator, or structural key walk.

A latch cannot be cleared by cloning/reboxing an unchanged prepared body or by
incrementing an epoch on a poll. Body identity changes only when the canonical
planner publishes a genuinely new immutable body allocation. Capacity epoch
changes only after committed logical/physical capacity, compaction state, or
acknowledged eviction state changes in a way that can alter the failed
preflight. These rules prevent identity churn from becoming a retry clock.

Recording a permanent pipeline, hidden, or allocator capability failure does
not advance capacity epoch. Its first observation is classified once;
subsequent preflight reads the capability fact. Otherwise the failure itself
would change the fingerprint and manufacture one redundant retry.

CPU lease arrival is not a fingerprint component. Missing relevant residency
is a `Waiting` state whose event is keyed to the exact target/body through the
renderer's existing relevant-offer index. An unrelated scope lease neither
clears a deterministic latch nor wakes color work.

## Authority And File Change Map

The implementation boundary is fixed as follows. Moving an authority to a
different crate requires an owner-approved amendment.

| Location | Required responsibility after cutover | Required removal |
| --- | --- | --- |
| `crates/mirante4d-app/src/presentation_scheduler.rs` | Own `PresentationTransactionMember/Targets`, semantic cutoff, and complete-member readiness. | Prepared/reused semantic classification without renderer proof. |
| `crates/mirante4d-app/src/display_refresh.rs` | Project complete renderer sets, own `RenderAttemptState`, exhaustively map outcomes, and apply full reports. | `transaction_target_is_compatibly_reusable`, negative-list determinism, and generic failure-to-fidelity mutation. |
| `crates/mirante4d-app/src/workbench_playback_runtime.rs` | Consume the coordinator's typed wake decision only. | Direct target-work queries and raw renderer background ORs. |
| `crates/mirante4d-app/src/workbench_ui.rs` | Schedule only named immediate/event/timed wakes and project separate attempt status. | Unqualified immediate repaint for renderer-owned dirty work. |
| `crates/mirante4d-app/src/render_state.rs` | Map typed terminal/request failures to UI vocabulary without changing presented fidelity. | Generic `AllocationFailed` mapping for cause-free hidden failure. |
| `crates/mirante4d-app/src/viewer_pick_runtime.rs` | Give every accepted pick one terminal result and one bounded map-poll wake. | Pending-ticket survival after capability/device failure. |
| `crates/mirante4d-app/src/native_presentation.rs` | Project independent InitialRender/Pick capability and renderer-terminal facts. | Aggregate pipeline error that hides ready color. |
| `crates/mirante4d-render-api/src/lib.rs` | Own `ValidatedShaderAffine`, typed numerical rejection, exact half-step constants, and Plane/volume work-envelope contracts. | Absolute determinant policy and row-only success result. |
| `crates/mirante4d-app/src/semantic_demand.rs` | Consume canonical affine and Plane/volume envelope facts for demand, coverage expansion, and view precision. | Independent render-affine condition/rounding policy and orthographic basis reconstruction. |
| `crates/mirante4d-app/src/volume_presentation.rs` | Consume validated quantized inverse/corners and ray-work envelope. | Second determinant threshold and local quantized-row inversion. |
| `crates/mirante4d-render-wgpu/src/lib.rs` | Expose fixed complete logical renderer sets, full-member reports, capability-local state, typed hidden/pick outcomes, and the backend-neutral keyed event sink. | Variable physical delta as the public logical frame and cause-free hidden error. |
| `crates/mirante4d-render-wgpu/src/runtime.rs` | Classify reuse under one `FrameCoordinator` observation; own typed progress, keyed event publication, terminal cleanup, pipeline capability, and pick retirement. | Global pipeline `first_failure`, permanently actionable failed hidden state, and unsafe raw terminal pending work. |
| `crates/mirante4d-render-wgpu/src/volume_common.wgsl` | Own the only volume direction classification, ray intersection, page exit, and monotone segment boundary. | Absolute direction/progress epsilon split. |
| `crates/mirante4d-render-wgpu/src/{mip,dvr,iso,mixed,pick}*.wgsl` | Import shared traversal without redefining it. | Any private page-exit rule or threshold. |
| `crates/mirante4d-render-reference` | Supply independent expected affine, traversal, color, coverage, validity, and pick facts. | Mirroring production threshold logic as its oracle. |
| `crates/xtask` product validation | Own bounded static-performance orchestration under the existing opt-in product-validation mode. | Benchmark-only product routes or unbounded host-stress behavior. |

## Target Attempt, Failure, And Wake Authority

### One Attempt State

The application render coordinator owns exactly one state:

```text
RenderAttemptState =
  Idle
  | Ready { fingerprint }
  | Waiting { fingerprint, reason, wake: WaitingWake }
  | Failed { fingerprint, failure }
  | ColorUnavailable { failure }
  | RendererTerminal { first_gpu_cause }

ColorUnavailableFailure =
  Startup(RendererStartupFailure)
  | InitialRenderCapability(PipelineCompilationFailureCause)

RenderWake =
  Immediate
  | Waiting(WaitingWake)
  | None

WaitingWake =
  Event(RenderWakeReason)
  | After { reason, interval }
```

State meanings are strict:

- `Idle`: no requested renderer work;
- `Ready`: one execution can make progress immediately;
- `Waiting`: no execution can make progress until its named event;
- `Failed`: unchanged execution would deterministically produce the same
  request-scoped failure;
- `ColorUnavailable`: this runtime cannot create later color work, but an
  already-safe front and auxiliary capability are not declared device-lost;
  and
- `RendererTerminal`: the global GPU authority forbids all later WGPU work.

Only `Ready` produces immediate color repaint. `Waiting/Event`, `Failed`,
`ColorUnavailable`, and `RendererTerminal` do not. The pipeline compiler gains
a backend-neutral keyed wake sink on each ordered event and therefore does not
retain its current 50 ms readiness polling.

`mirante4d-render-wgpu` does not import egui. The app supplies a `Send + Sync`
`RendererEventSink` whose only operation is `wake()`. Before calling it, a
producer commits its reason, exact device/job/submission/revision key, and
latest outcome to the producer's existing fixed-capacity or latest-only
authoritative state. The sink never owns a key queue. It uses one atomic
`ui_turn_pending` flag: a `false -> true` transition asks the UI event loop for
one turn and every later notification coalesces into that turn. The UI handler
clears the flag before acquiring and rereading producer state; a producer that
commits after that clear performs another transition, while a commit before
the clear is visible to the current reread. The implementation uses
release/acquire ordering and the event-proxy send occurs only after the release
transition.

On that turn, the app coordinator compares its current wait key with the
authoritative producer fact before choosing execution, composition, or no
work. A callback is notification, never repaint authority. The sink holds a
weak app event proxy behind one atomic `closed` flag; runtime/app drop sets
`closed` and clears the proxy before worker teardown. `wake()` checks `closed`
before and after winning the pending transition, so a racing or late callback
either schedules the already-safe UI turn or becomes a no-op without retaining
the UI or touching destroyed state. Failed proxy delivery clears
`ui_turn_pending`; it does not retry or execute renderer work. This is bounded
to one pending UI notification and does not create a second progress queue.

The projection is total: `Idle`, `Failed`, `ColorUnavailable`, and
`RendererTerminal` return `RenderWake::None`; `Ready` returns
`RenderWake::Immediate`; `Waiting` contains exactly one `WaitingWake::Event`
or permitted `WaitingWake::After`. `Waiting(Immediate)` and `Ready(None)` are
unrepresentable by type.

Newly published pixels use a separate one-shot composition fact owned by the
same coordinator:

```text
PendingComposition = { target, texture_revision }
```

A successful full report inserts at most one entry per fixed target when that
revision has not yet been handed to native presentation. The next UI turn may
request immediate **composition**, never another color execution, and clears
the entry only after texture registration/paint-command construction
acknowledges the same revision. Re-observing the same report cannot insert it
again. The fixed target set bounds this to four entries. Thus `Ready` is the
only immediate color decision while a genuinely new texture still gets
exactly one paint opportunity.

Renderer-related `After` is permitted only for bounded asynchronous mapping
that requires device polling:

- pending Pick map: 8 ms;
- pending validation-capture map: 50 ms; and
- pending optional GPU-timing map: 50 ms.

Those wakes poll only their named auxiliary operation; they cannot request a
color refresh. They terminate immediately when the bounded pending set becomes
empty or the renderer becomes terminal. "Bounded" here means a fixed maximum
number of live map operations, not an assumed wall-clock completion deadline.
Each callback, including a mapping error, retires its operation; device failure
retires all of them. All other renderer waits require an event callback.
Non-renderer application-service timers remain outside this plan.

The event vocabulary and key are fixed:

| Waiting reason | Event key | Sole wake and recomputation |
| --- | --- | --- |
| `WaitingForLogicalMember` | transaction identity plus missing target | `LogicalMemberPublished { family_revision, target }`; rebuild the complete one/four-member set. |
| `WaitingForMailboxAdvance` | target plus renderer-reported minimum current frame | `LogicalMemberPublished { family_revision, target }` at or above that frame; rebuild rather than resubmit the stale request. |
| `WaitingForCandidatePlan` | semantic-demand signature | `CandidatePlanPublished { demand_revision }`; consume only a matching latest result. |
| `WaitingForInitialPipeline` | renderer device generation | `PipelineCapabilityChanged { InitialRender, device_generation }`; re-read capability state once. |
| `WaitingForSubmissionCleanup` | exact WGPU submission identity | `SubmissionCompleted { submission }`; perform the stored cleanup/retry transition once. |
| `WaitingForEvictionAcknowledgement` | renderer eviction-ledger revision | `EvictionAcknowledged { ledger_revision }`; retry only if the acknowledged revision advanced. |
| `WaitingForRelevantResidency` | target plus immutable body identity/role | `RelevantResidencyChanged { target, body }`; re-preflight only that current complete set. |
| `WaitingForHiddenWorker` | hidden job identity | `HiddenWorkerResult { job }`; consume only that job's latest typed result. |

If more than one prerequisite is absent, the first applicable row in this
table owns the one registered wake. Its event causes a full coordinator
recomputation; if another prerequisite is still absent, the coordinator moves
to that row and registers its wake. Stale or differently keyed events can
schedule ordinary event delivery but cannot execute color or clear a failure
latch. Capacity-epoch, device-generation, source, layout, surface, and user
input changes already cause their own application turn; they change the
fingerprint rather than becoming a retry timer owned by a failed attempt.

The UI no longer calls `coordinated_target_requires_execution` directly and
does not OR raw pipeline, residency, eviction, capture, or hidden-refinement
facts into repaint policy. `progressive_render_submission_work` is deleted or
reduced to a direct projection of `RenderAttemptState` with no renderer query
of its own.

Every transaction or preview color call is normalized exactly once at the app
boundary:

```text
ColorAttemptObservation =
  Current(full_report)
  | Published(full_or_preview_report)
  | Waiting(RendererWaitFact)
  | Failed(WgpuRenderRuntimeError)
```

`Waiting` absorbs both existing retryable `Err` variants and non-error progress
facts. In particular, an `Ok` report with `deferred_by_backpressure`, an Exact
report lacking required residency, a running hidden job, and a pipeline-not-
ready error cannot fall through the success or generic-error branches. Each is
converted to the keyed waiting vocabulary above. A preview report that really
published valid progressive pixels is `Published`, not `Waiting`; its
`FrameFidelity` remains progressive while its next Exact/refinement attempt has
its own state. `Current` and `Published` are the only observations allowed to
apply a report. This normalization function and the runtime-error match are
private, exhaustive, and contain no boolean/default catch-all.

### Presented Fidelity Versus Requested Attempt

`FrameFidelity` continues to describe the pixels currently displayed.
Renderer attempt status moves to a separate `RenderAttemptStatus` projection:

```text
RenderAttemptStatus =
  Settled
  | Waiting(RenderWaitStatus)
  | Failed(RenderFailureStatus)
  | ColorUnavailable(RenderFailureStatus)
  | RendererTerminal(RenderFailureStatus)
```

The generic error path must not set displayed completeness to `Incomplete`.
A retryable wait produces a neutral loading/refining/capacity-wait status and
no error banner. A deterministic failure produces a visible failure bound to
the failed successor fingerprint while retaining predecessor fidelity. A
successful matching report clears that attempt status. A different fingerprint
replaces it rather than allowing a stale error to survive.

The existing `last_capacity_error` field may remain only for true hard
capacity failure if its name and ownership are narrowed accordingly. It may
not remain the catch-all destination for every renderer error. Dataset plan
errors and per-panel failures remain separately scoped.

### Exhaustive Runtime Outcome Classification

Classification is one exhaustive `match`; no wildcard arm is allowed. Adding
a `WgpuRenderRuntimeError` variant must fail compilation until its disposition
is selected.

| Disposition | Baseline/target variants or causes | Front/UI effect | Next execution |
| --- | --- | --- | --- |
| Startup color unavailable | `InvalidConfiguration`, `SoftwareAdapter`, `UnsupportedBackend`, `AdapterLimitsInsufficient`, `DeviceLimitsInsufficient`, `PipelineCompilerSpawnFailed`, `RendererDeviceGenerationExhausted` | Enter `ColorUnavailable(Startup(...))`; no renderer-ready presentation and one typed startup failure. | None without an explicitly constructed new renderer; this plan adds no reconstruction path. |
| Device terminal | `DeviceLost`, `DeviceOutOfMemory`, `BackendInternal`, `BackendValidation` | Enter `RendererTerminal`; project the first cause once and stop unsafe GPU work. | None; no recovery/fallback. |
| InitialRender-local pipeline failure | `PipelineCompilationFailed` for `InitialRender`, with cause `Validation`, `WorkerPanicked`, or `WorkerStopped` | Enter `ColorUnavailable(InitialRenderCapability(...))`; there is no color capability and Pick compilation does not begin. | None in this runtime. |
| Pick-local pipeline failure | `PipelineCompilationFailed` for `Pick`, with cause `Validation`, `WorkerPanicked`, or `WorkerStopped` | Fail Pick and retire its tickets; an already-ready InitialRender capability and safe front remain usable. | None for Pick in this runtime. |
| Device-terminal pipeline failure | `PipelineCompilationFailed` with `DeviceOutOfMemory` or `BackendInternal` | Promote the cause into the global GPU latch. | None. |
| Capability event wait | `PipelineNotReady` | InitialRender keeps the color attempt in neutral `Waiting`; Pick leaves color state unchanged and keeps only its bounded latest request waiting. | Exact named-capability compiler event/wake. |
| Stale frame suppression | `StaleFrame` | No user error and no fidelity mutation; discard the stale fingerprint. | Recompute once from the already-current mailbox. Execute only a different current fingerprint that is `Ready`; if the mailbox still exposes the stale frame, enter `WaitingForMailboxAdvance` keyed to the renderer-reported current frame. |
| Stale capture suppression | `StaleValidationCapture` | Retire that auxiliary capture with no color-fidelity mutation. | A separately requested current capture only. |
| Eviction backpressure | `ResidencyEvictionEventCapacityExceeded` | Preserve front; neutral status. | One retry after the app drains and acknowledges at least one relevant event. |
| Placement recovery | `PayloadPlacementUnavailable` | Run the existing single compaction attempt; otherwise invoke monotone adaptive replan. No generic error for an intermediate candidate. | Compaction completion, capacity epoch change, or newly selected smaller union. |
| In-flight recovery wait | `PayloadRecoveryDeferred` | Preserve front; neutral status. | Exact in-flight submission-completion wake. |
| Deterministic request failure | `FrameContractMismatch`, target `ShaderWorkEnvelopeMismatch`, `ExtentExceeded`, `RequirementSetChanged`, `InvalidResourceGridCatalog`, `RequirementCapacityExceeded`, `PresentationCapacityExceeded`, `PresentationNotRegistered`, `DuplicateCoordinatedTarget`, `CoordinatedTargetNotConfigured` during execution, `CoordinatedTargetViewMismatch`, `InvalidVolumeColorSchedule`, `InvalidCoordinatedPublicationGroup`, `CoordinatedTargetExtentMismatch`, `LeaseCapacityExceeded`, `DuplicateLease`, `UnexpectedLease`, `PayloadContractMismatch`, `UnsupportedView`, `CoordinateLimitExceeded`, `ControlCapacityExceeded`, `CapacityExceeded`, `ResidentMetadataCapacityExceeded`, `FrameProgressContract` | Retain predecessor; visible typed successor failure; latch exact fingerprint. Adaptive capacity becomes hard failure only after the minimum-candidate policy says no valid candidate remains. | Only fingerprint/capacity epoch change. |
| Hidden-refinement capability failure | Baseline `HiddenRefinementWorkerSpawnFailed`, `HiddenRefinementIdentityExhausted`, or target worker-panic cause | Permanently fail only hidden refinement; retain the safe front and do not claim device or direct-color failure. | A later request may use an independently valid `VolumeDirect`; `VolumeAtomicRefinement` preflights directly to the typed capability failure without calling a missing/dead worker. |
| Private-target identity exhaustion | `PrivatePresentationIdExhausted` | Permanently fail only allocation of another private presentation; existing fronts/candidates remain valid. | A request using existing allocations may execute; one requiring a new front/candidate preflights to the typed failure. |
| Texture-revision identity exhaustion | `TextureRevisionExhausted` | Permanently fail only creation/replacement of a target texture revision; retain every already-registered texture/front. | Same-texture work may execute; work requiring allocation or extent replacement preflights to the typed failure. |
| Auxiliary validation/timing failure | `UnknownValidationCapture`, `ValidationCaptureFailed`, `UnknownGpuTiming`, `GpuTimingFailed` | Fail the requesting diagnostic or automation operation only; do not alter color fidelity. | A separately requested new auxiliary operation. |
| Pick staleness/request mismatch | `PickQueryMismatch`, `PickFrameUnavailable` | Complete the exact pick as `Stale`; never alter color fidelity. | A newer user request only. |
| Pick wait | `PickCapacityExceeded`, `PickBackpressure` | Keep only the latest current request queued; neutral status. | Slot/submission/map completion wake. |
| Pick execution failure | `UnknownVolumePick`, `VolumePickFailed` | Complete the exact pick as `ExecutionFailed`; never alter color fidelity. | A separately accepted newer pick. |
| Pick capability exhaustion | `PickTicketExhausted` | Fail Pick capability for this runtime; preserve color. | None for Pick. |

`CoordinatedTargetNotConfigured` remains a normal `false` result when a
read-only work query asks about an intentionally inactive target. It is a
contract failure only if that target appears in an executing logical set.

Capacity and shader admission are stage-sensitive without being
classification-sensitive:

```text
CandidateFeasibility =
  Feasible(validated_request)
  | Rejected(CandidateLimitReason)
```

During finite catalog selection, `RequirementCapacityExceeded`,
`LeaseCapacityExceeded`, `ControlCapacityExceeded`, `CapacityExceeded`,
`ResidentMetadataCapacityExceeded`, and `ShaderAdmissionError` are converted
to `Rejected` when choosing a strictly smaller valid scale map can change the
fact. The existing monotone selector consumes that rejection without a red
status and never sends the rejected candidate to WGPU. Only failure of every
valid minimum candidate becomes the table's visible deterministic successor
failure. Limits independent of scale/layout are immediately hard.

`PayloadPlacementUnavailable` remains separate because it is learned from the
sole physical allocator after logical selection; its table row owns one
compaction and the strictly smaller residual-bound replan. If an executing
request returns a capacity/admission error that its successful preflight was
required to exclude, it is a deterministic contract failure for that exact
fingerprint and is logged as preflight/execution disagreement. It is not put
into a timer loop. Thus the variant name never decides whether an intermediate
candidate is red; the authoritative planning/execution stage does.

`Failed` is reserved for fingerprint-scoped work. Startup and InitialRender
capability failure never enter `Failed`, because changing a request fingerprint
cannot make those conditions executable. Conversely, Pick-only failure never
enters the color attempt state. It terminates Pick work through the
capability/ticket state described below.

Hidden refinement owns one permanent capability fact:

```text
HiddenRefinementCapabilityState =
  Ready
  | Failed(WorkerSpawnFailed | JobIdentityExhausted | WorkerPanicked)
```

Construction succeeds with `Failed(WorkerSpawnFailed)` because direct color
does not use that worker. A request may use `VolumeDirect` only when the
ordinary scheduler independently selected it within the direct-work envelope;
hidden failure never overrides `VolumeAtomicRefinement` into an unsafe direct
pass. A request that requires failed hidden capability becomes request
`Failed` in preflight without entering the worker. Pick and Plane do not query
this capability.

The two remaining monotone renderer-resource identities are permanently
exhausted once their checked increment fails, but their operation scopes
differ. The renderer records `Available | Exhausted(cause)` independently for
private-presentation allocation and texture-revision allocation. Complete-set
preflight determines whether the current request needs each resource. An
exhausted but unused resource does not affect the attempt. A needed exhausted
resource produces request `Failed` without re-entering the failed allocator; a
changed fingerprint is reconsidered once because it may no longer need that
resource. These are renderer-owned preflight facts, not second application
failure latches.

### Retry Bounds

- Fragmentation compaction remains at most one attempt per exact union.
- Residual placement recovery uses the existing strictly smaller payload bound;
  the finite catalog, not a numeric retry counter, bounds candidate attempts.
- Eviction-capacity retry occurs only after an acknowledgement changes the
  ledger.
- Pipeline compilation is not restarted inside one renderer runtime.
- A checked identity allocator is called at most once after it reports
  exhaustion; later requests consult its operation-scoped exhausted state.
- Hidden submission timeout receives the P2 policy below.
- Deterministic request failure executes once per fingerprint.
- Terminal GPU failure is projected once per renderer device generation.

### Terminal Quiescence

On the first global GPU cause, the renderer and app perform one ordered cut:

1. retain the first typed cause;
2. cancel or close admission to hidden-refinement work;
3. resolve every queued or pending Pick, capture, and timing request with a
   typed terminal result;
4. release CPU-side pending offers and retry bookkeeping without issuing new
   WGPU work;
5. stop raw pending residency, eviction, capture, timing, or pipeline state
   from owning a repaint; and
6. leave ordinary non-renderer application services and shutdown cleanup
   functional.

The UI may retain the last known texture registration for diagnostics, but it
must not claim that the failed device can present it. There is no automatic
device recreation.

`ColorUnavailable` is deliberately narrower than device terminal, but broader
than an exhausted renderer resource. It disables every new color execution
because startup or InitialRender capability failed; it does not call a safe
current-front auxiliary operation device-lost. A resource-identity failure,
by contrast, disables only operations that need that exact allocator. This
distinction prevents both unsafe device continuation and unnecessary loss of a
still-valid color or auxiliary operation.

## Hidden Refinement Failure Contract

The cause-free `HiddenRefinementWorkerOutcome::Failed` and public
`HiddenRefinementFailed` error are deleted. Worker outcomes become:

```text
HiddenRefinementOutcome =
  Complete(progress)
  | Cancelled(progress)
  | SubmissionTimedOut { progress, submission }
  | WorkerPanicked { progress, in_flight_submission }
  | DeviceFailed { progress, first_gpu_cause }

HiddenRefinementFailure =
  SubmissionTimedOutTwice

HiddenRefinementState =
  Running(progress)
  | WaitingForSubmission {
      submission,
      after_completion:
        RetryOnce
        | FailRequest(HiddenRefinementFailure)
        | FailCapability(WorkerPanicked)
    }
  | RetryReady
  | Failed(HiddenRefinementFailure)
  | CapabilityFailed(HiddenRefinementCapabilityFailure)
  | Complete(progress)
```

The worker records the most recent submitted cutoff before any operation that
can fail. Therefore timeout or panic cannot lose knowledge of an in-flight GPU
submission. `in_flight_submission = None` on panic transitions directly to
`CapabilityFailed(WorkerPanicked)`; `Some` transitions through
`WaitingForSubmission`.

`HIDDEN_REFINEMENT_BATCH_TIMEOUT` remains exactly two seconds. It starts when
the batch submission's completion wait is registered, not when semantic
planning or the whole hidden job starts. Expiry emits one typed worker result;
it is not a UI repaint interval and does not imply that the GPU work was
cancelled. Changing this duration is outside R4 and requires measured evidence
plus a plan amendment.

Policy is exact:

- `Cancelled`: expected latest-only replacement; no failure and no retry.
- `DeviceFailed`: enter the global terminal cut immediately.
- `WorkerPanicked`: immediately close hidden-job admission, retain the
  predecessor, quarantine the candidate until any recorded in-flight
  submission completes, then discard it and permanently set hidden capability
  to `Failed(WorkerPanicked)`. Do not retry any fingerprint through the dead
  worker; independently valid direct color remains available.
- first `SubmissionTimedOut` for a fingerprint: retain the predecessor,
  preserve the in-flight completion lease, issue no duplicate submission, and
  wait for its completion callback. After completion, discard the unpublishable
  candidate and permit exactly one fresh hidden job for that fingerprint.
- second `SubmissionTimedOut` for the same fingerprint: discard safely after
  completion and latch a typed timeout failure until the fingerprint changes.

A global device cause observed in any waiting state supersedes its stored
local transition: the candidate becomes permanently unpublishable, CPU-side
WGPU handles are released by ordinary safe drop/terminal cleanup, and neither
retry nor another completion poll is scheduled.

Rows from a timed-out or panicked job are never published, captured, picked,
or used as a resumable exact candidate. A new job starts from a cleared private
target after the old submission is known complete. The retry count is stored
with the attempt fingerprint, not in the worker mailbox, and is cleared on
success or fingerprint change.

`WaitingForSubmission` uses only that submission's completion event. On the
event, it discards the candidate and performs its stored `after_completion`
transition exactly once. `Running` wakes from the hidden worker's result event;
`RetryReady` is immediately executable; `Failed`, `CapabilityFailed`, and
`Complete` have no hidden retry wake. Cancellation removes the replaced state
after safe cleanup.

The public renderer progress report carries `HiddenRefinementState`; it does
not translate local timeout/panic back into a `WgpuRenderRuntimeError`.
Application classification is exhaustive over this state: `Running` and
`WaitingForSubmission` are neutral waits, `RetryReady` is `Ready`, local
`Failed` becomes fingerprint-scoped attempt `Failed`, `CapabilityFailed`
preflights only hidden-required requests to `Failed`, `Complete` may publish,
and `DeviceFailed` has already entered the global terminal latch. This is why
the deleted cause-free `HiddenRefinementFailed` variant has no row in the P3
runtime-error table.

## Pipeline Capability And Pick Contract

### Capability State

The one global `PipelineState::first_failure` is replaced with independent
capability state plus the existing global GPU latch:

```text
PipelineCapabilityState<T> =
  Compiling
  | Ready(T)
  | Failed(PipelineCompilationFailureCause)

PipelineSet {
  initial_render,
  pick,
  global_gpu_failure
}
```

The ordered compiler still creates Plane, MIP, DVR, ISO, and Mixed before
Pick. Initial color becomes `Ready` at the existing ordered event. Pick then
becomes `Ready` or capability-local `Failed`. A local Pick failure cannot make
an InitialRender query fail.

Failure promotion is fixed:

- `Validation`, `WorkerPanicked`, and `WorkerStopped` fail only the capability
  being compiled;
- `DeviceOutOfMemory` and `BackendInternal` enter the global GPU latch; and
- failure while compiling InitialRender leaves no color capability, even if a
  later Pick stage would theoretically be independent. Pick is not compiled
  after InitialRender failure.

The aggregate readiness projection may report partial capability facts but may
not collapse a local Pick failure into a renderer-wide error.

### Pick Queue Terminality

Every accepted `VolumePickTicket` has exactly one terminal application result:

```text
PickTerminalResult =
  Completed(VolumePickResult)
  | Stale
  | CancelledByNewerRequest
  | CapabilityFailed(cause)
  | RendererTerminal(first_gpu_cause)
  | ExecutionFailed(cause)
```

On Pick capability or renderer failure, the renderer retires its bounded pick
slot after any in-flight/map callback is safe to release. The application then
clears both `pending` and `queued` entries and resolves automation interest
with the same typed result. `clear_unsubmitted` is not used as a substitute for
pending-ticket retirement.

Pick backpressure keeps the latest current request queued and wakes only from
slot/map completion. It does not create an immediate repaint loop. A stale or
superseded request terminates explicitly and cannot block a newer pick.

## Canonical Numerical Contract

### One Validated Shader Affine

`mirante4d-render-api` replaces the row-only helper with this constructor:

```text
ValidatedShaderAffine::new(grid_to_world, grid_shape)
  -> Result<ValidatedShaderAffine, ShaderAdmissionError>
```

A successful immutable `ValidatedShaderAffine` contains:

- canonical binary32 world-to-grid rows;
- a finite f64 center plus per-entry verified error radius for the
  grid-to-world inverse of those quantized rows used for volume bounds;
- an outward upper bound on the normalized infinity-norm condition fact;
- a conservative maximum grid-coordinate error over the declared grid domain;
- and the exact grid shape and half-voxel coordinate domain for which those
  facts were proved.

Rejection is the `Err(ShaderAdmissionError)` branch; a successful object never
contains a latent rejection reason.

Construction follows this exact order:

1. require finite affine input and nonzero maximum absolute linear coefficient;
2. evaluate whether the 3x3 determinant is exactly zero by decoding each
   binary64 coefficient into sign/integer-significand/base-two-exponent,
   forming the six signed triple products, and summing them in a fixed
   exponent-aligned 6,400-bit stack accumulator. This width covers the full
   finite binary64 triple-product exponent span plus carry bits; it allocates
   no heap and adds no arbitrary-precision dependency. Exact zero is
   `SingularAffine`; determinant magnitude is then discarded and never
   compared with a threshold;
3. scale the 3x3 linear matrix by that maximum coefficient before inversion,
   so determinant magnitude is dimensionless and cannot reject units alone;
4. invert the scaled 3x3 matrix with deterministic partial-pivot LU in f64;
   each pivot is the largest absolute candidate in the current column, ties
   select the lowest row index, and a zero/nonfinite computed pivot after the
   exact nonzero proof is `AffineConditionExceeded { stage: Input, ... }`;
5. for the LU inverse center `B`, compute outward bounds for both
   `r_left = norm_inf(I - A*B)` and `r_right = norm_inf(I - B*A)`; require
   `r = max(r_left, r_right) < 1`, then use the Neumann bound
   `inverse_norm_upper = norm_inf(B) / (1 - r)` and per-entry inverse-error
   radius derived from `norm_inf(B) * r / (1 - r)`;
6. compute
   `condition_inf_upper = norm_inf(A) * inverse_norm_upper` and require
   `condition_inf_upper * 2^-53 <= 1/64`; this is a binary64
   validator-reliability gate, not a substitute for the binary32 shader error
   test;
7. derive f64 world-to-grid linear and translation rows, convert each value to
   binary32, canonicalize negative zero, and reject any nonfinite conversion;
8. reinterpret those exact binary32 values as f64, repeat the exact-zero
   predicate, and invert their linear part using the same scaled partial-pivot
   residual and binary64 condition gates; derive the inverse translation
   `-inverse_linear * translation` with outward intervals that include the
   verified linear-inverse radius. An exactly singular quantized matrix is
   `ShaderQuantizedAffineSingular`, while an unreliable/nonfinite inverse is
   `AffineConditionExceeded { stage: Quantized, ... }`;
9. evaluate the composed grid-to-world then quantized-world-to-grid mapping
   over the complete half-voxel grid box. Because each component error is
   affine in grid coordinates, its maximum occurs at one of the eight box
   corners;
10. add a conservative binary32 bound for the shader's three-term
   world-to-grid dot-plus-translation operation over the world box induced by
   those corners, using every result permitted by the pinned WGSL operation-
   accuracy and subnormal rules; and
11. accept only when the total bound is finite and strictly less than `0.5`
   voxel on every axis.

The operation model is the
[2026-03-10 WGSL Candidate Recommendation Draft](https://www.w3.org/TR/2026/CRD-WGSL-20260310/#floating-point-evaluation),
not host IEEE round-to-nearest: addition, subtraction, and multiplication
enclose either permitted correctly rounded neighbor; division encloses its
declared 2.5-ULP range only for a normal-range denominator; `inverseSqrt`
encloses 2 ULP; `dot`, `length`, `normalize`, and `fma` expand according to the
specification's inherited-operation definitions; and every listed operation
also includes its permitted flush-to-zero result for subnormal input/output.
Runtime overflow/NaN is never treated as a usable interval value because WGSL's
finite-math assumption can make the result indeterminate.

Every interval operation widens its lower endpoint with `next_down` and upper
endpoint with `next_up`. Ordinary round-to-nearest f64 arithmetic, a no-FMA
model, or one observed adapter result without the full WGSL allowance is not an
accepted proof. An ordinary f32 reimplementation of the shader is supporting
test evidence, not the bound authority. If the pinned operation contract
changes, implementation stops for an owner-approved plan amendment rather than
silently retaining stale constants.

For verification only, `mirante4d-render-reference` decodes the nine
quantized binary32 coefficients into exact signed integer/exponent values and
forms the 3x3 adjugate and determinant as exact rationals. It checks that each
exact inverse-linear and derived inverse-translation value lies inside the
production interval. The reference does not call the LU/residual helper and is
not linked into the product.

The `2^-53` gate exists only because this implementation uses binary64 to
construct and verify the inverse. Binary32 suitability is decided from the
actual quantized rows plus the final absolute voxel-error envelope. Therefore
a large condition number does not fail merely because it amplifies a
hypothetical binary32 inverse that the shader never computes. If future work
adds an exact or verified-interval inverse, changing this binary64 reliability
gate requires a plan amendment and independent proof; it is not a tuning
constant.

The existing semantic-demand effective-affine residual/condition logic is
deleted in favor of this result. `shader_volume_world_corners` no longer
reinverts binary32 rows with another determinant threshold. Semantic demand
uses the returned inverse center and expands every derived world corner/bound
by its verified radius; it may not use the center as exact. The WGPU request
receives the validated object and cannot independently reinterpret an accepted
transform as unsupported.

The rejection vocabulary is closed:

```text
Axis3 = X | Y | Z
AffineColumn = X | Y | Z | Translation

ShaderControlField =
  WorldToGrid { row: Axis3, column: AffineColumn }
  | PlaneCenter(Axis3)
  | PlaneRight(Axis3)
  | PlaneUp(Axis3)
  | PlaneWorldUnitsPerLogicalPoint
  | PlaneScreenSpanX
  | PlaneScreenSpanY
  | VolumeRayOriginBase(Axis3)
  | VolumeRayOriginStepX(Axis3)
  | VolumeRayOriginStepY(Axis3)
  | VolumeRayDirectionBase(Axis3)
  | VolumeRayDirectionStepX(Axis3)
  | VolumeRayDirectionStepY(Axis3)

ShaderEnvelopeStage =
  PlanePixelCenter
  | PlaneWorldPoint
  | PlaneGridAddress
  | VolumeRayConstruction
  | VolumeDirectionNormalization
  | VolumeWorldToGrid
  | VolumeSlabIntersection
  | VolumePageExit
  | VolumeSamplePoint
  | GeneralDvrInterval
  | SubnormalDirection

ShaderEnvelopeAxis = Axis(Axis3) | All

ShaderEnvelopeFailure =
  NonFiniteBound
  | ZeroOrIndeterminateDirectionNorm
  | DivisionDenominatorNotProvablyNormal
  | ReachableQuotientNotProvablyFinite
  | ErrorBudget { upper_voxels }
  | AllDirectionsStationary

ShaderAdmissionError =
  SingularAffine
  | AffineConditionExceeded {
      stage: Input | Quantized,
      condition_upper
    }
  | ShaderControlNotFinite { field: ShaderControlField }
  | ShaderQuantizedAffineSingular
  | ShaderCoordinatePrecisionExceeded {
      axis: Axis3,
      error_voxels
    }
  | ShaderCoordinateEnvelopeExceeded {
      stage: ShaderEnvelopeStage,
      axis: ShaderEnvelopeAxis,
      reason: ShaderEnvelopeFailure
    }
  | ShaderSampleCountExceeded { layer, bound, maximum }
```

`ShaderCoordinatePrecisionExceeded` is emitted only by the affine round-trip
test in construction steps 9–11. `ShaderCoordinateEnvelopeExceeded` is emitted
only by a target-specific Plane or volume proof. The latter evaluates stages
in the order shown by the relevant proof algorithm; within a vector stage it
tests X, Y, then Z. It reports the first failure in that deterministic order.
`ShaderEnvelopeAxis::All` is used only for a scalar norm/common interval or an
all-direction fact. `ErrorBudget` records the outward upper bound, including
only finite values greater than or equal to `0.5`; a missing, infinite, or NaN
bound is `NonFiniteBound`. No stringly typed field/stage or generic fallback is
allowed.

`SemanticPlanError::ShaderAdmission` carries this error without erasing it.
The validated renderer request carries only successful objects. Generic
`UnsupportedView` call sites used for affine/camera conversion are deleted;
generic integer/buffer `CoordinateLimitExceeded` sites outside shader
admission remain. Product UI wording may group the typed errors under an
unsupported-view heading, but logs, tests, and attempt status retain the exact
cause.

Validation lifecycle is bounded. `ValidatedShaderAffine` depends only on
dataset runtime, layer/scale affine, and shape, so the app dataset runtime owns
one lazy result slot (`Ok` or typed `Err`) for each already-finite catalog
layer/scale entry. The catalog supplies the bound; there is no growing hash
cache. Slot storage is charged to the existing bounded dataset/render-planning
CPU ledger before publication. A dataset runtime epoch replacement drops all
slots. Reboxing a cached result does not change semantic identity.

Plane/volume work envelopes additionally depend on camera, physical viewport,
ordered layer-scale map, sampling/mode schedule, and the cached affine results.
The existing latest-only semantic-demand worker constructs them once per exact
demand signature and stores at most one current result per fixed target. A UI
turn consumes the matching result or waits for `CandidatePlanPublished`; it
never recomputes interval geometry synchronously. P7 measures both cold proof
construction and warm cache hits.

### Exact Binary32 Half-Step Envelope

Current shaders use half-voxel boundaries, `floor(grid + 0.5)`, and
`f32(index) + 0.5`. Binary32 represents every required integer and half-step
only while the exclusive upper bound is at most `2^23`.

This plan therefore defines:

```text
MAX_EXACT_SHADER_GRID_END_EXCLUSIVE = 8_388_608
MAX_EXACT_SHADER_SAMPLE_COUNT       = 8_388_608
```

Before renderer control construction:

- physical render width and height used in binary32 pixel-center arithmetic
  must each be in `1..=8_388_608`;
- every volume shape component must be in `1..=8_388_608`;
- every resource origin plus shape must be checked without overflow and must
  not exceed its volume shape or the same ceiling;
- every page lower/upper coordinate must remain within that envelope;
- every per-layer MIP/DVR/ISO/Pick ray sample-count upper bound must not exceed
  8,388,608; and
- the common-world general-DVR interval across affine layers must have a
  conservative viewport-wide count bound no greater than 8,388,608.

The render API returns a target-specific proof with every render intent:

```text
ShaderWorkEnvelope =
  Plane(ValidatedPlaneWorkEnvelope)
  | Volume(VolumeRayWorkEnvelope)
```

`ValidatedPlaneWorkEnvelope` retains the existing Plane protection rather
than weakening it during centralization. It records the exact quantized Plane
camera controls, viewport, per-axis grid rounding radius, and address range.
Its O(1) proof evaluates affine screen/world extrema at rectangle corners,
widens every binary32 operation, combines that radius with the
`ValidatedShaderAffine` error, and requires the total to remain strictly below
`0.5` voxel on every axis. Semantic-demand coverage expands by the returned
radius; WGPU consumes the same controls and cannot recalculate a looser bound.

`VolumeRayWorkEnvelope` records the exact quantized camera ray controls,
viewport, participating validated affines, per-layer count bounds, optional
general-DVR common-world count bound, and total per-axis grid error. Its
construction is fixed and O(layer count), never O(output pixels):

1. bound affine ray-origin and unnormalized-direction controls over the closed
   physical pixel-center rectangle; affine component extrema use its four
   corners;
2. prove the unnormalized direction cannot be zero by minimizing its squared
   norm, a convex quadratic in pixel x/y, at the in-rectangle stationary point,
   each edge's clamped one-dimensional stationary point, and the four corners;
3. widen control quantization, normalization, world-to-grid point/vector
   transforms, slab arithmetic, reciprocal grid speed, and sample point
   construction with the same outward binary32-operation bounds used by the
   Plane proof;
4. derive a positive lower bound for grid speed from the validated inverse
   norm and the direction-normalization bound, then derive finite entry/exit
   and step bounds from the finite grid box and origin bounds;
5. for a single affine layer, bound
   `ceil((exit - entry) * grid_speed)` using the axis-aligned box chord bound
   plus the widened shader arithmetic; for general DVR, bound the union world
   interval divided by the minimum quantized layer step; and
6. require the affine error plus camera/ray/sample-construction error to be
   finite and strictly below `0.5` voxel per axis, and every count upper bound
   to satisfy the declared maximum.

This proof covers every physical output pixel continuously; it does not assume
that perspective or normalized-direction extrema occur at viewport corners.
The convex-quadratic candidate set above is exhaustive for the only nonlinear
camera denominator. Max/min composition and slab/count bounds use intervals,
not sampled rays.

The application cannot claim placeability for a view whose matching
`ShaderWorkEnvelope` is absent. The renderer request borrows that exact object
and verifies its viewport, affine identities, layer order, and schedule before
control upload. A mismatch is the target runtime variant
`ShaderWorkEnvelopeMismatch`, distinct from semantic frame mismatch; a failed
proof retains its exact `ShaderAdmissionError` before any renderer call.

The volume envelope proves, for every physical output pixel and participating
layer: finite binary32 ray origin and normalized direction; finite final entry
and exit; finite positive step; finite sample-point construction; a finite
page-boundary quotient whenever that boundary is reachable before ray exit;
and the maximum sample count above. An outward-boundary quotient that
would overflow is allowed only when the finite ray exit is provably earlier;
the shader takes the no-division `ray_exit` branch. It never computes infinity
and then relies on `min`, which is unsafe under WGSL finite-math assumptions.
NaN, a reachable nonfinite quotient, a denominator not proven normal before a
reachable division, or an unknown comparison fails closed. Thus harmless far-
boundary overflow does not reject a view, while an in-segment overflow cannot
silently alter traversal.

WGSL permits listed floating operations to flush subnormals to zero. The shared
shader therefore classifies the raw binary32 direction representation before
performing direction arithmetic. `magnitude_bits = bitcast<u32>(direction) &
0x7fff_ffff` is `Stationary` when zero and `PortableStationary` when nonzero but
less than `0x0080_0000`, the smallest normal binary32 bit pattern. CPU demand
and the independent reference use the identical bit rules. Admission of a
`PortableStationary` component is legal only if the envelope adds the maximum
grid displacement removed by that canonicalization over the complete finite
ray interval to the per-axis error budget and the total remains strictly below
`0.5` voxel. All components becoming stationary is
`ShaderCoordinateEnvelopeExceeded { stage: SubnormalDirection, axis: All,
reason: AllDirectionsStationary }`. An unknown/nonfinite displacement is the
same error with the component axis and `reason: NonFiniteBound`; a finite total
at least `0.5` uses `reason: ErrorBudget { upper_voxels }`. Slab/page/address
semantics are therefore deterministic even exactly at a half-open boundary.
The `0x0080_0000` boundary is fixed by WGSL binary32 portability; it is not the
former tunable `1e-7`/`1e-6` geometry epsilon.

`f32(index) + 0.5` is then exact and distinct for every legal sample index.
This is the precise R8 guarantee: it prevents parameter-center aliasing caused
by integer-to-binary32 conversion. It does not claim that arbitrary oblique
rays visit each voxel exactly once; MIP/DVR/ISO sampling semantics remain
those already documented.

This is a mathematically derived limit of the current shader representation,
not restoration of the deleted 16,384-voxel policy. Packages beyond the
rendering envelope may still open and support non-rendering operations; the
viewer reports the typed limitation. Expanding beyond this ceiling would
require a separately approved origin-rebasing or integer/page-local shader
representation, not a hidden branch in this cutover.

### Orthographic Camera Geometry

Orthographic planning uses `CameraFrame::axes()` directly. The current
`camera_basis` reconstruction from `target - eye` is deleted.

The orthographic near plane is evaluated in stable relative form:

```text
dot(forward, world_point - target) + perspective_view_distance_world >= 0
```

It is not constructed by subtracting a small distance from a large target and
then subtracting the two points again. Right and up come directly from the
validated camera axes. Positive distance remains part of culling and ray
origin semantics; this change does not delete the near plane or accept
nonfinite camera controls.

Camera-to-shader conversion still passes through `ValidatedShaderAffine` and
the coordinate-error envelope. A camera that is valid in f64 but cannot be
represented within the shader envelope fails with the exact precision cause,
not `CameraMathNotFinite`.

## Volume Page Traversal Contract

The shared volume traversal removes the two ad hoc geometry epsilons.
Intersection and page exit use one WGSL-portable binary32 representation
classification. With `bits = bitcast<u32>(direction)` and
`magnitude_bits = bits & 0x7fff_ffff`:

```text
magnitude_bits == 0             -> Stationary
magnitude_bits < 0x0080_0000    -> PortableStationary
bits & 0x8000_0000 != 0         -> Negative
otherwise                       -> Positive
```

Admission has already excluded exponent-all-ones values, so the last two
branches contain finite normal values only. The `0x0080_0000` branch implements
the admitted canonicalization/error proof above and is not configurable. Every
normal Positive/Negative component participates in both slab intersection and
page exit. The canonical ray-work envelope makes every reachable boundary
candidate inside the ray interval finite before submission. If a quotient
would overflow only because that boundary is provably beyond finite ray exit,
the shared function takes a no-division `ray_exit` branch; it never computes
infinity and hopes a later `min` repairs it. Any NaN or nonfinite candidate that
could be reached before exit is an inadmissible-control invariant.

`segment_end_index` owns forward progress. It clamps the computed boundary to
at least `current_index + 1` and at most `count`; page-exit code must not use an
epsilon to turn a near boundary into whole-ray residency. Half-open page
ownership and the sample-center formula remain unchanged.

The shared helper evaluates each axis in this order: exact raw-bit zero
classification; raw-bit subnormal canonicalization; raw sign classification
of the remaining finite normal value; outward boundary selection (`upper` for
positive, `lower` for negative); signed positive numerator; reachability
against the remaining ray interval; division only for a reachable boundary
with a normal denominator; and the minimum across axes and `ray_exit`. A
rounded candidate at or behind the current sample cannot stall because
`segment_end_index`, not a distance epsilon, advances exactly one index
minimum.

All volume paths consume the same corrected mechanics:

- homogeneous and Mixed MIP;
- fused and general DVR;
- homogeneous and Mixed ISO;
- SmoothLinear variants; and
- MIP/DVR/ISO Pick.

No kernel may carry a private page-exit threshold. Plane fallback remains
separate and unchanged.

## Static Performance Recovery Protocol

### Immutable Baselines

Before production behavior edits, P0 records two source baselines in separate
worktrees:

- pre-multichannel-correction baseline:
  `24f7da1531056c950cb5479098bd723c5fd8dc91`;
- audited current baseline: `d5032c43525ddfa9d524d490e3391aa800c6d470`.

The protocol names these revisions A and B respectively. The implementation
candidate after P1-P6 is C. P0 measures A against B only where A can express
the same scientific work. P7 first checks C against B to detect a regression
introduced by the corrective cut, then checks C against the better admissible
A/B threshold. A failed A/B equivalence check excludes A for that case; it
never excludes B or turns C's regression into an old-baseline artifact.

All three are measured with one `EvidenceOverlay` commit. P0 authors the
overlay against B and applies the identical patch bytes to A. The P1-P6 branch
starts from B plus that overlay, so C inherits it; those milestones may not
edit the overlay's observation semantics. The report records A/B/C source
commit, overlay patch/tree hash, and final measured tree hash separately. Each
measured worktree must be clean. If later attribution needs another field, P7
creates one replacement overlay, applies it to all three revisions, reruns the
noninterference gate, and discards every earlier timing sample.

The overlay may add only opt-in counters, CPU timestamps, existing supported
GPU timestamp queries, sanitized report serialization, and harness commands.
It may not change a branch condition, request/order, allocation, wake,
submission, shader, control byte, LOD, residency action, or default release
behavior. A structural diff audit plus paired overlay-disabled equivalence
test must show identical request/report/pixel/submission traces on B before the
overlay is admitted. If the same observation point cannot be applied to A, B,
and C, that metric is ineligible for cross-revision attribution; the protocol
does not fill it with a proxy.

Both use the repository-pinned toolchain, identical release profile, same
selected Vulkan adapter/driver session, same renderer settings, same physical
viewport, same package, same visible layers, same transfer/mode/sampling,
and same camera script. The fixed renderer and cold-settlement cases use the
same exact per-layer scale map. Navigation requires the complete requested,
selected, navigation, and displayed map sequence to match at corresponding
accepted input samples; a revision that chooses different scientific work is
a correctness/selection mismatch, not an admissible timing comparison.
Private package paths, labels, geometry, and scientific identities are not
written to the repository.

If the old revision cannot express the same visible-layer set or exact scale
map, that case is excluded from direct timing comparison and retained only as
historical behavior. The protocol must not compare different scientific work
and call the delta a performance regression.

### Workloads And Samples

The controlled boundary contains:

1. the existing warm-resident 1920x1080 fixed-LOD renderer matrix, with five
   warmups and thirty measured samples per case. The matrix remains exactly
   1, 2, 4, and 8 co-registered Float32 64x64x64 channels, one resident 1 MiB
   full-volume resource per channel, VoxelExact and SmoothLinear sampling, and
   homogeneous MIP, compatible/general DVR as applicable, ISO, and Mixed
   control cases. It does not substitute a smaller extent or a different
   resource topology for a performance result. Each revision runs three full
   matrix repetitions in the same pair/block order defined below;
2. a normal-release warm-resident static navigation scenario with three
   independent 30-second sessions per revision. Sessions run as three matched
   pairs in A/B, B/A, A/B order at P0. P7 remeasures all three revisions in
   three blocks ordered A/B/C, B/C/A, and C/A/B, so every revision occupies
   every order position once. An interrupted or invalid pair/block is
   discarded in full and rerun; and
3. three process-cold-to-exact settlements per revision for the same target
   body,
   using the same pair/block order and the reset condition below before every
   sample.

"Process-cold" means a newly launched normal release process, newly created
WGPU runtime, empty app/dataset in-memory caches, no recovered project state,
and the same initial window/camera/settings. It does **not** claim an OS page-
cache drop. Before every measured launch, one untimed throwaway process opens
the same package to the same first-ready boundary and exits cleanly; the
measured fresh process then starts from a deliberately filesystem-warm but
application/GPU-cold condition. Timing begins at acceptance of the scripted
open command and ends at the matching Exact/current report. A throwaway or
measured launch with a different read/decode identity invalidates its whole
pair/block.

The navigation input source is the existing independently clocked real-window
`viewer-oblique-continuity --workflow zoom` driver: 30 seconds, the same
physical 3D panel, 60 Hz wheel-source clock, its existing balanced return to
the initial zoom, and no `--allow-host-stress`. `representative_static_rendering_recovery`
orchestrates that driver and the overlay; it does not add a second synthetic
camera path. A session is inadmissible unless generated and received wheel
counts, accepted camera revisions, initial/final camera facts, and per-sample
map sequence match across every revision in its pair/block.

The warm scenario must complete one unmeasured dry run and prove before
measurement that every body/map in the accepted zoom sequence is resident and
retained, not merely the initial body. During its measured interval it requires
zero physical reads, zero codec decodes, zero dataset requests, zero payload
uploads, and zero evictions. If any is nonzero, the run is a
reuse/residency/warm-admission failure and is not admitted as a shader/UI timing
sample.

All pair/block runs within one P0 or P7 campaign use the same workstation
login session and fixed power state.
The private raw report records adapter/driver, CPU model, toolchain, process
arguments, revision, run order, start/end temperature and clock facts where
exposed, and the SHA-256 identity of the private workload. The sanitized
repository report contains only a campaign-local random workload token and
`workload_identity_matched: true`; neither the SHA-256 value, token mapping,
path, nor scientific metadata leaves the private evidence directory. A
driver, adapter, viewport, settings, or private workload hash change
invalidates the pair/block rather than becoming a covariate.

For every timing metric, one run-level value is calculated first; the reported
revision value is the median of its three run-level values. Ratios below divide
candidate median by baseline median. "Better valid baseline" means the lower
median among A and B after that exact case passes the scientific-equivalence
and workload-admission checks; B is used when A is ineligible. Per-block values
are also reported so the median cannot hide an order-position reversal. A
run-level percentile is the nearest-rank value after sorting all admitted
samples in that run: rank `ceil(p * N)`, one-based, with no interpolation. The
median of three run-level values is the sorted middle value. GPU timestamp
deltas use the adapter timestamp period; CPU and wall-time spans use one
monotonic host clock. Raw samples and the derived rank/value remain in the
private evidence report so every aggregate is reproducible.

### Required Measurements

Every run records:

- requested, selected, navigation, and displayed per-layer maps;
- accepted input samples and presentation/texture revisions;
- UI-update p50/p95/p99/max;
- semantic-demand and render-plan CPU time;
- dataset requests, reads, decodes, leases, and cancellation waste;
- residency preflight, allocator, compaction, upload counts/bytes, and queue
  submissions;
- renderer control/planning time, color submissions, and target order;
- per-target GPU color timestamps where supported;
- hidden job, batch, row, timeout, and settlement wall time;
- egui texture-registration/paint-command changes; and
- renderer errors, validation errors, attempt waits/failures, and repaint
  reasons.

Internal publication and X11 client pixels are not called monitor-visible.
The 3D external/client pixel boundary may be used for causal timing if labelled
accurately; final human-visible acceptance remains owner observation.

The designated adapter is already expected to support GPU timestamps. If a P0
or P7 campaign reports them unsupported or invalid, fixed-LOD GPU acceptance
is unevaluated and R11 cannot close; CPU submission time or external-pixel time
may localize a boundary but may not substitute for GPU p95.

### Deterministic Attribution

The first optimization branch is selected by this ordered table:

1. If fixed-LOD GPU p95 regresses by more than 5% with matching pixels and
   work, select the affected shared/kernel renderer branch.
2. Otherwise, if warm navigation performs any read, decode, request, upload,
   eviction, allocator plan, or body rebuild absent from the earlier matching
   revision, select reuse/residency/currentness.
3. Otherwise, if color submissions exceed one per accepted direct cutoff,
   hidden batches exceed the declared scheduler work, or settled idle submits
   anything, select attempt/presentation scheduling.
4. Otherwise, if semantic-demand plus render-plan CPU p95 exceeds the earlier
   matching revision by both 10% and 0.5 ms, select the first measured planner
   stage crossing that threshold.
5. Otherwise, if egui/texture composition accounts for the product delta,
   select native presentation/UI handoff.
6. Otherwise, the slowdown is not localized: no optimization is authorized.
   Collect a narrower owner-approved trace or leave R11 open.

Only one branch is changed at a time. After it meets its component gate, all
three workloads are rerun before another branch can be selected. Storage,
codec, brick shape, GPU budget, output resolution, sampling, and LOD constants
cannot be changed unless their own measured row selects them and the owner
approves an amendment.

### Performance Acceptance

For matching scientific work on the designated workstation:

- fixed-LOD GPU p95 must be no more than 1.05 times the better valid baseline;
- warm navigation must retain zero reads/decodes/requests/uploads/evictions;
- settled idle must submit zero renderer work and request no immediate repaint;
- each accepted direct color cutoff must use at most one color submission;
- the median of the three run-level UI-update p95 values and the median exact-
  settlement wall time from the three process-cold launches must each be no
  more than the greater of 1.10 times that case's better valid baseline or
  that same baseline plus 1.0 ms; and
- pixels, coverage, validity, exact per-layer maps, and final currentness must
  match the independent expected facts.

If A is scientifically/workload-ineligible, B is the sole baseline. If both A
and B are ineligible for a required case or metric, that row is unevaluated:
C must still pass its correctness assertions, but no timing ratio or
"improvement" may be calculated from nonmatching work. R11 then remains open
unless another owner-approved plan amendment establishes a valid pre-correction
baseline and reruns the complete protocol.

Failure to meet these thresholds leaves performance recovery open. It does not
permit weakening a correctness milestone.

## Required Regression Inventory

The following exact tests or product scenarios are required. If implementation
needs a different test name, this table must be amended first so no coverage is
silently dropped or merged into a weaker assertion.

| Owner | Required test/scenario | Mandatory assertions |
| --- | --- | --- |
| `mirante4d-app` | `composed_same_frame_successor_body_cannot_reuse_predecessor` | Same frame/time/scale/extent with a different immutable body produces physical work; predecessor stays visible until publication. |
| `mirante4d-app` | `composed_prefetch_role_change_cannot_reuse_predecessor` | Same body allocation with changed promoted role is not reusable. |
| `mirante4d-render-wgpu` | `composed_all_reused_validates_body_role_and_renderer_lineage` | Body, role, device generation, private front, texture revision, schedule, completeness, and residency freshness all match before `Reused`. |
| `mirante4d-render-wgpu` | `composed_zero_delta_submits_nothing_after_current_renderer_proof` | Complete one/four-member report, zero transfer/color submissions, and no earlier app-only proof. |
| `mirante4d-app` | `composed_physical_delta_matrix_keeps_complete_logical_members` | All 16 four-target changed/reused partitions keep four logical members and derive exactly the changed physical group. |
| `mirante4d-app` | `composed_source_time_scale_spatial_extent_or_surface_mismatch_never_reuses` | Every named semantic mismatch independently prevents completion. |
| `mirante4d-render-wgpu` | `same_frame_changed_render_intent_is_contract_mismatch` | Altering view/presentation/layer/transfer semantics under one `FrameIdentity` fails; cloning an unchanged intent does not create retry identity. |
| `mirante4d-app` | `latched_failure_is_quiescent_for_128_unchanged_ui_turns` | One execution, one visible failure projection, zero later immediate refresh/repaint, unchanged predecessor fidelity. |
| `mirante4d-app` | `published_texture_revision_requests_exactly_one_composition_turn` | A new revision paints once without re-executing color; repeated report/current-state observation produces no second immediate repaint. |
| `mirante4d-app` | `render_fingerprint_change_reopens_exactly_one_attempt` | Each fingerprint dimension independently clears the latch; no unrelated event does. |
| `mirante4d-app` | `retryable_renderer_outcomes_preserve_presented_fidelity_and_use_neutral_status` | Pipeline wait, recovery deferral, and eviction capacity never enter the error banner or mutate displayed completeness. |
| `mirante4d-app` | `non_error_backpressure_and_progress_are_normalized_before_report_application` | `Ok(deferred_by_backpressure)`, missing Exact residency, and running hidden work become keyed waits; a genuinely published preview alone updates progressive fidelity. |
| `mirante4d-app` | `render_wait_priority_registers_one_keyed_causal_event` | Every wait row uses its exact key; simultaneous missing prerequisites choose table priority; stale/unrelated events do not execute color. |
| `mirante4d-app` | `renderer_event_sink_coalesces_keys_and_ignores_late_callbacks_after_drop` | Multiple producer commits before one UI read schedule one turn with no sink-owned key queue; a committed wrong key performs no work; a commit racing the handler clear is observed in that read or schedules exactly one successor turn; closed weak sink retains no UI state and a late worker callback is harmless. |
| `mirante4d-app` | `stale_frame_waits_for_reported_mailbox_advance_without_resubmission` | Same stale mailbox fingerprint becomes one keyed neutral wait; only an at-or-newer family publication can produce a different eligible attempt. |
| `mirante4d-app` | `eviction_acknowledgement_causes_one_retry_without_signature_change` | No retry before acknowledgement; exactly one after relevant acknowledgement. |
| `mirante4d-app` | `payload_recovery_completion_causes_one_retry_without_hot_polling` | Submission completion is the sole wake; no timer/immediate loop. |
| `mirante4d-app` | `candidate_limit_rejection_is_neutral_until_every_minimum_fails` | Finer capacity and shader-envelope rejection selects the next strictly smaller candidate with no red status; failure of all valid minima produces one typed hard failure. |
| `mirante4d-app` | `terminal_renderer_with_pending_internal_work_has_no_background_wake` | First cause projected once; pending residency/capture/timing/pick cannot keep 50 ms or immediate repaint alive. |
| `mirante4d-app` | `initial_render_failure_is_color_unavailable_across_fingerprint_changes` | InitialRender validation/worker failure projects once, never executes again after body/camera/time changes, does not claim device loss, and starts no Pick compilation. |
| `mirante4d-render-wgpu` | `resource_identity_exhaustion_is_operation_scoped` | Exhaust hidden-job, private-presentation, and texture-revision identities independently; unrelated existing-allocation/direct/Plane/Pick/auxiliary work remains eligible according to the scope defined above, while work needing the exhausted identity fails in preflight without a second allocation call. |
| `mirante4d-render-wgpu` | `hidden_timeout_waits_for_submission_then_retries_once` | No duplicate submission, old candidate discarded after completion, one fresh job maximum. |
| `mirante4d-render-wgpu` | `hidden_second_timeout_latches_without_publishing_partial_rows` | Second timeout is quiescent for the fingerprint; no texture revision/capture/pick/publication. |
| `mirante4d-render-wgpu` | `hidden_worker_panic_retains_front_and_quarantines_in_flight_candidate` | Cause retained; candidate released only after known completion; hidden capability fails permanently; an independently eligible direct pass still works. |
| `mirante4d-render-wgpu` | `hidden_worker_spawn_failure_is_capability_local` | Renderer construction succeeds with hidden capability failed; Plane/direct color and Pick remain usable; hidden-required work fails in preflight without unsafe direct substitution. |
| `mirante4d-render-wgpu` | `hidden_device_failure_uses_global_first_cause` | Device cause wins once and no later hidden work executes. |
| `mirante4d-render-wgpu` | `pick_validation_failure_preserves_ready_initial_render_capability` | Initial capability remains ready and can submit color; Pick reports its local cause. |
| `mirante4d-render-wgpu` | `pick_device_failure_promotes_to_global_renderer_terminal` | OOM/internal compilation cause invalidates all capabilities through the global latch. |
| `mirante4d-app` | `pending_pick_reaches_terminal_result_after_capability_failure` | Pending and queued state clear; hover/automation receives the typed terminal result; `has_work` becomes false. |
| `mirante4d-render-api` | `uniform_affine_scale_is_not_rejected_by_determinant_magnitude` | Uniform `1e-6` and `2e6` cases pass when the declared grid meets the error envelope. |
| `mirante4d-render-api` | `singular_condition_control_and_coordinate_failures_are_distinct` | Exact input singularity, condition failure, nonfinite control, quantized singularity, coordinate error, envelope stage, and sample count are independently produced; no determinant-magnitude threshold. |
| `mirante4d-render-api` | `shader_half_step_coordinate_envelope_has_exact_boundary` | End/count `2^23` accepted; `2^23 + 1` rejected; overflow rejected before conversion. |
| `mirante4d-render-api` | `plane_work_envelope_retains_viewport_wide_rounding_radius` | Quantized Plane controls plus affine error stay below half a voxel across the complete viewport; a boundary-crossing error fails with its exact stage/axis. |
| `mirante4d-render-api` | `volume_ray_envelope_checks_interior_direction_minimum_and_count` | Convex-quadratic interior/edge/corner candidates bound perspective normalization; finite entry/step and every per-layer/general-DVR count satisfy the exact shader formula. |
| `mirante4d-app` | `affine_and_work_envelope_cache_is_semantically_keyed_and_bounded` | Affine success/failure computes once per dataset layer/scale; target envelope replaces latest-only by exact demand signature; stale result cannot enter a request. |
| `mirante4d-app` | `orthographic_tiny_distance_uses_canonical_axes_and_stable_near_plane` | Positive distance below the former epsilon and cancellation-prone large target plan without false camera-math failure. |
| `mirante4d-render-reference` | `quantized_affine_error_envelope_bounds_every_grid_corner` | Independent f64 corner facts lie strictly inside the declared bound; rejection boundary cases lie outside. |
| `mirante4d-render-api` | `verified_quantized_inverse_radius_contains_exact_reference_inverse` | Two-sided residual is below one; the reference crate's decoded-binary32 exact-rational 3x3 adjugate/determinant oracle lies inside every returned linear/translation interval, including ill-scaled accepted cases. |
| `mirante4d-render-wgpu` trusted GPU | `volume_page_traversal_crosses_sub_epsilon_direction_for_all_kernels_and_pick` | Positive/negative `5e-7` directions cross distinct pages; exact/SmoothLinear MIP/DVR/ISO/Mixed color, coverage, validity, and Pick match independent facts. |
| `mirante4d-render-wgpu` trusted GPU | `volume_page_traversal_zero_direction_stays_in_one_page` | Exact zero remains stationary and terminates monotonically. |
| `mirante4d-render-api` and `mirante4d-render-wgpu` trusted GPU | `volume_page_traversal_subnormal_direction_is_portably_canonicalized_or_rejected` | A subnormal component whose maximum discarded displacement keeps the complete work envelope strictly below `0.5` voxel is canonical stationary in admission, CPU demand, reference, intersection, every color kernel, and Pick. All components canonical stationary produces `axis: All, reason: AllDirectionsStationary`; an unknown component displacement produces its axis plus `NonFiniteBound`; a finite total at least `0.5` produces its axis plus the exact `ErrorBudget { upper_voxels }`. Every rejection has stage `SubnormalDirection` and occurs before renderer submission. |
| `mirante4d-render-wgpu` trusted GPU | `volume_page_traversal_far_boundary_overflow_clamps_only_to_ray_exit` | Tiny nonzero normal direction whose outward boundary quotient exceeds binary32 but lies beyond finite ray exit remains moving, does not reject or produce NaN, and matches independent color/coverage/Pick facts. |
| `mirante4d-render-wgpu` trusted GPU | `fixed_lod_multichannel_gpu_timing_matrix` | The existing matching-work matrix retains five warmups/thirty samples, prints `gate_met: true` exactly when the measured homogeneous linear ratio is at most `1.20`, asserts that gate, and reports zero validation/upload work. This repairs evidence polarity only; P7's cross-revision `1.05` threshold remains separate. |
| `xtask` | `representative_static_rendering_recovery` | Enforces matching-work admission, required metrics, three sessions, thresholds, and sanitized report output. |
| `mirante4d-app` | `viewer_render_failure_state_machine` | Bounded test-only composition covers retryable wait, deterministic failure, Pick-local failure, hidden timeout, and terminal quiescence through the ordinary app coordinator. |

### Audit Closure Traceability

An audit row closes only when all evidence in its row is green and its named
predecessor behavior is absent. A milestone checkbox or a broader integration
pass cannot substitute for a row below.

| Audit | Required focused evidence from the inventory | Required deletion or measurement gate |
| --- | --- | --- |
| R1 | `composed_same_frame_successor_body_cannot_reuse_predecessor`; `composed_prefetch_role_change_cannot_reuse_predecessor`; `composed_all_reused_validates_body_role_and_renderer_lineage`; `composed_zero_delta_submits_nothing_after_current_renderer_proof`; `composed_physical_delta_matrix_keeps_complete_logical_members`; `composed_source_time_scale_spatial_extent_or_surface_mismatch_never_reuses`; `same_frame_changed_render_intent_is_contract_mismatch` | App-only/frame-scale reuse and app-built physical-as-logical target paths are deleted; P1 exit passes. |
| R2 | `latched_failure_is_quiescent_for_128_unchanged_ui_turns`; `published_texture_revision_requests_exactly_one_composition_turn`; `render_fingerprint_change_reopens_exactly_one_attempt` | Independent renderer-work/UI-repaint predicates and unconditional dirty-bit repaint are deleted; P3 quiescence exit passes. |
| R3 | `retryable_renderer_outcomes_preserve_presented_fidelity_and_use_neutral_status`; `non_error_backpressure_and_progress_are_normalized_before_report_application`; `render_wait_priority_registers_one_keyed_causal_event`; `stale_frame_waits_for_reported_mailbox_advance_without_resubmission`; `eviction_acknowledgement_causes_one_retry_without_signature_change`; `payload_recovery_completion_causes_one_retry_without_hot_polling`; `candidate_limit_rejection_is_neutral_until_every_minimum_fails` | Generic error-to-`Incomplete`, report-before-normalization, negative-list classification, and raw polling are deleted; every wait has one keyed wake. |
| R4 | `hidden_timeout_waits_for_submission_then_retries_once`; `hidden_second_timeout_latches_without_publishing_partial_rows`; `hidden_worker_panic_retains_front_and_quarantines_in_flight_candidate`; `hidden_device_failure_uses_global_first_cause` | Cause-free hidden failure and timer retry of unknown in-flight work are deleted; P2 timeout/panic/device exit passes. |
| R5 | `pick_validation_failure_preserves_ready_initial_render_capability`; `pick_device_failure_promotes_to_global_renderer_terminal`; `pending_pick_reaches_terminal_result_after_capability_failure` | Global local-cause pipeline poisoning, `clear_unsubmitted` failure cleanup, and unterminated Pick entries are deleted; P4 exit passes. |
| R6 | `volume_page_traversal_crosses_sub_epsilon_direction_for_all_kernels_and_pick`; `volume_page_traversal_zero_direction_stays_in_one_page`; `volume_page_traversal_subnormal_direction_is_portably_canonicalized_or_rejected`; `volume_page_traversal_far_boundary_overflow_clamps_only_to_ray_exit` | Former `1e-7`/`1e-6`, mode-private, and epsilon progress branches are absent; all P6 color/coverage/validity/Pick facts and GPU gate pass. |
| R7 | `uniform_affine_scale_is_not_rejected_by_determinant_magnitude`; `singular_condition_control_and_coordinate_failures_are_distinct`; `verified_quantized_inverse_radius_contains_exact_reference_inverse` | Both absolute determinant authorities and duplicate inverse policies are deleted; accepted/rejected P5 boundary cases pass. |
| R8 | `shader_half_step_coordinate_envelope_has_exact_boundary`; `volume_ray_envelope_checks_interior_direction_minimum_and_count`; `quantized_affine_error_envelope_bounds_every_grid_corner` | Unchecked coordinate/sample conversion and shader-only count discovery are deleted; exact `2^23`/`2^23 + 1` boundaries pass. |
| R9 | `orthographic_tiny_distance_uses_canonical_axes_and_stable_near_plane` | Target-minus-eye basis reconstruction is deleted; both tiny-distance and large-target cancellation fixtures pass. |
| R10 | `terminal_renderer_with_pending_internal_work_has_no_background_wake`; terminal branch of `viewer_render_failure_state_machine` | Raw terminal queues cease to own background work; P2 cleanup and P3 scheduling exits both pass. |
| R11 | `fixed_lod_multichannel_gpu_timing_matrix`; `representative_static_rendering_recovery`; every admitted raw/derived metric required by the static protocol | The existing 1.20 matrix gate has correct polarity and is enforced; A/B/C matching-work admission and all P7 thresholds pass. Unsupported GPU timestamps, an invalid baseline, or unlocalized slowdown leaves R11 open. |
| R12 | `hidden_worker_spawn_failure_is_capability_local`; `hidden_worker_panic_retains_front_and_quarantines_in_flight_candidate`; hidden-job branch of `resource_identity_exhaustion_is_operation_scoped` | Hidden-worker spawn is no longer renderer-wide; dead/exhausted hidden capability is permanent and preflighted; no unsafe direct substitution exists. |
| R13 | Private-presentation and texture-revision branches of `resource_identity_exhaustion_is_operation_scoped`; `render_fingerprint_change_reopens_exactly_one_attempt` | Each allocator records one permanent operation-scoped exhaustion fact, is never called again after exhaustion, and cannot disable an operation that does not need it. |

Fault injection is private and `#[cfg(test)]`. It may select outcomes at
existing ownership seams but may not add a production renderer path, new
release-build environment behavior, or a mock that bypasses the ordinary app
coordinator.

## Work Sequence

### P0 — Truth Reset, Failing Tests, And Baselines

Outcome: freeze every audited failure before changing production behavior.

Required work:

- record the two clean performance baselines above;
- correct and enforce the existing fixed-LOD matrix's inverted 1.20
  `gate_met` evidence flag without changing its workload or renderer path;
- create, hash, backport, and noninterference-qualify the one
  `EvidenceOverlay` before any production behavior edit;
- add focused failing tests for R1 through R10 plus R12 and R13;
- make the current transaction test cover incompatible body, prefetch role,
  source, timepoint, render-intent semantics, scale map, spatial frame, extent,
  device generation, texture/front lineage, and residency freshness—not only
  the sixteen prepared/reused bit patterns;
- add a repaint-observer test that runs at least 128 unchanged UI turns after
  a deterministic failure and observes one execution and zero subsequent
  immediate repaint requests;
- add neutral-status tests for pipeline wait, recovery deferral, and eviction
  acknowledgement, plus finer candidate-limit rejection before hard-minimum
  failure and the one-keyed-wake priority table;
- add InitialRender-unavailable, hidden-worker-spawn, operation-scoped
  identity-exhaustion, post-InitialRender Pick-failure, and pending-ticket
  tests;
- add pure numerical cases for tiny/large uniform scale, singular and
  ill-conditioned matrices, precision-bound translations, the `2^23`
  coordinate boundary, Plane rounding envelope, volume ray/count envelope,
  sample-count boundary, and tiny-distance orthographic cameras; and
- add independent GPU-oracle fixtures for positive and negative page crossing
  below, at, and above the former `1e-6` threshold, plus a finite-exit case
  whose irrelevant far-boundary quotient overflows positively; and
- add a pure admission/reference fixture plus trusted-GPU fixture for both
  branches of subnormal canonicalization: a safely irrelevant component and a
  component whose discarded displacement would consume the half-voxel budget.

No production behavior changes in P0. Each test must fail for the intended
reason on `d5032c43525ddfa9d524d490e3391aa800c6d470`, not because of an
unrelated setup error.

Exit:

- all failures have a named reproducer or, for terminal device wake, a
  deterministic injected first-cause test;
- baseline reports contain matching-work proof plus source/overlay/measured
  tree identities, and overlay-disabled noninterference is green; and
- R11 is either reproduced quantitatively or remains explicitly qualitative.

### P1 — Complete Logical Request And Renderer Reuse Cutover

Outcome: close R1 without losing incremental physical work.

Required work:

- introduce app-owned `PresentationTransactionMember/Targets` and renderer-
  facing `CoordinatedLogicalTargetRequest/Set`;
- bind exact immutable bodies and quality facts before renderer invocation;
- move prepared/reused classification into one `FrameCoordinator` operation;
- return full logical-member dispositions plus the physical recorded-target
  list;
- validate the all-reused case inside the renderer with zero GPU submission;
- rebuild `RenderAttemptFingerprint` from the complete members; and
- preserve retain-before-release lease transfer for changed same-frame bodies.

Required deletions:

- application-only `transaction_target_is_compatibly_reusable`;
- frame/scale-only transaction reuse;
- application construction of a physical publication group before renderer
  classification; and
- any empty-delta completion that does not observe current renderer fronts.

Exit:

- all sixteen physical-delta partitions retain a complete logical set;
- every incompatibility dimension forces renderer execution or a typed stale
  result;
- a compatible all-reused transaction performs zero transfer/color
  submissions; and
- same-frame body replacement remains O(1) for comparison and preserves
  retain-before-release ownership.

### P2 — Hidden Refinement And Terminal Cleanup Cutover

Outcome: close R4, R12, and the cleanup half of R10.

Required work:

- implement the typed hidden outcomes and exact timeout policy;
- make worker spawn, job-identity exhaustion, and worker panic permanently
  hidden-capability-local without changing the scheduler's direct-work gate;
- retain in-flight submission ownership through timeout/panic cleanup;
- make retry count fingerprint-scoped and bounded to one timeout retry;
- route device failure into the existing first-cause latch;
- retire hidden, residency, capture, timing, and pick background ownership on
  terminal cut without unsafe WGPU calls; and
- expose cause-specific diagnostics and UI failure wording.

Required deletions:

- cause-free hidden `Failed`;
- generic `HiddenRefinementFailed`;
- renderer-construction failure solely because the dedicated hidden worker
  could not spawn;
- failed hidden state that remains permanently `is_actionable`; and
- timer-driven retry of an unobserved in-flight submission.

Exit:

- cancellation is neutral;
- timeout never duplicates in-flight work and retries at most once;
- panic is visible and quiescent for hidden work without disabling an
  independently eligible direct/Plane request;
- device failure is globally terminal; and
- no failed/timed-out partial texture can publish, capture, or pick.

### P3 — Attempt State, Retry Projection, And Wake Cutover

Outcome: close R2, R3, R13, and the scheduling half of R10.

Required work:

- install the sole `RenderAttemptState` owner;
- implement the exhaustive runtime disposition table after P2 has deleted the
  cause-free hidden variant;
- normalize retryable `Err` values and non-error deferred/progress reports into
  `ColorAttemptObservation` before either report application or UI projection;
- separate displayed fidelity from requested attempt status;
- project startup/InitialRender failure as fingerprint-independent
  `ColorUnavailable`, distinct from global `RendererTerminal`, and keep each
  exhausted renderer allocator operation-scoped;
- route renderer progress, retry, deterministic latch, and repaint decisions
  through that owner;
- make new texture revisions use the coordinator's bounded one-shot
  `PendingComposition` acknowledgement rather than color-dirty state;
- bind every transient wait to the sole reason/key/event row in the target wake
  table, including logical-member, mailbox, candidate-plan, pipeline,
  submission, eviction-acknowledgement, relevant-residency, and hidden-worker
  waits;
- install the weak, coalescing backend-neutral `RendererEventSink` and remove
  pipeline readiness polling;
- normalize scale-dependent capacity/shader admission into neutral candidate
  rejection before execution and reserve hard failure for all-minimum or
  preflight/execution disagreement;
- clear or replace attempt status only on matching success or fingerprint
  change; and
- expose bounded diagnostics counting decisions and their wake reasons.

P3 installs the total application match for both pipeline capabilities, but
the current renderer can still emit its existing globally poisoned capability
fact until P4. P3 unit tests exercise the InitialRender/Pick dispositions
directly; P4 changes renderer capability ownership and makes the Pick-local
rows reachable in the normal runtime. Therefore P3 may close R2/R3/R13 without
claiming R5, and P4 must not add another application classification path.

Required deletions:

- negative-list `product_render_failure_is_deterministic` classification;
- generic mutation of `FrameCompleteness` for renderer attempt errors;
- UI-side direct iteration over renderer target work;
- unconditional immediate repaint from a renderer dirty bit; and
- raw terminal renderer queues as background-work authority.

Exit:

- an unchanged deterministic failure executes once and produces neither an
  immediate nor periodic renderer repaint;
- every retryable row in the table preserves predecessor fidelity and has
  exactly one causal wake;
- each new texture revision receives at most one composition-only repaint and
  never reopens color execution;
- true hard capacity remains visible and typed;
- device-terminal failure projects once even with pending residency/capture
  state, while non-device color unavailability does not erase a safe front or
  viable auxiliary capability; and
- exhausting one renderer allocator does not disable work that does not need
  that allocator.

### P4 — Pipeline Capability And Pick Retirement Cutover

Outcome: close R5.

Required work:

- replace the global pipeline failure field with per-capability state;
- promote only device-level compilation causes to the global GPU latch;
- route InitialRender-local permanent failure to P3's `ColorUnavailable`
  state, never to a fingerprint-scoped retry;
- keep InitialRender usable after local Pick failure;
- make aggregate readiness expose partial state truthfully;
- give every pick ticket one terminal result; and
- retire pending and queued application pick state on capability or renderer
  failure.

Required deletions:

- `first_failure` behavior that makes every capability query fail;
- `clear_unsubmitted` as the pipeline-failure cleanup path; and
- pending pick entries that survive an unrecoverable Pick capability failure.

Exit:

- Pick validation/worker failure leaves color rendering and presentation live;
- Pick device OOM/internal failure terminates the renderer;
- initial pipeline failure still prevents color;
- pending automation and hover picks terminate without hanging; and
- color fidelity never changes because of a pick-only result.

### P5 — Affine, Coordinate, And Orthographic Contract Cutover

Outcome: close R7, R8, and R9.

Required work:

- implement `ValidatedShaderAffine` and typed rejection causes;
- install bounded dataset layer/scale result slots and latest-only per-target
  work-envelope slots before moving proof construction off the UI thread;
- remove both absolute determinant thresholds and duplicate inversion policy;
- consume the same quantized result in semantic planning, volume bounds, WGPU
  controls, and independent tests;
- enforce the `2^23` grid-end and sample-count ceilings before GPU work;
- feed scale-dependent shader-admission refusal into
  `CandidateFeasibility::Rejected` rather than bypassing the existing monotone
  scale selector;
- move the existing Plane viewport rounding proof into the canonical
  `ValidatedPlaneWorkEnvelope` without weakening its coverage expansion;
- add the O(layer-count) `VolumeRayWorkEnvelope`, including the exact
  convex-quadratic camera bound and general-DVR span;
- use canonical camera axes and stable relative orthographic near-plane math;
  and
- update UI wording to distinguish singular, condition, control-range, and
  coordinate-precision failures.

Required deletions:

- `determinant.abs() <= f64::EPSILON` render admissibility;
- renderer-local reinversion of accepted shader rows;
- semantic-demand's private effective-affine condition/residual authority;
- unchecked u32-to-f32 coordinate admission;
- shader-only discovery of an unbounded sample count; and
- orthographic `camera_basis(target - eye)` reconstruction.

Exit:

- uniform `1e-6` and `2e6` scales pass when their complete domain satisfies
  the error envelope;
- singular, over-conditioned, nonfinite, and half-voxel-ambiguous cases fail
  with their exact typed cause;
- the exact `2^23` boundary is accepted and `2^23 + 1` is rejected in pure
  metadata tests;
- Plane and volume requests carry the exact work-envelope object whose
  viewport/affine/layer identity the renderer verifies;
- tiny positive orthographic distance and large-target cancellation cases use
  canonical axes without false `CameraMathNotFinite`; and
- all accepted controls match the independent f64 reference within the
  declared sub-half-voxel bound.

### P6 — Shared Volume Page Traversal Cutover

Outcome: close R6.

Required work:

- install the shared representation-level subnormal canonicalization followed
  by sign/zero classification for remaining normal components;
- charge the worst discarded subnormal displacement across the complete finite
  ray interval to `VolumeRayWorkEnvelope`, and reject all-stationary or
  non-provably-irrelevant canonicalization before renderer submission;
- remove the former absolute direction and page-exit progress epsilons;
- distinguish a reachable finite boundary from a positively overflowing
  boundary already proven beyond finite ray exit;
- make `segment_end_index` the sole monotone-progress guard;
- ensure every volume and pick core imports the same traversal function; and
- compare GPU color, coverage, validity, and pick results with the independent
  reference across the threshold matrix.

Required deletions:

- the `1e-7` versus `1e-6` parallel-classification split;
- every configurable or mode-private direction threshold other than the fixed
  WGSL binary32 normal/subnormal boundary used by the shared helper;
- any mode-private page-exit rule; and
- structural string tests as the only threshold evidence.

Exit:

- positive and negative `5e-7` grid-direction cases cross all expected pages;
- exact and SmoothLinear MIP/DVR/ISO/Mixed and Pick match independent facts;
- zero direction stays stationary;
- a safely irrelevant subnormal component is canonical stationary everywhere;
  all-stationary, nonfinite-displacement, and half-voxel-budget cases reject
  before renderer submission with the exact `SubnormalDirection` axis/reason
  combinations specified above;
- a tiny nonzero direction with only an unreachable overflowing far boundary
  remains accepted and contributes `ray_exit` for that axis;
- coverage does not become missing merely because traversal retained a prior
  page; and
- the corrected fixed workload does not regress GPU p95 by more than 5%.

### P7 — Evidence-Selected Static Performance Recovery

Outcome: close R11 or retain it honestly with a localized measurement.

Required work:

- repeat the controlled protocol after P1–P6;
- apply the deterministic attribution table;
- implement only the selected first-boundary correction;
- rerun all workloads after each retained optimization; and
- delete every candidate that misses its component threshold or adds
  unmeasured complexity.

Exit:

- all performance acceptance thresholds pass with matching scientific work;
  or
- the milestone remains open with the exact first boundary, measurements, and
  reason no safe correction met the threshold.

No qualitative “feels faster” statement closes P7. Owner observation is still
required after the quantitative gates, but it cannot replace them.

### P8 — Integrated Verification And Product Closeout

Outcome: close the corrective package without broadening its claims.

Required automated checks:

```bash
cargo fmt --all
cargo test -p mirante4d-render-api
cargo test -p mirante4d-render-reference
cargo test -p mirante4d-render-wgpu
cargo test -p mirante4d-application
cargo test -p mirante4d-app --lib
cargo test -p mirante4d-ui-egui
MIRANTE4D_XTASK_ALLOW_TRUSTED_LOCAL=1 \
  cargo xtask verify-local trusted-gpu-correctness
cargo xtask verify-pr
cargo xtask docs-check
```

The trusted-GPU run uses the designated workstation and clean-revision guard.
The quarantined linked-S0 workflow is explicitly excluded.

Required normal-product exercises:

1. `target_fixture_render_modes` for MIP/DVR/ISO and Pick correctness;
2. `representative_native_navigation` for retained front, direct/hidden exact
   handoff, linked independence, both sampling modes, and idle quiescence;
3. `representative_temporal_playback` for full logical transaction reuse,
   partial physical deltas, held input, and Stop handoff;
4. the P7 static performance scenario on the designated workload.

The automated `viewer_render_failure_state_machine` test separately exercises
retryable wait, deterministic capacity failure, Pick-only failure, hidden
timeout, and terminal-renderer quiescence without inducing real hardware
loss/OOM or adding a product fault-injection route.

Product acceptance requires:

- no blank, downgraded, partial, mixed-timepoint, or wrong-body transaction;
- no repeated identical renderer error in the runtime log;
- no immediate repaint after stable deterministic or terminal failure;
- no red product error for retryable backpressure;
- preserved color operation after local Pick failure;
- correct typed terminal result for pending picks;
- correct pixels/picks for the numerical and page-crossing fixtures;
- exact/current settlement and zero settled-idle submissions; and
- owner confirmation of the normal mapped result.

Actual destructive device-loss/OOM injection remains unperformed unless the
owner separately authorizes that host risk. Structural first-cause and bounded
fault injection are the accepted evidence for this plan.

## Deletion Gate

The cutover is incomplete while any predecessor behavior below remains live:

- app-only transaction reuse based on frame/scale/extent without body and
  renderer validation;
- an app-built physical target list treated as the logical transaction;
- generic error-to-`Incomplete` projection;
- report application before non-error backpressure/progress normalization;
- independent renderer-work and UI-repaint predicates;
- negative-list failure classification;
- cause-free hidden failure;
- renderer-wide startup failure solely from hidden-worker spawn failure;
- re-entering an exhausted hidden-job, private-presentation, or texture-
  revision allocator, or projecting its exhaustion outside the operation that
  needs that identity;
- global pipeline failure poisoning unrelated ready capability;
- pending Pick without a terminal result;
- absolute determinant thresholds or ad hoc direction-magnitude thresholds
  other than the shared fixed WGSL binary32 normal/subnormal boundary;
- duplicate affine inversion policy;
- semantic-demand-private Plane/volume shader precision policy or a renderer
  request that carries no matching validated work envelope;
- unchecked shader coordinate/sample-count conversion; or
- target-minus-eye orthographic basis reconstruction.

No compatibility flag, alternate old path, or dormant fallback may retain one
of these behaviors after its milestone closes.

## Risks And Controls

### Full Logical Sets Could Reintroduce O(N) Work

Control: members retain shared immutable bodies; reuse and fingerprint equality
are allocation identity plus scalar facts. Any implementation walking body
keys per UI turn fails P1.

### Moving Reuse Into The Renderer Could Blur Semantic Ownership

Control: the app still chooses source/time/spatial/quality coordinates and
assembles the fixed logical set. The renderer decides only whether its current
private front proves a member or needs physical work.

### Quiescence Could Strand Legitimate Progress

Control: every `Waiting` state names a concrete event and focused tests fire
that event. Only exact `Failed` fingerprints and terminal devices have no
progress wake.

### Neutral Backpressure Could Hide A Real Failure

Control: classification is exhaustive; retryable states require a bounded
completion path. Absence of such a path is deterministic failure, not neutral
waiting.

### Timeout Retry Could Race An Old GPU Submission

Control: the in-flight completion lease is retained, no duplicate work is
submitted, the candidate is discarded after completion, and at most one fresh
job is allowed.

### Capability Isolation Could Continue After Unsafe Device Failure

Control: OOM and backend-internal compilation causes promote to the global
device latch. Only validation and compiler-worker lifecycle causes remain
capability-local. Hidden worker spawn/panic/identity causes are local only
because the worker owns no device failure fact; any observed device cause still
wins globally before capability projection.

### A Numeric Relaxation Could Admit Unstable Pixels

Control: determinant magnitude is replaced by normalized condition and an
outward-rounded error envelope over actual quantized controls. Singular and
sub-half-voxel-ambiguous cases remain rejected.

### A Viewport-Wide Proof Could Reject A Renderable View Conservatively

Control: the proof algorithm and candidate set are fixed above, use the actual
quantized controls, and report the failing stage/axis/count. It cannot fall
back to corner sampling, a tunable epsilon, or per-frame pixel enumeration.
Any fixture believed to be a false rejection must first add an independent
counterexample and amend the proof algorithm; callers may not bypass the
envelope. The P7 CPU-planning gate also prevents the centralized O(layer-count)
proof from becoming an interaction regression.

### The `2^23` Ceiling Could Surprise Large-Dataset Users

Control: it is typed, documented, checked before GPU work, and does not prevent
package open or analysis. Silent wrong pixels are not accepted. Larger
rendering support requires an explicit representation cutover.

### DDA Correction Could Cost GPU Time

Control: shared mechanics and independent pixels are mandatory; the fixed
workload has a 5% p95 gate. A faster incorrect threshold cannot be retained.

### Performance Work Could Become Speculative

Control: immutable baselines, matching-work proof, ordered attribution, one
branch at a time, and deletion of candidates below threshold are required.

## Hard Stop Conditions

Stop implementation and request an owner amendment if any milestone requires:

- a storage/package/brick/pyramid format change;
- a GPU-budget increase;
- reduced output resolution or a sampling/transfer/scientific change;
- a CPU or alternate-backend product renderer;
- device recreation or automatic recovery;
- a second residency/presentation/repaint authority;
- hashing or walking complete requirement bodies on the hot path;
- an ad hoc determinant/direction epsilon or a second local shader-admission
  validator;
- private dataset facts in repository evidence;
- running the quarantined linked-S0 workflow;
- destructive hardware-loss/OOM injection; or
- performance tuning without matching-work attribution.

Correctness work may continue when R11 cannot be reproduced. P7 may not claim
completion in that case.

## Explicit Non-Goals

- New rendering modes, transfer functions, segmentation, or analysis.
- Temporal interpolation, frame skipping, or playback-policy redesign.
- SmoothLinear removal or approximation.
- 4K or non-Linux qualification.
- Color-space redesign; current RGBA/egui handoff remains unchanged until a
  separately specified scientific color contract exists.
- Storage, cache-brick, shard, compression, or TIFF changes.
- A universal all-hardware performance claim.
- Monitor-continuity automation from internal publication or X11 client
  pixels.
- Reopening the quarantined linked-S0 evidence program.

## Documentation And Authority Closeout

During implementation, this plan owns target state only. Actual facts must be
updated at each completed milestone in:

- `docs/CURRENT_STATE.md` for implemented behavior and remaining limitations;
- `docs/ARCHITECTURE.md` for final authority and data flow;
- `docs/TESTING.md` for real commands, evidence boundaries, and claim language;
- `docs/DEVELOPMENT.md` for retained developer commands only;
- `docs/planning/NOW.md` for active milestone and authorization status;
- `docs/BACKLOG.md` if R11 remains unresolved;
- `docs/README.md` and `docs/documentation-index.json` for human/machine
  inventory; and
- the predecessor rendering plans for narrow supersession links without
  rewriting their historical evidence.

No implemented wording may appear before the relevant predecessor is deleted
and focused checks pass. No automated-verified wording may appear before the
named automated checks pass on the exact revision. No product-validated
wording may appear before the owner exercises the normal mapped application.

## Completion Standard

This plan is complete only when P0 through P8 are closed, every R1–R10 and
R12–R13 reproducer is green, R11 meets its performance threshold, every
predecessor branch in the deletion gate is absent, automated and trusted-GPU
checks pass, the normal mapped product exercises pass, and the owner accepts
the visible result.

If R11 cannot be reproduced or safely corrected, the correctness cut may be
reported separately as implemented and validated, but this plan remains open
for performance and must say so in `CURRENT_STATE.md`, `NOW.md`, and the plan
status. Passing unit tests, retaining a predecessor image while the UI spins,
suppressing an error without a retry event, or accepting numerically ambiguous
pixels does not satisfy this plan.
