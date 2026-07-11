# Architecture

Last updated: 2026-07-10

## Purpose

This is the concise current architecture map for Mirante4D. Binding
implementation facts live in `docs/CURRENT_STATE.md`; target replacement
architecture lives in the active foundation handoff and its owning briefs.

## Product Shape

Mirante4D is a native Rust desktop viewer and analysis workbench for large 4D
microscopy datasets. It is not a browser app, server app, headless product, or
compatibility layer for `llsm_viewer`.

The core app opens strict native `.m4d` dataset packages and `.m4dproj`
project packages. Source microscopy data enters through explicit
import/preprocessing workflows that write the current native package format.

Current persisted identities:

- Native dataset format: `mirante4d-v1`, schema version `1`.
- Project/session format: `mirante4d-project-v14`.
- Preferences format: `mirante4d-preferences-v1`.

## Crate Boundaries

- `mirante4d-app`: egui/wgpu workbench, application state, project/session
  workflows, UI commands, import dialogs, and product orchestration.
- `mirante4d-analysis`: typed analysis operations, measurements, tables, plots,
  ROI helpers, and scene-artifact models.
- `mirante4d-core`: shared IDs, units, geometry, coordinates, transforms, and
  pure domain types.
- `mirante4d-data`: validated dataset access, async brick reads, cache budgets,
  prefetch, cancellation, and runtime diagnostics.
- `mirante4d-format`: native manifest/schema models, validation, checksums,
  Zarr-backed storage, fixtures, and writers.
- `mirante4d-import`: GUI-backed TIFF/OME-TIFF import and preprocessing into
  native packages.
- `mirante4d-renderer`: CPU/GPU rendering, camera rays, transfer mapping,
  render products, GPU-resident display targets, and GPU resource contracts.
- `xtask`: developer, CI, benchmark, packaging, and evidence automation only.
  It is not a user-facing product mode.

Dependency direction stays narrow: app orchestrates; lower crates do not import
app/UI layers; renderer does not read files directly; format does not know
viewer state.

These are current implementation facts. The owner-approved replacement graph
and staged deletion/cutover mechanics are in the [workspace architecture
brief](plans/active/foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md), under
the sole program authority of the [foundation implementation
handoff](plans/active/FOUNDATION_REFACTOR_HANDOFF.md). They remain an unimplemented target
and are not current source architecture authority.

## Runtime Flow

```text
native package/project
  -> strict format and identity validation
  -> data engine handle
  -> async brick/shard reads plus bounded cache/prefetch
  -> resident CPU/GPU brick sets
  -> per-channel display graph and render cohorts
  -> renderer-owned GPU display texture
  -> egui-wgpu presentation, overlays, hover, picking, diagnostics
```

Small fixtures and huge local datasets use the same runtime contracts. Full
residency may happen because a dataset is tiny or budgets are large, but it is
not a separate product architecture.

## Current Display Architecture

The viewer uses per-channel render state. Fresh native datasets open visible
intensity channels in `MIP`. A project restores per-channel render modes and
typed mode parameters only after strict dataset identity validation.

The supported intensity render modes are:

- `MIP`
- `DVR`
- `ISO`

The live product display is built from a mixed display graph. Visible channels
are grouped into render cohorts, cohort products are composited deterministically,
and hidden channels must not schedule, decode, upload, render, composite, pick,
or report current-frame intensity values.

Normal eligible interactive display uses renderer-owned GPU display targets.
CPU-visible frame products remain reference, diagnostic, export, benchmark, and
test machinery, not the normal product presentation path.

## Guardrails

- No legacy readers, compatibility shims, silent downgrade paths, or old-format
  fallback branches unless the user explicitly asks for them.
- No full-dataset in-memory product architecture.
- Missing occupied data is loading/incomplete, not empty.
- Analysis must use data-engine contracts, not incidental renderer residency.
- Project/session changes are hard cutovers while the project is greenfield.
- Renderer, data-loading, GPU, interaction, or large-dataset work needs
  product-open validation before completion is claimed unless explicitly waived.
- Broad architecture, renderer, format, preprocessing, data residency, or
  corrective refactor work follows the high-risk workflow in `docs/AGENTS.md`.
- `cargo xtask verify-fast` enforces the current source-size architecture gate;
  new large files require explicit policy, not silent growth.
