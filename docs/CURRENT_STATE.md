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

WP-05 is complete at `97ba103463a419d696b445c414515b17a5df215f`
(`foundation-wp-05-exit-1`).

WP-06A merged at `c7cc4636a6fd8555fb58100311f1db35e40db28b`. Its
twenty-attempt cache-free Main calibration passed, with policy p95/max at 92/95
seconds and Rust critical-path p95/max at 403/408 seconds. Repository rules now
require exactly `PR / policy` and `PR / rust`.

This revision removes the transitional Bootstrap workflow, command, profile,
and audit rules. WP-06 is not yet accepted because its exact protected-main
merge revision still needs product-open validation and the exit tag.

## Current Verification Boundary

The WP-06 checkpoint discovers 878 live tests: 838 normal tests assigned once
across the public CPU leaves and 40 ignored tests assigned to the trusted GPU
lane. The six leaves are available through `cargo xtask verify-leaf`, while
`cargo xtask verify-pr` runs the two public groups without recursive aggregate
commands.

On the protected repository, `PR / policy` and `PR / rust` are the only
required pull-request contexts. Matching `Main / policy` and `Main / rust`
checks verify protected-main revisions. Hosted jobs use standard public runners
without caches or artifacts.

## Known Limitations

- WP-06 still needs exact protected-main product-open validation and its
  create-once exit tag.
- The package-capability lane remains pending because there is not yet an
  honest unsupported-GPU package command.
- The committed T1 source archive checks source TIFF facts only. Target-format
  T1 conformance remains false until WP-10A.
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
