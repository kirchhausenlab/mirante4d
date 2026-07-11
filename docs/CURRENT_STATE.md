# Current State

Last reviewed: 2026-07-11

Mirante4D is public, pre-alpha research software. Persisted formats and APIs
can change through explicit hard cutovers; there is no supported public release
or public full microscopy dataset yet.

## Implemented Product

- Native Rust desktop application for Linux x86_64 using `wgpu`, `winit`, and
  `egui`.
- Strict experimental `mirante4d-v1` schema-1 dataset packages using sharded
  Zarr v3 storage.
- `mirante4d-project-v14` project/session state and
  `mirante4d-preferences-v1` preferences.
- MIP, DVR, and ISO intensity rendering with per-channel controls.
- Bounded streaming, cache, and renderer paths for data beyond memory.
- TIFF/OME-TIFF import and preprocessing, typed intensity analysis,
  ROI/annotation/track/measurement tools, and deterministic exports.
- Linux release-directory, tarball, and AppImage build paths.
- No segmentation or derived-label subsystem.

The workspace currently has the application, analysis, core, data, format,
import, renderer, and developer-automation crates described in
[architecture](ARCHITECTURE.md).

## Foundation Status

WP-01 installed a bounded temporary verification bridge. WP-02 removed
segmentation and cut project state to v14. WP-03 sanitized the source and built
the public root. WP-04 made `kirchhausenlab/mirante4d` public and installed its
zero-cost repository controls.

The preserved parentless root is
`d0594436c0739e19000ce5bdb9ff9fc65e8a9028`
(`foundation-public-root-v1`). Its sole corrective child and WP-04 exit is
`5872e7cdf27040dd65fe324d6daf6b0e4e7bd32e`
(`foundation-wp-04-exit-1`). The correction made a cold hosted bootstrap
possible without changing product behavior.

WP-05 is complete at the revision containing this documentation reset. It
replaces the fragmented specification tree with 32 registered documents and a
bounded `cargo xtask docs-check`. WP-06 is the next package; later technical
cutovers remain targets, not current implementation.

## Current Verification Boundary

`cargo xtask verify-bootstrap` is temporary partial feedback: formatting,
workspace compilation, 169 selected CPU tests, and documentation checks. The
single `Bootstrap / required` pull-request job runs that bridge on a standard
public runner. WP-06 replaces both the bridge and the inefficient legacy test
topology.

## Known Limitations

- `verify-fast` stops on a superseded source-size rule, and `report-audit`
  reports an inherited evidence mismatch.
- Raw workspace Clippy still reports inherited warnings outside the bridge.
- Packaged runtime does not expose unsaved-autosave recovery.
- Direct X11 close of a clean project can hit an inherited Winit shutdown
  panic; the dirty-project save/discard/cancel route exits cleanly.
- GPU, packaged E2E, performance, scientific, and real-data evidence remain
  trusted-local work rather than ordinary pull-request checks.
- Windows, macOS, and 4K are not qualified product targets.
- Current persisted formats are experimental and have no compatibility
  promise.

The [foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) owns the
remaining target sequence. [Current work](planning/NOW.md) identifies the next
checkpoint.
