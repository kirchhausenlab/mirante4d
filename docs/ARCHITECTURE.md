# Architecture

Last updated: 2026-07-11

This document describes the current source tree. The
[foundation handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md) and its
briefs describe approved replacements; they are not implemented merely because
they are accepted.

## Product Shape

Mirante4D is a native Rust desktop viewer and analysis workbench. It opens
strict `.m4d` dataset packages and `.m4dproj` project packages. Source
microscopy data enters through explicit import/preprocessing workflows.

The current workspace has eight crates:

- `mirante4d-app`: egui/wgpu workbench, application state, UI workflows, and
  product composition.
- `mirante4d-analysis`: typed operations, tables, plots, measurements, and
  scene artifacts.
- `mirante4d-core`: shared IDs, units, geometry, coordinates, transforms, and
  domain types.
- `mirante4d-data`: dataset access, asynchronous brick reads, caches, prefetch,
  cancellation, and runtime diagnostics.
- `mirante4d-format`: manifests, validation, checksums, sharded Zarr storage,
  fixtures, and writers.
- `mirante4d-import`: TIFF/OME-TIFF review, preprocessing, and native-package
  publication.
- `mirante4d-renderer`: CPU/GPU rendering, cameras, transfer mapping, display
  targets, and GPU resources.
- `xtask`: developer, verification, benchmark, packaging, and evidence tools;
  it is not a product mode.

The application orchestrates lower crates. Lower crates do not depend on the
app/UI layer; the renderer does not read files; format code does not own viewer
state; analysis reads dataset contracts rather than incidental renderer
residency.

## Current Runtime

```text
native package/project
  -> strict format and identity validation
  -> data-engine handle
  -> bounded asynchronous shard/brick reads and cache/prefetch
  -> resident CPU/GPU resources
  -> per-channel display graph and render cohorts
  -> renderer-owned GPU target
  -> egui-wgpu presentation, overlays, picking, and diagnostics
```

Small fixtures and large datasets use the same path. Full residency can occur
for a tiny dataset but is not a separate product architecture. Large work must
remain bounded, cancellable, and generation-aware so stale results are not
presented as current.

## Display And Fidelity

Current intensity modes are per-channel `MIP`, `DVR`, and `ISO`. Visible
channels form render cohorts and are composited deterministically. Hidden
channels must not schedule, decode, upload, render, pick, or report current-
frame intensity values.

Normal interactive display uses renderer-owned GPU targets. CPU images are
reference, diagnostic, export, benchmark, and test tools—not a silent product
fallback. Status must distinguish shown and target scale, completeness,
backend, viewport, timing, and freshness. Missing occupied data means loading
or incomplete, never empty.

## Errors And Observability

Unsupported formats, invalid data, cancellation, capacity exhaustion, and GPU
capability failures should remain typed across ownership boundaries. User-
facing errors must be actionable; capacity failure must not silently choose an
alternate dense, CPU, or legacy path.

Diagnostics and logs are local only; there is no telemetry service. Public
reports, logs, screenshots, and evidence must redact private paths, dataset
identities, credentials, and unpublished metadata.

## Change Guardrails

- One live authority per model, resource, operation, and persisted identity.
- No compatibility reader, fallback renderer, or parallel old path by default.
- No full-dataset in-memory product path or file-per-brick storage layout.
- Product-visible large work has explicit CPU/GPU byte and queue budgets.
- Rendering, loading, GPU, interaction, and large-data work requires real
  product validation as defined in [testing](TESTING.md).
- Broad or corrective work follows the high-risk workflow in
  [the agent guide](AGENTS.md).

The approved replacement ownership graph is in the
[workspace architecture brief](plans/active/foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md).
It becomes current only through its owning hard cutovers.
