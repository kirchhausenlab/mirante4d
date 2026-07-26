# Current State

Last reviewed: 2026-07-18

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

The importer now indexes the reviewed TIFF inventory once, traverses admitted
native strips/tiles in source order, decodes each admitted native chunk once
into a two-file canonical base cache, and derives base chunks, pyramids, and
scientific identity without a second TIFF decode. Its sole checkpoint authority
is that cache plus a four-file batched-durability spool. Eligible one-plane
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

Reviewed `uint8` sentinel imports now use one guarded policy at every product
boundary. Base chunks and scientific-identity tiles classify exact source
sentinel bytes through a clipped one-voxel halo, dilate invalidity in 2D or 3D,
and store canonical zero for final invalid samples. Every coarse sentinel LOD
uses valid-only aligned factor-two means with half-up rounding, derives support
from final parent validity, and applies the same one-voxel invalid dilation.
Fused workers read ordinary parent pixels separately from expanded
validity-only halos; packed-index uniform facts avoid mask decoding, and
halo-only parents never read pixel payloads. Sentinel mean LODs publish
axis-aware centered transforms. Imports without a sentinel retain their
existing point-decimation and origin-anchored transforms.

Source inspection admits grayscale `uint8`, `uint16`, and finite `float32`
TIFF/OME-TIFF pages using uncompressed, LZW, Deflate (current or old TIFF
code), or PackBits compression. JPEG, WebP, Zstd-in-TIFF, fax, and other
compression paths are rejected because their decoder workspaces are outside
the audited memory authority. A cancellation-aware, fixed-read raw preflight
bounds every primary IFD, eager decoder field, native chunk table, and the
65,536-page retained-decoder authority before `tiff::Decoder` is constructed.
Inspection, ingest, and the final exact SHA-256 pass compare the opened file
descriptor's device/inode/generation facts with the guarded path generation
before and after use; ingest and final revalidation also require the reviewed
generation. None of these changes alters the active dataset or identity
profiles.

`mirante4d-analysis-core` owns exact `uint8`, `uint16`, and finite `float32`
intensity statistics and artifact payloads. `mirante4d-analysis-runtime` runs
those operations through the shared dataset scheduler with a fixed two-block
window, lower priority than interactive work, scoped cancellation, and stale
result suppression. The product exposes whole-layer summaries over time and a
numeric axis-aligned box at the current timepoint. A complete table/plot pair
becomes visible only after one atomic project-store commit, and authenticated
pairs are restored when the project reopens. There is one live canonical
application reducer and one canonical project model.

The viewer hot path now uses one persistent, byte-accounted logical WGPU
storage-buffer arena, segmented across at most four maximal bindings to honor
adapter binding limits, with direct `uint8`, `uint16`, and `float32` loads. A
single allocator, residency epoch, eviction policy, and byte ledger govern the
segments; they are not alternate representations. Each active
layer has one compact sparse hash page table, so shader lookup no longer scans
the resident-resource list for every sample. The renderer traverses volume
bricks, skips missing, all-invalid, and mode-noncontributing bricks, applies
DVR early termination, and jointly integrates multichannel DVR even for
smooth sampling or mixed affine grids. All-invalid bricks are metadata-only
residents. Uploads use one bounded persistent mapped staging pool, and exact
residency addition/removal deltas coordinate CPU-lease retirement and
recovery. Reused mapped slots clear only alignment gaps and tails, not payload
spans that the upload immediately overwrites. GPU timestamp results and volume
picks are asynchronous; neither requires the UI thread to wait for a GPU
mapping operation.
Full static render-control publication uses three queue writes regardless of
the 65,536-entry availability pattern. Compatible exact successor bodies with
at most 32 total added-and-removed keys preserve stable record/hash slots,
publish copy-on-write cell deltas, and reconcile pins/LRU proportional to that
delta;
larger or incompatible changes use the sole dense builder. Individual
residency changes patch only their stable records, including record-only
dormant-prefetch uploads. Incremental runtime publication is
capped at 64 writes before issuing any GPU operation and replaces a more
fragmented delta with one bounded dense record-slab write. Diagnostics expose
the total, per-frame peak, and fallback count.

Application demand is signature-diffed and split by 3D, linked-panel,
playback, and analysis scope. One bounded latest-only worker performs exact
camera candidate testing, contribution ranking, canonicalization, scope
deltas, render-requirement preparation, and static sparse-page preparation;
the UI swaps completed immutable artifacts instead of rebuilding a large
camera cohort. Sustained orbit replaces one pending request and cancels the
older traversal while retaining the last exact presentation. A full-volume
resident cohort bypasses camera planning entirely. A bounded 17/16 camera
guard appends at most one quarter of the primary resources plus two as a
dormant prefetch suffix. It may decode and publish GPU control records after
visible work, but does not affect readiness, coverage, fidelity, rendering, or
picks until an O(1) scalar promotion. Contained camera and full-volume reuse
promote the installed dataset/render wrappers without a membership walk or
static rebuild. A queued guard is reprioritized in place without a duplicate
waiter, decode, or queue slot, and exact admission-cursor rewinds preserve
canceled staged guard work across atomic promotion.

Candidate bricks come from the selected-scale view volume rather than
per-pixel ray discovery. Bounded admission cursors, exact eviction-recovery
sets, and retained leases preserve overlapping work. Worker-prepared retained-
union removal deltas and one atomically prevalidated batch cancellation make
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
bytes as explicit waste. A short fixed post-interaction grace keeps background
verification below warm resident navigation. Verification blocks on a
condition variable instead of periodically polling, then resumes after
interaction settles.

Normal local reads retain one accounted package-root descriptor, resolve
named objects beneath it with Linux `openat2` no-symlink/no-magic-link
constraints, and reuse at most 64 authenticated object handles with their
decoded shard indexes. One byte-accounted eight-entry physical-brick cache
coalesces sequential and concurrent semantic consumers, including the sixteen
2D semantic regions that may share one physical chunk. Aligned full 3D bricks
stream through bounded writable spans directly into the runtime-owned sink
without a payload-sized copy. Current-view cohort members share one fail-closed
pre-use/post-use currentness transaction while preserving member-local faults
and cancellation. Verified delivery trusts packed payload facts only after the
scientific verifier has decoded and checked every pyramid scale. Reuse and
batching do not weaken mutation detection or scientific identity. Native
codec/range workspace is released immediately after physical decode; only the
exact decoded-brick retention charge may survive a residency-capacity bypass
while joined semantic consumers finish.

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

See [testing](TESTING.md) for commands and claim language.

## Viewer Performance And Development Recovery Status

The renderer, demand, scheduling, storage-read, capability-restoration, and
small-tool implementation described above remains the normal product baseline
on `main`. Revision `90c0f20` passed 11 policy phases, eight Rust phases, 1,275
selected tests, and the two native viewer scenarios. Owner product testing
nevertheless rejected it as the performance outcome: wheel zoom can lag or
freeze, the four-panel workflow can settle at an unjustifiably coarse LOD, and
compound-angle movement can visibly expose arriving bricks.

That mismatch demonstrated that the subsequent EP-00/EP-01 development
qualification was not proportionate or sufficiently product-directed. The
five qualification commands, selection authority, nine schemas, receipt and
replay implementation, harness, and test-only shader evidence generator are
deleted. Remaining qualification-only counters in shared product automation
are frozen for focused cleanup. The retired protocol establishes no current
performance claim and is not a prerequisite for implementation.

The damaged unpublished linked worktree has been independently preserved and
reconstructed. Its 199 recoverable commits through `3d967f0` plus the surviving
seven-file delta now exist as clean recovery commit `3dfe9de` in a verified
external bundle. That branch includes real successor product experiments as
well as a much larger qualification substrate. It is a research spike, not an
implemented product fact, and will not be merged wholesale.

The active [development-simplification and viewer-recovery
plan](plans/active/DEVELOPMENT_SIMPLIFICATION.md) now owns recovery, process
right-sizing, product-only extraction, and three independently validated
viewer slices: resident interaction/per-view LOD, cold complete refinement,
and MIP/DVR/ISO kernels. A storage-format rewrite is not presumed; it requires
measurement showing that storage geometry remains a dominant blocker after
runtime fixes.

No absolute performance, comparison-viewer, successor-completion, or release
claim follows from current evidence.

## Import And Preprocessing Performance

The import/preprocessing hard cutover is implemented and automated-verified at
immutable revision `eb5c9ffd12cbd9fce65bd03559b8e7f93170d72e`. Both
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
its recorded release executables. The restored sentinel cutover now has the
separate correctness evidence described below but does not inherit this
historical T5 performance claim.

## Known Limitations

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
- Target packages open provisionally while bounded exact-package and
  scientific-content verification runs in the background. Project attach,
  open, and save remain blocked until verification succeeds; observed source
  drift invalidates that result and requires verification again.
- Imported-package capability transfer is fail-closed within its cooperative
  local destination-parent namespace. Observed mutation, an unlisted object,
  root substitution, ambiguous rename binding, or durability failure yields no
  open-ready authority and never selects the ordinary full verifier as a
  fallback. The contract does not defend against a hostile actor able to
  concurrently rename or unlink entries in that parent: Unix cannot atomically
  bind a source name to an already-open directory descriptor. A later explicit
  external open remains an independent normal open and performs full
  background verification.
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
- Packaged runtime does not expose unsaved-autosave recovery.
- Direct X11 close of a clean project can hit an inherited Winit shutdown
  panic; the dirty-project save/discard/cancel route exits cleanly.
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
