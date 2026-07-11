# DVR Opacity Transfer Function Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the current source-value opacity-transfer model for `DVR`.

## Scope

This contract covers per-channel DVR opacity state, UI controls, persistence,
renderer parameters, cache invalidation, range skipping, and evidence
requirements.

## Non-Goals

- arbitrary transfer-function editor with many control points
- gradient-based DVR lighting
- modality-specific preprocessing flags or stored DVR products
- automatic opacity inversion tied to invert-LUT color inversion

## Contract

- DVR color transfer and opacity transfer are separate runtime models.
- Color transfer maps source values to display color. Opacity transfer maps
  source values to optical density/alpha contribution.
- `invert LUT` affects DVR color by default and must not silently invert DVR
  opacity.
- Each visible intensity channel has explicit opacity low/high/gamma state plus
  density scale.
- Defaults should show useful content without depending on modality-specific
  package flags.
- Opacity state is durable per-channel display state in the current
  `mirante4d-project-v14` project/session format.
- Presets that store display state must store the opacity-transfer state.
- Changing color transfer, opacity transfer, density scale, channel color,
  channel opacity, visibility, LOD, sample spacing, or resident-data
  completeness must invalidate or rerender affected DVR output.
- Transfer-aware range skipping for DVR must use opacity-transfer intervals.
- Readout remains source-value based.

## UI Requirements

Expose compact controls for:

- DVR density scale
- opacity low/high
- opacity gamma

The UI must make clear that color inversion and opacity inversion are separate
concepts.

## Failure Modes

- opacity derived only from display gamma
- invert-LUT silently inverting opacity
- opacity edits not invalidating cached DVR frames
- valid zero-valued samples treated as invalid/background
- near opaque samples failing to occlude farther samples
- real-sample evidence proving only "nonblank" without opacity state

## Testing Requirements

Tests and reports must cover default opacity, narrow-window `float32` DVR,
valid zero contribution, invert-LUT color behavior, opacity-cache invalidation,
range skipping, CPU/GPU parity where available, project/session persistence,
and source-value readout.
