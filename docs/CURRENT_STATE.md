# Current State

Last reviewed: 2026-08-02

Mirante4D is public, pre-alpha academic research software. Persisted formats
and APIs can change through explicit hard cutovers; there is no supported
public release or public full microscopy dataset yet.

## Implemented Product

- Native Rust desktop application for Linux x86_64 using `wgpu`, `winit`, and
  `egui`.
- Strict experimental `m4d-science-1.0` datasets in the
  `m4d-zarr3-local-1.0` OME-NGFF 0.5.2/Zarr v3 sharded storage profile.
- Canonical framework-neutral domain, identity, project-model, application,
  dataset-catalog, render-API, and settings boundaries.
- MIP, DVR, and ISO intensity rendering with per-channel controls,
  orthographic and perspective projection, voxel-exact and smooth-linear
  sampling, flat and gradient-lit ISO, and attached or detached ISO light.
- One bounded, byte-accounted semantic-resource runtime for 3D, linked 2D,
  playback prefetch, histogram, interactive readout, and analysis demand.
- TIFF/OME-TIFF import plus exact whole-layer time traces and numeric box
  intensity statistics, with atomic table/plot project artifacts and CSV copy.
- Dataset-optional native startup with a welcome launcher for opening an
  existing package or configuring explicit per-channel TIFF preprocessing.
- Linux release-directory, tarball, and AppImage build paths.
- No segmentation or derived-label subsystem.

The workspace has eighteen packages: seventeen `mirante4d-*` crates plus
`xtask`. `mirante4d-storage` owns the active package catalog, bounded
validation and reads, and create-only package publication.
`mirante4d-import-pipeline` is the active bounded, cancellable, restartable
TIFF/OME-TIFF producer. Native composition now owns its bounded worker results,
latest-only progress, cancellation, and explicit shutdown; egui owns no import
thread or channel. `mirante4d-ui-egui` owns shared egui visuals,
application-problem presentation, and transient UI drafts and interaction
state. `mirante4d-render-wgpu` is the sole product renderer. The unpublished
`mirante4d-render-reference` CPU oracle is test-only.

Native clean window-manager close is accepted on its first close event.
Dirty close alone is cancelled for an explicit Save, Discard, or Cancel
decision; successful Save or Discard authorizes one later close. Native
SIGINT/SIGTERM handling sets one monotonic latch, wakes egui, and issues one
prompt-free close request. `on_exit` is the sole exit-time shutdown owner and
performs normal cancellation and joins; there is no frame-loop project-store
join, second synthetic close, guardian, or competing shutdown path.

Earlier-launch provisional autosaves are discovered only as bounded canonical
project-store locators. When any exist, startup opens the normal recovery
panel and visibly reports that unsaved work is available. Recovery remains an
explicit user choice and uses the application reducer and project-store actor
to validate the selected store. It does not wait for a whole-package dataset
scan. A recovered provisional branch opens dirty and still requires Save or
Save As; startup never silently opens, repairs, advances, or deletes it.

The normal native process now creates one dataset-independent application
shell before any package is selected. Cancelling a package chooser leaves the
window open. The centered launcher can open a strict `.m4d` package or start
the same preprocessing workflow available from an open workbench. A dataset
session is created only after a package opens successfully; no dummy catalog
or runtime exists in the empty state. Successful preprocessing transfers its
validated create-only publication directly into the ordinary source-open
boundary.

Preprocessing source structure is user-authored rather than inferred. Each of
one through 64 named channels explicitly selects a single 3D TIFF, a folder of
3D TIFF timepoints, or a folder of 2D TIFF Z planes. Immediate TIFF children
are non-recursive and lexicographically ordered; filenames carry no channel,
time, or Z semantics and unrelated channel filenames are valid. Asynchronous
metadata inspection reports file progress, reads no pixel payload or whole-
source hash, and suppresses stale row-generation completions. Channel
validation requires unique normalized labels and common logical shape and
dtype. The labels are persisted in optional canonical display metadata and
become runtime layer labels after reopen.

One process CPU broker now owns the managed-byte ceiling above optional
dataset and preprocessing sessions. Purpose categories are accounting labels,
not percentage partitions. An open dataset installs a derived foreground
decode/queue reserve; preprocessing borrows the remaining capacity and
atomically retains one maximum phase-minimum progress lane for the run. Unused
progress capacity is protected from both ordinary background and foreground
growth, and converts to normal leases without double charging. Ordered workers
narrow their sliding window and retry transient capacity refusal instead of
failing a running import. The product has no import-local working-memory
selector.

The importer now indexes the explicit reviewed TIFF manifest once, traverses
admitted native strips/tiles in source order, decodes each admitted native
chunk once into a two-file canonical base cache, and derives base chunks,
pyramids, and the scientific content address without a second TIFF decode or
raw-source hash pass. One canonical owner may additionally prepare exactly one
future unit cache while it processes the current unit. That optional cache is
admitted only from CPU bytes and filesystem headroom left after the serial
progress path is protected; it has no spool, hashing, shard, journal, or
publication authority and is consumed strictly in unit order. Its worker uses
one slot from the existing system-parallelism ceiling, and temporary resource
contraction simply returns scheduling to width one. Automatic no-data
resolution is durable before decode-ahead begins. No second future lane or
user concurrency control is shipped.

The checkpoint authority remains the current cache, at most one ordinal-bound
future decoded cache, and one four-file batched-durability spool.
Eligible one-plane
files, normalization, downsampling, scientific-tile preparation, and inner
encoding use byte-ledger-admitted workers while one owner commits deterministic
order; multipage inputs retain one exact-once streaming decoder.
Descriptor-bound checkpoint files and shared positional readers prevent path
reopens proportional to the worker count. Package publication passes validated
canonical inner encodings directly to the sharded writer, and staged
scientific validation reads each present base brick once. After every staged
object closes, one counted Linux filesystem barrier replaces per-object file
syncs; deepest-first staged-directory syncs, validation, create-only rename,
and destination-parent sync retain the publication ordering. The writer retains
that exact/scientific capability through create-only rename and, within the
cooperative local destination-parent namespace assumed by the storage
contract, proves the public destination is the same directory by filesystem
identity and hands a linear capability to the importer. Product admission
consumes it through a bounded inventory/snapshot/inventory currentness check
and opens the imported package directly as verified; it does not repeat
SHA-256 or scientific validation. That consumer issues a storage execution
receipt whose separate strict-open phase deltas reconcile the two inventories
and proof-derived snapshot sweep with zero codec decodes; product evidence also
observes successfully started and failed ordinary-verifier runs instead of
inferring their absence from accepted progress alone.

Reviewed no-data imports now use one first-volume resolver and typed guarded
policy at every product boundary. The UI independently enables automatic
spatial-mask detection (all admitted dtypes), manual uint8 entry, and exact
constant-Z-plane hiding. Detection reads only channel zero at timepoint zero.
The first uniform `5 x 5 x 5` block resolves an exact typed value; a complete
second pass reconstructs every face-connected equal-value component containing
such a block. Equal-valued voxels disconnected from all seeds remain valid. No
uniform block is an ordinary no-match. The fixed reconstructed geometry and
plane indices then apply unchanged to every channel/timepoint. Reconstruction
uses row-packed exact-value, seed, and visited/final-mask bits plus a fixed
in-memory run window and checkpoint-owned spill file. Spill I/O is fixed-size,
its worst-case bytes are included in scratch preflight, and explicit
checkpoint reset recognizes and removes that owned transient.

Automatic base chunks and scientific-content tiles classify fixed spatial-mask
membership through a clipped one-voxel halo; manual uint8 mode alone performs
dataset-wide exact-value classification. Both exclude hidden planes from that
morphology. Constant planes remain strictly plane-local. Every explicit-
validity LOD uses valid-only aligned factor-two means, half-up integer or finite
float rounding, value-support dilation, and geometric plane support.
Fused workers read ordinary parent pixels separately from expanded
validity-only halos; packed-index uniform facts avoid mask decoding, and
halo-only parents never read pixel payloads. Validity-aware mean LODs publish
axis-aware centered transforms. An automatic no-match with no hidden planes
emits no validity arrays and retains point decimation and origin-anchored
transforms.

Source inspection admits grayscale `uint8`, `uint16`, and finite `float32`
TIFF/OME-TIFF pages using uncompressed, LZW, Deflate (current or old TIFF
code), or PackBits compression. JPEG, WebP, Zstd-in-TIFF, fax, and other
compression paths are rejected because their decoder workspaces are outside
the audited memory authority. A cancellation-aware, fixed-read raw preflight
bounds every primary IFD, eager decoder field, native chunk table, and the
65,536-page retained-decoder authority before `tiff::Decoder` is constructed.
Inspection and ingest compare the opened file descriptor's
device/inode/generation facts with the guarded path generation before and
after use; final structural revalidation also requires the reviewed generation
without rereading payload bytes. The canonical decoded-source digest is fused
into ingest and is explicitly an identity of admitted values, not an
authentication of biological authorship. None of these changes alters the
scientific identity profile.

`mirante4d-analysis-core` owns exact `uint8`, `uint16`, and finite `float32`
intensity statistics and artifact payloads. `mirante4d-analysis-runtime` runs
those operations through the shared dataset scheduler with a fixed two-block
window, lower priority than interactive work, scoped cancellation, and stale
result suppression. The product exposes whole-layer summaries over time and a
numeric axis-aligned box at the current timepoint. A complete table/plot pair
becomes visible only after one atomic project-store commit, and authenticated
pairs are restored when the project reopens. There is one live canonical
application reducer and one canonical project model.

The viewer hot path now has one private renderer `ResidencyOwner`. It owns one
persistent, byte-accounted logical WGPU storage-buffer arena segmented across
at most four maximal bindings, one exact physical-resident map, LRUs, the
bounded mapped staging pool, an optional fixed compaction scratch buffer
inside surplus transfer budget, fixed page records, and one sparse open-addressed
directory shared by every target and render mode. Per-segment exact-byte
allocators report aggregate free and largest-contiguous ranges separately.
The logical payload allowance is no longer allocated eagerly: normal startup
commits at most 64 MiB of usable payload backing plus minimum dummy bindings,
and exact-union preflight grows selected segments toward bounded geometric
targets no more than 128 MiB beyond their exact per-segment placement
high-watermarks. Empty segments are preferred before a populated-prefix copy.
Growth preserves offsets and bytes, rebuilds all payload bind groups, and
remains capped by the configured logical maximum.
Diagnostics distinguish logical capacity, committed usable capacity, physical
buffer allocation, resident payload, free/placeable spans, growth count, and
growth-copy bytes.
Directory entries carry the exact compact layer/time/scale/cell key and
page-record index, so masked-hash collisions and tombstones are resolved by
exact key comparison rather than a per-presentation table or a resident-list
scan. `uint8`, `uint16`, and `float32` samples are loaded directly from the
arena. All-invalid bricks remain metadata-only residents.

Presentation state retains only an opaque resident-frame lease. Immutable
requirement bodies, exact pin counts, payload slots, transfer state, and
eviction policy remain inside `ResidencyOwner`; neither the app nor the CPU
data plane mirrors them. Decoded leases enter one bounded renderer-global
first-seen queue. Each live opaque frame lease has a derived relevant-order
index, so a resident target does not scan up to 65,536 unrelated offers on
every execution or dirty query. Completion and retirement remove an exact
batch from the global queue and all live derived indexes.

Destructive residency changes publish bounded, sequenced eviction events that
remain retryable until acknowledged. A CPU lease handle is reoffered directly
when it still exists; otherwise the normal dataset dispatcher submits the one
missing demanded key. The app keeps no GPU-resident set or per-scope recovery
set. Event-capacity refusal is typed and transient for render retry, and
transactional preflight accounts for same-batch readmissions before any
directory mutation or queue submission.

The renderer traverses volume bricks, skips missing, all-invalid, and
mode-noncontributing bricks, applies DVR early termination, and jointly
integrates multichannel DVR even for smooth sampling or mixed affine grids.
DVR opacity uses physical world-step length for off-axis and affine-transformed
rays. Exact FirstThreshold ISO picks include the six-sample gradient halo
before reporting complete. Reused staging slots clear only alignment gaps and
tails, not payload spans that the upload immediately overwrites. GPU timestamp
results and volume picks are asynchronous; neither requires the UI thread to
wait for GPU mapping. One first-cause terminal GPU latch classifies device
loss, out-of-memory, backend-internal, and validation failures before later
frame, poll, submission, or mapped-buffer work; it creates no fallback or
device-recovery epoch.

Normal native startup queries memory facts from the exact eframe-selected
adapter before deriving default settings. Linux Vulkan reports adapter
identity, device type, device-local heap bytes, and optional
`VK_EXT_memory_budget` budget/usage facts. Only a discrete-device result can
supply the existing dedicated-memory recommendation policy; shared/unified
heaps remain labelled shared, and unsupported discovery retains the explicit
unknown-device default. Persisted settings remain explicit overrides, and the
same stored facts drive the `Use recommended settings` action.

Native existing-device startup no longer blocks product command admission on
driver pipeline compilation. After fixed buffers and layouts exist, one
capacity-two renderer worker compiles dedicated Plane, MIP, DVR, ISO, and
Mixed color pipelines, then the separate Pick compute pipeline. The UI
consumes at most one ordered readiness event per turn, keeps
demand/decode/lease work live, and cannot publish rendered 3D, cross-section,
or pick currentness before the corresponding capability exists. A terminal
no-data 3D result is not GPU work: it publishes independently through the same
surface-generation check and cannot manufacture pixels or accept a stale
generation. A truthful renderer-initializing projection repaints at a bounded
50 ms cadence. The first failing shader/pipeline operation and typed cause are
retained. Closing
during an uninterruptible cold driver call cancels remaining stages and
detaches rather than waiting on the UI thread.

Per-target control now contains camera, layer, mode, grid, and global-directory
parameters only; it does not rebuild or own residency layout. Resident
camera motion therefore reuses the same directory, page records, arena
allocations, and pins. One private renderer frame coordinator owns the four
fixed target fronts, a private eager 3D replacement, texture revisions,
currentness, target retry, completion leases, recording order, capture
freshness, and color submission. Changed targets record active first, linked
2D next, and passive 3D last into one color encoder and one queue submission.
Application presentation records are accepted only for the target's current
surface generation and exact extent; a stale generation can retain its old
display identity for truthful diagnostics but cannot become current.
An exact 3D replacement swaps only after its coordinated color submission;
the prior complete front remains visible and pickable until then. Same-body
resident movement rebinds an opaque lease and shared pin cohort without
entering residency transfer or arena allocation.

Resident 3D color work is no longer assumed cheap merely because residency is
complete. The application classifies the exact visible-layer/mode/scale/output
profile through one bounded work controller. It receives every complete
full-volume navigation rung retained from terminal toward fine plus the
camera-local exact target, rejects incomplete, nonresident, finer-than-target,
or interaction-unsafe bodies, and selects the finest remaining body. Only the
terminal rung may bootstrap a cold renderer from retained CPU payload; it is
not reported resident or presented until its upload completes. Every preview
request uses the physical 3D panel extent; reduced internal textures, dynamic
resolution, preview upscaling, and visible timing probes are deleted. One
gesture freezes the selected per-layer scale map, so timing or resource
arrival cannot make its LOD or resolution flicker.

The private 3D replacement renders the selected exact per-layer-map body through one
renderer-owned latest-only worker. It submits and waits for one bounded
horizontal row batch at a time, adapts the next batch toward a 3 ms target,
and checks cancellation between submissions. Partial rows publish no frame,
capture, pick target, or texture revision; only a complete matching candidate
can swap atomically. Hidden work advances while the visible UI is idle and
requests one wake for exact handoff rather than one repaint per row. The
same preview remains visible and labelled after release, with no intermediate
`pending output` transition. A newer camera waits behind at most one submitted
batch. Application staging must explicitly authorize the final promotion
after its own fallible preflight, which closes the worker-completion race.
GPU timing may supply conservative initial work facts but cannot change
visible navigation policy. ISO timing identity includes one of 32
display-threshold bands. Inspector and automation report native preview,
direct output, and exact screen-row progress rather than data-tile counts.

The terminal scale is a geometry contract, not a fixed ordinal. New imports
factor-two reduce spatial dimensions until both the maximum dimension is at
most 64 and the spatial volume is at most 262,144 voxels per layer/timepoint.
Small datasets may remain S0; the largest representable dimension terminates
within the profile's 64-level bound. Every visible layer's terminal body
remains in the ordinary global requirement union. When one canonical resource
covers the complete layer, renderer control names that page directly and
volume kernels bypass repeated sparse-directory hashing. Voxel-exact rays
reuse its decoded address, and smooth-linear rays reuse the same address for
all eight taps.

The static 3D navigation planner keeps that terminal map mandatory and admits
successively finer coherent full-volume maps in deterministic max-min
per-layer order while an optional tail bounded to one quarter of the
configured logical payload/resource limits, at most 512 MiB and 16,384
resources, fits the exact global union. Each candidate wrapper ranks only its
own one-scale-per-layer body as frame-required. The rest of the same canonical
aggregate is a permanently dormant residency-prefetch suffix: the sole
renderer residency owner may upload it terminal-to-fine, but it cannot affect
coverage, control, pixels, fidelity, or prefetch promotion. Playback retains
its separate lockstep quality order.

The transient mailbox is the sole render-revision allocator and now allocates
the render API's `FrameIdentity` directly; the duplicate application
`RenderIntentRevision` value type is gone. The fixed target identity is
`mirante4d_render_api::PresentationTarget::{ThreeD,Xy,Xz,Yz}`. The
application's `PresentationSlot` name is only a type re-export, so no target
identity allocator or numeric conversion remains at that boundary.

One monotone sequence supplies unique values, while the mailbox retains
independent latest identities for 3D and linked 2D. Camera/3D-viewport input
advances 3D; cross-section/linked-viewport input advances linked 2D; shared
render changes advance both once. A gesture's durable commit keeps its final
sample identity instead of allocating a second frame. A linked-only change
therefore leaves an unchanged visible 3D front and hidden exact candidate
intact.

One accepted `FrameIdentity` may first render from a provisional immutable
requirement body and later replace it with the latest worker-prepared body.
That cutover does not manufacture another input revision. The application
asks the renderer whether the exact frame, extent, body identity, and prefetch
role have already been presented; frame identity alone cannot adopt retained
pixels. The renderer accepts a different same-frame body, retains its
resources before releasing the predecessor, rebuilds coverage for that body,
and treats it as dirty color work. The same-frame eviction-regression guard
applies only while the exact rendered body remains active. The 3D hidden
candidate remains exact-only and atomic.

Application demand is signature-diffed and split by 3D, linked-panel,
playback, and analysis scope. Every visible layer independently selects LOD
from its affine cell footprint in the physical pixels of the actual 3D or
cross-section view. Each nonempty linked-panel body starts with a complete
coarsest full-volume navigation floor as its first-useful prefix, then the
directly selected plane target, then an optional bounded rolling fine guard.
The final deduplicated union and its exact host/GPU obligations are admitted
once; a discarded trial does not retain a false ledger charge. The renderer
receives an explicit target-to-coarser scale chain per visible Plane layer.
Already-resident intermediate scales may be sampled, but they are not load
dependencies and do not delay the latest selected target.

Playback is transient and now has one authoritative `PlaybackSession`.
Warmup selects the finest sustainable full-volume scale map for the chosen
1–24 FPS rate, active layout, exact CPU/GPU overlap, startup runway, and
bounded rotating slot ring. The admitted session freezes that scale map and
its target set until Pause or Stop; temporal length does not change
steady-state memory. Each successor advances only after its complete
same-scale bundle is resident and renderable. A late successor applies clock
backpressure and leaves the predecessor visible instead of flashing a coarser
rung, skipping time, blanking a target, or publishing a partial four-panel
layout.

Temporal frame contracts contain no camera or linked-plane geometry. The
fixed-scale full-volume bodies therefore survive camera and linked-plane input
without rebuilding or rebasing the slot ring. Four-panel playback also gives
the linked panels geometry-independent bodies from that same session contract.
That resource independence is implemented.

One private application `ComposedPresentationScheduler` now owns semantic
presentation transactions. Its temporal, 3D-spatial, linked-spatial, and
retained-quality coordinates advance independently and are accepted
componentwise monotonically. A ready due playback successor is composed with
the latest spatial snapshots rather than waiting for newest whole-layout
spatial settlement, so held 3D or linked input cannot turn that temporal action
into `Wait` or rebuild the slot ring.

Every transaction first assembles the complete fixed-shape logical target set
from newly prepared and exactly compatible reused members. Only then does it
derive the physical target delta and exact atomic publication group. An empty
delta completes without artificial renderer work, while a partial delta cannot
be mistaken for an incomplete four-panel frame. The renderer's private
`FrameCoordinator` remains the sole GPU target, queue-submission, completion,
and texture-swap authority.

Pause or Stop starts a generic retained-quality transaction after the reducer
has established the final post-reconciliation render revisions. The last
coherent playback front stays visible until one complete, renderable stationary
plan for the active layout is ready; future playback-only demand is released
without routing the visible image through a coarse navigation rung. Recoverable
candidate failures are scoped and latched by transaction fingerprint instead
of blanking, downgrading, or repeatedly retrying the front. The
[composed presentation scheduler plan](plans/active/VIEWER_COMPOSED_PRESENTATION_SCHEDULER_CUTOVER.md)
owns the completed design and evidence handoff.

Incremental linked-2D settlement keeps current-geometry floor coverage
separate from installed exact-demand identity; fallback can make the newest
geometry renderable but cannot make a changed scale/body exact. One bounded
latest-only worker performs exact camera/plane candidate testing,
contribution ranking, canonicalization, scope deltas, and render-requirement
preparation. Newer scale or guard requests replace obsolete work and plan the
latest projected target directly. The UI swaps completed immutable artifacts
instead of rebuilding a large cohort and prepares no renderer page table or
GPU slot layout.

For 3D, sustained orbit replaces one pending request and cancels the older
traversal while retaining the last complete uniform presentation. A complete
resident target body still rebinds without decode, transfer, allocation, or
residency rebuild, but it renders natively during input only when its exact
work profile is conservatively safe. Otherwise current camera geometry uses
the finest complete resident target-eligible navigation rung inside the native
interaction-work envelope; the terminal rung is only the last emergency floor.
Exact replacement then follows the asynchronous hidden row-batch route above.
A full-volume resident cohort bypasses camera planning entirely. A bounded
17/16 camera guard appends at most one quarter of the primary resources plus
two as a dormant prefetch suffix. It may decode and enter global GPU residency
after visible work, but does not affect readiness, coverage, fidelity,
rendering, or picks until an O(1) scalar promotion. Contained camera and
full-volume reuse promote the installed dataset/render wrappers without a
membership walk or residency rebuild.
A queued guard is reprioritized in place without a duplicate waiter, decode,
or queue slot, and exact admission-cursor rewinds preserve canceled staged
guard work across atomic promotion.

One constant-size framework-neutral render-intent mailbox owns raw camera and
linked-cross-section wheel/drag samples. Egui emits typed samples and finish
events but owns no second latest-camera or durable-currentness authority. Raw
samples do not dispatch the application reducer or extend project history.
Resident camera samples reuse the installed geometrically valid body without a
planner run. Every active cross-section sample applies one effective mailbox
geometry to XY, XZ, and YZ. It first proves all three installed navigation
floors complete. A complete fine guard that contains the geometry remains the
exact fast path; once a contained footprint consumes 70% of its acceptance
radius, the latest-only planner starts an overlapping successor. Crossing the
guard before that successor is ready still renders all three panels at the
latest geometry through their installed scale chains, ultimately the complete
navigation floor. The active panel is scheduling priority rather than an
exclusion rule, and a finite fine guard is no longer a presentation boundary.
Drag completion and a short scroll-settle boundary commit only the latest
value once. When projected durable LOD still matches and the target body is
complete, all three exact transient frames are adopted atomically into the new
durable linked generation with the same frame, texture, and binding identities
and no replacement GPU work.

When projected durable LOD differs after contained incremental linked zoom,
the exact-demand comparison includes the selected scale map and immutable body
identity and prepares the selected feasible replacement. Plane targets publish
the newest geometry after their complete first-useful floor is resident, then
replace fallback regions in bounded batches as target bricks arrive.
Voxel-exact sampling selects one level per sample. Smooth-linear sampling
retries the whole interpolation footprint at one coarser level if any fine tap
is missing; a resident invalid fine result is terminal and never exposes
coarse data. Volume targets do not use this plane fallback.

The owner has confirmed that ordinary linked zoom can reach visibly fine S0
output and that the progressive four-panel presentation behaves as intended
on the representative mapped workload.
The inspector reports the independent `3D scale` separately from linked-2D
shown, selected, ideal, exact/provisional, refining, target completion, and
display-current facts. A provably uniform level is reported as one `sN`;
partial target coverage reports `mixed target–floor`, while zero target
coverage with reusable intermediate levels reports a conservative fallback
range. It splits XY/XZ/YZ into separate rows if they diverge.

Terminal empty geometry clears all three superseded linked presentations.
Product automation uses the same mailbox and waits for its final transient
sample to become current before emitting the durable finish. Canonical view,
source, project, layout, and tool transitions retire stale intents.

The logical decoded-resource identity is named `BrickKey` consistently from
demand through dataset scheduling, decoded leases, render requirements, and
GPU residency. It remains the same storage-independent identity over source,
layer, timepoint, scale, and exact logical region/shape; physical package
chunks and shards are not product brick identities, and no compatibility key
or adapter exists.

Volume candidate bricks come from the selected-scale view volume rather than
per-pixel ray discovery. Cross-section candidates instead come from physical
render-pixel centers projected through the selected scale's affine transform.
The plane planner traverses the two projected brick axes, analytically clips
the dominant axis, and includes only the declared sampling-support halo plus a
bounded outward envelope for the renderer's f32 arithmetic. Controls that
cannot preserve a sub-half-voxel addressing envelope fail with a typed
precision error; an outside-volume plane installs a terminal empty scope and
clears the superseded panel presentation without retrying.

Bounded admission cursors, separate monotone visible/full-body readiness
cursors, and retained leases preserve overlapping work without a GPU
residency mirror. Exact renderer eviction events rewind only affected scope
positions and reoffer an existing CPU lease or use the normal dispatcher.
Worker-prepared retained-union removal deltas and one atomically prevalidated
batch cancellation make
accepted plan publication proportional to changed waiters; rejected plans
leave installed scopes and useful pending work intact. The shared runtime uses
a lazy versioned priority heap, generation-predicated ledger/scheduler wakeups,
bounded decode cohorts, and coalesced progress; capacity release cannot be
lost in a check-to-wait window and does not rely on timeout polling. Cohort
formation virtually
reserves every selected member's still-uncharged working bound, so an
individually admissible set cannot form an aggregate batch that repeatedly
fails before publishing any bytes. The runtime
reports cohort membership plus cancelled decode execution count, time, and
bytes as explicit waste. There is no automatic verification job or
post-interaction verification grace in this scheduler. If all workers are
occupied by playback, analysis, or prefetch, newly admitted foreground work
cancels one low-priority execution at
a source checkpoint, preserves its logical job, ticket, waiters, and dedupe
identity, runs the foreground cohort, and then resumes the interrupted job.

Cold volume refinement uses one atomic visible/hidden handoff. Any existing
complete 3D presentation remains exact-frame-only through every retry. The
hidden replacement can promote only when its presented and requested frame
identity, extent, requirement count, and actual GPU coverage match and are
Exact; dormant prefetch does not count toward completeness. No partial or
mixed-scale volume frame is published as current.

Plane presentation is deliberately different: a frame may publish whenever
its complete navigation-floor prefix is resident, because plane-only
multiscale sampling guarantees valid current geometry without dark missing
regions. Its target completion remains provisional until every selected
target requirement is resident and shown. Current 3D and exact
cross-section settlement still require the installed demand signature to
match the current snapshot, so fallback coverage cannot bind an old body as a
new exact result.

When a cold 3D bootstrap first publishes a complete coarse frame, it queues the
successor refresh needed to render its already staged hidden exact target and
requests an egui repaint if event-driven idle would otherwise strand that
work. Static-layout deltas are accepted only from the predecessor layout
actually installed on the exact presentation token that will execute them; a
semantically matching plan that was never installed on a blank, recycled, or
deactivated target cannot authorize incremental preparation. Successful
presentation records that token-local lineage, and target reset or atomic
handoff clears it. Exact transient cross-section finish adopts the retained
frame without rerendering it, while terminal empty intent makes the target
current and clears the superseded pixels.

Normal local reads retain one accounted package-root descriptor, resolve
named objects beneath it with Linux `openat2` no-symlink/no-magic-link
constraints, and reuse at most 64 authenticated object handles with their
decoded shard indexes. One byte-accounted eight-entry physical-brick cache
coalesces sequential and concurrent semantic consumers, including the sixteen
2D semantic regions that may share one physical chunk. Aligned full 3D bricks
stream through bounded writable spans directly into the runtime-owned sink
without a payload-sized copy. Current-view cohort members share one fail-closed
pre-use/post-use currentness transaction while preserving member-local faults
and cancellation. For an externally opened package, packed payload facts are
only acceleration hints until their corresponding payload is decoded and
checked; they cannot suppress a first required payload read. The active
importer's self-consistent publication capability may transfer already checked
facts without another scan. Reuse and batching do not weaken per-object
mutation detection. Native codec/range workspace is released immediately
after physical decode; only the exact decoded-brick retention charge may
survive a residency-capacity bypass while joined semantic consumers finish.

## Foundation Status

The foundation refactor through WP-15 is complete. It
established the current bounded storage, runtime, renderer, project,
application, analysis, UI, verification, and local packaging authorities. Git
history and immutable tags retain the individual package record.

## Current Verification Boundary

The public repository requires exactly `PR / policy` and `PR / rust` on pull
requests, with matching non-required `Main / policy` and `Main / rust` checks.
Hosted jobs use free public runners without caches or artifacts. GPU,
format, project-store, packaging, and product checks remain explicit local
commands used only when their boundaries change.

The routine Rust group has five leaves: lint, unit, contract, and UI under the
Rust job, with policy separate. The empty doctest phase is removed, Clippy
checks libraries and binaries without recompiling test targets, and six
exhaustive or redundant integration cases are explicit developer-local checks.
The first revised run passed 1,357 routine cases in 81.9 seconds after a
35.4-second Clippy phase.

The changed-boundary trusted Vulkan command runs the focused ignored GPU tests
directly under its hardware, timeout, and clean-revision guards. Ordinary test
source is no longer treated as a hash-bound fixture, and no separate WP-09A
receipt parser can override the native test result.

The current local Linux x86_64 pre-alpha package passed dependency policy,
AppStream validation, release-directory/AppImage/tarball construction, and
all three package smoke checks from a clean committed tree. Its unpacked
executable passed the mapped promoted-fixture MIP/DVR/ISO matrix and the
fixed three-launch reliability scenario: durable provisional autosave before
external SIGKILL, explicit startup-exposed dirty recovery, and successful
clean native X11 close without cleanup fallback. The generated contents and
product-validation reports retain the exact revision, binary, source
nonmutation, process, and artifact evidence.

See [testing](TESTING.md) for commands and claim language.

## Rendering Correctness Audit Status

A 2026-08-02 read-only audit found rendering cases not covered by the closed
viewer evidence, and the owner authorized the complete corrective package.
The correctness cut is implemented in the current working tree. Composed
Exact transactions now enter the renderer as a complete fixed logical target
set. Every member binds source/time/frame, exact per-layer map, spatial and
surface facts, immutable requirement body, promoted-prefetch role, output
extent, schedule, and renderer lineage. The renderer alone classifies reused
versus rebuilt members, returns the complete logical dispositions plus the
physical delta, validates an all-reused transaction against its current
private fronts, and performs no pipeline/allocation/submission work for a
genuine zero delta.

One application render-attempt coordinator now owns fingerprint-scoped
execution, deterministic failure, displayed-fidelity projection, causal
waiting, texture-composition acknowledgement, and repaint eligibility.
Retryable pipeline, submission, placement, eviction, residency, candidate,
mailbox, logical-member, and hidden-worker waits preserve the predecessor and
name the exact event that can make progress. A coalescing weak renderer event
sink makes a satisfied wait eligible for exactly one new admission; consumed
`Ready`, stable deterministic failure, color-unavailable, and terminal state
are quiescent. A completed `Ready` report authorizes exactly one same-
fingerprint continuation, and asynchronous shader-envelope completion restores
the consumed render refresh before waking the UI. Progressive preview and
exact refinement compare the same semantic request while retaining stricter
proof identity for actual reuse, so a valid same-frame refinement is not
misreported as a frame-contract mismatch. Ordinary UI service also retires
pending GPU timing tickets without requiring a render refresh. Terminal
projection retires non-executable renderer background ownership, so raw
residency/capture state cannot retain an immediate or 50 ms wake.

Hidden refinement now preserves cancellation, first/second timeout, worker
spawn, worker panic, identity exhaustion, and device causes separately. It
retains an in-flight completion lease before retry or cleanup, never publishes
partial rows, permits at most one fresh timeout retry, and permanently disables
only hidden work after a hidden-local worker/identity cause. InitialRender and
Pick pipeline capabilities are also independent: a local Pick failure drains
pending tickets to a typed terminal result without disabling ready color,
while device-level compilation causes still promote through the one global
first-cause latch. Hidden-job, private-presentation, and texture-revision
identity exhaustion is operation-scoped and never re-enters the exhausted
allocator.

The render API is the sole binary32 affine and work-envelope authority.
Normalized condition and outward-rounded control/inverse error replace
absolute determinant magnitude; exact singular, condition, nonfinite-control,
quantized-singular, coordinate, envelope, and sample-count failures remain
distinct. Grid ends and counts are accepted through `2^23` and rejected at
`2^23 + 1`. Bounded layer/scale affine results and latest-only target envelope
results feed semantic demand and renderer controls. Orthographic planning uses
the validated quaternion axes and relative near-plane arithmetic instead of
reconstructing a basis from `target - eye`.

All volume and Pick WGSL paths share binary32 normal/subnormal direction
classification, finite page-exit logic, and represented-sample segment
correction. Continuous page and general-DVR entry predictions are checked
against the actual binary32 sample and shortened by bounded search when they
would assign a sample to the wrong page/state. Positive and negative `5e-7`,
zero, admitted/rejected subnormal, and far-boundary-overflow cases now compare
MIP, fused/general DVR, ISO, Mixed, both sampling modes, coverage, validity,
and Pick with independent facts on the trusted Vulkan adapter.

Current-tree real-display automation passes all 141 mapped
`target_fixture_render_modes` commands and all 60
`representative_native_navigation` commands on the NVIDIA RTX/Vulkan product
path, with exact/current settlement and no renderer validation error. These
are internal normal-application automation results, not owner observation.
The attempted temporal scenario reached an Exact/current timepoint-1 frame but
its independent capture gate rejected that fixture because the retained
timepoint-0 transfer window left no intermediate RGB pixels. The direct
trusted temporal replacement regression passes, but a suitable normal-product
temporal workload is still required.

The active
[viewer rendering correctness, recovery, and numerical cutover](plans/active/VIEWER_RENDERING_CORRECTNESS_RECOVERY_AND_NUMERICAL_CUTOVER.md)
retains three open closeout boundaries. Its required pre-edit P0 A/B overlay
campaign was not captured before these production edits, so that milestone is
not retroactively claimed. The immutable A/B/C campaign orchestrator and
sanitizing evaluator now exist, but the designated private workload has not
run and R11 performance recovery remains open. The full trusted-GPU lane
requires a clean revision and correctly refuses this dirty implementation
tree. The temporal normal-product exercise and owner mapped acceptance remain
required before the correctness cut is product-validated. Earlier product-
validation claims remain valid only for their named revisions and do not
cover these new edge cases.

## Viewer Performance And Development Recovery Status

The 2026-08-02 static multichannel visible-layer correction is implemented and
closed. The standard PR gate, renderer GPU equivalence checks, and fixed-LOD
matrix pass. After normal product use, the owner explicitly accepted the
correctness-first closeout while reporting materially worse static viewer
performance. No controlled before/after product measurement exists, so this
is qualitative negative evidence rather than a quantified regression, and no
performance improvement is claimed. Static performance recovery is owned by
the authorized corrective plan linked above. Its private A/B/C orchestrator
now enforces matching scientific work, three balanced order blocks, complete
raw metrics, nearest-rank/median aggregation, the 5% fixed-GPU and 10%/1 ms
product thresholds, deterministic first-boundary attribution, and sanitized
output. No qualifying campaign has run, so R11 remains open and no tuning or
recovery claim follows from that implementation alone. Durable active-layer selection is now analysis
focus only and may be hidden. Ordered visible layers own render membership,
and ideal, capacity-selected, navigation, and displayed fidelity are exact
per-layer maps. Active-only changes preserve planning, residency, render
revision, and presentation authority. Hiding the active channel leaves the
remaining channels renderable; hiding all channels installs and publishes the
explicit empty state before asynchronous zero-body planning can retain or
repaint a predecessor. The empty cutoff retires renderer-front, frame-lease,
pick, capture, and native-binding authority; the already-resolved egui
registration survives only to the next UI-turn boundary. Empty idle is
quiescent, and unhiding a channel uses the normal planner and renderer path.
Uniform scalar LODs survive only as optional diagnostic summaries and cannot
decide currentness or reuse.

Static capacity refinement and navigation construction now use deterministic
max-min normalized per-layer progress with authored-order ties. Playback keeps
its prior active-first/lockstep quality ordering and its retention, runway,
scheduling, memory, and presentation policies are unchanged. The old
full-viewport `maximum dimension × taps` navigation estimate is replaced by a
renderer-schedule-aligned analytical bound over projected affine coverage,
transform-aware traversal, fixed ray work, per-layer samples/optics, and
kernel-specific sharing. Native output resolution, complete-frame publication,
the 12 ms interaction guideline, and existing CPU/GPU/resource ceilings are
unchanged.

The controlled warm-resident 1920x1080 matrix ran with 64x64x64 Float32
channels, five warmups and thirty samples on the NVIDIA GeForce RTX 3070 Ti
Laptop GPU/Vulkan adapter. The largest homogeneous four/eight-channel p95
ratio normalized to ideal linear scaling was 1.0317 (eight-channel
SmoothLinear MIP), below the predeclared 1.20 shader gate. Sampled work issued
zero uploads or residency submissions; renderer-planning p95 was at most
0.812712 ms, queue-submit p95 at most 0.309468 ms, and WGPU reported zero
validation errors. No shader, storage, cache, upload, or playback overhaul was
therefore made.

Owner product use found one shared-path playback regression before closeout:
starting playback could adopt the terminal rung of a static fair ladder and
freeze the fixed contract at that coarse scale. Static and playback ladder
baselines are now mode-local, incompatible ladders are rejected as a whole,
and the pre-cutover playback work formula, active-first priority/rewarm input,
and observation identity are isolated from the static work cut. The normal
two-channel/three-timepoint application transition now proves an S0 playback
contract after a mixed static rung, and the playback-filtered suite is a
regression boundary rather than a new performance claim. Exact tables, model
decisions, regression coverage, the owner closeout, and the deferred
performance boundary are recorded in the
[multichannel performance and visible-layer authority plan](plans/active/VIEWER_MULTICHANNEL_PERFORMANCE_AND_VISIBLE_LAYER_AUTHORITY.md).
One ignored application GPU handoff check remains red at the identical
immediate-refresh assertion on both the planning baseline and this tree; it is
recorded as pre-existing harness debt, not counted as positive package
evidence, and was not weakened.

The development-simplification and viewer-recovery program is complete.
EP-00/EP-01 qualification commands, selection, schemas, receipt/replay,
harness, shared gate/counter choreography, repeated resource-union hashing,
test-source self-hashing, and post-test WP-09A receipt interpretation are no
longer development prerequisites. More than 43,000 lines were removed while
normal product validation, hard resource caps, source/import/project
workflows, asynchronous GPU timing, and independent numerical oracles remain.

The damaged unpublished linked worktree was independently preserved and
reconstructed. Its 199 recoverable commits through `3d967f0` plus the surviving
seven-file delta exist as clean recovery commit `3dfe9de` in a verified
external bundle. Only reviewed product mechanisms were reimplemented on
`main`; the remainder is not an alternate product or merge candidate. A
2026-07-28 read-only audit identified selected plane-demand, residency,
frame-coordination, and kernel mechanisms as possible donors for a proposed
single-authority overhaul. The reviewed projected-demand and global-residency
mechanisms have since been reimplemented through the normal product
authorities; recovered session, transaction, qualification, and provenance
machinery remains rejected and inactive.

The resident-interaction/per-view-LOD slice is implemented. Transient wheel
and drag input no longer reconstructs durable state per sample, and a preview
renders only from a complete geometrically valid resident body. Narrow mapped
scenarios reported zero reads, decodes, requests, or uploads during their warm
resident motions, and settled idle caused no queue submissions. Those
scenarios did not exercise sustained real oblique-control input and do not
product-validate interaction continuity.

The global-residency slice is implemented and product-integrated. Exact
Vulkan checks cover masked-hash collision, tombstone lookup, zero-upload
cross-target reuse, eviction and reupload, multi-presentation pin unions,
independent plane/MIP/ISO/depth/pick values, and multichannel order
independence. The mapped release resident-navigation scenario on the NVIDIA
RTX 3070 Ti/Vulkan adapter published two exact warm camera frames with zero
new physical reads, codec decodes, dataset requests, uploaded resources, or
uploaded bytes. Its GPU render passes were approximately 1.38–1.39 ms, and
120 settled frames added zero queue submissions. The mapped MIP/DVR/ISO
scenario also passed with zero WGPU validation errors. This closes the P2
authority/correctness milestone on the small target fixture; it is not an
absolute or representative large-data performance claim.

The native terminal-navigation cut has a separate trusted Vulkan measurement
on the NVIDIA GeForce RTX 3070 Ti Laptop GPU. For one warm resident 64³
full-volume page at 1920×1080, five-trial p95 GPU pass times were 1.247 ms
MIP voxel-exact, 3.253 ms DVR voxel-exact, 9.988 ms ISO voxel-exact,
6.776 ms MIP smooth-linear, 10.278 ms DVR smooth-linear, and 11.128 ms ISO
smooth-linear. All six satisfy the 16.667 ms product guideline. This is
repeatable component evidence for the terminal floor, not a claim about every
finer exact body or monitor-visible interaction.

The normal release application also completed the representative Cell
native-navigation scenario. Its 60 commands exercised four-panel linked
rotation, 3D zoom/orbit, linked-only movement during hidden 3D settlement,
voxel-exact and smooth-linear sampling, MIP, DVR, ISO, final exact settlement,
and standalone 3D. Nine GPU readback captures were nonblank. The terminal
report was complete/current at the physical 1280×720 output, with zero target
revision gaps and no validation, capacity, demand, or renderer fault.

After the resident-navigation-ladder cut, the same mapped scenario passed
again on the NVIDIA GeForce RTX 3070 Ti Laptop GPU/Vulkan adapter. Cell
retained complete S6, S5, S4, and S3 navigation candidates plus the exact S2
target. At the recorded 1280×720 camera/profile, the selector chose resident
S4 (1,916,928,000 neutral work units), kept S6/S5 eligible, and truthfully
rejected resident S3 and S2 as interaction-unsafe. The run finished all 60
commands, uploaded the bounded 1,629-resource aggregate through the sole
residency owner, and reported zero WGPU validation errors.

The owner subsequently exercised the normal application and reported that the
four-panel and 3D navigation behavior works as expected. This closes the
spatial viewer performance refactor as a product program on 2026-07-31. Its
renderer, LOD, and ordinary-navigation scope remains closed. The separate
playback-presentation coordination correction is also implemented and owner
product-validated; it does not reopen those spatial performance results.
Smooth-linear sampling remains
functionally supported and covered, but its fine-scale refinement can be much
slower than voxel-exact on the representative Cell workload; that is a known
non-blocking limitation and any optimization or product-surface removal is a
separate future decision.

The frame-coordination slice is implemented and product-integrated. The app
no longer owns per-panel encoders, submits, texture allocation, a fifth
staging target, or presentation/capture scheduling. A direct Vulkan
four-target check produced distinct nonblank images matching the independent
CPU oracle, recorded XY/XZ/YZ/3D in active-first order, submitted the color
work once, mutated no residency or allocator state, and submitted nothing for
an identical idle cutoff. Focused real-GPU product-path checks cover
coarse-to-exact hidden 3D wake and atomic swap plus linked-plane retained-frame
finish and terminal empty clearing.

The named mapped release resident-navigation scenario published two warm
camera changes using exactly two color submissions. Those changes added zero
physical reads, codec decodes, dataset requests, uploads, evictions, arena
allocator plans, directory/page writes, buffers, bind groups, pipelines, or
staging allocations; GPU timing was disabled and post-settle idle added zero
submissions. The separate 103-command MIP/DVR/ISO mapped walkthrough passed.
This closes P3 authority and structural resident-work acceptance on the small
fixture; it is not the representative large-data performance claim required
at final acceptance.

The dedicated-kernel slice has completed its hard cut. One private exhaustive
selector classifies Plane, MIP, DVR, ISO, and heterogeneous Mixed volume
intent, and every production color-recording site routes through it. Plane is
now a separate 53-line color module over shared accepted
binding/directory/payload/sampling code. Its source contains no volume ray,
page DDA, MIP, DVR, ISO, pick, alpha-termination, or dynamic view-kind branch;
the predecessor cross-section function and branch are deleted. The direct
Vulkan four-target coordinator check
rendered the three Plane views plus MIP 3D as distinct nonblank CPU-oracle
matching images in one color submission, with zero residency/allocator
mutation and zero identical-idle work.

MIP is also a dedicated module. A shared volume source contains only ray and
page-segment mechanics, while one canonical MIP core retains the current raw-
maximum, missing-coverage, page-local sampling, and transfer-once semantics.
The dedicated Mixed path calls that core but cannot redefine it; the old
standalone MIP fragment and entrypoints are deleted.
A focused real-Vulkan 65x65 float32 voxel-exact off-axis perspective frame
matched RGBA, coverage, and validity at pixel `[48,32]` to the independent
numerical oracle. No terminal-maximum or donor evidence path was restored.

DVR is also a dedicated module. Its homogeneous pipeline alone owns
compatible-grid fused traversal and common-world joint-medium integration;
Mixed shares only the single-layer emission-absorption primitive. Homogeneous
DVR layer records are
CPU-canonicalized once by logical key, with one order-reversal regression
covering different scales, transforms, transfers, and logical cell shapes.
The focused off-axis Vulkan oracle matched RGBA8 `[109, 0, 0, 151]`, coverage,
validity, and maximum-contribution pick facts.

ISO and Mixed are dedicated modules and pipelines. ISO owns the six-tap
inverse-transpose world gradient, attached/detached lighting, physical hit
depth, depth-sorted homogeneous composition, and its color/validation entries.
Mixed alone owns explicit MIP/DVR/ISO per-layer dispatch and authored-order
over-composition; invalid modes fail closed and heterogeneous subsets cannot
reach homogeneous joint-DVR or ISO-stack algorithms. Focused Vulkan checks
passed affine lighting, incomplete/exact gradient halo color and Pick facts,
off-axis physical ISO depth/order, and authored-order Mixed hand facts
`[26, 69, 0, 194]` and `[10, 115, 0, 194]`.

Pick compiles only shared binding/sampling, volume ray/page, scalar DVR
optical, six-tap ISO-gradient, and compute-program code. It contains no color
kernel, fragment entry, or compositor. The former general pipeline and
monolithic shader are deleted.

The owner-observed failure on 2026-07-29 remains the qualitative baseline: in
the normal application on the representative dataset, four-panel layout, and
S3, continuous oblique dragging froze every one or two seconds and sometimes
took several seconds to resume. The linked-cross-section authority correction
described below remains implemented, and the owner later observed smooth
ordinary S3 interaction away from a separate resident-envelope boundary.

The former three-session command and command-driven resident-navigation
cadence proxy are deleted. They did not traverse the real UI gesture,
independently drive input while the UI was blocked, or measure visibly changed
presentation.

The first replacement `viewer-oblique-continuity` implementation did not
measure human-visible output. Its linked “visible” samples were synthesized
from internal target-publication events, while its `XGetImage` artifacts read
the X11 client surface and could change even when the mapped monitor remained
static. All linked visible-change counts, visible gaps, input-to-visible
latencies, and resulting pass/fail conclusions are withdrawn.

The first blocking production boundary was the split linked-cross-section
authority: common transient geometry was applied only to the active panel
while XY, XZ, and YZ actually form one linked interaction. The corrected
runtime applies one latest geometry to all three panels and renders them with
active-first priority. Exact resident guards still promote atomically, but a
cold or exhausted fine guard now keeps the latest geometry visible through
the complete multiscale navigation floor while the replacement refines.
Durable release adopts all three exact rendered frames together. No storage
format or logical-brick change was involved.

The linked-wheel workflow now retains only endpoint-correctness claims for an
ordinary S3-to-S0-to-S3 path. Client-surface artifacts, internal publication,
input receipt, and settlement metadata are labelled at those actual
boundaries. They are not compositor or monitor evidence, and the owner's
direct observation remains authoritative for human-visible continuity.

A separate diagnostic workload now drives bounded nonstationary 60 Hz
Shift-drag at exact warm S3, S1, and S0 in the normal release application. It
records generated and received input, UI-update duration, demand planning,
read/decode/upload counters, renderer CPU work, linked GPU timings, and egui
texture-paint command queuing. It explicitly reports surface present and
monitor continuity as unobserved and applies no performance threshold.

The first full diagnostic attempt completed its generated S3 and S1 phases,
then froze the desktop while trying to reach S0. A reboot left the app trace
empty. The prior boot contains no OOM kill, NVIDIA Xid, GPU reset, or kernel
hang report, so neither a performance result nor a freeze cause is established.
The two linked S0 workflows are quarantined by default behind
`--allow-host-stress`. Scale selection is capped at 16 inputs, every wheel
input requires app receipt before another is sent, settlement supervises live
UI turns, Shift-drag receipt is checked twice per second, and held input is
released on failure. The bounded timeline checkpoints on input/UI heartbeats
instead of waiting for graceful exit. Those safeguards pass focused static
tests but have not been product-validated after the reboot.

The progressive multiscale correction is now implemented and
automated-verified without rerunning that quarantined workflow. Focused
backend-neutral coverage tests distinguish a single fallback, a conservative
fallback range, partial target mixing, and complete target. A trusted
headless Vulkan fixture independently renders coarse fallback, proves a
partially resident fine interpolation footprint returns the identical coarse
pixel, proves complete fine residency replaces it, and proves invalid fine
data remains invalid. Serial application integration crosses repeated former
guard windows with current fallback and latest-only settlement, and a
separate regression proves a complete resident 3D target rebind creates no
new planning or decode work. These are correctness and internal-continuity
facts, not mapped monitor evidence. Separately, the owner exercised the normal
mapped four-panel viewer and reported the progressive result working as
intended. After the same-intent requirement-body cutover correction, the owner
observed at most a millisecond-scale S4-or-S5 safety image during change before
the proper scale returned; the former indefinite coarse display did not
recur.

The later owner-observed finer-LOD aggregate-capacity freeze is corrected. The
viewer keeps screen-derived ideal LOD, feasible selected LOD, and complete
displayed LOD as separate transient facts. Generic catalog selection begins
from a complete coarsest valid navigation floor and admits deterministic
per-layer refinements only when the exact global requirement union fits. The
renderer's sole residency owner preflights that union, including existing
fronts, shared resources, and replacement overlap. Fixed equal panel quotas
and terminal latching of an unaffordable ordinary refinement are deleted.

The subsequent physical-placeability defect is also implemented and
automated-verified. Aggregate free payload bytes are no longer described as
one allocatable range. A placement-only refusal names the requested
allocation, aggregate free bytes, largest contiguous range, and per-segment
state. `ResidencyOwner` may stably compact allocations within each segment,
rewrite their canonical page records, and retry the exact union once. The
copy is queue-ordered after prior users and before later work, uses bounded
scratch, preserves logical residency and pins, and respects the existing
in-flight submission ceiling.

If the union remains physically unplaceable, the application excludes that
aggregate body with a strictly smaller scalar payload bound and reruns the
same catalog selector. Repeated refusals therefore move monotonically toward
a feasible coarser candidate without scale-specific branches. Saturated
in-flight work defers compaction and retries without a red error. Only failure
of the minimum valid candidate is terminal, and later view/layout/layer/time,
dataset, or physical-capacity input reopens selection.

The installed 3D navigation ladder and each linked navigation floor remain
ordinary canonical residency. The 3D controller uses the finest complete
resident target-eligible rung inside its fixed native work envelope while
finer work is cold or unaffordable; the mandatory terminal rung remains the
last emergency floor.
Complete old pixels remain visible until an exact hidden replacement can swap.
The UI reports the actual shown scale, selected scale, ideal scale, refinement,
and neutral adaptive-capacity state. Successful linked-only installation
clears its matching historical planning/capacity warning; successful
presentation clears renderer failure state when no plan error remains. A red
hard capacity failure is reserved for the exceptional case where no
catalog-provided minimum navigation plan is physically feasible.

Visible-demand worker results bind the renderer-union base against which their
O(delta) update was prepared. If an earlier hidden refinement changes that
base before installation, the result is stale and replanned; it is not
misreported as a user-visible capacity error. Camera and cross-section wheel
input also consumes only current-frame raw wheel or pinch events. Egui's
multi-frame smoothing tail is no longer reused as authoritative input and
cannot recursively create camera revisions and repaints.

Three 30-second mapped normal-app 3D-panel zoom sessions passed on the
representative four-panel workload and NVIDIA GeForce RTX 3070 Ti Laptop GPU.
Every run
observed both a feasible finer displayed level and an ideal boundary
constrained to a coarser selected/displayed level, followed by complete S3
recovery. Across them the worst input, window-receipt, main-loop, externally
visible-change, and p99 input-to-visible observations were 31.096 ms,
35.838 ms, 33.638 ms, 54.763 ms, and 28.945 ms.

The five-minute combined normal-app exercise passed after an exact
1854×1011 → 1100×650 → 1854×1011 real-window resize and a 120-sample real 3D
orbit round trip. It generated 18,004 independently clocked wheel inputs and
the app applied 17,910 authoritative camera samples. Worst input, receipt,
main-loop, and external visible-change gaps were 32.714 ms, 39.150 ms,
34.181 ms, and 86.384 ms; p99 input-to-visible latency was 46.697 ms. Both LOD
boundary classes were observed, no ordinary hard capacity error occurred, and
the final frame was complete S3. The
[adaptive LOD and global capacity plan](plans/active/VIEWER_ADAPTIVE_LOD_AND_GLOBAL_CAPACITY.md)
preserves the approved design and implementation/evidence handoff.

The final adaptive-LOD integration gate passed formatting, exact verification
registry synchronization, fixture validation, architecture, documentation,
dependency and workflow policy, zero-warning workspace Clippy, exact ignored
lane discovery, and its then-current 1,239 PR-lane unit, contract, and UI
tests. The final refactor checkpoint later passed all 1,298 current PR-lane
cases.

Those results support the narrower 3D adaptive-capacity path. They do not
validate linked-oblique monitor continuity or linked-2D LOD-transition
performance. Generic product capture still implicitly selects the 3D target,
and metadata-only `Current`, `Exact`, settlement, and idle facts are not
independent evidence of the scale represented by linked-panel pixels.

The cold pipeline-admission prerequisite is also product-integrated. With a
fresh NVIDIA shader-cache directory, command zero was admitted in 46 ms while
the first-frame wait continued for roughly 20 seconds with live progress
heartbeats. The same mapped run then produced current pixels and passed warm
resident navigation and idle-settlement checks. Focused worker tests cover
ordered capability publication, first-operation failure, cancellation, and
nonblocking drop; real-Vulkan readiness and resident rendering also pass.

The cold-refinement slice is also product-validated. On a real-display
65×1025×1537 public import fixture with four scales, the previous visible frame
remained Exact while the hidden target held 20 of 100 requested resources and
the runtime continued queued/in-flight work. No partial frame was presented as
current. The target then promoted once as Complete/Current with no source,
decode, or render fault. Focused atomic-handoff checks and the full app,
application, and renderer suites passed.

The kernel slice corrected DVR physical-distance opacity and ISO gradient-halo
pick completeness. On the trusted RTX 3070 Ti Vulkan adapter, the independent
off-axis DVR oracle produced expected RGBA8 `[109, 0, 0, 151]`; the focused
nine-test Vulkan inventory passed with no validation errors and at most one
RGBA8 channel of oracle delta. The real-display render-mode workflow passed
after the correctness change.

Two speculative shader edits were measured and deleted. In a fixed resident
nine-brick 1920×1080 Vulkan timestamp workload with five trials after one
warm-up, the smooth-ISO endpoint rewrite had a 28.68 ms baseline median and
28.87/29.12 ms candidate medians. The MIP early-stop rewrite moved repeat
medians only from 2.049–2.052 ms to 2.042–2.046 ms. Neither met the declared
five-percent retention threshold, so no optimization claim or extra kernel
complexity was retained.

Compact failure handling is implemented. Native SIGINT/SIGTERM exits through
the ordinary joined shutdown path in an actual release-process probe. WGPU
terminal failures preserve the first typed cause and stop later unsafe work;
focused classification, first-cause, mapped-access, and app-mapping checks
passed. Actual hardware device loss or out-of-memory was not destructively
induced, so that part is automated-verified rather than product-validated.
The former P6 storage conclusion is revoked with the invalid performance
boundary. The current package format and `64³` logical renderer/cache brick
edge remain unchanged defaults, not a newly accepted performance result.
Storage work remains outside the active correction unless a faithful failing
profile identifies required unavailable-brick physical I/O/decode as the first
blocking boundary; any format experiment or cutover would still require an
owner-approved amendment.

No universal all-hardware performance, comparison-viewer, or release claim
follows from this closeout. The owner-observed product acceptance above closes
the refactor without broadening those public claims.

## Import And Preprocessing Performance

The current working tree implements the
[large-dataset preprocessing and storage cutover](plans/active/LARGE_DATASET_PREPROCESSING_AND_STORAGE_CUTOVER.md).
`M4D-COMPOSITIONAL-1` is the sole admission contract; the predecessor
aggregate dataset classes no longer select package support. Aggregate
addressability is bounded by format/object constraints and by a 65,536-entry
manifest descriptor ceiling derived from the smaller of wire capacity and an
explicit 64 MiB recovery/duplicate-validation working set. Long `T/C` remains
linear in output objects and time until one of those real authorities is
reached.

Import now retains one `(timepoint, channel)` decoded cache and encoded-inner
spool, commits outer shards directly into one destination-bound resumable
final-layout stage, and stores only compact unit, decoded-digest,
packed-record, no-data, stage, and scientific-hash control history. The
corrected placement planner computes each actual edge object's occupied-slot
codec bound. Its whole-package ceiling is guidance only. Hard Start and
runtime admission use one bounded unit, one compact increment, and
finalization headroom; increasing only `T` or `C` cannot increase that hard
unit requirement. Runtime subtracts exact current-unit payload already named
by the durable stage journal and narrows to actual remaining finalization work
after unit production.

Setup and running projections label non-reserved package guidance separately
from immediate additional headroom. A genuine capacity shortage or safe
prepublication `ENOSPC` keeps the checkpoint and offers `Resume` without
deletion; corruption keeps the separate confirmed `Reset and Restart` path.
Focused suites and the bounded normal-app scenario pass durable-prefix
cancel/resume, publication, direct open, navigation, exact generated-source
preservation, and the corrected statistics surface. On 2026-08-01 the owner
reported that the completed normal preprocessing workflows all worked
correctly and accepted the result as product-validated. Private source facts
remain outside repository evidence, and no universal throughput or maximum-
dataset claim follows from that acceptance.

The predecessor import-throughput boundary was automated-verified at immutable
revision `eb5c9ffd12cbd9fce65bd03559b8e7f93170d72e`. Both
qualification sets used the owner-accepted HW-2/ext4 tuple, a 256 MiB import
budget, and declared warm-cache/no-competing-activity conditions. The public
T2 workload passed five independent release sessions with a
4.609749805-second median against the 60-second gate. The normal native private
T5/DS-3 workflow is product-validated at that revision on the owner-attested
physical display and externally observed mapped X11 client: three fresh
release sessions had a 691.093848488-second (11-minute 31.094-second) median
against the 15-minute gate.

Every mandatory per-sample, invariant, correctness, source-preservation,
determinism, resource, and publication-to-open-ready gate passed. The T5 report
records no failures, skips, or waivers, and no mandatory closeout check was
skipped or waived. The non-blocking 10-minute stretch target was not met.
[Testing and evidence](TESTING.md) owns the exact protocol, revision-bound
evidence, and accepted remaining risks. Revision
`85350219efcc0c96b492f9a5029ba80752b49306`, the clean predecessor to the
sentinel restoration, was documentation-only and was not performance-
qualified. These claims attach only to the named qualification revision and
its recorded release executables; the current dataset-optional shell,
explicit-manifest, resource-broker, and automatic-mask implementation does not
inherit them. The restored sentinel cutover now has the separate correctness
evidence described below but does not inherit this historical T5 performance
claim. The predecessor T5 configuration pin is intentionally cleared because
its aggregate profile/checkpoint expectations no longer describe the current
format; a new pin requires owner review after current product validation.

## Known Limitations

- The large-dataset import/storage cutover is closed and owner product-
  validated, but private-source geometry, actual compressed output size, and
  workstation throughput are not repository evidence. The acceptance is not
  a universal dataset-size or performance qualification.
- The restored sentinel policy is implemented and closed. Generated 2D/3D
  fixtures cover boundaries and every LOD; clean revision `f73cb36` passed the
  complete repository/local set and accepted five-session T2 qualification;
  and two bounded source-oracle runs at `d6478d9` supplied the byte-identical
  one-time private fact freeze. Clean revision
  `30bb16758d22055deb1e52fe6803b95592094eee` then passed one real-display
  normal-product private correctness sample against those frozen facts,
  including all seven scale digests, centered transforms, canonical invalid
  values, source preservation, resource bounds, publication, and navigation.
  Owner observation also confirms that the affected workflow's visible fringe
  is fixed. The sample establishes correctness only: the earlier T5
  performance qualification predates the semantic cutover, and a current
  three-sample performance run remains optional and explicit. Routine runs do
  not recompute the oracle; the accepted T2 evidence remains attached to the
  unchanged production-importer boundary.
- Target packages now open after bounded structural admission. Ordinary open,
  idle, project I/O, recovery, analysis, and export start no whole-package
  scan. Consumed objects retain exact checksum and pre/post-use currentness
  checks, and untrusted packed facts cannot suppress their first required
  payload validation. The UI exposes a separate explicit, cancellable full
  package-integrity audit with real object/byte/brick progress and exact
  failure stages. A successful audit establishes agreement with the package's
  own manifest and declared content address; it does not establish provenance,
  authorship, biological correctness, or any externally anchored scientific
  identity. Audit completion or failure does not promote/revoke the source or
  clear its presentation.
- Imported-package capability transfer is fail-closed within its cooperative
  local destination-parent namespace. Observed mutation, an unlisted object,
  root substitution, ambiguous rename binding, or durability failure yields no
  open-ready authority and never selects the ordinary full verifier as a
  fallback. The contract does not defend against a hostile actor able to
  concurrently rename or unlink entries in that parent: Unix cannot atomically
  bind a source name to an already-open directory descriptor. A later explicit
  external open remains an independent normal structurally admitted open with
  lazy per-read integrity.
- The target dataset profile and project-store format are experimental and
  carry no compatibility promise.
- TIFF import and create-only dataset publication require Linux kernel 5.8 or
  newer. Their single stage-finalization `syncfs` barrier flushes the whole
  containing filesystem, so unrelated dirty data can delay cancellation or
  make publication fail closed on an unrelated writeback error; it never makes
  an incomplete stage publishable.
- The project store uses immutable objects and generations, bounded direct or
  paged closure, atomic refs, held leases, and generation-last publication.
  Its application service is the sole product route for Create, Open, Save,
  Save As, autosave, recovery selection, dirty close, and joined shutdown.
  Writable durability is qualified only for the accepted Linux ext4 tuple.
- Project maintenance and Purge UI remain absent. Full verification does not
  validate artifact scientific semantics, repair data, inspect trash, or
  broaden the durability claim. Compaction planning does not authorize Trash,
  expose a physical object/byte plan or reclaim estimate, or prove backup
  approval. Private Trash and Purge accept only bounded
  zero-non-regenerable subsets; their process-crash checks do not simulate
  power loss, and the API cannot authorize removal of non-regenerable
  artifacts.
- Linux release candidates are local x86_64 artifacts, not a supported public
  release.
- Terminal GPU loss, out-of-memory, backend-internal, and validation failures
  are typed and latched, but there is no device-recovery path or CPU fallback.
  Actual hardware loss/OOM fault injection remains unperformed.
- Incremental linked-2D zoom now separates provisional resident coverage from
  exact settled demand, and the owner has observed ordinary S0 output. There
  is still no valid automated linked monitor-continuity or LOD-transition
  performance result. The first faithful S3/S1/S0 diagnostic froze the
  desktop while approaching S0 and lost its app timeline at reboot; the S0
  workflows remain quarantined and are not a product-closeout prerequisite.
  The later fragmented payload-placeability correction is implemented,
  automated-verified, and owner product-validated in the normal viewer. The
  [GPU placeability plan](plans/active/VIEWER_GPU_PLACEABILITY_AND_RECOVERY.md)
  owns that correctness handoff; the
  [closed corrective handoff](plans/active/VIEWER_LINKED_2D_LOD_TRUTH_AND_SETTLEMENT.md)
  owns the evidence correction and diagnostic handoff.
- The composed presentation scheduler, fixed-LOD playback session, and bounded
  temporal ring are implemented and owner product-validated on the
  representative time-series workflow. The mapped 960x640 run on the NVIDIA
  GeForce RTX 3070 Ti Laptop GPU passed seven stationary-versus-held-input
  cadence comparisons, coherent four-panel publication, and direct retained-
  front Stop traces. This qualifies that workload and workstation only; it is
  not a universal cadence or all-hardware guarantee. The
  [composed presentation scheduler plan](plans/active/VIEWER_COMPOSED_PRESENTATION_SCHEDULER_CUTOVER.md)
  owns the exact evidence and thresholds.
- Smooth-linear sampling remains scientifically supported and covered by the
  renderer/product checks, but fine-scale exact refinement on the
  representative Cell workload is substantially slower than voxel-exact.
  This does not block the closed performance refactor. Removing or
  redesigning that option requires a separate product decision.
- Exact linked-panel cursor readout remains lease-backed. The 3D viewer now
  submits latest-only asynchronous picks against the exact presented GPU
  residency for MIP, DVR, and ISO; stale or retired frames fail visibly rather
  than reading a placeholder.
- Rendered-viewport-derived statistics remain unavailable. Product analysis
  instead computes exact source-voxel statistics for a whole layer over time or
  a numeric box at the current timepoint; loading placeholders are never
  reported as scientific zeros.
- Product rendering remains bounded to 1920x1080, at most 64 active layers,
  65,536 requirement records per presentation, and the configured/device GPU
  byte limits. The former 256-requirement, 128-lease, voxel-exact-only,
  flat-ISO-only, orthographic-only, and 16,384-voxel global-dimension ceilings
  are absent. Capacity and unsupported-adapter cases remain typed instead of
  silently changing scale or display semantics.
- Windows and macOS are not qualified targets. 4K is intentionally out of
  scope.
- Current persisted formats have no compatibility promise.

[Current work](planning/NOW.md) records the current development status.
