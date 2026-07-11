# Current State

Last reviewed: 2026-07-11

Mirante4D is pre-alpha research software. This tree is the clean source
snapshot prepared for public development; it contains no pre-public Git object
history or private operational evidence.

## Implemented Product

- Native Rust desktop application for Linux x86_64.
- Experimental `mirante4d-v1` dataset packages and
  `mirante4d-project-v14` project/session state.
- MIP, DVR, and ISO intensity rendering with per-channel controls.
- Streaming data and renderer caches for datasets larger than memory.
- TIFF import/preprocessing, intensity analysis, ROI/annotation/track/
  measurement tools, and deterministic exports.
- Linux release directory, tarball, and AppImage packaging.
- No segmentation or derived-label subsystem.

Domain specifications describe details where they still match the
implementation. This file is the sole authority for overall implementation
status.

## Foundation Refactor

The approved foundation program is active:

1. WP-01 installed the bounded temporary local verification bridge.
2. WP-02 removed segmentation and hard-cut project state to v14.
3. WP-03 sanitized the source, cleared dependency advisories, installed public
   governance, added independently checked TIFF fixtures, and constructed the
   deterministic public root.

The repository-publication operation in WP-04 leaves that root unchanged. The
next source packages are WP-05 documentation/governance ownership and WP-06
verification replacement, followed by the technical cutovers through WP-15.

The temporary normal local check is `cargo xtask verify-bootstrap`. It pins its
tool versions, enforces a 169-test CPU subset, and states its exclusions. The
sanitized pre-foundation disposition preserves all 1,055 predecessor
verification records for WP-06 without private revision or machine bindings.

The complete order and contracts live in the
[foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md). Current work
is summarized in [planning/NOW.md](planning/NOW.md).

## Known Limitations

- `cargo xtask verify-fast` stops on the superseded source-size rule.
- `cargo xtask report-audit` reports a blocking legacy evidence mismatch.
- Raw workspace Clippy reports inherited warnings outside the temporary bridge.
- Packaged runtime does not yet expose unsaved autosave recovery.
- Direct X11 close of a clean project can hit an inherited Winit shutdown
  panic; the dirty-project save/discard/cancel route exits cleanly.
- The sole hosted bootstrap workflow is provisional; WP-06 replaces it.
- There is no public full microscopy dataset release.
- Windows and macOS are not release-supported.
- Current persisted formats are experimental and carry no compatibility
  promise.

These limitations are owned by the remaining foundation work packages; they
are not claims that the current product is production-ready.
