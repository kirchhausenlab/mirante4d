# Current State

Last reviewed: 2026-07-14

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
- MIP, DVR, and ISO intensity rendering with per-channel controls.
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
thread or channel. The predecessor `mirante4d-data`, `mirante4d-format`,
and `mirante4d-import` crates are deleted. `mirante4d-ui-egui` now owns shared
egui visuals, application-problem presentation, and transient UI drafts and
interaction state. `mirante4d-render-wgpu` is
the sole product renderer. The unpublished `mirante4d-render-reference` CPU
oracle is test-only, and the predecessor `mirante4d-renderer` crate is deleted.

`mirante4d-analysis-core` owns exact `uint8`, `uint16`, and finite `float32`
intensity statistics and artifact payloads. `mirante4d-analysis-runtime` runs
those operations through the shared dataset scheduler with a fixed two-block
window, lower priority than interactive work, scoped cancellation, and stale
result suppression. The product exposes whole-layer summaries over time and a
numeric axis-aligned box at the current timepoint. A complete table/plot pair
becomes visible only after one atomic project-store commit, and authenticated
pairs are restored when the project reopens. The predecessor
`mirante4d-analysis` crate and its segmentation code are deleted.

`mirante4d-core`, the application
`AppState` god-state, `WorkbenchCommand`, project-v14 authority, and
preferences-v1 authority have been deleted. There is one live canonical
application reducer and one canonical project model.

## Foundation Status

WP-01 through WP-06 are complete. The public root is
`d0594436c0739e19000ce5bdb9ff9fc65e8a9028`; the WP-04 bootstrap correction is
`5872e7cdf27040dd65fe324d6daf6b0e4e7bd32e`; WP-05 exited at
`97ba103463a419d696b445c414515b17a5df215f`; and WP-06 exited at
`9f398f6d19b9ce918395cb4191ccbd9d134e2344`.

WP-07A accepted the canonical domain, identity, and project model at
`5383cbb93c13c59e6f035bfa551356c75fb426dc`. WP-07B completed its live cutover
at `61cd39263d5f663d9af3fc75fa63ef054c3f4540`, tagged
`foundation-wp-07b-exit-1`.

WP-08A's corrected contract exit was accepted at
`f2e520da891134d1b3f65d8fcac7afb4140579a2`, tagged
`foundation-wp-08a-exit-2`.

WP-08B accepted the unified dataset runtime at
`0e3bdb0f5257c820841cee215cee38747efbda75`, tagged
`foundation-wp-08b-exit-1`. One scheduler and CPU byte ledger now own live
interactive demand; the predecessor app/data read pools, unbounded result
channels, resident payload mirrors, and analysis execution worker are deleted.

WP-10A accepted the off-product dataset schema, storage, index, and identity
package at `9b3a81d79a50027c0a8ddedc535021809a99d928`, tagged
`foundation-wp-10a-exit-1`. Its profile and target evidence include:
the storage profile, scientific and exact
identities, strict control scalars, closed profile, canonical-value,
scientific, and display-defaults grammars, restricted JCS, and exact
compatibility tuple bytes exist. Exact recipe bodies and verified RecipeId
payloads also exist, together with exact object descriptors, canonical greedy
manifest pages, authenticated roots, and PackageId derivation. Portable source,
recipe, derivation, rights, and citation records have exact closed schemas;
embedded recipe and derivation payloads and detached release records verify
their typed identities. Exact 64-byte packed-index records and the bounded
in-memory zstd/CRC32C shard and end-index codec also exist. Strict Zarr
group/array metadata, closed OME image-group metadata, root-confined bounded
Unix range reads, and an authenticated local metadata catalog now exist. A
separate cancellable inventory checks the exact finalized file/ancestor-
directory closure, object types and lengths, counts, depth, and fan-out under
global bounds. Descriptor-derived address plans validate one requested brick;
the bounded brick core reads only its packed-index, pixel, and validity ranges,
verifies their index/inner CRC32C and bounded zstd decoding, applies record-
authorized fill elision, and reports exact storage-boundary counters. Caller-
selected DS admission derives logical and addressed counts arithmetically,
separates actual shard files, bounds every
listed shard coordinate, and requires exact packed-index shard coverage. The
structural pass verifies every packed-index shard digest, record coordinate,
edge capacity, canonical padding, and pixel/validity inner-
slot presence. Full validation then stream-hashes the manifest root, pages, and
every descriptor object with fixed memory, requires structural/hash snapshot
coherence, repeats inventory, and finishes with a cancellable identity sweep.
Only its owning exact-package capability exposes PackageId-attributed brick
reads, checking manifest authority and every consumed shard against the proved
snapshot. Consuming that capability through the bounded, cancellable
scientific scan recomputes base-scale layer roots and ScientificContentId and
issues a stronger verified-scientific-package capability only after both
package declarations match. A deterministic off-product writer now derives
encoded objects and manifest bytes from typed metadata and a lazy sequence of
bounded decoded outer shards, hashes while writing, structurally validates a
private sibling stage, and publishes a previously absent destination with Linux
`RENAME_NOREPLACE`. Cancellation and precommit failure remove only the owned
stage during normal operation under the documented local threat model; a
post-rename parent-sync failure is reported as durability-indeterminate without
deleting the valid package. WP-10A-C now pins the exact OME-NGFF 0.5.2, Zarr
core 3.0, bytes, CRC32C, indexed-sharding, and registered zstd source artifacts
in a small offline digest-checked mirror. A hash-locked zarr-python 3.2.1 probe
independently decodes one manually built array using the selected shard subset.
That diagnostic is not T1, IO-3, OME semantic readback, or complete-package
evidence. The promoted `target-m4d-v1` authority tracks three bounded synthetic
USTAR archives, independent expected facts and critical vectors, a full-array
independent reader with pinned OME-schema results, 15 executed mutations, and
byte-identical two-run reproduction. It is the registry's sole `T1-target`
record and `target_format_t1_available` is true. The accepted production path
consumes all three packages through the production exact and scientific path,
matches every full-array and per-layer value/validity digest plus the exact
counts and one-brick amplification facts, and rejects all 15 mutations.
Production-writer outputs also pass the pinned schema and independent reader
with the same scientific and image facts while exact PackageIds may differ.
Isolated 2,750/5,500/11,000-descriptor opens satisfy the linear metadata-work
contract, and the largest stays inside its 10-second and 64-MiB post-open RSS
ceilings. `cargo xtask verify-local format-lifecycle` is the real local gate.
At WP-10A acceptance the profile was EXPERIMENTAL and off-product; no stable-
format, performance, or generic OME-Zarr claim follows from that evidence
alone.

WP-09A accepted the bounded off-product progressive Vulkan runtime and
independent CPU reference at
`1b1e7d5534f29b010cc346d434811a3906fb40e1`, tagged
`foundation-wp-09a-exit-1`. Exact protected-main policy/Rust and trusted Vulkan
evidence passed. WP-10B B1 froze the successor project-store wire, limits, public boundary,
transition inventory, and independent fixture. B2 implemented the off-product
transactional store: immutable direct/paged object closure and generations,
manual and autosave refs, leases, Create/Open/Save As/recovery, bounded
verification and maintenance, and conservative Trash/Purge subsets.

B2 is accepted on protected main at
`4a246a1bb7bfe099673ef10d6cb5951729b3ff37` (tree
`af5531d8ffbda0c13b342a0b4df47a894e7f99fb`). The clean aggregate report at
`target/mirante4d/verification/local-1-2659586/verify-local-project-store-lifecycle.json`
has SHA-256
`ced8c82c75c480810e7ebf81e2c032e579f89bbb28c1f854d1681a3ddad1f9e5`:
all 120 hosted tests and 60 rootless-VM cuts passed with zero harness retries.
The exhaustive hosted matrix took 85,683 ms and the VM evidence phase took
340,235 ms. Protected-main policy and Rust checks also passed in
[run 29273392030](https://github.com/kirchhausenlab/mirante4d/actions/runs/29273392030).
This qualifies only the exact off-product B2 ext4 tuple and revision. B3 is
accepted on protected main at
`8fdd94dc9c60406e8de8a96749d7148d38b1dc7a`; it added bounded current-source
D-009 verification, source-generation-aware promotion and invalidation,
authenticated project-object reuse and Save As closure copying, and the
revision-aware autosave service.

WP-10B B4 is accepted on protected main at
`8257f8c5bdc011651c8e74ab85dfdc86717b82d6` (tree
`56e4ac27f50311b49226520ae6c382aacfe9dde6`), tagged
`foundation-wp-10b-exit-1`. The service and actor are the sole product
project-persistence route. New, Open, Save, Save As, autosave, recovery, dirty
close, and joined shutdown are wired; the project-v15 bridge and
`CurrentProjectRuntime` are deleted. The exact-main public run passed, and the
owner accepted the preceding durability and three-launch product evidence
without requiring a redundant power-cut rerun.

WP-11 is accepted on protected main at
`04987f64c309166caddf931be9c1ef4948010128` (tree
`fd39e0be3a0883726972b25d037c916f0e3ca4c0`), tagged
`foundation-wp-11-exit-1`. Its focused importer checks, independent target
readback, and exact-main run passed. WP-10C subsequently activated that
pipeline and removed the predecessor importer.

WP-12 is accepted on protected main at
`5be750d060284d0a591ea6b5c0007bfeb136ac8d` (tree
`d704f02e4dc530c8c144fc5a2f29c572012835a2`), tagged
`foundation-wp-12-exit-1`. The bounded exact analysis runtime is the sole
product analysis route; its predecessor and segmentation code are deleted.

WP-10C is accepted on protected main at
`b9ac2a5f08101094933f80a0ce98fbdbdbe6c8d6`, tagged
`foundation-wp-10c-exit-1`. The sharded target package and bounded importer are
the sole product dataset path, verification remains responsive, proved package
and scientific identities bind to projects, and the predecessor data, format,
and import crates are deleted.

WP-09B is accepted on protected main at
`b73dd86fed8cc3ac7b34f75f20dcd8bb8ac85672`, tagged
`foundation-wp-09b-exit-1`. `mirante4d-render-wgpu` is the sole product render
route; the predecessor renderer and CPU placeholder route are deleted. WP-09C
is accepted at `d33276b6de0287da7f225da278ee016aac26358a`, tagged
`foundation-wp-09c-exit-1`. The visible egui workbench has one snapshot-in,
typed-output-out entry. UI layout and interaction live in
`mirante4d-ui-egui`; the native app retains process/service composition and
presentation-token resolution without a second widget path.

## Current Verification Boundary

The public repository requires exactly `PR / policy` and `PR / rust` on pull
requests, with matching non-required `Main / policy` and `Main / rust` checks.
Hosted jobs use free public runners without caches or artifacts. GPU,
format, project-store, packaging, and product checks remain explicit local
commands used only when their boundaries change.

See [testing](TESTING.md) for commands and claim language.

## Known Limitations

- Target packages open provisionally while bounded exact-package and
  scientific-content verification runs in the background. Project attach,
  open, and save remain blocked until verification succeeds; observed source
  drift invalidates that result and requires verification again.
- The target dataset profile and project-store format are experimental and
  carry no compatibility promise.
- The product project-store crate has its frozen API,
  control-record wire, typed generation/direct-and-paged closure, and
  generation-last immutable publication. Its crate-private transaction core
  can create the initial manual head and advance an established manual head
  under held leases, and can create or advance an established-project autosave
  head. It can also install a new initial package privately with exact Create
  facts or a caller-bound Save As fork tuple, without replacing an existing
  destination. Its public unbound actor now executes fresh Create, healthy Open,
  provisional Autosave and manual handoff, established-session work, explicit
  recovery selection, and authenticated Save As while retaining exact roots and
  leases. B2 durability qualification now passes for its exact off-product
  revision. B3 added actor-authenticated unchanged-object reuse, destination-
  local Save As closure copying, and exact autosave scheduling. The application
  service is the sole product path for ordinary project persistence and
  recovery. Product maintenance and
  Purge UI remain absent. PlanCompaction
  does not authorize Trash, expose a physical object/byte plan or reclaim
  estimate, or prove backup approval. Private FullVerify does not validate
  artifact scientific semantics, repair data, inspect trash, or establish a
  durability claim.
- Private actor-routed Trash is covered only for its bounded authorized subset;
  its callback and process-crash matrix does not simulate power loss or qualify
  filesystem durability. Private actor-routed Purge is likewise limited to its
  strict zero-non-regenerable subset; its callback and process-crash matrices
  do not simulate power loss, qualify a filesystem, or expose public/product
  execution. The current API cannot authorize removal of non-regenerable
  artifacts; supporting that later needs separately approved snapshot-bound
  itemized confirmation and verified-backup proof.
- Linux release candidates are local x86_64 artifacts, not a supported public
  release.
- Packaged runtime does not expose unsaved-autosave recovery.
- Direct X11 close of a clean project can hit an inherited Winit shutdown
  panic; the dirty-project save/discard/cancel route exits cleanly.
- Exact linked-panel cursor readout is available from retained leases; 3D GPU
  intensity hover remains unavailable rather than sampling a placeholder.
- Rendered-viewport-derived statistics remain unavailable. Product analysis
  instead computes exact source-voxel statistics for a whole layer over time or
  a numeric box at the current timepoint; loading placeholders are never
  reported as scientific zeros.
- Product rendering currently supports voxel-exact sampling, flat ISO shading,
  one semantic scale per layer, 256 requirement records, and 128 supplied
  leases per call. Unsupported cases fail explicitly instead of silently
  changing the scientific display request.
- Windows and macOS are not qualified targets. 4K is intentionally out of
  scope.
- Current persisted formats have no compatibility promise.

The [foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) owns the
remaining package sequence. [Current work](planning/NOW.md) names the active
exit gate.
