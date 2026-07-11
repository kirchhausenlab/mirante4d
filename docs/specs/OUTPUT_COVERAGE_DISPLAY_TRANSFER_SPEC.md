# Output Coverage And Display Transfer Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define how input validity, output pixel coverage, display transfer, invert LUT,
and background color interact.

## Scope

This contract applies to intensity render modes, app compositing, uncovered
background, multi-channel display, empty/no-data handling, and readout status.

## Core Rules

- Input voxel validity and output pixel coverage are distinct.
- Covered pixels may receive display transfer; uncovered pixels show background.
- Invert LUT changes covered intensity/color mapping only. It must not brighten
  outside-volume, invalid, missing, or uncovered background.
- A no-validity-mask dataset treats source voxels as valid, but output coverage
  still depends on the render mode.
- Missing occupied data is incomplete/loading, not empty.
- Empty/unoccupied bricks may be skipped only from validated metadata.
- Readout must distinguish source value, covered/uncovered state, missing data,
  and unavailable mode semantics where relevant.

## Mode Contracts

- `MIP`: covered when at least one valid sample participates in the projection;
  source scalar is display-transferred after selection.
- `ISO`: covered when a valid display-space threshold crossing/surface hit is
  selected; no fake scalar transfer is applied after the surface product.
- `DVR`: covered when the ray produces nonzero DVR contribution under active
  opacity/transfer state; the product is already display RGBA.

## Multi-Channel Compositing

- Each visible channel carries its own coverage/completeness.
- Hidden channels do no current-frame display, pick, readout, decode, upload, or
  render work.
- Background is applied once for pixels not covered by any visible rendered
  channel.
- Composite order must be deterministic and mode-appropriate.

## Failure Modes

- invert LUT brightens uncovered background
- invalid samples create output coverage
- missing resident data renders as completed black/transparent output
- no-validity-mask data is treated as invalid by default
- app display applies scalar transfer to a typed DVR or ISO surface product

## Testing Requirements

Tests must cover valid zero data, invalid/no-data exclusion, no-validity-mask
datasets, invert LUT, outside-volume background, missing occupied bricks,
multi-channel coverage, MIP/ISO/DVR mode boundaries, and source-value readout.
