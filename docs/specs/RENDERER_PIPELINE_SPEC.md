# Renderer Pipeline Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the current renderer pipeline boundary, value representation, GPU
resource policy, compositing order, and readout behavior.

## Scope

This contract covers shader/backend policy, GPU resources, CPU/GPU renderer
products, `MIP`/`DVR`/`ISO` value semantics, empty-space/range skipping,
overlays, picking, and projection integration.

## Render Mode Policy

- 3D intensity datasets open in `MIP` by default.
- `DVR` is the primary depth-aware direct volume rendering mode.
- `ISO` is the threshold/surface mode.
- User-facing mode names are exactly `MIP`, `DVR`, and `ISO`.

## GPU Resource Model

- Normal eligible interactive presentation uses renderer-owned GPU display
  targets.
- CPU-visible products remain reference, diagnostics, export, benchmark, and
  test machinery.
- Renderer resources are keyed by dataset/layer/channel identity, timepoint,
  viewport, projection, LOD, transfer/mode state, visibility, and resident-data
  completeness as relevant.
- Capacity/budget failures must be explicit and must not silently change mode
  semantics.

## Value And Compositing Contract

- `MIP` produces scalar source-value output plus coverage.
- `DVR` produces premultiplied display RGBA plus coverage/completeness and is
  governed by `DVR_TRANSFER_RGBA_RENDERING_SPEC.md` and
  `DVR_OPACITY_TRANSFER_FUNCTION_SPEC.md`.
- `ISO` produces typed surface-hit output and is governed by
  `ISO_DISPLAY_SPACE_SURFACE_RENDERING_SPEC.md`.
- App compositing must respect product type. It must not route typed `DVR` or
  `ISO` products through scalar display transfer.
- Range/empty-space skipping must be conservative for the active mode and
  transfer state.

## Pass Ordering

The normal frame order is:

1. select visible layers/channels and display graph
2. plan data residency and LOD target/fallback
3. render mode-specific cohort products
4. composite intensity products into the display target
5. composite labels, overlays, hover, and interaction affordances
6. present through egui/wgpu

## Picking And Readout

- Picking follows the same product semantics as rendering.
- Readout reports source values or typed hit/source metadata, not final display
  RGB unless explicitly requested as a display diagnostic.
- Missing, incomplete, hidden, or uncovered states must be distinguishable.

## Invariants

- Renderer code does not read dataset files directly.
- Hidden channels do no current-frame scheduling, decode, upload, render,
  composite, pick, or readout work.
- Unsupported GPU/resource states fail clearly instead of falling back to stale
  or semantically weaker output.

## Testing Requirements

Coverage must include CPU/GPU product parity where available, typed DVR/ISO
product boundaries, display-target rendering, range skipping correctness,
hidden-channel exclusion, overlays, picking/readout, projection behavior, and
product-open validation for renderer/GPU/display changes.
