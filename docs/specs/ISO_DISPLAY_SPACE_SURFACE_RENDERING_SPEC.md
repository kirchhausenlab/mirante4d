# ISO Display-Space Surface Rendering Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the current `ISO` rendering model. `ISO` is a display-space surface-hit
renderer, not a MIP-like scalar projection.

## Scope

This contract covers hit selection, typed ISO surface products, coverage,
normals, shading, multi-channel compositing, picking/readout, UI semantics, and
evidence requirements.

## Non-Goals

- scalar `MipImage` as the primary ISO product
- mesh extraction as the default interactive ISO path
- DVR opacity or same-ray DVR semantics
- per-channel ISO thresholds without a separate UI/state decision

## Contract

- ISO hit selection uses display-space scalar intensity after the active
  transfer/window/invert state.
- The renderer emits a typed surface-hit product with coverage, depth/position,
  normal, material/source scalar where needed, and completeness.
- Output coverage follows `OUTPUT_COVERAGE_DISPLAY_TRANSFER_SPEC.md`.
- Smooth ISO refines the first accepted threshold crossing; voxel-exact ISO may
  keep first-voxel-hit semantics but must state that policy.
- Normals should be evaluated at the refined hit for smooth ISO and may use the
  selected voxel center for voxel-exact ISO.
- ISO is shaded as a surface. Default lighting is deterministic and further
  controlled by `ISO_SURFACE_LIGHT_CONTROL_SPEC.md`.
- Multi-channel ISO compositing uses coverage and hit depth. Hidden channels do
  no current-frame ISO work.
- Picking/readout uses the same hit semantics as rendering and reports source
  values, not display color.
- Changing transfer/window/invert/ISO level/camera/LOD/visibility/completeness
  must invalidate affected ISO output.

## UI Requirements

- Label the control as a display-level threshold, not a raw source threshold.
- Show that transfer, display window, auto contrast, and invert LUT can change
  the selected surface.
- Show the ISO level against the active histogram where practical.

## Failure Modes

- ISO routed as scalar MIP output
- invert LUT changing background instead of surface eligibility
- smooth ISO flattening material scalar to the exact ISO level
- missing occupied data rendered as completed empty output
- multi-channel compositing ignoring depth
- readout reporting display color rather than source data

## Testing Requirements

Coverage must include display-space thresholding, invert-LUT eligibility, valid
zero data, invalid/no-data exclusion, smooth and voxel-exact policies, typed
surface products, CPU/GPU parity where available, multi-channel depth
compositing, picking/readout, and product-open validation when ISO behavior is
changed.
