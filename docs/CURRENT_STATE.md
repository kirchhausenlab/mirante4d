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

The workspace has sixteen crates. `mirante4d-storage` is an off-product
WP-10A library whose first slice owns frozen profile facts, portable package
paths, checked package-count arithmetic, and size/amplification ceilings. It
performs no filesystem I/O and is unreachable from the product.

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

WP-10A entry, dependency disposition, and profile freeze are accepted.
WP-10A-B implementation is active: the storage profile, scientific and exact
identities, strict control scalars, closed canonical-value, scientific, and
display-defaults grammars, restricted JCS, and exact compatibility tuple bytes
exist. The profile, record, manifest, and release DTOs, shard/index I/O,
reader, writer, validator, independent T1 corpus, and product activation
remain incomplete. Current schema-1 packages remain transitional T2 fixtures
and the sole product route.

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
- Windows and macOS are not qualified targets. 4K is intentionally out of
  scope.
- Current persisted formats have no compatibility promise.

The [foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) owns the
remaining package sequence. [Current work](planning/NOW.md) names the active
exit gate.
