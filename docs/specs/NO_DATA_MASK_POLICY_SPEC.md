# No-Data Mask Policy Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define explicit no-data/validity metadata and its effect on preprocessing,
rendering, analysis, and UI.

## Scope

This covers optional no-data masks for dense intensity layers, validity
propagation, statistics, multiscales, range metadata, rendering behavior,
analysis behavior, and validation.

## Non-Goals

- using reserved intensity values as universal no-data sentinels
- renderer-only no-data toggles
- treating zero as no-data by default

## Contract

- Invalid/no-data state is explicit metadata when present.
- Layers without no-data masks treat every geometrically present source voxel as
  valid.
- Valid zero-valued voxels remain valid data.
- Validity feeds preprocessing statistics, multiscales, occupancy, range
  hierarchy, rendering, readout, and analysis.
- Optional dilation/edge policy must be deterministic and documented when used.
- Missing occupied data is incomplete/loading, not no-data.

## Rendering And Analysis

- `MIP` ignores invalid samples.
- `DVR` contributes no color or opacity from invalid samples.
- `ISO` never emits a surface hit from invalid samples.
- Analysis measurements must state whether they include valid voxels, signal
  voxels, or all geometrically present voxels.

## UI Requirements

The UI should expose validity/no-data status clearly enough to explain dark or
missing regions without making invalid data look like measured zero signal.

## Failure Modes

- zero-valued valid voxels treated as invalid
- invalid voxels included in histograms/ranges as signal
- no-data implemented as a hidden renderer equality check
- missing data reported as no-data
- no-mask datasets accidentally treated as fully invalid

## Testing Requirements

Coverage must include mask validation, valid zero values, no-mask datasets,
statistics/range/occupancy propagation, MIP/DVR/ISO exclusion, analysis
measurements, readout status, and large-volume missing-data distinction.
