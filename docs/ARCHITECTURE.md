# Architecture

Last updated: 2026-08-01

Mirante4D is a native Rust desktop viewer and analysis workbench. It opens
strict `.m4d` packages; source microscopy data enters through explicit
import/preprocessing workflows.

## Workspace Boundaries

The workspace has eighteen packages (seventeen `mirante4d-*` crates plus
`xtask`):

- `mirante4d-domain`: validated framework-neutral geometry, view, transfer,
  render-intent, and tool values.
- `mirante4d-identity`: strict typed identities plus pure SHA-256, NFC, and
  scientific-tree primitives; no filesystem I/O.
- `mirante4d-project-model`: canonical durable project/view state and
  persistence-neutral generation projections.
- `mirante4d-project-store`: experimental product project storage, reached only
  through the application-owned project-store service.
- `mirante4d-application`: the sole command reducer, revision/history owner,
  transient semantic state, operations, events, snapshots, and typed faults.
- `mirante4d-settings`: closed settings document and bounded background I/O.
- `mirante4d-dataset`: immutable multiscale catalog, semantic resource keys,
  source/decode-sink boundary, value-plus-validity payload views, lease
  contract, and the dependency-inverted CPU byte-ledger admission interface.
- `mirante4d-dataset-runtime`: unified request, cancellation,
  deduplication, bounded configuration/diagnostics/progress, CPU-ledger,
  completion, fault, and accounted-lease contract plus the sole production
  scheduler and worker owner. Its production queue is a lazy versioned
  priority heap with byte-size admission, per-ticket cancellation, ledger
  wakeups, and low-cost decode/cancellation-waste accounting.
- `mirante4d-analysis-core`: pure exact intensity operations, deterministic
  statistics, and canonical table/plot artifact payloads.
- `mirante4d-analysis-runtime`: bounded, cancellable analysis execution over
  shared dataset-runtime requests, producing pending atomic artifact bundles.
- `mirante4d-render-api`: backend-neutral intent, requirements, progressive
  frame status, opaque presentation lifecycle, camera math, and bounded
  asynchronous volume-pick contracts.
- `mirante4d-render-reference`: unpublished, bounded CPU oracle for renderer
  correctness; it owns no product route or GPU authority.
- `mirante4d-render-wgpu`: sole product renderer, with bounded progressive GPU
  residency through one global directory/arena owner, direct exact page
  lookup, opaque frame leases, brick traversal, asynchronous timing and
  picking, first-cause typed terminal-device latching, and presentation built
  only against dataset leases and render contracts.
- `mirante4d-storage`: one active compositional package-safety contract,
  portable package paths, bounded local validation/reads, exact and scientific
  capabilities, dataset source, and deterministic resumable/create-only local
  writer.
- `mirante4d-import-pipeline`: active bounded, cancellable, restartable
  TIFF/OME-TIFF producer for validated sharded target packages.
- `mirante4d-ui-egui`: active egui visual components, UI-facing message
  projection, and transient egui interaction state; its only Mirante
  dependency is `mirante4d-application`.
- `mirante4d-app`: native process/service composition and presentation-token
  resolution.
- `xtask`: developer and verification tooling, never a product mode.

Lower crates do not depend on the app/UI layer; the renderer does not read
files; format code does not own viewer state. The product uses
`mirante4d-storage`, `mirante4d-import-pipeline`, and
`mirante4d-render-wgpu`; the CPU reference renderer is test-only.

## Application Composition

`MiranteApplicationShell` is the native process composition root. It owns the
selected-adapter facts, settings connection, process CPU broker,
preprocessing service, asynchronous package-open transaction, and an optional
`MiranteWorkbenchApp`. The normal window therefore exists in `Welcome` and
`Opening` states without constructing a catalog, dataset runtime, or fake
workspace. A successful open constructs the dataset-bound workbench session;
a failed or cancelled open returns to the empty shell.

`MiranteWorkbenchApp` holds `ApplicationState`, bounded
`DatasetDemandState`, process diagnostics, egui state owned by
`mirante4d-ui-egui`, the opt-in product-validation controller, and narrow
project-store/settings/source-open handles. It is a
composition root, not a second model.

Product automation is composed directly and its render-size override exists
only in test builds. The native app projects one immutable workbench view,
calls `mirante4d-ui-egui` once, and resolves the returned typed commands,
service requests, and opaque presentation paints.
Widget layout and interaction state do not have a second native path.
`ProjectStoreApplicationService` is the sole product project I/O route; its
actor owns project roots, sessions, leases, refs, recovery, and filesystem
mutation. There is no compatibility reader or fallback.

Native SIGINT/SIGTERM handling sets one monotonic process-termination latch and
wakes egui. The app issues one prompt-free close request; the existing
`on_exit` path remains the sole cancellation and join owner. There is no
guardian process, timeout kill path, or second shutdown authority.

The native `ImportWorkflow` owns TIFF worker cancellation, bounded terminal
results, typed checkpoint recovery, explicit setup rows and their inspection
generations, and explicit joining. It projects immutable import facts through
`ApplicationSnapshot`; egui owns only transient text/control drafts and
returns ID-checked import commands. Egui owns no selected path, TIFF
inspection, worker channel, or thread handle. Capacity exhaustion retains the
checkpoint and exposes non-destructive `Resume`; a corrupt or mismatched
checkpoint remains a distinct, confirmed `Reset and Restart` operation.

Every setup row declares one source kind: one 3D TIFF, immediate 3D TIFF
children as timepoints, or immediate single-page TIFF children as Z. The
source manifest stores canonical channel labels and the complete ordered
logical mapping. No later stage rediscovers a hierarchy or parses filename
tokens. Inspection is metadata-only, cancellable, first-mismatch terminating,
and generation checked; it does not hash or decode the full source.

The import pipeline has one source-native authority. It captures the reviewed
inventory, traverses each admitted TIFF strip/tile once, and writes canonical
little-endian planes into the current `(timepoint, channel)` unit cache or the
single ordinal-bound future decode cache admitted by the temporal coordinator.
Source admission is deliberately closed to uncompressed, LZW, current/old
Deflate, and PackBits grayscale `uint8`, `uint16`, or finite `float32` pages.
JPEG, WebP, Zstd-in-TIFF, fax, and other unaudited decoder workspaces fail as
unsupported sources rather than selecting another path. Inspection and native
decode bind reads to the opened descriptor's generation as well as guarding
the source path before and after use. Final structural revalidation compares
the reviewed generation without a payload pass. Per-unit decoded digests are
stored at fixed channel/time offsets and folded in canonical `[c,t]` order
without retaining all digests in RAM. Before decoder construction, a bounded
positional-read preflight walks the primary IFD chain, rejects oversized or
duplicate eager fields and more than 65,536 pages, and charges retained
multipage decoder state separately from one-plane parallel workers.

The active factor-two pyramid geometry is storage policy, so
`mirante4d-storage` owns its terminal constants and sole pure shape-sequence
function. Import planning and generation consume that sequence;
`mirante4d-import-pipeline` continues to own the actual bounded pixel and
validity reduction. Package admission composes checked spatial geometry with
codec/object limits and aggregate addressability. Its 65,536 manifest
descriptor ceiling follows from the smaller of the wire format capacity and
an explicit 64 MiB descriptor working-set contract; temporal/channel growth is
not compared with a reference acquisition.

The current checkpoint is the destination-bound hidden final-layout stage.
Only one canonical current unit cache, one optional future decoded cache, and
one encoded-inner unit spool may be live. The future cache has no spool or
commit authority. Their files are opened with the existing no-follow and
descriptor-owned rules. The current cache and spool are deleted after the
final-layout shard prefix and unit journal are durable; unused speculative
cache scratch is reclaimed before a genuine capacity pause or terminal
failure. The stage keeps chained records for final-relative objects, a canonical unit
journal, sparse decoded-digest storage, packed-index records, resolved no-data
facts, and resumable per-channel scientific-hash frontiers. Recovery accepts
only checksummed, plan/source-bound canonical prefixes and removes at most one
incomplete unit/object suffix. A canonical plane above 64 MiB is rejected
before ingest; there is no predecessor checkpoint reader.

CPU work is admitted by one process-level broker with a hard managed total,
accounting-only purpose categories, an interactive foreground reserve, and
one run-scoped preprocessing progress reservation. The progress reservation
protects the maximum phase-local minimum execution path and converts to normal
leases without a second charge. Borrowed capacity expands a contiguous
ordered sliding window; temporary refusal stops admission, drains useful
results, and retries. Eligible single-plane source decodes,
normalization, downsampling, scientific-tile preparation, and inner encoding
run concurrently inside the active bounded unit; multipage or
over-task-ceiling TIFFs use one streaming decoder. The calling owner alone
advances scientific identity, final-layout objects, and journal state in
canonical order. Cache and spool positional readers are shared, so worker
tasks add no per-task checkpoint descriptors.

After first-volume no-data resolution is durable, the temporal coordinator
may run exactly one future source ingest while the owner performs canonical
processing. The future worker receives a child cancellation token and a CPU
ledger capped to the source decode's conservative transient ceiling; the cap
cannot borrow the current unit's protected progress bytes. While it is active,
the current unit's ordered-worker parallelism ceiling is reduced by one slot,
so nested work never exceeds the system authority. Runtime disk admission
requires both mandatory current-unit headroom and the missing future-cache
ceiling. Failure to obtain optional CPU or disk capacity is an ordinary
width-one schedule, not an import error. Ready caches are consumed only at the
next canonical ordinal, all exit paths join the worker, and source-generation
failures retain their typed meaning. The hard Start requirement therefore
remains independent of this throughput optimization and of total `T/C`.

The no-data resolver has one hard-cut typed production route. Canonical ingest
first decodes channel zero at timepoint zero exactly once. The resolver then
reads that local cache, inspects only that volume, detects exactly constant Z
planes and, when requested, uses rolling X/Y/Z equality runs to find the first uniform
`5 x 5 x 5` block in linear voxel work and O(Y×X) state. Constant planes are
excluded from automatic value inference when plane hiding is enabled. When a
value resolves, a complete cache pass marks every matching cube and performs
face-six reconstruction through exact-value voxels using row-packed bitsets,
a fixed in-memory run window, and a checkpoint-owned spill file. The
immutable row-packed spatial mask, resolved typed value or no-match result, and
sorted plane indices bind the plan digest, recipe, scientific content, and
every worker task. Its retained bytes remain charged to the import CPU ledger.

Base-production and scientific-identity tasks read a clipped
Chebyshev-radius-one source halo only when a value rule resolved. Automatic
mode classifies fixed first-volume mask membership; manual uint8 mode alone
classifies dataset-wide exact source values. Tasks scatter the applicable
rule's dilation into the logical core, overlay strictly plane-local invalidity,
and canonicalize final invalid values to typed zero. A coarse task reads pixel
components only for its aligned factor-two core (at most four 2D or eight 3D
parents), while a separately charged parent-validity window supplies the
target-plus-one value-support halo (at most sixteen or sixty-four parents).
Uniform packed-index facts synthesize validity without codec work; mixed
parents decode only their validity component. Integer means use half-up
rounding; float means use finite f64 accumulation and canonical float32 output.
Plane-only parent gaps are excluded from values but do not acquire value-rule
morphology. A coarse plane sample is invalid only when its complete base-Z
support is hidden. If resolution finds neither a value nor a plane, the exact
plan emits no validity payload and retains the prior point-sampled route.

Production streams validated inner encodings into indexed outer shards at
their final package-relative paths without decoding and re-encoding pixel or
validity chunks. Each bounded suffix crosses one stage-filesystem durability
barrier before its chained record advances. After all temporal units and
packed-index shards are present, the storage writer removes private control,
writes metadata/manifests, and performs structure, exact, and locality-aware
content validation before create-only atomic rename. Content validation
prepares the four `z=16`
content-address leaves intersecting a `z=64` brick while that brick is resident,
so each present base brick is decoded once per scan. The writer keeps the
resulting non-cloneable self-consistent publication capability, seals it to the private stage's
filesystem identity, and rebinds it only when `RENAME_NOREPLACE` publishes that
same directory. Within the cooperative local destination-parent namespace
assumed by this contract, an inventory/snapshot/inventory sandwich before
rename and again when the product consumes the transfer detects extra paths,
missing paths, object mutation, and root replacement without hashing payloads
or decoding scientific bricks again. This proof is observational rather than
adversarial namespace isolation: a hostile actor able to rename or unlink
entries in the destination parent is outside the contract because Unix cannot
atomically bind a source name to an already-open directory descriptor. Failure
within the contract is terminal for automatic import opening; it never falls
back to an ordinary external-package open.

Staged-validation object-read evidence counts every successful strict-reader
object open, including whole reads, ranges, hashes, and snapshot-only
revalidations, and reports structure, exact, and scientific components plus
their checked sum. Directory-only metadata inspection is not an object read.
Codec operation/time evidence distinguishes unit inner encoding from outer
shard construction and staged-validation decoding. Durability evidence covers
unit cache/spool control, incremental final-layout stage commits, compact unit
and identity state, final staged directories, and destination-parent
synchronization after rename. Linux incremental commits use one counted
`syncfs` barrier rather than walking every accumulated directory, while final
publication performs the complete directory durability pass once. The barrier
is filesystem-wide rather than stage-scoped: unrelated dirty data can add
latency or surface a conservative writeback failure. Package creation rejects
Linux kernels older than 5.8, where `syncfs` did not reliably report those
failures.

The one-shot published-capability consumer returns a storage-issued execution
receipt for its closed inventory/snapshot/inventory route. The receipt splits
strict object-open deltas across both inventory passes and the intervening
complete snapshot sweep, binds the expected snapshot count to the retained
exact-package proof, reconciles the phase sum to the total, and records codec
decode calls. Storage and the T2, T5, and product validators require matching
inventory deltas, an observed snapshot delta equal to that independent
expected count, an exact phase sum, and zero codec decodes. A storage source
architecture test separately forbids exact-package hashing, scientific
validation, or brick-read calls in this transfer route. The workspace
architecture check also requires the imported `DatasetOpened` completion to
have exactly one production constructor, in the current-source-open adapter.
These are structural call-path and observed-I/O facts; they are not
self-declared zero pass counts.

On the qualified Linux path, one low-overhead sampler observes descriptors
throughout the primary import clock across all worker threads. It attributes
source, checkpoint, destination, and private-stage paths every 5 ms rather than
using an end-of-stage process-wide snapshot. This is path attribution, so an
unrelated descriptor to the exact same scoped path is conservatively included.
Because an open shorter than the sampling interval can be missed, evidence also
reports a structural descriptor bound tied to the maximum admitted source
workers, fixed checkpoint handles, shared worker readers, writer handles, and
validation transients. The enforced gate value is the greater of that bound and
the sampled peak. The current deliberately phase-summed structural bound is 35,
below the gate of 64.

```text
reviewed TIFF inventory
  -> source-native decode-once traversal
  -> one bounded temporal-unit cache
  -> bounded ordered base/pyramid/scientific workers
  -> one bounded encoded-inner unit spool
  -> durable final-layout shard prefix + compact unit/hash journal
  -> packed-index assembly
  -> staged structure + exact + content-address validation
  -> metadata-only stage currentness proof
  -> atomic create-only rename
  -> destination-bound self-consistent publication capability transfer
  -> metadata-only publication currentness proof
  -> admitted product runtime open
```

Planning reports compressed source size, complete decoded S0 size, and logical
pyramid bytes as information. `StoragePlacementPlan` separately computes a
summed per-object final-package ceiling for guidance, one-unit scratch, the
maximum encoded commit for one unit, one compact unit-control increment, and a
profile-bounded finalization reserve. The whole-package ceiling is never
passed to `statvfs` and is never described as reserved space. Edge outer
objects are charged from their occupied inner slots rather than as completely
populated shards.

Hard Start admission checks only the first bounded unit plus finalization.
During production, the stage journal's cumulative byte prefixes provide the
exact durable payload already present in the active input-ordinal range;
runtime therefore checks only unfinished scratch, the missing unit-output
suffix, one control increment, and finalization. Complete future timepoints
are absent from that comparison. After temporal production, the check narrows
again to the actual request's missing packed-index/metadata/manifest work.
Every safe prepublication `ENOSPC` route becomes a typed capacity pause and
keeps the resumable stage. The application projects the active phase,
timepoint/channel, exact durable stage bytes, actual unit scratch,
non-reserved remaining package ceiling, and immediate additional headroom.

`DatasetRequestDispatcher` is the sole application poll owner. It keeps only
bounded request correlation and cancellation generations; decoded allocations
remain owned and byte-accounted by `mirante4d-dataset-runtime`.
`mirante4d-storage::LocalDatasetSource` is the sole product dataset source.
`DatasetDemandState` retains exact runtime lease handles without copying their
payloads. Newly completed handles are offered once to
`mirante4d-render-wgpu`'s global decoded-lease queue; target execution selects
only offers relevant to its opaque resident-frame lease. Sequenced renderer
evictions reoffer a retained handle or return the exact demanded key to the
normal dispatcher. There is no app-owned GPU resident/recovery set, alternate
reader, scheduler, CPU display fallback, or app-owned payload map.

`LocalPackageReader` retains one byte-accounted package-root descriptor and
uses Linux `openat2` with beneath/no-symlink/no-magic-link resolution for every
normal named-object acquisition. One deterministic LRU retains at most 64
authenticated object handles together with decoded shard indexes and one
decoded packed-index inner per retained object. `LocalDatasetSource` adds one
byte-accounted, eight-entry physical-brick cache. It coalesces an in-flight
physical decode, retains a decoded physical brick only while the shared CPU
ledger admits it, and fans one result out to semantic regions. This makes the
current sixteen-quadrant 2D mapping one physical decode within the bounded
reuse epoch. An epoch lasts only while the in-flight generation or decoded
physical entry remains in that cache; LRU eviction starts a new epoch, and a
semantic lease does not pin a duplicate full physical brick.
Aligned full 3D resources instead stream codec output into the runtime-owned
sink in bounded writable spans and avoid a payload-sized intermediate copy.
Current-view workers may submit a bounded source cohort, so one guarded
pre-use/post-use transaction covers its deduplicated object set while every
member retains an independent cancellation and typed outcome. For an
externally opened package, packed min/max/validity records remain hints until
the corresponding payload has been decoded and checked; a claimed empty brick
cannot suppress that first payload read. A package published by the active
importer may transfer checked packed facts with its self-consistent publication
capability. Every cached hit still revalidates its guarded object snapshots.
The source retains one stable package-access authority for its lifetime; there
is no background promotion or reader swap. Handle, index, physical-brick,
codec, range, cohort/currentness, direct-span, post-decode-copy, and contention
counters describe actual work without adding a second reader path.

`AnalysisProductRuntime` is the narrow product bridge to the analysis
runtime. It uses the shared dispatcher below interactive priority and keeps at
most two analysis blocks in flight. Exact whole-layer time traces and numeric
box statistics produce one table/plot bundle; the application exposes decoded
values only after the project store publishes that bundle atomically. Reopen
validates the stored source-content-address binding and both artifact payloads
before installing either result.

Payload validity is explicit, so valid zero, invalid/no-data, and missing are
distinct. Cancellation generations are ordered only within their scope;
unrelated view and playback demand cannot cancel each other. Ordinary admitted
reads use an opaque per-open runtime source ID. The declared scientific content
address remains package metadata and is never fabricated as a runtime
integrity capability.

## Runtime Flow

```text
native package
  -> LocalPackageCatalog, LocalDatasetSource, and immutable logical catalog
  -> canonical application snapshot and selected-scale view volume
  -> signature-diffed 3D / linked-panel / playback / analysis demand
  -> one bounded priority scheduler and CPU byte ledger
  -> immutable accounted leases
  -> one renderer-global pending lease queue and residency owner
  -> growable committed GPU payload segments under one logical arena maximum
  -> page records and sparse directory
  -> bounded background pipeline readiness on the existing product device
  -> one renderer frame coordinator and coordinated color submission
  -> four renderer-owned GPU target fronts
  -> egui-wgpu presentation and diagnostics
```

Small fixtures and large datasets use the same path. Whole-volume residency
for a tiny fixture is an optimization inside that path, not a second product
architecture. Missing occupied data is loading/incomplete, never empty.
An explicit zero-resource plan means the view is outside selected data (or no
layer is visible); it is terminal and distinct from missing occupied data.

Temporal playback has one application-owned `PlaybackSession`. Warmup chooses
the finest full-volume layer-scale map that fits the exact CPU/GPU overlap for
the requested 1–24 FPS cadence, active layout, visible predecessor, startup
runway, and bounded rotating slot ring. The resulting contract freezes its
source generation, FPS, target set, scale map, slot count, and resource
ceilings until Pause or Stop. Total timepoint count therefore does not change
steady-state playback memory.

Each successor is one immutable temporal frame contract and one prepared
visible-target resource body. Standalone 3D owns only a 3D body; four-panel
owns fixed-scale 3D/XY/XZ/YZ bodies. Lateness retains the same-scale
predecessor rather than substituting a coarser playback LOD, skipping time, or
exposing `Loading...`. The temporal body contains no camera geometry. During
four-panel playback, the linked-panel wrappers bind geometry-independent
full-volume bodies admitted by the same session contract, so plane motion does
not rebuild temporal residency.

One private application `ComposedPresentationScheduler` is the semantic
presentation authority above the renderer. It owns a bounded latest
transaction with independent temporal, 3D-spatial, linked-spatial, and
retained-quality coordinates. A ready due `PlaybackFrameContract` is composed
with the latest spatial mailbox snapshots; newest whole-layout spatial
settlement is not a temporal clock gate, and spatial samples cannot rebuild or
discard the playback session's prepared body.

The scheduler assembles a fixed logical target set before deriving physical
work: standalone 3D contains exactly 3D, while four-panel contains exactly 3D,
XY, XZ, and YZ. Each member is either newly prepared or reused after proving
its exact source, timepoint, scale map, spatial revision, extent, immutable
body, and renderer lineage. Validation sees the complete logical set. A
separate projection then yields an empty, partial, or complete physical delta;
an empty delta completes without GPU work, and a partial delta receives an
exact atomic publication group for only its changed targets.

Pause or Stop creates a retained-quality transaction after application
reconciliation has established the final render-intent revisions. The current
playback front remains visible while future playback-only demand is released
and the ordinary stationary plan is prepared. That transaction cannot
assemble, reuse, or render until the entire active-layout stationary plan is
current, complete, and renderable, so a stale playback plan cannot masquerade
as the finer result. Recoverable failures are latched by transaction
fingerprint and leave the retained front intact until a relevant state change.

The renderer `FrameCoordinator` remains the sole owner of GPU targets,
recording order, queue submission, completion, and atomic swaps. The
application scheduler supplies semantic transactions only; it owns no GPU
texture, fence, residency map, or second submission path. The
[composed presentation scheduler cutover](plans/active/VIEWER_COMPOSED_PRESENTATION_SCHEDULER_CUTOVER.md)
records the completed hard cut and evidence.

Native resource defaults are derived from the exact adapter already selected
for the eframe window. On Linux Vulkan, the application queries that adapter's
device-local heaps and, when advertised, `VK_EXT_memory_budget` budget/usage
facts through its guarded wgpu HAL handle. Discrete heaps can supply the
existing conservative recommendation policy; shared/unified heaps remain
labelled shared and do not masquerade as dedicated VRAM. Unsupported backends
or failed discovery use the explicit unknown-device default. A persisted user
policy remains authoritative. No second adapter enumeration, subprocess, or
vendor-name heuristic participates in startup.

The product renderer's private `ResidencyOwner` owns one persistent exact-byte
logical WGPU storage-buffer arena. The arena is segmented across at most four
maximal storage bindings to honor per-binding adapter limits, but has one
allocation authority, one exact-byte allocator per segment, physical-resident
map, pin ledger, eviction policy, address space, and byte ledger. Logical
capacity is the hard feasibility maximum; it is not an instruction to allocate
that many bytes at construction. Backing buffers begin with a bounded 64 MiB
aggregate usable commitment, while an unopened segment owns only the minimum
valid binding allocation. Exact-union preflight temporarily exposes logical
tails, proves placement transactionally, records each selected segment's real
high-watermark, and rolls every simulated allocator mutation back. Required
segments then grow toward bounded geometric targets, capped both by their
logical maxima and by 128 MiB beyond the exact required high-watermark.
Allocation prefers an empty segment before a populated prefix so growth avoids
a copy when possible. Populated growth submits one prefix copy, preserves
every offset, extends the existing allocator, replaces the buffer, and
rebuilds all render/pick bind groups before later queue work consumes it.
Queue ordering keeps earlier users on the old reference-counted buffer and
later users behind the copy.

The logical capacity is the minimum of the configured payload-ledger category
and the adapter's aggregate usable storage-binding/device limits; `uint8`,
`uint16`, and `float32` samples are loaded directly from native payload words.
Exact-byte allocators and a bounded undo log make transfer admission
transactional without cloning the whole arena map. Diagnostics keep logical
maximum, committed usable bytes, physical buffer allocation, resident payload,
free/placeable spans, and growth-copy work distinct. Uploads use at most three
persistent mapped staging slots. When the transfer category can retain the
complete bounded payload-upload envelope, one fixed copy scratch buffer is
carved from its surplus; smaller runtimes preserve upload capacity and proceed
directly to adaptive fallback. This correction does not increase the
configured GPU budget.

Allocation failure distinguishes aggregate exhaustion from physical
placeability. The latter means aggregate free bytes are sufficient but no
single segment contains the requested contiguous range. `ResidencyOwner`
exposes aggregate, largest-contiguous, and per-segment facts. On exact-union
preflight it may prepare one stable within-segment packing, copy each moved
payload through the fixed scratch buffer, update that page's canonical record,
and retry once. Source order is preserved, so an earlier destination cannot
overwrite a later source. Queue ordering keeps prior color/pick work on old
records and all later work on the compacted records. Logical keys, payload
bytes, validity deltas, pins, LRUs, and presentation identity do not change.
In-flight saturation defers this optional recovery instead of exceeding the
submission bound.

Every target and mode reads one fixed-capacity renderer-global sparse
directory and page-record pool. A directory slot stores the exact compact
layer/time/scale/cell key and one page-record index; shader lookup uses a
bounded open-addressed probe, exact key comparison, and tombstones. One page
record may cover several regular logical cells. Batch mutation is
change-proportional, and only the bounded tombstone/probe recovery path builds
one full replacement directory image. A dataset-generation cutover hard-clears
the directory, so dataset identity is not duplicated in every shader key.

All-invalid pages occupy metadata but no payload arena bytes. Rays advance
brick segment by brick segment, skip missing and all-invalid pages, apply
transfer-aware min/max rejection, and stop DVR after effective opacity
saturation. Compatible voxel-exact DVR layers share the fast brick-segment
loop; smooth or mixed-affine DVR layers use one common-world integration loop
so channel ordering cannot change the optical result. ISO hits are depth
ordered, and gradients are transformed to world space by the inverse-
transpose affine.

One renderer-global first-seen queue owns pending decoded lease handles.
Admitting an offer appends its key to the derived relevant-order index of each
live opaque frame lease whose immutable body contains that key. Acquiring or
changing a body rebuilds that small per-frame index once from global order;
same-body rebind preserves it. Completion and retirement apply one exact
bounded removal set to the global queue and all live indexes. Resident-frame
dirty checks are therefore O(1), and same-body transfer selection does not
scan unrelated global offers.

Each committed presentation frame stores only an opaque
`ResidentFrameLease`; the residency owner keeps its immutable requirements and
reference-counted physical pins. Rebind, body replacement, hidden-target
handoff, deactivation, and retirement update that ledger without exposing
keys, slots, or pin deltas to presentation or app state. Per-target control
contains only camera, layer, mode, grid, and global-directory parameters.
Camera-only movement rebuilds neither residency directory nor page layout.

One private `FrameCoordinator` owns exactly four fixed presentation slots and
one private eager 3D replacement. It allocates and revisions target textures,
owns color retries and completion leases, validates capture freshness, records
active then linked-2D then passive-3D work, and is the only color-submission
authority. A changed coordinated cutoff uses one color encoder and one queue
submission; a settled identical cutoff submits nothing. The 3D replacement
cannot become visible or pickable until its exact post-submit atomic swap, so
the prior complete front remains authoritative while refinement is
incomplete. A presentation record is accepted only when its target, surface
generation, and extent match the coordinator's current target authority.
Advancing a surface generation can retain the old displayed identity for
truthful stale diagnostics, but that identity cannot satisfy currentness. The
application receives current presentation facts and texture identities but
owns no parallel target, fence, retry, or submission ledger.

Volume color work has three explicit schedules. A direct request records one
native-size complete pass for a workload accepted by the fixed product
navigation class. An interactive-preview request records a complete uniform
body at the physical panel extent; reduced internal extents and preview
upscaling are rejected at both application and renderer boundaries. An
atomic-refinement request creates one renderer-owned latest-only job for the
private native-size 3D replacement. A joined worker records one horizontal
screen-row batch, submits it, waits for that exact submission without blocking
the UI thread, adapts the next batch toward a 3 ms work target, and checks
cancellation before recording more. The first batch clears the private target;
successors load it. At most one hidden batch is outstanding, so newer input
waits behind at most that batch rather than an old full-panel fine ray pass.
A request-body, frame, or extent mismatch cancels the old job and suppresses
its result. Partial rows cannot capture, publish, become pick authority, or
change texture revision.

Completion places one bounded result and requests one application wake. It
does not request a repaint per row or use the visible vsynced presentation
rate as a work clock. The application first preflights its matching staged
dataset/residency promotion, then explicitly authorizes the complete private
candidate to capture and swap. A candidate that completes in the race between
preflight and execution remains private until a later authorized cutoff.

Preview schedule authority remains distinct from pixel dimensions. A native
interactive preview is provisional and owns no exact validation capture. The
same preview texture and label remain visible after input release while exact
work proceeds; loading state cannot replace it with an intermediate
presentation. A matching direct pass or the completed private candidate makes
the only preview-to-exact transition. Identical settled work is then
suppressed.

The visible navigation decision is static for one gesture. The controller
counts physical pixels, maximum selected-scale ray steps, visible layers, one
or eight taps for voxel-exact or smooth-linear sampling, and ISO gradient taps
against a fixed 1920×1080 product work class. It considers every complete
resident full-volume rung no finer than the current target plus the exact
camera-local body, selects the finest body inside that class, and freezes its
per-layer scale map for the gesture. If a camera-local body loses geometric
coverage, one monotonic cut chooses the finest safe resident full-volume rung;
that gesture cannot upgrade again. GPU timings cannot change visible LOD or
output resolution. They may certify the exact same camera/profile for a
future direct pass and supply a conservative initial hidden-batch size.

Hidden-work classification uses one bounded family cache. Families separate
projection, ordered layer mode, sampling, ISO shading, and one of 32 ISO
display-threshold bands. MIP's former 1x and DVR/ISO's former 2x values survive
only as conservative first-observation priors. The renderer worker owns
within-job batch adaptation from completed submissions, so a one-row initial
estimate can expand immediately and is never limited to one row per display
refresh. Fast or slow hidden observations do not probe or change visible
preview policy. Job/results, profile observations, family calibrations, and
timing associations all have fixed bounds.

The navigation floor is the terminal geometry generated by the import
contract, not a named ordinal. Its complete active-layer/timepoint body remains
in the ordinary globally accounted requirement union. When one canonical
resource covers a complete terminal layer, control field 62 names the
renderer-owned page record directly. Volume kernels then bypass per-segment
directory hashing, resolve origin/shape/payload/dtype/validity once, and reuse
that address through the ray. Smooth-linear sampling resolves one resource for
the complete eight-tap footprint rather than repeating sparse lookup for every
tap. Multi-page exact bodies retain the ordinary global sparse-directory path.

For 3D, the terminal body is the mandatory first rung of one bounded coherent
navigation ladder. The planner advances every visible layer by at most one
catalog level per rung and admits a finer full-volume body atomically only
while the optional one-quarter tail, 512 MiB/16,384-resource caps, and exact
global union all fit. Every candidate owns an exact one-scale selection body.
Its renderer wrapper places that exact body first and carries the common
aggregate ladder as a dormant suffix. Only the exact prefix participates in
coverage or volume control; the suffix is ordinary global residency prefetch
and cannot be promoted into this frame. A cold start may select only an
uploadable terminal prefix, and presentation still waits for actual GPU
residency.

When a new frame has the same immutable requirement body and full resident
coverage, the coordinator asks residency to rebind the opaque frame lease and
shared pin cohort before recording color. That bounded preparation does not
run the arena allocator, mutate the directory, upload payloads, or submit a
transfer. Completion leases are still acquired before a replaced front is
released, and retained-navigation accounting advances only after its color
work is submitted.

The native existing-device constructor creates fixed buffers and layouts, then
hands shader/pipeline creation to one capacity-two worker. Its current ordered
color set is dedicated Plane, MIP, DVR, ISO, and Mixed, followed by the
separate Pick compute pipeline. The application polls at most one event per UI
turn and treats compilation as bounded background work; data demand and lease
offers continue, but presentation and currentness cannot cross the capability
gate. Each shader or pipeline creation has its own error-scope check, so the
first failing operation stops later creation and supplies the retained typed
cause. Runtime drop cancels remaining stages and detaches from an in-progress,
driver-owned synchronous compile instead of blocking application teardown.
This is the sole renderer construction path. Focused GPU tests also request
their Vulkan device externally and poll this background readiness protocol;
there is no blocking or renderer-owned-instance test constructor.

One exhaustive private color-kernel selector maps every cross-section to the
dedicated Plane pipeline, homogeneous volume stacks to their MIP/DVR/ISO
family, and heterogeneous stacks to Mixed. Plane is composed only from shared
binding, directory, payload, sampling, transfer, additive-composition, and
fullscreen-vertex code plus its small Plane body. Its compilation unit has no
volume ray, page DDA, MIP, DVR, ISO, pick, or dynamic view-kind branch.

Plane requirements carry one immutable target-to-coarser catalog chain per
visible layer. Plane-only control records append each eligible level's shape,
cell geometry, and affine map after the fixed layer controls; volume control
layout and shaders are unchanged. A plane sample probes the target first and
then coarser resident pages through the same global directory. Voxel-exact
sampling chooses one level for the sample. Smooth-linear sampling retries the
entire interpolation footprint at one coarser level if any tap is missing, so
no interpolated value combines levels. Invalid fine data terminates lookup as
an invalid scientific result; only missing residency permits fallback.

Volume families share only ray construction and page-segment traversal
mechanics. The dedicated MIP module adds one canonical core that retains a
resolved page across its segment, computes the raw maximum over valid samples,
marks absent pages as incomplete coverage, and applies the layer transfer once
after traversal. The dedicated Mixed kernel calls this same core for an
authored MIP layer; it owns no duplicate MIP implementation. MIP contains no
DVR/ISO/mixed dispatch, opacity termination, pick binding, or donor evidence
path.

The dedicated homogeneous DVR module owns compatible-grid fused integration
and common-world joint-medium integration. Its physical step is measured in
world distance, layer order is canonicalized once on the CPU, and early
opacity termination is enabled only for exact coverage. Mixed can reach the
same canonical single-layer emission-absorption primitive but cannot call or
redefine either homogeneous whole-stack kernel.

ISO shares only the volume mechanics and premultiplied over primitive. Its
dedicated source owns the six-tap inverse-transpose world gradient,
attached/detached lighting, canonical first hit, physical world hit depth, and
depth-sorted homogeneous composition. Mixed composes the three canonical
single-layer cores in authored order and is the only color source containing
mode-code dispatch; invalid codes fail closed. Pick shares only the accepted
binding/sampling and volume mechanics plus narrow DVR-opacity and ISO-gradient
primitives. It compiles no color kernel or fragment entry. There is no general
pipeline or monolithic shader.

`FrameIdentity` is also the mailbox's opaque `RenderIntentRevision`; the
application is its sole allocator and renderer layers only copy and compare
it. `PresentationTarget` is the one fixed logical identity for 3D, XY, XZ,
and YZ. Application `PresentationSlot` is a type re-export of that identity,
not another state or conversion.

The mailbox allocates unique values from one monotone sequence but retains
independent latest identities for `ThreeD` and the linked XY/XZ/YZ family.
Camera and 3D-viewport changes advance only 3D; cross-section and linked
viewport changes advance only linked 2D; shared layer/time/transfer/source
changes advance both once. Finishing a raw gesture commits durable state
without allocating a second frame. Consequently linked-only interaction
cannot cancel, restart, rerender, or relabel an unchanged hidden 3D candidate.
An active gesture therefore exposes both its spatial sample identity and the
newest composed family identity: settlement uses the former, while visible
demand and presentation use the latter. The composed scheduler consumes the
latest family identity at each cutoff without making its settlement a
precondition for a ready due temporal successor.

Frame identity names semantic input, not asynchronous preparation completion.
A provisional and successor immutable requirement body may therefore be
presented under the same frame. Request-aware presentation matching compares
frame, extent, exact body allocation identity, and prefetch role before
retained pixels can satisfy a request. A successor body follows the ordinary
retain-before-release lease replacement, receives body-specific coverage, and
is independently dirty. Eviction-regression suppression is scoped to the
exact body that produced the exposed pixels; it cannot suppress a prepared
successor. This adds no body-generation counter or second frame allocator.

Pointer, button, wheel, and pinch events enter through the mapped UI boundary.
Only raw events delivered in the current egui input frame can create an
authoritative wheel-derived render-intent sample. Egui's visual scroll
smoothing tail is not durable input and cannot generate later camera or
cross-section revisions. The latest-intent mailbox accepts those samples
independently of demand-planning or refinement outcome.

Evictions produce a bounded sequenced event ledger. Events retry identically
until acknowledged, re-eviction preserves its later sequence, and capacity is
preflighted against the transaction's final
`(current ∪ evicted) - readmitted` key set before directory mutation or GPU
submission. Readmission cancels its pending event before a new victim is
recorded, so even a full net-neutral transaction never exceeds the bound.

The same renderer path supports MIP, DVR, ISO, orthographic and perspective
projection, full finite affine transforms, voxel-exact and SmoothLinear
sampling, flat and gradient-lit ISO, and attached or detached light. It also
owns latest-only asynchronous compute picks against an exact presented
page/arena snapshot. MIP argmax, first ISO threshold, and maximum DVR opacity
contribution are distinct pick policies; smooth results are explicitly marked
as interpolated samples. DVR opacity integrates the physical world length of
each ray step, including off-axis and affine-transformed views. Exact
FirstThreshold ISO picks include the six neighboring samples required by
gradient lighting before reporting complete. Crosshair, numeric ROI, and
distance tools consume current exact world hits and draw through one small UI
overlay model, not a scene graph.

The UI evaluates bounded demand signatures. The affine cell footprint in the
physical pixels of the actual 3D or cross-section view produces an independent
ideal LOD for each visible layer. Ideal is a quality target, not mandatory
admission. The planner traverses only that layer's actual catalog levels,
starts from its coarsest valid candidate, and deterministically refines toward
ideal while resource, payload, candidate, scratch, and transition-overlap
bounds fit. Exact installed or staged reuse is intended to require the same
selected projected map. Incremental linked-2D interaction keeps resident
geometric coverage provisional when its installed scale/body differs from the
selected settled target. The exact-demand signature retains installed scale
and immutable requirement-body identity, and only the complete coordinated
replacement can satisfy exact currentness.

The 3D path keeps the bounded ladder above, and every nonempty linked Plane
body keeps a complete coarsest full-volume navigation floor, inside ordinary
canonical residency. For Plane, that floor is the complete first-useful
prefix; directly selected plane resources follow as target refinement and the
optional rolling fine guard is last. The deduplicated floor, ladder, and every
accepted refinement are charged as part of the exact global retained
requirement union. The renderer's private residency owner transactionally
preflights that union against its one directory and arena; there are no fixed
panel shares or app-owned physical residency facts. A discarded trial
releases its temporary accounting rather than pinning a body that was never
installed.

A residual physical-placement refusal returns only typed scalar facts to the
application. The failed union's exact aggregate payload becomes a strictly
smaller temporary limit for the same generic selector; each retry therefore
excludes the prior candidate and finite catalog selection proceeds toward a
coarser body. The bound is keyed to semantic planning input and is discarded
when view, layout, layer, timepoint, dataset, or physical capacity changes.
Transient frame state separately records ideal, selected, target completion,
and displayed fidelity. The UI may describe a finer ideal as refining or
capacity constrained, but only failure of every valid minimum candidate is a
hard capacity error.

One latest-only camera-demand worker builds candidate bricks from each trial
selected-scale view-volume AABB plus exact brick intersection, then freezes
canonical sorted requirements, contribution-prioritized admission, scope
deltas, and render requirements. It prepares no GPU page table, slot
assignment, or renderer-specific layout. Worker results share immutable arrays
and their CPU-ledger lifetime, so the UI commits at scope/layer scale without
cloning, sorting, indexing, or traversing the complete cohort.

Sustained camera input replaces one pending job, cancels an older traversal at
bounded checkpoints, and leaves the last exact image visible until the newest
generation is ready. Each accepted camera body may append a fixed 17/16
visibility guard after the exact primary prefix. Guard-only resources are
bounded to at most one quarter of the primary count plus two, enter admission
last at prefetch priority, and are resident-but-dormant: they do not affect
coverage, readiness, fidelity, rendering, or picking until promoted. A camera
contained by that envelope promotes the already-installed dataset and render
wrappers through one scalar change, with no membership walk or residency
rebuild. Complete full-volume reuse is the same O(1) case and skips worker
submission. A live queued guard is reprioritized in place rather than creating
a second waiter or decode; its admission cursor is rewound to the old primary
boundary so the promoted tail is revisited even when the queue is full.

Linked cross-section guards use one common transient geometry for XY, XZ, and
YZ; the active panel controls scheduling priority rather than geometry
ownership. The application first proves each installed navigation floor
complete. A complete resident fine guard can still promote all three exact
bodies atomically for contained interaction. At 70% of the guard's acceptance
radius, the latest-only worker begins an overlapping successor. If interaction
crosses the old guard first, the same installed multiscale bodies render the
latest geometry from target pages where available and their floor elsewhere.
The finite guard is therefore a fine-prefetch window rather than a maximum
responsive rotation.

Resident geometric coverage remains provisional and separate from installed
exact-demand identity. A selected-scale or immutable-body mismatch requires
normal worker replacement; fallback cannot be relabelled exact merely because
it covers the new geometry. The planner directly pursues the latest selected
LOD instead of serially anchoring through intermediate levels, and stale
window/LOD completions cannot publish. Durable adoption commits all three
matching exact linked bodies together.

Demand signatures and prepared retained unions preserve overlapping scope
interest. The worker also computes the exact old-to-new retained-key removal
delta and retains the renderer-union base against which that delta was
prepared. Publication first validates every scope, immutable-body identity,
charge, and exact union-base identity, then asks renderer residency to
preflight the proposed next global union. A result made stale by an intervening
hidden refinement is cancelled and replanned rather than classified as a
capacity failure. Publication then retires obsolete runtime waiters with one
atomically prevalidated batch and performs an infallible scope/union commit.
Cancelled staged waiters carry their minimum exact retry cursor into the
promoted scope, so atomic current/refinement replacement cannot strand an
already-advanced guard position. A rejected result therefore neither cancels
useful overlap nor exposes a mixed generation.

Volume rendering first records the newest camera through either a certified
native direct pass or a complete uniform navigation preview at the physical
panel extent. A complete selected body is not automatically
interaction-safe merely because it is resident. During active input the
controller uses the finest complete resident target-eligible ladder rung
inside the fixed work envelope; only absence of a finer safe rung reaches the
terminal body. After a current preview exists, the selected uniform body renders
through the renderer-owned asynchronous row-batch scheduler and swaps
atomically only after every row for the same camera and controls completes and
the application authorizes promotion. Visible coordinated cutoffs observe
progress but do not drive it. An incomplete selected resource body remains
hidden under the same exact-promotion contract. A statically safe finer body
still rebinds and renders directly without a compulsory coarse flash. Dormant
prefetch is excluded from volume presentation completeness.

Plane rendering instead permits every current-geometry frame whose complete
navigation prefix is resident. Target pages become visible in bounded
refinement batches, while missing regions sample a coarser eligible level.
This provisional presentation cannot satisfy exact settlement. Exact 3D and
linked publication both bind selected scale maps, immutable requirement-body
identity, and installed demand signature to the current snapshot. Target,
scope, retained ownership, and ticket reconciliation then publish as one
staged transaction.

Exact renderer residency additions allow CPU display leases to retire after
GPU commit. Exact sequenced removals rewind only the affected monotone
admission/readiness positions, then reoffer an existing CPU handle or submit
the one missing demanded key through the normal dispatcher; no per-scope GPU
recovery set exists. Required-prefix and full camera-guard readiness have
separate monotone cursors. Current-only, refinement-only, dual-scope, and
installed-empty progressive states check every installed body without
requiring an absent companion scope. Generation/dirty tracking suppresses a
volume submission for an unchanged settled view. There is no automatic
background package verifier or post-interaction verification grace. The
optional package-integrity audit starts only from an explicit user command,
runs on its own cancellable worker with byte-accounted scratch, and never owns
renderer demand or presentation state.

Dataset scheduler and ledger wakeups carry predicate generations. A queue or
capacity change that lands between a worker's check and wait is therefore
observed without timeout recovery polling. While forming a decode cohort, the
worker also subtracts the summed not-yet-charged source working bounds from
available capacity; final sink buffers remain normally charged. This prevents
self-overcommitted all-or-none cohorts while retaining optimistic competition
between independent workers. Priority promotion validates the exact live
ticket, updates its shared job and lazy-versioned heap entry under the same
lock, and consumes neither another queue slot nor another source waiter.

Renderer diagnostics include planning/submission work, payload/control upload,
control-publication write count/peak/fallbacks, residency/reupload/eviction,
aggregate and per-segment payload placeability, largest contiguous ranges,
compaction/moved-resource counts, staging and GPU-byte peaks, successfully
adopted usable pipeline handles, and optionally asynchronous GPU upload and
volume-pass timestamps. Timing queries are opt-in for
diagnostics/qualification rather than unconditional product readback.
The developer-local linked interaction trace also records generated and egui-
received input, UI-update duration, demand-planner and dataset/source/renderer
counters, renderer CPU planning/submission, linked-pass GPU timestamps, and
egui texture-image command queuing. The latter is a UI-construction boundary,
not a surface-present event. No trace event claims compositor or monitor
visibility.
Automation-only validation capture remains asynchronous. The independent CPU
oracle owns expected RGBA, coverage, and validity facts; it is not a product
renderer. The current generic product capture and image-stat boundary
implicitly selects the 3D target, so it cannot validate a linked-2D fidelity
claim. Target-explicit linked artifacts are client/GPU-output evidence only;
`XGetImage` client-surface reads are never classified as compositor-presented
or monitor-visible. The inspector likewise keeps independent 3D fidelity
separate from linked XY/XZ/YZ shown, selected, ideal, provisional/refining,
and display-current facts.
Checked API ceilings are 64 active layers, 65,536 requirements per
presentation, four fixed public presentation targets, five private
allocations (four fronts plus one hidden 3D candidate), and 1920x1080. The
candidate may retain the fifth opaque lease only while staging an atomic exact
3D replacement; there is no public presentation-token allocator. The old 256-record,
128-lease, and 16,384-global-dimension ceilings are deleted.

The WGPU runtime shares one `OnceLock`-backed terminal-failure latch with the
device-loss and uncaptured-error callbacks. The first observed device loss,
out-of-memory, backend-internal, or validation cause wins. Frame, poll,
submission, and mapped-buffer boundaries return that typed cause before
continuing unsafe work. Cleanup remains usable, but there is deliberately no
device-recovery epoch, CPU renderer, silent backend fallback, or second
application-wide GPU latch.

## Persistence And Settings

Target packages receive bounded structural admission through
`LocalPackageCatalog` and `LocalDatasetSource`. Ordinary use performs no
automatic whole-package scan: each consumed object is checksum/currentness
checked, and externally supplied packed facts become authoritative only after
the corresponding payload is decoded and checked. Project attach, open, save,
recovery, analysis, and export do not wait for unrelated package bytes.

A package produced by the active importer instead arrives with the linear
self-consistent publication capability described above and is installed
through the same atomic `DatasetOpened` reducer completion. Dirty-project
deferral retains that exact capability until Save, Discard, or Cancel; it is
never reduced to a path-only request. A user may start or cancel an explicit
full package-integrity audit. That read-only operation reports exact encoded
objects, recomputed S0 content address, and packed-fact agreement with real
work counters and exact failures. Its result never authenticates the producer,
gates ordinary work, changes source authority, or clears visible pixels.

`mirante4d-project-store` is the sole project-storage authority, reached by
the product only through `ProjectStoreApplicationService`. Its directory-backed
format uses immutable content-addressed objects, complete immutable
generations, and small atomic manual, autosave, recovery, and pin refs. Direct
and paged object closure is deterministic and bounded. One background actor
owns filesystem mutation, open sessions, leases, requests, cancellation, and
joined shutdown.

A session holds a shared maintenance lease; writable sessions also hold the
single writer lease. Writer contention opens an existing project read-only.
Create, Open, Save, Save As, autosave, explicit recovery selection, dirty
close, and application exit all use the same service and actor. Save As copies
and verifies the authenticated source closure into destination-local staging.
Autosave runs after 30 idle seconds or 120 seconds at most while the project is
dirty. Indeterminate durability suspends further writes until reopen rather
than treating visible files as a successful commit.

Open and recovery validate bounded control records, generation closure,
continuity, provenance, and filesystem object types without repairing the
source project. A recoverable failed Open retains a recovery-only session for
inspection and explicit selection; Save As can then install the selected state
in a new project while leaving the damaged project untouched. Writable
qualification is limited to the accepted Linux ext4 filesystem tuple;
unqualified existing stores open read-only and unqualified new destinations
fail before mutation.

Maintenance features remain deliberately narrow. Full verification hashes the
stable active closure but does not validate artifact scientific semantics,
repair data, inspect trash, or establish a broader durability claim.
Compaction planning is metadata-only and does not authorize deletion, estimate
reclaimable bytes, or prove backup approval. Private Trash and Purge operations
accept only their bounded zero-non-regenerable subsets and fail closed on
unknown, linked, malformed, foreign, or unsafe content. They have no product
UI, cannot authorize removal of non-regenerable artifacts, and their
process-crash coverage does not establish power-loss durability.

Settings use `mirante4d-settings-v1` at the Linux XDG/HOME path. The UI submits
validated changes; one background actor owns persistence. Legacy preferences
files are neither read nor changed.

## Guardrails

- One live authority per model, resource, operation, and persisted identity.
- No compatibility reader, fallback renderer, or parallel old path.
- No full-dataset in-memory product path or file-per-brick layout.
- Large work is bounded, cancellable, generation-aware, and stale-suppressing.
- Normal interactive rendering requires a working GPU. CPU rendering is for
  reference, diagnostics, export, benchmark, and explicit tests.
- Rendering/loading/UI/GPU changes require real product validation under
  [testing](TESTING.md).

`mirante4d-storage::PackagePath` is the sole package-path authority.
`mirante4d-identity` owns raw typed object facts and exact hashing, but no
parallel path type.
