# Decision Index

Last updated: 2026-07-11

Accepted rationale lives in [`docs/decisions/`](decisions/README.md). The
[foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) owns package
order and implementation gates; [CURRENT_STATE.md](CURRENT_STATE.md) owns
implemented facts.

## Binding Decisions

- Mirante4D is a native Rust desktop viewer, not a browser/server/cloud viewer.
- The project is greenfield. Replaced readers, formats, models, runtimes, and
  fallbacks are deleted rather than preserved as compatibility paths.
- Linux x86_64/Vulkan is the sole foundation product/release target. Windows
  and macOS are portability lanes until separately qualified.
- Interactive viewing requires a qualifying GPU. CPU rendering is for tests,
  reference output, diagnostics, and export only.
- Large datasets use bounded streaming, multiscale access, explicit CPU/GPU
  byte ledgers, cancellation, and current-generation result suppression.
- The target dataset package is a strict M4D profile over OME-NGFF 0.5 and Zarr
  v3. Pixel and large-index arrays are sharded; one file/sidecar per brick is
  forbidden.
- Scientific, package, recipe, derivation, release, and artifact identities are
  distinct versioned SHA-256 contracts.
- The target project store uses immutable content-addressed objects, complete
  generations, atomic head/recovery refs, revision-aware autosave/recovery,
  leases, and conservative garbage collection.
- Persisted formats remain experimental until their owning cutovers accept
  them. No current compatibility promise exists.
- Segmentation is absent throughout the foundation program. Reintroduction
  requires a separate post-foundation plan.
- Foundation display qualification stops at 1920x1080; 4K is out of scope.
- Public CI uses only standard free hosted runners and retains the `$0` spend
  boundary. Trusted GPU/private-data machines never join the public runner
  pool.
- Product-facing rendering/loading/GPU/interaction/large-data work requires
  opening and exercising the packaged real viewer; automated smoke is support,
  not completion authority.
- Source is MIT-licensed with lightweight maintainer-led pull requests, no CLA
  or DCO initially, and no external dataset contributions.
- Source publication is independent of full microscopy-data publication. Full
  data requires a later rights, hosting, integrity, and citation handoff.

## Current Architecture Decisions

- Current user-visible intensity modes are MIP, DVR, and ISO.
- Render mode and typed transfer parameters are per-channel display state.
- Source intensity types are `uint8`, `uint16`, and finite `float32`; integer
  paths do not widen solely for an obsolete renderer representation.
- No-data/validity is typed metadata shared by import, storage, rendering, and
  analysis.
- Camera scale is explicit state; resizing a viewport is not zooming.
- Status must distinguish displayed/target scale, completeness, backend,
  viewport, timing, and freshness.

Changes to these decisions require a new accepted ADR rather than an implicit
code or documentation drift.
