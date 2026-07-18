# Viewer Performance Overhaul Plan

Status: ACTIVE — EP-00 WORKLOAD, FIDELITY, AND COST-TRUTH BINDING IN
PROGRESS; BOUNDED V5 DEVELOPMENT PROTOCOL IMPLEMENTED
Planning authorization: OWNER REQUESTED 2026-07-17
Implementation authorization: OWNER GRANTED 2026-07-17
Last reviewed: 2026-07-18
Current product baseline: the integrated first overhaul described in
[Current State](../../CURRENT_STATE.md)
Target sequence: EP-00 through EP-07

## Purpose

The first viewer overhaul removed important pathological work and restored the
required rendering and interaction surface. Owner testing confirms that it is
materially better, but also that it remains far below the required product
standard:

- wheel zoom can lag severely or freeze the viewer;
- a four-panel cross-section workflow can settle at an unjustifiably coarse
  LOD;
- compound-angle plane movement can expose bricks as they arrive; and
- the combined rendering, loading, and interaction result remains far behind
  what the workload and hardware should permit.

Those failures reject qualification of the current implementation. They also
show that neither another local renderer optimization nor an architecture-only
rewrite is sufficient. Preprocessing, stored layout, I/O, decode, caching,
scheduling, GPU residency, frame submission, LOD, presentation, arbitrary-
plane rendering, and MIP/DVR/ISO kernels must be designed and measured as one
pipeline.

This plan replaces the previous plan in this path. The integrated first
overhaul remains the current product baseline until an authorized hard
cutover. Its useful mechanisms and evidence are retained where they survive
the new gates; its runtime compensations for the current storage layout are
not presumed to survive.

The target is not a relative percentage improvement. The target is:

- resident interaction limited primarily by display cadence and required GPU
  shading;
- cold interaction limited primarily by the unique compressed bytes, decode,
  and upload that the requested view actually needs;
- settled 2D fidelity chosen from physical screen sampling rather than from
  implementation pressure;
- resident 3D rendering limited primarily by necessary ray samples after
  conservative skipping and termination; and
- no repeated work merely because the camera, viewport, rendering mode, or
  consumer changed.

The implementation strategy is architecture-led and low-level-informed. It
does not first tune the presentation-owned buffers, ordered-container walks,
or monolithic shader that the cutover will delete. It also does not postpone
GPU work until after new interfaces are frozen. EP-00 repairs measurement
truth, EP-01 selects the representation with production-shaped kernels, and
EP-03 through EP-05 land vertical slices in which residency, frame
orchestration, control layout, shader math, and presentation are optimized and
proved together.

Neuroglancer and other viewers remain useful design references and
same-machine sanity checks. They are not an oracle, dependency, format target,
or acceptance gate. Mirante4D must serve arbitrary planes and full MIP, DVR,
and ISO rather than adopting a slice-first architecture.

## Selected Architecture

The replacement has one data plane and a few small geometry/semantic kernels:

    calibrated multiscale scalar pyramid
      -> independently compressed cubic bricks in bounded indexed shards
      -> one canonical BrickKey and one shared resource state table
      -> one byte-bounded decoded-brick cache
      -> one persistent GPU brick arena and residency directory
      -> arbitrary-plane shader OR MIP/DVR/ISO ray shader
      -> complete-coverage presentation

The following are design decisions, not benchmark candidates:

1. Every three-dimensional inner brick is cubic in grid coordinates. A
   non-degenerate brick is B by B by B; a genuinely length-one spatial
   dimension remains one. Anisotropic bricks are outside this plan.
2. There is one stored spatial layout per scale. There are no XY, XZ, YZ,
   shallow-slab, camera-oriented, rendering-mode-specific, or duplicated
   payload families.
3. BrickKey identifies an immutable dataset-instance/content generation,
   layer, time, scale, and cubic brick coordinate. Provisional-to-verified
   authority is orthogonal to the key and may promote matching entries in
   place only after an identity proof; observed mutation or incompatible
   reopen creates a new content generation. Plane panels, 3D modes, picking,
   playback, readout, analysis, and verification request that identity rather
   than creating semantic sub-bricks or alternate cache keys.
4. A physical inner chunk, decoded-cache entry, scheduler resource, and GPU
   page have the same spatial identity. Shards batch physical storage and
   range reads; they do not create another rendering tiling. Intensity and
   validity are either one compound encoded brick or component ranges owned
   atomically by the same BrickKey entry; they never become separate cache or
   residency identities.
5. Camera and control changes never rebuild the GPU residency directory.
   That directory changes only when a brick is uploaded, evicted, invalidated,
   or its dataset generation retires.
6. Cross-sections and volume modes may use different demand calculations and
   shaders because their geometry is different. They share all stored values,
   summaries, I/O, decoded residency, GPU residency, transforms, validity,
   and accounting.
7. Each visible layer in a view displays one complete LOD at a time. Complete
   coarser layer coverage remains available while its target LOD loads; an
   incomplete target brick mosaic is never promoted as the current image.
8. Performance pressure cannot silently select a coarser settled LOD, smaller
   render extent, fewer channels, cheaper interpolation, different projection,
   unlit ISO, or approximate scientific mode. Capacity failure is typed and
   visible.
9. One admitted application display frame coordinates every dirty product
   target. Panel count does not create independent backpressure queues. The
   pending batch is always the newest admitted generation; the frozen encode/
   submit race policy permits at most one sealed older batch and cannot create
   an obsolete submission chain.
10. Camera/view constants, immutable per-scale transforms, residency records,
    and payload bytes have separate update frequencies and GPU bindings. A
    camera change updates only bounded hot control state; it never rebuilds or
    republishes the resident-brick directory.
11. Cross-section, MIP, DVR, ISO, and a necessary mixed-mode fallback have
    small purpose-built shader entry points over shared sampling/residency
    helpers. Runtime mode branches or fixed per-fragment arrays from an
    unrelated mode cannot burden an ordinary plane or single-mode volume pass.
12. Algebraic and traversal optimizations preserve the accepted scientific
    result. A LUT, reduced precision, preintegration, larger ray step, or other
    approximation requires an explicit error contract and measured admission;
    it is never smuggled in as a performance fix.

## Required Product Surface

The overhaul must retain and qualify, through the same data plane:

- arbitrary cross-sections at any finite affine orientation;
- the complete four-panel product layout with per-panel LOD and readiness;
- MIP, DVR, and ISO;
- orthographic and perspective projection;
- voxel-exact and SmoothLinear sampling;
- flat and gradient-lit ISO with attached or detached light;
- multichannel composition with correct layer order and mixed affine grids;
- time navigation and bounded next-timepoint prefetch;
- latest-only asynchronous MIP, DVR, and ISO picking;
- crosshair, numeric ROI, distance measurement, and exact linked readout; and
- exact whole-layer and numeric-box analysis through the shared scheduler.

No capability may retain or gain a private payload, cache, residency model,
fallback renderer, or slow compatibility path.

## Non-Negotiable Invariants

1. Source microscopy data is never modified. New packages are staged,
   independently checked, and published create-only only when complete.
2. Valid zero, invalid/no-data, missing/loading, preview, and exact complete
   data remain distinct through preprocessing, storage, rendering, readout,
   analysis, and evidence.
3. Large work is byte/count bounded, cancellable, generation-aware, and stale
   suppressing across import, I/O, decode, queues, caches, GPU work, readback,
   and verification.
4. Dataset storage remains sharded with a bounded physical-object count.
   File-per-brick data or sidecars are forbidden.
5. There is one product reader, one resource scheduler, one decoded-brick
   cache authority, one GPU residency authority, and one product renderer
   after each hard cutover.
6. The UI thread performs no filesystem I/O, decompression, payload
   conversion, demand-set traversal proportional to the dataset, blocking GPU
   wait, or synchronous readback. If bounded command encoding or
   `Queue::submit` remains on that thread, its complete worst-case renderer
   slice must pass the frozen UI latency gate; otherwise only bounded state
   publication/polling remains there.
7. Current source-mutation detection, exact-package/scientific verification,
   source-generation binding, and bounded security guarantees are preserved.
8. Import, verification, analysis, and playback cannot starve current
   interaction. Foreground priority cannot make background work unbounded or
   permanently unable to finish.
9. Unsupported adapter and capacity cases fail visibly. They do not select a
   dense CPU path, a legacy format, or another rendering strategy.
10. Rendering and large-data work is incomplete without trusted-GPU and
    real-display product validation on the accepted workload and hardware.

## Combined Execution Strategy

The overhaul is one program with three ordered kinds of work:

1. Establish cost truth and remove measurement ambiguity. GPU timestamps must
   bracket the commands they claim to measure; queue writes outside an encoder
   are accounted separately. Per-target submission, deferral, supersession,
   UI-thread rebuild, shader occupancy/spill, and unique-byte facts are
   observed before choosing a remedy.
2. Freeze architecture and low-level contracts together. Brick shape, payload
   representation, residency lookup, descriptor layout, hot uniforms, upload
   placement, frame batching, and kernel inputs are selected using
   production-shaped arbitrary-plane and MIP/DVR/ISO paths. A fetch-only
   microbenchmark or an unoptimized shader cannot choose the format.
3. Land vertical hard-cut slices. EP-03 removes camera-dependent resource and
   control reconstruction while installing the persistent residency and frame
   coordinator. EP-04 then lands each compact production kernel with its
   descriptor caching, coordinate math, traversal, and correctness proof.
   EP-05 connects those kernels to latest-only input, independent LOD, and
   complete presentation.

An optimization specific to a predecessor structure is admitted before the
cutover only when it corrects evidence or is the exact successor mechanism
being installed early. Conversely, a new architecture interface cannot exit
while its production kernel still relies on known per-sample metadata reloads,
monolithic unrelated shader state, or per-panel submission.

## Decisions That Must Be Measured Before The Format Cutover

The architecture is fixed, but several constants must be selected with small
development bakeoffs before a new profile is frozen.

### Cubic brick edge

The initial complete-package candidates are 64 cubed and 32 cubed. The
comparison must use the same pyramid semantics, sharding policy, codec,
renderer, workload, and fidelity:

- 64 cubed reduces chunk count, index pressure, scheduler operations, page
  lookups, and volume boundary crossings.
- 32 cubed reduces arbitrary-plane decoded/uploaded bytes and gives finer
  empty-space rejection, but increases request, decode, index, and page count.

The winner is selected from end-to-end arbitrary-plane, four-panel, MIP, DVR,
ISO, import, verification, and analysis results. No product runtime selector
or dual package is retained.

A 16-cubed candidate is admitted only if 32 cubed still fails a cold-plane
byte-amplification gate and counters show that codec, request, index, page,
and GPU lookup overhead would remain within budget. A small per-scale table of
cubic edges is admitted only if no single edge can pass both the plane and
volume gates for opposing measured reasons. Either exception requires a plan
revision before implementation. Cubic geometry remains mandatory.

### Outer shard geometry and ordering

The outer shard remains a bounded storage container containing many
independently compressed bricks. Its edge, brick count, and ordering are
selected to:

- keep physical object count within the existing storage invariant;
- let nearby arbitrary-plane and volume demand coalesce useful ranges;
- keep index reads and retained index memory bounded;
- avoid whole-shard reads for sparse demand; and
- support deterministic streaming import and verification.

The current 256-cubed spatial cohort is the baseline. A simple row-major
versus Morton-order comparison is allowed if range traces show ordering is
material. Losing candidates are deleted.

The byte-exact EP-01 selection authority fixes the compound-shard candidates
as a native Mirante4D profile with no Zarr or OME-NGFF interoperability claim.
It emits no Zarr/OME array, group, or mirror objects. This remains an isolated
candidate contract until the atomic EP-02-through-EP-06 activation. If a
candidate passes and is selected, that hard cut also supersedes the current
OME-NGFF/Zarr-specific parts of ADR-0002 and updates Data Format; the current
product contract remains implemented until then. A non-authoritative mirror
or dual reader is not an allowed transition mechanism.

### Codec

The current native little-endian, independently framed Zstandard level-3
payload plus CRC32C is the baseline. A faster codec or level is evaluated only
if normal-product traces show codec time or GPU starvation is a material
critical-path cost. Encoded size, decode throughput, bounded workspace,
package size, import time, and range amplification are measured together.
The selected profile has one codec contract, not a runtime strategy.

### GPU payload and lookup

The existing native-byte storage-buffer arena, direct dtype loads, persistent
staging, brick DDA, and sparse hashed lookup are the baseline. They are not
discarded because of unrelated interaction failures.

The existing fetch-only `float32` buffer-versus-texture diagnostic cannot
select this contract. EP-01 compares production-shaped kernels after applying
the common low-level floor: compact mode-specific entry points, CPU-composed
grid coefficients, one descriptor resolution per brick/footprint, dtype and
validity branches outside tight tap loops, and exact page-coordinate math.
Otherwise a representation can appear slow only because its shader is
needlessly doing unrelated work.

The new lookup authority is residency-based rather than presentation-based:
one persistent resident-brick directory keyed by the full BrickKey is owned by
the global residency authority and patched only for uploads, evictions,
invalidation, or source retirement. Layer membership is part of the key rather
than a second directory authority. Camera demand contains BrickKeys and
priorities; it does not prepare renderer page layouts.

The CPU authority owns the full typed BrickKey. Its GPU projection uses
compact, collision-checked runtime generation/layer/time/scale identifiers and
integer brick coordinates or an equivalent stable slot handle; a fragment does
not hash a cryptographic content identity. This projection remains one global
residency directory rather than a camera/presentation page table.

A texture-atlas candidate is reopened only if a complete production-kernel
benchmark, including u8/u16/f32, validity, SmoothLinear, page lookup, and
MIP/DVR/ISO, materially beats the buffer design within the accepted adapter
and memory envelope. Upload bytes/time, gutters or boundary support, residency
updates, filtering semantics, and additional VRAM are included. A fetch-only
float microbenchmark is insufficient, and the losing representation is
deleted.

The selected lookup contract must permit two small geometry-specific helpers:
point/footprint lookup for arbitrary planes and once-initialized page
traversal for volume rays. They share the same directory and descriptor
schema. Point lookup does not calculate unused ray-exit bounds, and a volume
loop does not re-resolve immutable dtype, shape, validity, offsets, or
summaries for every voxel. If the selected cubic edge/alignment permits it,
page coordinates use checked integer shifts; otherwise they use checked
integer division rather than lossy floating-point page addressing.

### GPU control, upload, and kernel contract

EP-01 selects the renderer-facing contract alongside the persisted
representation so their costs are measured together. The renderer contract is
runtime-only and does not enter package/recipe identity:

- a small hot view/layer uniform region contains camera state, transfer
  constants, and CPU-precomposed per-layer grid coefficients;
- persistent storage regions contain resident-page records, summaries, and
  payload addresses and change only with residency or dataset generation;
- upload placement and fixed-size slots, when selected, permit bounded
  coalesced copies rather than one allocator transaction and command per
  brick merely because a cohort contains many bricks;
- hot-control publication becomes queue-visible only under the sealed display-
  batch policy. An encoded staging copy is directly timestampable and
  discardable; a retained `Queue::write_buffer` path must account for its GPU
  queue interference even though host-call time is not transfer duration;
- each production entry point can resolve a compact brick descriptor once and
  pass typed/all-valid/masked variants into a tight sample loop; and
- the CPU supplies the already-known view/mode family and compatible-layer
  facts instead of making every fragment rediscover them.

The exact binding layout remains a measured implementation decision within
one renderer. It cannot create a second residency or payload authority.
Pipeline variants are admitted only for a dimension whose measured branch,
register, private-memory, or sample cost is material. The default bounded
family is plane, MIP, DVR, ISO, and one mixed fallback; layer-count, dtype,
sampling, or validity variants require separate evidence and a fixed small
ceiling.

### Spatial summaries

Every brick has conservative any-valid, min, max, and valid-count facts in its
compact index. Those facts are independently verified and used for
all-invalid rejection, MIP bounds, transfer-function-empty DVR rejection, and
conservative ISO display-predicate reachability rejection.

If SmoothLinear is selected, brick extrema used for skipping include the
minimal conservative neighbor support required by its reconstruction rule.
They may be derived or stored in the common summary contract, but cannot be
mode-private or silently ignore cross-brick taps.

EP-01 may select one fixed sub-brick summary lattice only if representative
3D traces show that samples inside noncontributing bricks remain a dominant
cost after brick skipping and early termination. The candidate must be a
small conservative index over the same scalar payload, useful to MIP, DVR,
and ISO. An octree, BVH, general hierarchy, or mode-specific volume is not
admitted.

## Target Preprocessing And Format

### One calibrated pyramid

The importer writes one scalar pyramid used by every rendering mode and
analysis consumer:

- s0 is the exact canonical source result after the reviewed validity policy.
- At each level, let p be the smallest world-space norm of the current affine
  basis vectors for non-singleton axes. Reduce every non-singleton axis whose
  norm is strictly less than 2p; exact ties therefore reduce together and an
  axis exactly at 2p waits. Each selected axis uses factor two, with an
  odd-length tail clipped to the samples that exist. This deterministic rule
  is calibration-aware without making bricks anisotropic.
- A child is supported when at least one sample in its aligned parent block is
  valid. It is the valid-only arithmetic mean in canonical z/y/x order;
  integer accumulation is wide and rounds half up, while float32 accumulation
  uses finite float64 arithmetic and a checked finite float32 result.
- Unsupported children are invalid. The reviewed in-bounds Chebyshev-radius-
  one invalid dilation is applied in the dataset's actual dimensionality at
  every level, and every final invalid value is canonical zero. Source
  sentinel equality is classified only at s0; ordinary inputs begin all-valid.
- Each scale has an explicit affine transform including the correct centered
  translation for every reduced axis.
- Coarse MIP, DVR, and ISO are visibly preview-quality samples of that scalar
  pyramid. Exact final results use s0; no coarse mode is mislabeled exact.
- The deterministic scale chain stops when every non-singleton spatial extent
  of one layer/timepoint is at most the selected cubic brick edge, subject to
  the profile's fixed checked maximum-level bound. Runtime admission
  separately checks whether the aggregate coarsest sets of all active layers
  fit their CPU/GPU budgets; hardware configuration never changes package
  contents.

The current no-sentinel point-decimation route is deleted. Sentinel and
ordinary inputs use one declared reduction model except for the reviewed
source-validity classification that only sentinel data requires.

### One compact inner record

Coordinates and other facts implied by scale geometry and ordinal ordering
are not repeated in every record. The compact inner index contains only the
facts needed to locate, bound, validate, and conservatively skip a brick,
including:

- presence and validity flags;
- encoded offset and length;
- decoded length or its checked derivation;
- conservative min/max and valid count; and
- the required payload integrity fact.

The exact record schema and byte bound are fixed in EP-01 and covered by an
independent reader. An optional selected sub-brick summary is stored in a
bounded shard-level region rather than one filesystem sidecar per brick.

### Bounded import despite smaller bricks

Work-unit count must not translate into resident memory. The importer uses
ordinal-addressed, fixed-record checkpoint state with bounded windows rather
than retaining a record vector proportional to all bricks. Pyramid
generation, codec workers, ordered publication, verification, and resume
retain explicit byte/count bounds.

The new layout, pyramid algorithm, summaries, index, and codec selections
receive new format/recipe identities. Existing complete packages are not
reinterpreted. Accepted datasets are reimported. Old checkpoints are rejected
and explicitly reset; there is no compatibility reader or migration shim.

The identity transition is explicit:

- ScientificContentId remains stable only when exact s0 values, s0 validity,
  logical layers, base shape, time calibration, and grid-to-world facts remain
  unchanged.
- The storage-profile, pyramid recipe, derivation, package, fixture, and
  release-input identities change for this cutover.
- An open session's immutable dataset-instance generation remains stable
  across proof-backed provisional-to-verified promotion; verification
  capability is not part of BrickKey.
- Persisted pyramid/storage-representation identity is part of the runtime
  dataset generation, so coarse bricks from different recipes never share a
  cache or GPU key even when their ScientificContentId is equal. Runtime GPU
  projection choices—buffer versus atlas, descriptor/binding packing, slot
  policy, lookup implementation, and cache budgets—are not package/recipe or
  project-binding identity. Changing one retires and rebuilds renderer
  residency under the same dataset generation; it does not require reimport.
- Existing project/package bindings are updated explicitly to reimported
  package identities; no automatic project or package migration is inferred
  from equal scientific content.

## Target Runtime

### One canonical resource state

Each BrickKey has one entry with orthogonal, explicitly synchronized state:

    work: idle | queued | reading | decoding | failed
    CPU:  absent | decoded
    GPU:  absent | uploading | resident

This is intentionally not one linear lifecycle. CPU data may retire while its
GPU page remains resident, and a later analysis/readout consumer may restore
CPU residency without invalidating or duplicating the GPU state.

The entry owns or references:

- immutable content generation, orthogonal verification authority, and guarded
  object facts;
- joined consumer interest and maximum current priority;
- request/decode generation and cancellation state;
- decoded allocation and CPU LRU state;
- GPU slot, residency epoch, pin state, and GPU LRU state;
- atomic intensity/validity component readiness; and
- compact scientific and range summaries.

Every consumer joins this state. Overlapping panels or modes cannot create a
second read, decode, allocation, upload, or waiter for the same brick.
Interest changes update counts and priorities; they do not replace complete
scope-owned resource graphs.

An entry with zero interest, no in-flight work, no CPU allocation, no GPU
page, and no retained failure/currentness obligation enters one bounded recent
metadata LRU and is then removed. Entry count and bytes have explicit ledgers.
Retiring a dataset generation drops its table as one owned scope. Hash
tombstone maintenance is bounded, observable, and performed outside camera
input; metadata for previously touched bricks cannot grow with navigation
history.

### I/O and decode

- Retain one authenticated package-root descriptor and a bounded LRU of shard
  handles and parsed shard indexes.
- Group current demand by shard and coalesce only adjacent useful encoded
  ranges under a fixed maximum range and gap policy.
- Decode every independently framed brick once per live in-flight/cache epoch.
- Decode directly into its final accounted cache allocation when the codec
  permits; otherwise count the unavoidable copy.
- Reuse bounded decoder contexts and keep decode parallelism below CPU,
  memory-bandwidth, and foreground-latency limits.
- Release compressed/range workspace immediately after decode. Rely on the OS
  page cache unless measurements prove a separate compressed cache removes a
  named dominant cost.
- Preserve pre-use/post-use source-currentness checking without repeating
  equivalent object work for every brick in one guarded cohort.

The decoded-brick cache is byte-budgeted and shared by all consumers. A fixed
entry count such as eight is not its capacity authority. CPU data may retire
after a successful GPU upload when no CPU consumer requires it; eviction never
invalidates an independently pinned GPU page.

### GPU residency

- Use one logical native-payload arena segmented only where adapter binding
  limits require it.
- Use fixed size classes when the selected brick/dtype/validity layouts make
  them sufficient; retain a general allocator only if measured fragmentation
  or mixed payload facts require it.
- Maintain one persistent resident-brick authority keyed by full BrickKey on
  the CPU and publish its compact checked GPU projection under the same
  residency owner. The GPU table contains resident bricks, not the current
  camera's requirement body.
- Patch table and record changes in bounded batches only on upload, eviction,
  invalidation, or source retirement.
- Place a bounded upload cohort to minimize allocator transitions and copy
  commands when fixed slots or adjacent destinations permit it. Copy-command
  count, staging CPU copy time, and uploaded bytes remain separately visible;
  command coalescing cannot enlarge the residency identity or read range.
- Pin every page required by the currently displayed complete LOD before
  reusing its slot.
- Keep uploads asynchronous and time/byte sliced so a loading burst cannot
  monopolize the UI or GPU queue.
- Upload one BrickKey at most once per residency epoch. A working set that
  fits its configured budget cannot churn.

Camera motion therefore changes camera uniforms, view demand, and draw
dispatch. It does not rebuild allocator state, GPU page layouts, record slabs,
residency hashes, buffers, or bind groups. Ordered maps or sets may remain in
cold ownership/control code, but an admitted camera event cannot scan or
reinsert the resident/required body. Hot runtime IDs, free lists, and scratch
storage are dense or reused where the fixed selected contract permits it.

### Frame orchestration and backpressure

One renderer-owned frame coordinator receives the newest admitted generation
and all dirty targets for the product refresh. It retains at most one
replaceable not-yet-encoded display batch; a newer admissible generation
supersedes that batch without encoding, submitting, or presenting it. The
coordinator:

- polls completion once without waiting;
- applies shared bounded residency/control updates once;
- by default encodes shared payload/control copies and every dirty 3D and
  linked-panel color pass into one command encoder and one queue submission;
- records per-pass timestamps inside that batch;
- installs at most one completion notification per submission; and
- rechecks the sealed generation before the first queue-visible write/submit,
  discarding and metering stale encoded work when the selected policy requires
  latest-only submission.

Input arriving during encoding cannot create an unbounded race. EP-00 and
EP-04 freeze one policy: either discard a stale encoded-but-unsubmitted batch
before queue-visible work, or permit at most one already sealed older batch to
submit while the newest generation becomes the sole pending batch. Encoded-
but-dropped and sealed-obsolete-submitted work are separate waste metrics.

Backpressure counts coordinated application frames, not panels. The four-panel
layout cannot exceed an in-flight limit merely by containing four targets, and
fixed panel order cannot starve the last panel. Picking and diagnostic
readback use separately bounded latest-only work and cannot consume the last
interactive color-frame slot indefinitely. Any API-required submission
outside the display batch is named by purpose and cannot silently count as a
second color-frame submission; its reason, count, and interference are
measured.

One renderer submission is the default, not a dogma. If per-pass timestamps
prove that a bounded passive-view tail makes the active-view latency gate
impossible, EP-04 may select one fixed foreground/passive two-stage policy.
It remains under this coordinator, uses at most two renderer submissions, has
no per-panel queues, and deletes the losing single-stage implementation. The
egui/surface-present submission is named and measured separately from these
offscreen renderer batches.

The coordinator does not wait for all target LODs to match. It renders each
dirty target from that target's finest complete admissible coverage while the
global planner progresses the rest, then publishes the coordinated set of
per-target generation/scale/coverage facts. Unchanged targets are neither
redrawn nor allowed to force another target into a separate queue.

## Demand, LOD, And Presentation

### Exact arbitrary-plane demand

For each cross-section panel:

- reconstruct its finite world-space plane patch and transform it to the
  selected scale;
- enumerate only cubic cells intersecting that patch, using an analytical
  dominant-axis or equivalent exact cell traversal;
- add only the exact proven support footprint required by the selected plane
  interpolation operation; a gradient halo is included only for a consumer
  that actually evaluates a gradient;
- rank visible bricks by screen contribution and distance from the active
  region; and
- add a small orientation-neutral spatial/angular guard as the final
  prefetch tier.

Axis-aligned panels use this same enumerator. There is no fast path whose data
or correctness does not generalize to an arbitrary orientation. A volume AABB
scan or per-output-pixel CPU brick discovery is not accepted.

### Exact volume support demand

Volume demand and complete-coverage pinning include the exact support footprint
of the selected reconstruction and mode, not only bricks intersected by the
nominal ray interval. SmoothLinear adds its cross-brick taps; gradient-lit ISO
adds the composed interpolation/gradient footprint around every possible
candidate hit, including support pages just outside the projected ray set.
Demand remains expressed as ordinary BrickKeys under the same bounds and is
proved independently. A frame cannot be called complete if a required
interpolation or gradient tap can still discover an unrequested page.

### Independent per-view, per-layer LOD

An independent oracle and the product selector project each scale's affine
voxel basis into the view:

- Each visible cross-section layer selects the coarsest scale satisfying the
  frozen per-screen-axis sampling rule for that layer's affine.
- Each visible 3D layer selects from its projected voxel footprint, ray sample
  spacing, projection, and render extent.
- Every view/layer pair owns its expected and installed scale. A 3D scale, one
  global scale, or another layer's calibration cannot stand in for that
  decision.
- Scale changes use hysteresis around the same physical rule, not a
  performance-driven bias.
- The renderer reports expected target, displayed fallback, coverage, and the
  reason for each view/layer transition.

At least one qualification view must independently require s0. Displaying s3
for that view fails regardless of frame rate.

### Zoom and camera input

Wheel, drag, and continuous camera input update transient camera state
immediately. They do not synchronously:

- mutate durable project history for every raw event;
- wait for a demand worker;
- perform I/O, decode, or upload;
- enumerate the complete target cohort on the UI thread;
- rebuild GPU lookup/control structures; or
- wait for another panel.

The newest camera renders immediately from the finest complete resident LOD
available for that view. Raw interaction samples coalesce into one latest
transient state and one bounded durable commit after the gesture.

Demand preparation is latest-only and cancellable at bounded checkpoints, but
already queued, decoded, or resident overlapping BrickKeys survive generation
changes. The active view receives foreground priority; passive views retain
complete prior images and make bounded progress without blocking it.

### Complete progressive presentation

The complete coarsest set for every active layer at the current timepoint must
be GPU-resident and pinned before that source/timepoint becomes active. If it
cannot fit, activation fails with a typed capacity error; an old-camera
framebuffer is not treated as current-camera fallback. For each view:

1. Render the current camera from the finest complete required LOD already
   resident.
2. Load a finer target behind that presentation.
3. Switch a visible layer's LOD only when every mandatory target BrickKey for
   that view/layer is GPU resident and the complete composite frame can be
   rendered.
4. Keep the former complete set pinned until the replacement is presented.

This removes transparent holes and arbitrary brick-order assembly without a
second voxel representation, per-brick mixed LOD within one view/layer,
screen-tile switch, or per-brick visible replacement. Different visible
layers may still select different complete scales through the independent LOD
rule. If whole-view switch latency fails its target, the plan must be revised
from that measured failure before another presentation model is admitted.

If a declared target set cannot fit, the product reports a typed working-set
capacity failure. It does not remain silently coarse.

## Rendering

EP-00 freezes the numerical conformance contract for values, validity,
coverage, RGBA, depth/order, and picking before a faster expression replaces
the independent reference order. Integer addressing and conservative rejection
remain exact. Floating rearrangements—reciprocal multiplication, separable
lerps, moved clamps, transfer inversion, or rewritten `pow`/`exp`/`log`
expressions—are retained only when independent boundary cases meet the frozen
bit-exact or tolerance-bound contract. A transformed expression used only to
reject work must additionally prove that it cannot reject a contributing
sample.

### Arbitrary cross-sections

The baseline is one fullscreen pass through a dedicated plane entry point. For
each visible layer, CPU `f64` setup composes the camera plane and inverse affine
into grid-space origin, pixel-x increment, and pixel-y increment. The shader:

- evaluates the grid coordinate directly from those coefficients rather than
  reconstructing world space and loading a full matrix per layer/pixel;
- computes the cubic page coordinate with the selected exact integer rule and
  performs the point/footprint residency lookup without unused ray bounds;
- resolves payload base, strides, dtype, validity, and bounds once for the
  selected brick;
- runs a direct nearest-value fast path or one explicit SmoothLinear footprint
  with shared floor/fraction math, up to eight contributing taps, and the
  independently accepted interpolation expression;
- performs another page lookup only for taps that truly cross a brick
  boundary; and
- accumulates nonnegative channel contributions and applies equivalent clamps
  and output construction once where the accepted arithmetic permits it.

SmoothLinear preserves invalid/missing semantics, including the rule for
zero-weight invalid taps. All-valid and packed-validity paths branch outside
the tap loop where possible. Large world translations are composed in CPU
`f64`; EP-01 selects coefficient packing/rebasing and a checked GPU error bound
so conversion to shader arithmetic cannot reintroduce catastrophic translation
loss. The transmitted coefficients and shader calculations have independent
edge/boundary conformance tests.

There is no CPU-resliced image, per-brick draw loop, plane texture cache, or
axis-specific renderer. Render extent is the actual panel pixel extent, and an
unchanged panel is not redrawn merely because another panel changed.

Fullscreen point lookup remains the product baseline. A single-batch
instanced plane/brick polygon path or tiled compute path may be prototyped only
if post-optimization timestamps/profiling prove per-pixel lookup or descriptor
coherence is the remaining material plane cost. Such a candidate must use the
same pages and presentation semantics, include arbitrary orientations and
SmoothLinear boundaries, avoid per-brick CPU submissions, and materially beat
the fullscreen path end to end. The loser is deleted.

### MIP, DVR, and ISO

Volume rendering uses compact dedicated MIP, DVR, and ISO fullscreen entry
points over the same pages. CPU setup precomposes grid-space ray coefficients
per layer. The shader normalizes the world ray once or carries an explicit
world-distance conversion, so perspective DVR extinction, ISO depth, and ray
steps have the same physical meaning as the independent reference.

Each kernel:

- restricts fragments to a conservative projected union of visible volume
  bounds when that rectangle is smaller than the target;
- uses the selected once-initialized brick traversal. Incremental DDA is the
  baseline, but the retained production kernel must beat the simpler candidate
  without harmful register pressure while avoiding repeated boundary division
  and metadata work;
- performs page lookup once per crossed brick and resolves one compact local
  descriptor before its sample loop;
- skips missing and all-invalid bricks without treating missing as zero;
- skips MIP bricks whose conservative maximum cannot improve the result;
- rejects DVR bricks whose conservative range maps to zero opacity and stops
  front-to-back compositing at the accepted opacity threshold;
- rejects an ISO brick only when its conservative raw range cannot satisfy the
  accepted transferred-display-level predicate, including window, gamma, and
  inversion, and stops at the first valid depth-ordered satisfying sample;
- uses adaptive but fidelity-bounded sample distance derived from physical
  voxel spacing and view projection; and
- batches compatible channels while preserving mathematically correct
  multichannel DVR integration and mixed-grid behavior.

Compatible layers share page geometry, ray setup, and interpolation footprints
where their affine and sampling contracts permit it. The genuinely mixed-
affine path precomputes each required ray/interval once rather than recomputing
matrix transforms and slab intersections at every page or sample. A common
single-layer path uses scalar accumulators and does not construct or composite
a general result structure for every sample.

Transfer math first receives the cheapest semantic fast-path candidates:
reciprocal window spans, gamma-one, opacity-zero/one, zero-contribution
rejection, and conservative raw-space ISO reachability. A raw cutoff may
replace the actual transferred ISO hit comparison only after exact boundary-
equivalence proof; monotonicity alone is insufficient. An `exp`/`log`
rearrangement is retained only under the frozen numerical contract. A transfer
LUT is admitted only if remaining special-function cost is material and
exhaustive boundary/error tests satisfy an owner-approved error bound;
scientific picks remain exact unless that contract explicitly says otherwise.

ISO gradient lighting reuses the hit descriptor and interior sample footprint;
only taps that cross a page boundary perform another lookup. SmoothLinear
skipping uses the selected conservative neighbor-aware summaries. When DVR
opacity or a provable MIP bound terminates value shading before all pages have
been inspected, a cheaper continuation may inspect only the facts needed to
preserve missing/validity/coverage semantics. That continuation is admitted
only after an independent proof shows which page- or sample-level work remains;
otherwise early termination remains restricted to cases already known
complete.

Payload, page lookup, residency, validity, sampling policy, timing, and
presentation authority remain shared. CPU-known mode families are never
rediscovered through a full per-pixel layer scan. Large fixed arrays or
dynamic indexing proportional to the maximum 64 layers are removed where a
tight common-count path or bounded batch suffices and are isolated to a proved
general fallback otherwise. Shader variants remain subject to the fixed
specialization budget above.

Settled unchanged views produce no volume submission. Asynchronous timing and
picking never block the UI or force a framebuffer readback.

Traversal/address conformance explicitly covers negative rays, zero and near-
zero direction components, origins inside the volume, exact face/edge/corner
ties, partial boundary bricks, singleton axes, and maximum admitted integer
address calculations without overflow. These cases are required regardless of
whether incremental DDA or another measured traversal wins.

### Hardware-dependent low-level candidates

Adapter-specific mechanisms are admitted only after the portable structural
work above and only for a measured remaining bottleneck:

- storage buffer versus typed 3D atlas is the complete EP-01 representation
  bakeoff already defined above;
- compute tiling or workgroup caching requires measured page/descriptor reuse
  that materially exceeds fragment-path cache behavior for the geometry it
  targets, uses the same pages/authority, and causes no material regression in
  the other required rendering paths;
- subgroups, packet marching, compaction, or persistent-thread schemes require
  profiler evidence that divergence or scheduling, rather than sampling and
  memory traffic, dominates on the accepted adapter;
- `f16` is not used for coordinates, affine transforms, scalar comparisons,
  depth, or DVR accumulation; a color-only use still requires a measured
  occupancy/bandwidth win and conformance proof; and
- preintegrated DVR, larger steps, or analytic gradient substitutions are
  reconstruction changes and therefore require a separately accepted fidelity
  contract rather than entering as implementation details.

EP-00 records the current wgpu device memory hint. EP-01 A/B tests it through
the successor residency/upload harness only under the admission rule above.
The selected adapter, backend, driver, observable toolchain, and feature set
are recorded.
Pipeline caches, render bundles, CPU SIMD, and scratch-allocation cleanup are
not promoted above the structural work unless their named CPU or startup metric
is material. No external GPU profiler is a product dependency; it is
development evidence used to inspect compiled instructions, registers,
private-memory spills, occupancy, cache behavior, and divergence.

## Scheduling And Contention

One global planner unions and deduplicates all views before submitting
BrickKeys to one scheduler. Its fixed priority tiers are:

1. complete current-camera coarse coverage for the active view;
2. complete current-camera coarse coverage for every other visible view;
3. interactive readout and foreground tool work;
4. active-view target LOD;
5. remaining visible target LODs, fairly scheduled by screen contribution;
6. bounded camera guard and the declared next playback timepoint;
7. exact analysis; and
8. background verification.

Within a tier, already useful overlap and work nearest completion are retained
before new speculative work. Priority promotion updates one live entry in
place. Cancellation suppresses work before range read, before decode, before
upload, and before presentation when possible; cancelled bytes and CPU/GPU
time remain observable.

Independent budgets govern:

- open objects and parsed indexes;
- range requests and encoded bytes in flight;
- decoder workers and codec workspace;
- decoded CPU bytes;
- upload bytes and staging slots;
- GPU payload and summary bytes;
- pending demand entries; and
- verification and analysis windows.

No feedback controller or autotuner is admitted initially. Fixed budgets are
selected from measured hardware limits and workload traces.

## Complexity Budget And Explicit Non-Goals

A mechanism is admitted only when it removes an asymptotic defect, preserves a
required invariant/capability, or demonstrates a material end-to-end gain on
the accepted workload. Moving work behind another interface, cache, thread,
or queue is not an optimization.

This plan excludes:

- anisotropic bricks or per-orientation layout families;
- multiple stored copies for planes, modes, or camera directions;
- a render graph, ECS, generalized scene graph, scheduler DAG, plugin
  renderer, generic GPU virtual-memory framework, or new backend abstraction;
- runtime-selectable brick sizes, codecs, payloads, page tables, traversal
  strategies, caches, or adaptive autotuning;
- CPU display fallback, dense whole-volume prerequisite, compatibility reader,
  migration shim, or old/new parallel product path;
- sparse textures, bindless resources, GPU decompression, io_uring, custom
  filesystem caching, or a custom allocator without a named failed gate and
  measured justification;
- an octree, BVH, or broad macrocell hierarchy;
- a permanent tracing platform, benchmark DSL, or Cartesian benchmark matrix;
- copying another viewer's format, implementation, or target numbers;
- remote/cloud storage, segmentation, 4K qualification, new operating systems,
  or rendered-frame-derived scientific statistics; and
- broad annotation/track editing in the viewer hot path.

Candidate implementations may exist only in isolated development benchmarks.
They are deleted before product activation.

## Scope And Authority Changes

Implementation, once separately authorized, may change:

- mirante4d-import-pipeline: pyramid semantics, cubic work units, bounded
  checkpointing, summaries, and deterministic sharded publication;
- mirante4d-storage: the experimental profile, compact index, shard range
  delivery, canonical BrickKey mapping, verification, and hard-cut reader/
  writer;
- mirante4d-dataset and mirante4d-dataset-runtime: the canonical resource
  identity, central resource state, decoded cache, priorities, cancellation,
  byte accounting, and diagnostics;
- mirante4d-render-api and mirante4d-render-wgpu: persistent residency,
  hot/cold GPU control contracts, coordinated display batches, compact cross-
  section and volume kernels, uploads, lookup, timing, picking, and
  presentation;
- mirante4d-app, mirante4d-application, mirante4d-ui-egui: transient camera
  handling, latest-only display admission, per-view demand/LOD/readiness,
  durable gesture commit, progressive presentation, controls, and diagnostics;
- mirante4d-analysis-runtime: joining the canonical BrickKey state without a
  private data path;
- xtask, fixtures, and independent readers/oracles: representation selection,
  scientific facts, performance workloads, trusted-GPU checks, and E4 product
  validation; and
- the owning architecture, data-format, current-state, testing, development,
  decision, and planning documents when their facts change.

The cutover changes the experimental storage/profile and pyramid recipe. It
therefore requires new identities, reimported packages, new independent
fixtures, and deletion of predecessor readers/writers/checkpoints. Project
state refers to newly imported package identities; no compatibility promise
is added.

The following remain outside scope unless the plan is separately revised:
source TIFF admission, analysis operation semantics, project-store format,
settings format, release platform, segmentation, and public dataset
publication.

## Measurement And Acceptance Contract

### EP-00 owner delegation record

On 2026-07-17 the owner delegated the detailed EP-00 binding and threshold
selection needed to execute this plan. The implementation binds the existing
private difficult-workflow package through an external, non-repository opaque
profile and uses the already accepted HW-2 workstation/storage authority. Raw
paths, dataset metadata, and unpublished identities remain outside the
repository.

The owner-approved starting gates are warm operating-system cache, no
competing activity, a 1280 by 720 60 Hz blocking extent, a 1920 by 1080
exercise extent, resident input-to-current-presentation p95 at most
16.666667 milliseconds, no current-presentation gap above 33.333334
milliseconds, no event/UI-thread interaction slice above 2 milliseconds,
application-cold first useful data within 100 milliseconds, and complete
coarse coverage within 250 milliseconds. The exact target-settlement ceiling
will be frozen from measured unique-byte throughput and the usability floor
before EP-00 exits; this delegation does not permit EP-01 to begin while that
ceiling, the exact scripts, or the fidelity/oracle bundle remains unresolved.

The owner rejected the first v4 predecessor-baseline launch as an evidence
protocol on 2026-07-18. Scripted product actions did execute, but serial
acceptance observations each restarted an independent long deadline and left
the visible application static for minutes while their times accumulated.
That partial evidence is retained as interrupted lineage, never reused, and
does not establish a sample or performance claim.

The replacement v5 development protocol is an authorized bounded hard cut,
not a relaxation of any product gate. It runs exactly three ordered samples of
the ten scenarios, with one fresh instrumented role and one fresh matched
control role for each scenario and sample: 60 role attempts total, balanced
role order, zero retries, and no substitution or post-hoc filtering. Product
observations belonging to one contiguous acceptance checkpoint run concurrently
from one shared declared origin, so the checkpoint wait is bounded by the
maximum applicable deadline rather
than the sum of independent timers. Deadline classes are fixed exactly by the
validated profile, oracle, or protocol, use short explicit viewer ceilings, and share
the import-primary origin for the import observations. Fatal setup waits,
exact hard-safety termination, evidence integrity, and typed product failures
remain separate. Every non-import prerequisite is fixed by the validated
profile or protocol and is at most 30 seconds; no script-authored multi-minute
viewer prerequisite remains. Typed
product-gate misses continue through the complete population for attribution,
but the first fatal setup, process, hard-safety, or evidence-integrity failure
stops immediately and preserves the exact partial lineage. The role-process
timeout is derived from the validated action, prerequisite, and concurrent-
batch schedule plus a fixed 30-second startup-admission grace and 10-second
closeout grace, with the IP import wall counted once; there is no caller-
authored timeout. This three-sample population is development evidence under
the existing progression rule and cannot establish a performance claim.

Automatic full-package source verification is not a generic viewer-startup
prerequisite. RZ, ZB, RO, ST, NO, VM, and IP cancel the normal automatic
verifier through its real reducer operation and require an inactive,
nonquarantined verifier within five seconds before their first measurement
boundary. PT applies the same bounded quiescence before its package switch and
again before measuring the successor package. FC retains its application-cold
milestones before quiescence and requires its resident runtime-idle observation
afterward. VV also quiesces the unmeasured automatic run, then retains one
controlled cancelled setup run and one full measured verification request with
the existing active-throughout and typed completion evidence. This preserves
provisional-viewing semantics and scientific fail-closed behavior without
serializing every fresh process, or VV twice, through a package-scale scan.

Preflight and execution also share one exact clean-release executable gate.
The invoking `xtask` must embed the profile-bound revision, compiler, release
profile, and standard build settings; a stale cached preflight binary cannot
bless bundles for a newer runner. One canonical builder fixes every release
profile dimension and the host target. Execution builds the app and runs
conformance from a detached clean worktree of the profile revision, then
rechecks the live repository, detached source, and app digest before every
role launch. Private attempt-local output, open-ready, and cleanup paths are
cross-bound before any product process may launch.
Canonical numerical fact commitments normalize equivalent integer/float JSON
spellings before harness execution. An exact oracle-bound negative numerical
marker is retained as a product fidelity miss and does not abort the remaining
baseline; malformed commitments, markers, or process evidence remain fatal.

### Claim boundary

Normal packaged-application absolute gates establish viewer performance.
Component benchmarks diagnose mechanisms but cannot establish product
performance. Before any implementation package is accepted, EP-00 freezes:

- the owner-bound representative problematic package without recording its
  private path or unpublished metadata;
- clean immutable release revision;
- exact hardware, display, compositor scale, power state, storage/filesystem,
  CPU/GPU/VRAM budgets, and competing-activity rule;
- 1280 by 720 blocking qualification extent and 1920 by 1080 required
  exercise extent;
- application-cold and OS-cache conditions;
- cameras, planes, channels, transfer functions, interpolation, ray steps,
  lighting, wheel/drag samples, durations, and target scales;
- exact observable adapter/backend/driver and wgpu/Naga versions, requested
  features, device memory hint, and display/presentation mode;
- an independent LOD/fidelity oracle; and
- owner-approved absolute thresholds.

Thresholds come from required usability and hardware limits, not from the
current implementation. A relative speedup or comparison-viewer claim is not
required.

### State definitions

- Resident interaction: every mandatory brick in the scripted camera/plane
  envelope is GPU resident and foreground queues are idle before input.
- Nonresident refinement: the script intentionally crosses that envelope and
  requires a declared set of new bricks.
- Application-cold: a fresh process and empty Mirante CPU/GPU residency under
  the declared OS-cache condition.
- Current frame: reflects the newest admitted input generation at its render
  cutoff. Superseded raw input samples need not each produce a frame.
- Complete frame: includes every mandatory visible brick at its advertised
  scale. A complete coarse frame is labelled coarse and is not target-scale
  completion.
- Settled: the newest non-superseded input has a complete target presentation
  and all mandatory foreground work is idle.
- Freeze: during an active-input/loading interval in which a current
  presentation is expected, the main-loop heartbeat or current-presentation
  gap exceeds its gate, or the newest generation fails to settle before
  timeout. A settled unchanged view is instead required to produce no
  presentation churn.

Resident and nonresident results are never averaged together.

### Purposeful workload set

Use one owner-bound representative multichannel 3D/time package containing the
reported difficult s0 workflow, the promoted independent fixture for
correctness, and narrow storage/codec diagnostics only when a representation
decision requires them.

Do not build a dtype by mode by channel by projection by interpolation by LOD
matrix. Distribute material paths:

- cross-sections use the primary package's ordinary dtype and both voxel-exact
  and one SmoothLinear exercise;
- MIP is the primary resident volume-throughput case;
- DVR uses one nontrivial bounded opacity/ray-step scene;
- ISO uses one nontrivial threshold and gradient-lighting scene; and
- remaining dtype, projection, validity, and lighting combinations are
  correctness exercises rather than duplicate performance qualifications.

One focused off-axis perspective DVR/ISO correctness case compares rendered
world-distance extinction, hit depth, and asynchronous pick results with the
independent reference. It exists to catch ray-parameter normalization errors,
not to create a projection-by-mode performance matrix.

Required scenarios are:

| ID | Initial state | Action | Primary proof |
| --- | --- | --- | --- |
| RZ | resident cross-section, then resident 3D | two-second high-rate wheel zoom-in/out inside one declared LOD/residency envelope in each view | camera cadence, no freeze, bounded containment work, zero streaming/rebuild |
| ZB | resident fallback with a declared LOD boundary | cross the boundary once with both LODs resident, then in a separate phase with the target nonresident | LOD transition cadence, complete fallback, bounded new demand |
| RO | resident four-panel | continuous compound-angle active-plane rotation inside a declared resident guard | arbitrary-plane cadence and complete presentation |
| ST | resident four-panel | translate an ordinary axis-aligned active plane through a declared resident guard | common slice navigation uses the same bounded path |
| NO | nonresident four-panel | continue rotation/pan beyond the guard into a known partially overlapping target | delta reuse, bounded waste, progressive refinement |
| FC | application-cold | open the fixed four-panel product layout with independently pinned per-view/per-layer target scales, including one s0 view | cold I/O/decode/upload, LOD fidelity, per-panel and whole-layout coverage |
| VM | resident 3D | fixed MIP, DVR, and ISO scenes plus bounded camera motion | actual GPU and submission limits |
| PT | settled timepoint | advance once, return, and settle idle | temporal reuse, bounded prefetch, idle suppression |
| VV | controlled verification state | repeat one resident and one nonresident script with verification active and complete | foreground isolation |
| IP | fresh import | preprocess and publish the same accepted source recipe | format size, import resources, deterministic output |

RZ, ZB, RO, FC, and VM are the compact claim-bearing viewer core. ST, NO, PT,
VV, IP, the resource gates, and the 1920 by 1080 exercise remain mandatory
supporting qualification without multiplying every metric by the full tail
protocol.

### Mandatory permanent metrics

These metrics remain available in the product implementation, but they are not
all sampled on every frame. Minimal generation, scale, coverage, batch-bound,
and heartbeat/freeze signals stay always on. Detailed CPU/GPU timestamps,
byte/copy, cache, planner, and rebuild counters are bounded opt-in diagnostic/
qualification instrumentation with no synchronous readback; their disabled
path has a measured negligible cost. Keep both classes small and asynchronous:

- input generation to current presentation generation;
- admitted, coalesced, superseded-before-encode, encoded-but-dropped,
  sealed-obsolete-submitted, submitted, and presented display generations plus
  pending/in-flight display-batch peaks;
- main-loop heartbeat and current-presentation gaps;
- presented-frame intervals and final-input settlement;
- first useful, complete coarse, and complete target milestones;
- expected target scale, displayed scale, reason, and coverage per view/layer
  and for the complete four-panel layout;
- useful plane/sample bytes versus requested, encoded, decoded, and uploaded
  bytes;
- CPU demand/frame-coordinate/encode/submit time and asynchronous GPU
  payload-copy, plane, and volume-pass time;
- dirty target count, command encoders, color passes, queue submissions,
  completion notifications, in-flight application frames, backpressure
  deferrals, and superseded generations dropped before submission;
- hot-control bytes/writes and camera-triggered buffer, bind-group, allocator,
  record, or directory operations;
- object opens, index parses, range requests, physical reads, codec calls/time,
  and copy bytes;
- request joins, decoded/GPU cache hits, misses, evictions, uploads, and
  reuploads;
- cancelled/wasted count, encoded/decoded/uploaded bytes, and CPU time;
- page-table/hash updates and camera-triggered control rebuilds;
- plane-planner candidate visits versus exact intersected bricks and boundary
  support;
- peak accounted queues, handles, CPU bytes, upload bytes, and GPU bytes; and
- durable project/view commits during each interaction gesture.

Per-ray/sample diagnostics are temporary unless they can remain without
material synchronization or perturbation. Instrumentation overhead is
measured.

During EP-00, EP-01, and EP-04 development, temporary work-normalized kernel
evidence includes GPU time per output pixel and executed sample, descriptor
resolutions and hash probes per crossed brick, interpolation taps, skipped
samples, termination depth, and—where the available profiler/compiler can
report them—compiled instructions, registers, private bytes/spills, occupancy,
cache behavior, and divergence. These diagnose mechanisms; they are not
permanent telemetry or product acceptance substitutes.

Timestamp validation is itself an EP-00 gate. Payload-copy timestamps bracket
the actual copy commands in queue order. A `Queue::write_buffer` or CPU staging
operation outside that interval is not mislabeled as payload-copy GPU duration.
Because `Queue::write_buffer` still schedules queue-visible work, the selected
path either moves claim-bearing control copies into the timestamped encoder or
accounts for their interference through a separately named batch-level GPU
envelope and controlled differential. Bytes and CPU publication time alone are
not accepted as proof of zero GPU cost. Synthetic known-copy and empty-pass
controls must make placement errors visible before baseline evidence is
accepted.

Every asynchronous timing ticket binds the display-batch generation, pass
kind, and target view. A stale result cannot update current-frame metrics. A
batch with no copy or no relevant pass reports that interval as unavailable,
not a fabricated zero-duration operation.

### Structural zero-work gates

Resident zoom, rotation, and slice translation within their declared envelope
require:

- zero filesystem reads and codec decodes;
- zero dataset-resource creation for already joined BrickKeys;
- zero payload upload, reupload, eviction, or arena allocation;
- zero residency-directory or page-layout rebuild;
- zero GPU buffer, bind-group, or pipeline creation and only the bounded hot-
  control update required by the newest admitted camera;
- zero full demand traversal, plane-candidate enumeration, renderer static
  preparation, or cancellation churn after a bounded containment/signature
  check proves that the installed envelope still covers the view;
- zero UI wait for demand preparation;
- zero durable revision or undo/history entry per raw wheel/drag sample;
- exactly one durable commit at gesture end whose camera equals the final
  transient camera; and
- after any complete current-camera fallback exists, no incomplete
  data-bearing frame becomes current under any preview or target label.

A settled unchanged view additionally requires zero demand work, zero renderer
submission, and zero presentation churn.

Every admitted renderer cutoff with at least one dirty renderable target uses
the selected coordinated contract: one command encoder/submission by default,
or at most the fixed foreground/passive two-stage bound if EP-04 proves it
necessary. There is at most one completion notification per renderer
submission, no panel-count backpressure deferral, and no fixed-order starvation
of XY, XZ, or YZ. Egui/surface present and any API-required transfer/readback
submission are separately named and bounded. Superseded pending work obeys the
frozen encode/submit race policy, and an obsolete in-flight frame cannot cause
unbounded newest-generation latency.

For nonresident overlap:

- an overlapping brick is not reread, redecoded, or reuploaded;
- new bytes and work reconcile to the independently enumerated resource delta,
  bounded range coalescing, halo, and guard;
- cancellation waste stays within its fixed byte/time/count budget; and
- the prior complete or complete coarse current-camera image remains visible
  until replacement is complete.

Plane-demand candidate visits and planning work must scale with the exact
intersected cubic bricks plus the proven interpolation boundary and bounded
guard. They may not scale with the enclosing three-dimensional brick volume.

FC records milestones for every panel and every visible layer as well as the
complete four-panel layout. A fast active panel cannot satisfy the gate while
another visible panel is blank, stale, incomplete, or at the wrong scale.
ZB separately requires a resident boundary crossing to perform no I/O/decode/
upload and a nonresident boundary crossing to preserve current-camera complete
fallback while requesting only the independently declared new BrickKeys.

### Provisional engineering budgets

EP-00 must replace these with exact owner-approved workload-bound gates before
implementation acceptance. Until then they are design budgets, not product
claims:

- resident input-to-current-presentation p95 within one 60 Hz refresh interval
  and no observed current-presentation gap above two intervals;
- no UI-thread interaction task above 2 ms on the accepted hardware;
- resident MIP, DVR, ISO, and arbitrary-plane GPU work within the frame budget
  at the pinned fidelity and extent;
- after native window, GPU, and source admission, application-cold first useful
  data within 100 ms under the declared warm-OS cache condition, complete
  coarse coverage within 250 ms, and target-scale settlement below both a
  throughput-derived bound and an owner-approved usability ceiling;
- nonresident motion preserves frame cadence from complete fallback data while
  target settlement proceeds;
- zero silent target-scale violations and, after a complete fallback exists,
  zero incomplete data-bearing current frames under any label;
  and
- CPU/GPU peaks at or below their configured ledgers with no unaccounted
  payload-sized allocation.

If the accepted hardware/display cannot make a provisional value physically
meaningful, EP-00 records the hardware-derived value and rationale rather than
weakening fidelity.

### Evidence progression

Development measurements use three to five release observations to select or
reject mechanisms. They make no product claim.

Work-package evidence uses the smallest affected scenario set plus focused
correctness, mutation, cancellation, and resource checks. It records every
sample, failure, timeout, skip, and instrumentation state.

Final blocking qualification follows
[ADR-0005](../../decisions/ADR-0005-verification-and-zero-cost-ci.md):

- the exact protocol first moves into [Testing](../../TESTING.md);
- only explicitly named claim-bearing tail metrics use sixty independent
  clean release sessions and the zero-violation/Clopper-Pearson rule;
- supporting scenarios do not receive a Cartesian sixty-session expansion;
- a relative claim, if made, uses at least twenty interleaved independent
  baseline/candidate pairs and the fixed paired-bootstrap rule; and
- E4 exercises real OS wheel and compound-angle input on the same immutable
  candidate at externally observed 1280 by 720, inspects external compositor/
  window pixels for freeze, stale frames, LOD, and brick mosaics, and exercises
  the same build at 1920 by 1080.

Wrong, stale, partial, silently coarser, timed-out, automatically retried,
undeclared-manual-rerun, dirty-revision, hardware-mismatched, or incomplete
evidence fails. GPU timing requires asynchronous GPU timestamps rather than
CPU submission time.
Missing, skipped, cancelled, unsupported, or stale evidence also fails. There
are zero automatic retries; one manual rerun is allowed only for a recorded
external infrastructure failure under ADR-0005.

## Work-Package Sequence

The sequence is an acceptance dependency order. A later package cannot conceal
a failed earlier layer with a coarser LOD, fallback path, or more speculative
machinery. Every product activation is a hard cutover that deletes its
predecessor in the same package.

The sequence is architecture-led but kernel-informed: EP-00 establishes cost
truth; EP-01 compares representations through optimized successor-shaped
kernels; EP-03 through EP-05 land residency, frame orchestration, GPU kernels,
and interaction as vertical slices. There is no preliminary full optimization
of the predecessor and no deferred post-architecture shader cleanup phase.

EP-02 through EP-06 are staged acceptance layers behind one atomic normal-
product boundary. After EP-01 selection, they are integrated into one hard-cut
candidate revision in which the complete successor is the sole normal product
and the complete predecessor is already deleted. The normal packaged candidate
is then used to prove all five exit packages before that exact immutable
revision may be promoted. Failure rolls back the candidate revision through
Git; it does not reactivate an in-tree predecessor.

Isolated fixture, importer, reader, runtime, and renderer evidence may precede
that candidate, but no revision may expose a new-format/old-runtime, old-
format/new-runtime, partial-renderer, compatibility-adapter, or dual-reader
normal product. The hard-cut candidate updates Current State, Architecture,
Data Format, Testing, Development, and affected accepted decisions to label
its implemented-but-unaccepted facts honestly. EP-02 through EP-06 order the
evidence and dependencies inside that candidate; they are not separate mixed
product checkpoints.

### EP-00 — Workload, Fidelity, And Cost Truth

Goal: bind the real failure, repair measurement truth, and freeze all
acceptance gates before selecting the successor contracts.

Required work:

- Freeze the representative package, hardware/profile, exact interaction
  scripts, per-view cameras, per-view/per-layer target scales, render settings,
  budgets, metrics, and absolute thresholds.
- Add or reconcile low-overhead input/presentation heartbeat, per-view
  scale/coverage, correctly placed copy/plane/volume GPU timestamps, CPU stage
  clocks, frame-batch/submission facts, unique bytes, cache/residency, hot-
  control/rebuild, cancellation, and resource counters.
- Prove timestamp placement with known-copy and empty-pass controls. Remove or
  relabel every upload metric that does not bracket its claimed GPU commands;
  account queue writes and CPU staging separately.
- Capture the current four-target/three-in-flight behavior, per-panel
  encoder/submission/completion work, demand-body transition cost, buffer and
  bind-group creation, dense slab/tree work, and superseded-generation delay
  during real wheel and compound-angle input.
- Capture production shader call graphs and, where tooling permits, compiled
  instruction/register/private-memory/occupancy facts for plane, MIP, DVR,
  ISO, and mixed paths. A source-level runtime branch is not assumed to have
  removed unrelated private arrays.
- Record the current wgpu memory hint. A/B it through the EP-01 successor
  residency/upload harness only if EP-00 attributes a material allocation/
  residency cost to that dimension or the comparison is an already-admitted
  part of the selected harness.
- Build an independent projected-affine LOD oracle and independent image facts
  for cross-sections, MIP, DVR, ISO, interpolation, validity, and depth/order.
- Freeze the bit-exact or tolerance-bound numerical contract for sampling,
  transfer/compositing, RGBA, world-distance/depth, and picking, including
  boundary cases that can distinguish expression reordering.
- Run RZ, ZB, RO, ST, NO, FC, VM, PT, VV, and IP on the current baseline and
  attribute resident, cold, interaction, volume, and preprocessing critical
  paths.

Exit proof:

- Metrics reconcile and instrumentation overhead is below its accepted bound.
- Upload/copy, control publication, render-pass, and presentation intervals
  have nonoverlapping names and validated boundaries; the predecessor's
  mispositioned upload timestamp is not accepted as baseline evidence.
- The zoom freeze, wrong/coarse LOD, and visible-refinement reports have
  reproducible workload definitions and cannot be hidden by an average.
- The CPU, queue/submission, GPU-kernel, streaming, and presentation shares of
  each critical path are attributable enough to drive EP-01 admission.
- Every later gate has an owner-approved absolute threshold and fidelity floor.

Stop condition:

- Do not select a representation or implement another mechanism while frame
  generations, scales, coverage, GPU time, and unique data work are ambiguous.
- Do not treat increasing the in-flight limit, changing one ordered container,
  or moving work to another thread as closure for a measured ownership defect.

### EP-01 — Preprocessing/Rendering Co-Design Selection

Goal: select the smallest complete representation that can meet both
arbitrary-plane and 3D requirements before freezing the format.

Required work:

- Treat
  [`verification/viewer-performance-ep01-selection.json`](../../../verification/viewer-performance-ep01-selection.json)
  and its hash-bound files in `verification/schemas/` as the byte-exact
  candidate, trace, gate, and evidence authority. Candidate construction or
  qualification is invalid if that authority, any bound schema, its external
  profile binding, or the immutable revision differs.
- In the isolated successor importer, first replace total-work-unit-
  proportional journal/checkpoint bookkeeping with ordinal fixed records and
  bounded read windows. This benchmark/candidate substrate must be in place
  before a smaller-brick candidate is timed, but it does not alter the active
  product before the EP-02-through-EP-06 atomic hard cut.
- Build isolated complete-package 64-cubed and 32-cubed candidates using the
  same calibrated valid-aware pyramid and sharding/codec contract.
- Build an isolated successor-kernel harness against the target compact
  residency/descriptor interface. It does not optimize or bless the current
  presentation-body page table, dense control slab, or monolithic shader.
- Replay exact BrickKey traces for arbitrary planes, four panels, time, MIP,
  DVR, ISO, analysis, and verification.
- Measure index/package size, useful/fetched/decoded/uploaded bytes, request/
  decode/page counts, cache reuse, GPU page pressure, ray boundary crossings,
  preprocessing time, temporary storage, and peak resources.
- Recheck the storage-buffer/resident-directory design with production-shaped
  successor kernels. Every representation candidate receives the same common
  low-level floor: compact plane/MIP/DVR/ISO entry points, CPU-composed grid
  coefficients, exact page addressing, once-per-brick descriptor resolution,
  tight nearest/SmoothLinear sampling, and coordinated multi-target frame
  batching. The default bakeoff otherwise retains the current codec, shard
  ordering, buffer payload, and brick-level summaries.
- Open a codec, ordering, texture, compute-tile, or sub-brick-summary candidate
  only when EP-00 has named that exact dimension as a dominant avoidable cost
  and recorded its admission metric. Apply semantics-preserving, independently
  proved algebraic, descriptor, and traversal fixes before approximate or
  adapter-specific candidates.
- Freeze the persisted cross-layer contract: BrickKey semantics, scalar/
  validity and stored-summary facts, inner record, shard, pyramid, and format/
  recipe identities. Separately select the renderer contract: required
  descriptor inputs, cache-accounting formulas, qualification envelopes/
  headroom, and one GPU implementation. Runtime CPU/GPU budget values, buffer/
  atlas choice, bindings, packing, slots, and pipeline layouts remain renderer
  or hardware/config policy and never enter package/recipe identity.

Exit proof:

- One cubic representation meets every frozen EP-01 plane, volume,
  preprocessing, storage, and resource gate with the required measured
  headroom for the later normal-product stages; a projected or merely
  credible path is not an exit proof.
- Exactly one brick edge, shard/index contract, codec, GPU payload, lookup,
  and summary design is selected; all losing product candidates are deleted.
- The selection evidence is invalid unless the EP-00-observed common plane and
  volume layer cohorts have separate compact call graphs, no unrelated
  maximum-layer private arrays, one descriptor resolution per brick/footprint,
  exact page addressing, CPU-composed grid coefficients, and the coordinated
  multi-target frame harness. Spill/occupancy facts are required only when
  reproducible tooling exposes them; direct GPU gates and removal of unrelated
  call-graph/private-array state are unconditional. The exact general 64-layer
  fallback remains covered but cannot burden those common paths.
- The selected shader family has a fixed small pipeline ceiling. Pipeline
  compile/startup time and shader memory are recorded; every dtype, sampling,
  validity, or layer-count variant has a measured direct-kernel win and no
  end-to-end regression, and losing variants are deleted.
- Independent formulas prove object, index, checkpoint, CPU, GPU, queue, and
  working-set bounds.
- Candidate importer peak memory is independent of total brick count and
  remains within the accepted import ledger.

Stop condition:

- If no candidate can meet both plane and volume gates, revise the plan from
  the measured conflict. Do not activate two layouts or optimize only slices.
- Do not freeze the format or GPU contract from a fetch-only microbenchmark,
  an unoptimized common kernel, or a per-panel submission harness.

### EP-02 — Import, Pyramid, And Format Hard Cutover

Goal: produce the selected representation safely and efficiently.

Required work:

- Implement the calibration-aware valid-only reduction pyramid, centered
  transforms, cubic inner chunks, compact records/summaries, bounded indexed
  shards, and selected codec/order.
- Consume the already-proven ordinal fixed-record checkpoint substrate without
  reintroducing work-count-proportional memory.
- Update package identities, metadata, writer, strict reader, exact/scientific
  verifier, independent fixture/reader, import receipts, and publication.
- Reimport promoted and owner-bound packages through the new profile.
- Delete point-decimation, old geometry/index/profile readers and writers,
  legacy checkpoints, and the physical/semantic plane-subdivision mapping.

Exit proof:

- Independent s0, every-LOD, affine, validity, rounding, min/max, and package
  facts pass.
- Import remains deterministic, cancellable, resumable, source-preserving,
  below its CPU/RAM/temp/object bounds, and within the EP-00 time gate.
- The isolated successor has exactly one new-format reader/writer contract and
  no compatibility route; at the atomic EP-02-through-EP-06 activation it
  becomes the sole product contract and the current reader/writer is deleted.

Stop condition:

- Do not optimize the runtime around a candidate package that has not passed
  scientific, mutation, publication, and bounded-resource proof. The isolated
  EP-01 successor-kernel harness is selection evidence, not an activated
  product runtime.

### EP-03 — Shared Brick Streaming, Residency, And Frame Foundation Hard Cutover

Goal: make cold work proportional to unique new BrickKeys and make resident
work independent of storage, requirement-body reconstruction, and panel count.

Required work:

- Install the central BrickKey state machine, joined interest/priority, one
  decoded byte cache, and one GPU residency owner.
- Group reads by retained authenticated shard/index, coalesce bounded adjacent
  ranges, decode independently framed bricks into final cache storage, and
  upload through bounded persistent staging.
- Replace presentation-body GPU page layouts with persistent resident-brick
  authority and its compact GPU projection, patched only on residency or
  dataset-generation changes.
- Split bounded hot camera/layer uniforms from persistent residency/resource
  descriptors and payload bindings. Precompute immutable inverse-affine facts
  once per dataset generation/layer/scale and any declared time-varying grid
  authority; permit a camera event to update only the selected bounded view
  coefficients.
- Install the renderer-owned frame coordinator, latest admitted generation,
  one completion poll, shared uploads/control publication, and one encoded
  coordinated-batch policy for all dirty product targets. Backpressure counts
  application frames, not the number or fixed order of panels.
- Freeze command-encoding and `Queue::submit` ownership from EP-00 evidence.
  If they remain on the native event/UI thread, their complete worst-case slice
  must pass the UI-thread gate; otherwise that thread publishes and polls only
  bounded state while the renderer owner suppresses stale work. Moving work to
  another thread cannot hide queue growth or increase the pending-batch bound.
- Select and implement the EP-01 upload placement/slot policy, reusing bounded
  staging and coalescing allocator/copy operations when destination adjacency
  permits without enlarging requested bytes.
- Make every panel, mode, playback, readout, analysis, and verification
  consumer join the same entry.
- Delete the eight-entry physical fan-out cache, semantic sub-brick copies,
  presentation-body per-layer sparse page tables, renderer-specific camera
  page-layout preparation, stable-slot/32-key copy-on-write replacement,
  dense record-slab fallback, the 64-write publication choice, duplicate
  waiters, camera-triggered buffer/bind-group/static-control publication, and
  independent per-panel encoder/submission/backpressure ownership.
- Delete the general exact-byte allocator if EP-01 selects sufficient fixed
  size classes; one losing allocator cannot remain dormant in the product.

Exit proof:

- One new BrickKey causes at most one live read, decode, decoded allocation,
  and upload per relevant epoch regardless of consumers.
- Resident camera changes cause zero I/O/decode/upload/residency rebuild,
  buffer or bind-group creation, record-slab construction, full-body scan, or
  ordered-key reinsertion.
- One frame cutoff can encode every dirty 3D/XY/XZ/YZ target without a panel-
  count deferral, fixed-order starvation, or more than the accepted bounded
  frame submissions/completion notifications.
- The measured event/UI-thread slice satisfies its frozen gate. If encoding or
  submission is renderer-thread-owned, UI publication/polling is bounded and
  renderer queues retain the same latest-only limits.
- Cold unique bytes and calls reconcile; source security and currentness remain
  fail-closed; every cache and queue remains within its ledger.

Stop condition:

- Do not add speculative prefetch or more cache layers while duplicate unique
  work, payload copies, or camera-dependent residency work remains.
- Do not tune the deleted dense slab/tree path or merely raise the predecessor
  in-flight limit in place of the frame/residency cutover.

### EP-04 — Unified Plane And Volume Renderer Hard Cutover

Goal: render arbitrary planes and every 3D mode efficiently from the same
resident pages.

Required work:

- Implement a dedicated compact fullscreen arbitrary-plane entry point per
  dirty panel inside the coordinated frame batch. Use CPU-composed `f64`
  plane-to-grid coefficients, exact point page addressing, one resolved brick
  descriptor, tight native nearest/SmoothLinear sampling, boundary validity,
  and equivalent one-time channel output/clamping.
- Implement compact dedicated MIP, DVR, and ISO entry points plus the one
  necessary exact mixed fallback. Common paths cannot inherit unrelated
  maximum-layer arrays, dynamic mode scans, or private-memory spills.
- Implement the EP-01-selected brick traversal, with incremental DDA as the
  baseline candidate, plus once-per-segment descriptor resolution,
  conservative projected scissoring/skipping, adaptive fidelity-bounded steps,
  independently proved completeness-preserving DVR/MIP termination, and
  compatible-layer sharing without changing mixed-grid mathematics.
- Add independently proved transfer fast paths, conservative raw-space ISO
  reachability, exact-equivalent actual hit comparison where proved, hit-page
  gradient reuse, and neighbor-aware SmoothLinear bounds before considering a
  LUT, texture, compute, subgroup, or preintegrated candidate.
- Normalize perspective rays once or explicitly convert ray parameter to world
  distance; prove off-axis DVR extinction, ISO depth, and pick agreement.
- Retain asynchronous timestamps and picks without a UI wait.
- Delete per-brick draw orchestration, CPU reslicing, per-sample resident-list
  scans, repeated per-tap descriptor loads, repeated per-segment affine/slab
  setup, monolithic general-mode fragment dispatch, unconditional rerender,
  and any mode-owned payload/residency path.

Exit proof:

- Cross-section, MIP, DVR, and ISO correctness facts pass for affine geometry,
  dtypes, validity, interpolation, channels, projection, world-distance
  extinction/depth, picking, and lighting.
- Traversal/address facts pass for negative, zero/near-zero, internal-origin,
  face/edge/corner tie, partial-brick, singleton-axis, and maximum-address
  cases without missed, duplicate, nonterminating, or overflowing traversal.
- Fixed resident arbitrary-plane kernels and VM meet their direct GPU, CPU
  submission, correctness, and pinned-extent gates. End-to-end RZ, ZB, RO,
  ST, and FC acceptance belongs to EP-05 after its interaction/LOD mechanisms
  exist.
- Direct-kernel evidence reports output/sample-normalized time and every
  reproducible compiler/profiler fact exposed by the accepted toolchain.
  Common paths meet the EP-01 pipeline/private-state ceiling; each retained
  specialization has a material win, and all losing variants and experimental
  representations are deleted. Missing vendor-only occupancy data cannot
  replace or invalidate the unconditional direct GPU and structural gates.
- All dirty product passes for one cutoff execute through the accepted
  coordinated frame batch; one pass per dirty panel never means one encoder,
  queue submission, or completion callback per panel.
- Page lookup and descriptor resolution scale with crossed bricks plus proved
  interpolation/gradient boundary support, not with executed interior samples;
  no tight sample loop reloads a full affine or immutable descriptor.
- Lookup cost is independent of unrelated resident-brick count within the
  accepted bound, and settled unchanged views submit no volume work.

Stop condition:

- If a resident mode fails, repair its actual GPU/CPU critical path. Do not
  lower fidelity, change LOD, or mask it with streaming behavior.
- Reprofile after the structural and independently proved semantic math work.
  Do not admit a texture, tiled compute, subgroup, `f16`, LUT, or
  reconstruction-changing mechanism without its named remaining bottleneck
  and admission proof.

### EP-05 — LOD, Interaction, And Complete Presentation Cutover

Goal: make camera interaction immediate and refinement visually stable.

Required work:

- Install independent per-view/per-layer projected-affine LOD with hysteresis
  and observable target/fallback facts.
- Install exact plane-cell demand, global deduplication, active/passive
  fairness, and a small bounded orientation-neutral guard.
- Separate transient wheel/drag camera updates from one durable gesture commit.
- Coalesce raw input to the newest transient generation before frame encoding;
  apply the frozen encode/submit race policy, meter stale preparation, and
  bound the latency contribution of already in-flight frames.
- Render the current camera immediately from complete resident fallback data,
  then switch each view only to complete target coverage.
- Preserve overlapping entry interest across generations and cancel only
  obsolete work at bounded checkpoints.
- Delete global/profile-driven LOD, whole-scope replacement, 17/16 wrapper/
  promotion and admission-cursor machinery that the central entry table makes
  obsolete, prepared retained-union and per-scope recovery-set machinery,
  stable predecessor/refinement tokens, the hidden fifth target, arbitrary
  key-order presentation, and hidden incomplete mosaics.

Exit proof:

- RZ has zero UI wait, I/O, decode, upload, eviction, GPU-table rebuild, and
  per-event durable revisions in both the cross-section and 3D runs; it also
  has no camera-triggered buffer/bind-group/body rebuild or obsolete queued
  frame chain, and its maximum gap and settlement gates pass.
- RZ, RO, and ST retain at most one replaceable not-yet-encoded display batch;
  superseded pending generations encode, submit, and present zero work, and no
  stale generation becomes current.
- Input that arrives during encoding follows the frozen discard-or-one-sealed-
  frame rule; encoded-but-dropped and sealed-obsolete-submitted waste remain
  within their gates and cannot form a chain.
- ZB passes both the fully resident and nonresident LOD-boundary phases without
  blocking camera presentation or exposing incomplete data.
- Four-panel FC reports and installs the independently expected scale for each
  visible view/layer, including the s0 case, and passes both per-panel and
  complete-layout milestones without panel-count backpressure deferral or
  fixed-order starvation.
- RO, ST, and NO satisfy the intersected-brick planner bound and show no
  incomplete data-bearing mosaic; overlap is reused and new work is
  proportional to the declared delta.
- Resident ST additionally satisfies every resident structural zero-work gate;
  ordinary slice translation cannot hide reread, redecode, reupload, or
  camera-dependent static rebuilding.

Stop condition:

- Do not add a broad velocity predictor, adaptive controller, mixed-LOD
  presentation, or tiled switch under this plan. A measured failure of the
  simple complete-LOD policy requires an explicit plan revision.

### EP-06 — Time, Background Work, Tools, And Contention

Goal: make all remaining consumers coexist without weakening the fast core.

Required work:

- Add one-next-timepoint bounded prefetch through ordinary BrickKeys.
- Enforce interaction priority and explicit post-interaction grace for
  analysis and verification without starvation or polling.
- Complete source verification promotion without reread/redecode/reupload of
  unchanged bricks when identity proof permits.
- Revalidate latest-only asynchronous picking, readout, crosshair, ROI, and
  measurement against exact presented residency.
- Audit all restored controls and remove or accurately label anything
  ineffective.

Exit proof:

- PT and VV pass reuse, progress, resource, and interaction-tolerance gates.
- Verified promotion preserves matching resident work.
- Tools use the presented shared residency and create no synchronous readback,
  private cache, or scene framework.

Stop condition:

- Capability or background work cannot gain a slow alternate product route to
  pass its own test.

### EP-07 — Qualification, Deletion, And Closeout

Goal: prove the complete normal product and leave one understandable
architecture.

Required work:

- Run focused public, local scientific, storage, mutation, cancellation,
  resource, trusted-GPU, and product checks for every changed authority.
- Move the exact approved performance protocol into Testing and run final
  trusted-local qualification on one clean immutable revision.
- Perform E4 real-display validation with real wheel and compound-angle input
  on the owner-bound workload.
- Search for and delete predecessor profiles, readers/writers, checkpoints,
  semantic subdivisions, scope graphs, page layouts, cache paths, benchmark
  candidates, temporary counters, ineffective controls, and fallback routes.
- Delete the monolithic unrelated-mode shader entry, per-panel submission and
  backpressure ownership, camera-body control rebuilding, dense record slabs,
  discarded pipeline variants, losing buffer/texture/compute candidates, and
  obsolete or mislabelled timing fields. Retain only bounded diagnostics that
  proved cheap and have a current owner.
- Update Current State, Architecture, Data Format, Testing, Development, and
  accepted decisions; then delete this active plan. Git history remains the
  archive.

Exit proof:

- Every owner-approved absolute gate and structural zero-work gate passes at
  the same named revision, workload, hardware, fidelity, extent, cache state,
  and protocol.
- The normal product shows no freeze, unexplained coarse LOD, visible brick
  assembly, hidden/undeclared/compatibility or silent-fidelity fallback,
  repeated GPU error, or incomplete data presented as scientific zero.
- Repository checks find one current format, BrickKey authority, scheduler,
  decoded cache, GPU residency directory, frame coordinator, bounded shader
  family, renderer, and presentation model.

## Principal Risks

- A smaller cube can reduce plane bytes while multiplying request/page work;
  a larger cube can help 3D while wasting cold plane bytes. EP-01 measures the
  complete trade and prohibits a slice-only decision.
- A new pyramid changes preview values and transforms. Independent expected
  facts must precede activation, and s0 remains exact.
- Compact summaries can cause incorrect skipping around validity,
  interpolation, transfer functions, multichannel composition, or ISO
  thresholds. Every bound is conservative and independently tested.
- A residency-wide hash can grow or accumulate tombstones. Its load factor,
  maintenance work, key scope, byte formula, and worst-case probe bound are
  frozen before activation; maintenance cannot run synchronously in a camera
  event.
- Pinning complete fallback and target sets can exceed VRAM. Working-set
  formulas and typed capacity failure are required; silent LOD reduction is
  forbidden.
- More decode concurrency can worsen interaction through memory bandwidth or
  queue contention. Duplicate work is removed first, then bounded concurrency
  is selected from measured headroom.
- Format reimport can increase preprocessing time or checkpoint pressure.
  Ordinal bounded state, deterministic publication, and import gates prevent
  moving viewer cost into an unusable producer.
- Instrumentation can perturb the path it measures. Expensive counters remain
  diagnostic and their overhead is recorded.
- Shader specialization can trade branch cost for compilation time, shader
  memory, and a Cartesian maintenance problem. EP-01 fixes a small measured
  ceiling; common paths and one exact fallback are retained, all losing
  variants are deleted, and no pipeline compiles during interaction.
- One coordinated batch can let an expensive passive pass extend active-view
  GPU completion. The active pass is encoded first, dirty/pass count is
  bounded, timestamps remain per pass, unchanged panels add no work, and a
  measured failure may select the fixed two-stage bound rather than restoring
  four independent unbounded submissions.
- CPU-precomposed `f64` coefficients, integer page addressing, the selected
  once-initialized traversal, descriptor caching, and reduced-fact termination
  continuation can expose boundary or stale-state defects. Independent affine,
  half-open boundary, perspective, validity, missing, and completeness facts
  gate every optimized kernel.
- Transfer LUTs, hardware filtering, reduced precision, analytic gradients,
  and larger/preintegrated ray steps can change values at scientifically
  important boundaries. Independently proved semantic fast paths come first;
  approximate candidates require explicit error contracts and never inherit
  acceptance from a faster image alone.
- Rewriting working first-overhaul mechanisms without evidence can regress
  correct behavior. Current components remain baselines and are replaced only
  at named hard-cut deletion points.

## Completion Definition

The plan is complete only when EP-00 through EP-07 exit proofs pass, the
normal product is product-validated on the owner-bound workload, all absolute
and structural gates pass at one clean immutable revision, and every
predecessor/temporary path is deleted.

Implemented source, component benchmarks, a smoother small fixture, a lower
displayed LOD, or a comparison-viewer observation cannot close the plan.
Rollback is by reverting a complete hard-cut work package, never by retaining
a runtime selector or compatibility route.

The owner granted implementation authorization on 2026-07-17. Source, format,
fixture, workflow, generated-asset, and repository-setting changes remain
subject to the ordered work-package gates above; authorization does not waive
an exit proof, stop condition, or final product-validation requirement.
