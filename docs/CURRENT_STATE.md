# Current State

Last reviewed: 2026-07-12

Mirante4D is public, pre-alpha academic research software. Persisted formats
and APIs can change through explicit hard cutovers; there is no supported
public release or public full microscopy dataset yet.

## Implemented Product

- Native Rust desktop application for Linux x86_64 using `wgpu`, `winit`, and
  `egui`.
- Strict experimental `mirante4d-v1` schema-1 dataset packages using sharded
  Zarr v3 storage.
- Canonical framework-neutral domain, identity, project-model, application,
  dataset-catalog, render-API, and settings boundaries.
- MIP, DVR, and ISO intensity rendering with per-channel controls.
- One bounded, byte-accounted semantic-resource runtime for 3D, linked 2D,
  playback prefetch, histogram, and interactive readout demand.
- TIFF/OME-TIFF import and a passive table/plot/export workspace. Analysis
  execution and viewport artifact authoring are deferred until WP-12.
- Linux release-directory, tarball, and AppImage build paths.
- No segmentation or derived-label subsystem.

The workspace has nineteen packages: eighteen `mirante4d-*` crates plus
`xtask`. `mirante4d-storage` is an off-product
WP-10A library whose first slice owns frozen profile facts, portable package
paths, checked package-count arithmetic, size/amplification ceilings, and
bounded read-only local range I/O. It remains unreachable from the product.
The new `mirante4d-render-wgpu` successor and unpublished
`mirante4d-render-reference` oracle are accepted off-product and remain
deliberately unreachable; `mirante4d-renderer` remains the only product render
route until WP-09B.

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
The profile remains EXPERIMENTAL and off-product; no stable-format,
product-support, performance, or generic OME-Zarr claim follows.

WP-09A accepted the bounded off-product progressive Vulkan runtime and
independent CPU reference at
`1b1e7d5534f29b010cc346d434811a3906fb40e1`, tagged
`foundation-wp-09a-exit-1`. Exact protected-main policy/Rust and trusted Vulkan
evidence passed; product activation and product-open validation remain WP-09B
work. WP-10B's entry is accepted. B1 freezes the successor project-store wire,
limits, public boundary, transition inventory, and independent project fixture;
B2 transactional implementation is active off-product. Its current slice owns
typed canonical generations, direct and deterministic paged object closure,
immutable generation-last publication, process-held maintenance/writer leases,
exact initial manual-head publication, and crate-private established manual
recovery-before-head replacement. The same private transaction now creates and
advances the established-project autosave lane while preserving the manual
authority. Ref publication is preceded by a bounded descriptor-relative whole-
store entry/fan-out inventory. The corrected recovery-ahead state keeps the old
lane head authoritative and supports an exact retry without repair or
promotion. A crate-private established-session actor owns the root and leases,
serializes those manual/autosave transactions, bounds requests and completions,
coalesces queued autosaves, cancels active or queued work, and releases its
session through Close or shutdown. The same actor authenticates Save As against
the live manual head and scientific identity, installs the fork through the
shared no-clobber package transaction, and changes its owned root and leases
only after durable success. A shared private inspection core now opens
and validates established stores for actor startup and transaction preflight,
including exact ref/generation continuity, bounded physical metadata closure,
autosave classification, and explicit read-only writer fallback without eager
bulk-payload hashing. A bounded read-only graph pass now also recognizes the
exact provisional autosave-only state, enumerates canonical generation/object
namespaces, validates every generation metadata closure, and separates the
declared live roots from capped orphan recovery candidates without treating
parent or autosave-base provenance as liveness. A crate-private destination-
local installer now builds a
complete initial Create or Save As package in a sibling stage, validates and
synchronizes it, and installs it with a no-clobber rename while retaining the
root and leases. The corrected public API now requires successful Open and
OpenRecovery completions to return both session and loaded projection, and
distinguishes manual recovery branches from autosave divergence without
changing any persisted bytes. A bounded private recovery reader and actor path
now discover validated manual/autosave fallbacks or capped scan candidates,
load only an explicit fresh selection, preserve actual head facts, and support
corrupt-head and writer-contended read-only sessions without repair or
promotion. The frozen public actor remains non-constructible and the crate
remains off-product. Private Pin/Unpin execution now validates the complete
graph and prospective recovery-candidate cap, preserves duplicate-pin liveness,
rejects read-only sessions, and makes directory-sync uncertainty
write-suspending. The accepted transition authority names pin, unpin, and purge
phases, but the exhaustive failpoint/kill matrix is not yet implemented. A
private read-only FullVerify path now hashes every physical object in one
bounded stable active-store snapshot, reconstructs paged logical objects,
supports cancellation and read-only sessions, and changes no store bytes.

Replacement, import/multiscale generation, and product activation remain
incomplete.
Current schema-1 packages remain transitional T2 fixtures and the sole product
route.

## Current Verification Boundary

The public repository requires exactly `PR / policy` and `PR / rust` on pull
requests, with matching non-required `Main / policy` and `Main / rust` checks.
Hosted jobs use free public runners without caches or artifacts. GPU,
performance, packaged, scientific, and private-data evidence remains local.

See [testing](TESTING.md) for commands and claim language.

## Known Limitations

- Current sources have no verified `ScientificContentId`; project attach,
  open, and save therefore reject at the typed identity gate before I/O.
- Dataset schema 1 and the private project-v15 bridge are experimental, not
  target-format conformance claims.
- The successor project-store crate exists off-product with its frozen API,
  control-record wire, typed generation/direct-and-paged closure, and
  generation-last immutable publication. Its crate-private transaction core
  can create the initial manual head and advance an established manual head
  under held leases, and can create or advance an established-project autosave
  head. It can also install a new initial package privately with exact Create
  facts or a caller-bound Save As fork tuple, without replacing an existing
  destination. Its private established-session actor executes and bounds manual
  save, autosave, and authenticated Save As work, using the same private
  established-store inspection authority as transaction preflight. Public
  Create/Open/Save As execution,
  provisional autosave publication, public/product recovery workflow, timers,
  garbage collection, public actor construction, durability qualification, and
  every product path remain unimplemented. Private FullVerify does not validate
  artifact scientific semantics, repair data, inspect trash, or establish a
  durability claim.
- The package-capability lane remains pending until there is an honest
  unsupported-GPU package command.
- Packaged runtime does not expose unsaved-autosave recovery.
- Direct X11 close of a clean project can hit an inherited Winit shutdown
  panic; the dirty-project save/discard/cancel route exits cleanly.
- Exact linked-panel cursor readout is available from retained leases; 3D GPU
  intensity hover remains unavailable rather than sampling a placeholder.
- Frame intensity statistics remain unavailable until a real lease-backed
  computation exists; loading placeholders are never reported as scientific
  zeros.
- The off-product WP-09A successor qualification is limited to voxel-exact
  sampling, flat ISO shading, one semantic scale per layer, 256 requirement
  records, and 128 supplied leases per call. Unsupported cases fail explicitly.
  Product support remains a WP-09B decision.
- Windows and macOS are not qualified targets. 4K is intentionally out of
  scope.
- Current persisted formats have no compatibility promise.

The [foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) owns the
remaining package sequence. [Current work](planning/NOW.md) names the active
exit gate.
