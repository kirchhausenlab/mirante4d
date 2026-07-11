# Current State

Last reviewed: 2026-07-11

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
- Bounded streaming, cache, prefetch, cancellation, and stale-result
  suppression for data beyond memory.
- TIFF/OME-TIFF import, typed intensity analysis, ROI/annotation/track/
  measurement tools, and deterministic exports.
- Linux release-directory, tarball, and AppImage build paths.
- No segmentation or derived-label subsystem.

The workspace has fourteen crates. `mirante4d-core`, the application
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
`5383cbb93c13c59e6f035bfa551356c75fb426dc`. WP-07B-A accepted the initially
unreachable boundary crates at `dfe49398fbacc933140cfd9a7992c7f86b3a9548`.

The current revision implements WP-07B-B's live hard cutover. Canonical
application commands/snapshots now drive the viewer; settings use the closed
`mirante4d-settings-v1` background owner; project persistence is a private
experimental v15 bridge; and later work is isolated in seven exact temporary
runtime owners. WP-07B is not complete until automated checks, trusted GPU
verification, real product-open validation, protected-main checks, and the
exit tag all pass on the accepted revision.

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
- Windows and macOS are not qualified targets. 4K is intentionally out of
  scope.
- Current persisted formats have no compatibility promise.

The [foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) owns the
remaining package sequence. [Current work](planning/NOW.md) names the active
exit gate.
