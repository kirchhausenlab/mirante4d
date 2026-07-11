# Intensity, Validity, And Range Hierarchy Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define source intensity, validity, occupancy, range metadata, display transfer,
and auto-contrast semantics shared by rendering and analysis.

## Scope

This contract covers dense intensity pyramids, valid/min/max range metadata,
histograms/statistics, MIP/DVR/ISO range use, preprocessing requirements,
runtime requirements, and failure modes.

## Core Invariants

- Source intensity, voxel validity, signal presence, brick occupancy, output
  coverage, and display transfer are separate concepts.
- Invalid/no-data state is explicit metadata when present, not a reserved source
  intensity value.
- Valid zero-valued data remains valid data.
- Missing occupied data is loading/incomplete, not empty.
- Source dtype and source-value readout must remain truthful.
- Invert LUT changes display mapping. It must not change which source sample
  wins `MIP` or brighten uncovered background.

## Required Metadata

- Primary scalar image pyramid for display/analysis.
- Occupancy metadata that distinguishes empty/unoccupied regions from missing
  data.
- Valid/min/max range metadata over valid source samples where range skipping,
  auto contrast, ISO, or DVR transfer-aware culling requires it.
- Statistics and histograms must document whether they use all values, valid
  values, signal values, or view-local samples.

## Render Mode Semantics

- `MIP` selects the maximum valid source scalar along the ray, then display
  transfer maps the selected scalar.
- `DVR` accumulates display color/opacity from valid source samples using the
  current DVR color and opacity transfer contracts.
- `ISO` selects a valid display-space threshold crossing under the current
  transfer/window/invert state.
- Range skipping must be conservative for the active mode and transfer state.

## Auto Contrast

Auto contrast modes must state their sample scope:

- dense/package-level
- signal/valid-data focused
- view-local/current-frame

Auto contrast must not infer that invalid/no-data, missing data, or uncovered
background is signal.

## Preprocessing And Runtime Requirements

- Preserve supported source dtypes.
- Propagate validity through multiscales and range metadata.
- Validate range/occupancy metadata before using it to skip work.
- Keep renderer, analysis, histogram, and readout semantics aligned.

## Failure Modes

- valid zero data treated as no-data
- invalid values included in range/histogram evidence as signal
- max-only metadata used where an interval is required
- LOD changes alter contrast because range semantics drift
- missing occupied data treated as empty
- readout reports display color instead of source value

## Testing Requirements

Coverage must include valid zero data, invalid/no-data masks, no-mask datasets,
range hierarchy generation/validation, MIP/DVR/ISO range use, invert LUT,
auto-contrast scopes, LOD consistency, source-value readout, and large-volume
missing-data behavior where relevant.
