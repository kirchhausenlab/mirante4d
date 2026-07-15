# Current State

Last reviewed: 2026-07-15

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
scientific validation reads each present base brick once. The writer retains
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

See [testing](TESTING.md) for commands and claim language.

## Known Limitations

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
- The import performance implementation, public T2 tooling, and strict private
  T5 normal-product qualification runner exist, but clean five-session public
  timing and private three-session HW-2 execution remain acceptance work. The
  public supporting target-source and generated-import scenarios have passed
  on a mapped physical X display, but they are not the private T5/HW-2 product
  qualification. The local host/storage and private-facts digests are not
  owner-accepted yet, so no final product-performance claim is recorded.
- The target dataset profile and project-store format are experimental and
  carry no compatibility promise.
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

[Current work](planning/NOW.md) records the current development status.
