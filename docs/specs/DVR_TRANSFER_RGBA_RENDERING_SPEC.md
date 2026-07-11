# DVR Transfer RGBA Rendering Specification

Status: ACCEPTED
Last updated: 2026-06-27

Implementation: implemented for current runtime DVR paths.

## Purpose

Define the active `DVR` output boundary. `DVR` is direct volume rendering, not a
scalar projection like `MIP`.

## Scope

This contract covers dense intensity `DVR` for dense and resident CPU/GPU paths,
`uint8`, `uint16`, and `float32` channels, same-ray multi-channel DVR, app
display compositing, cache identity, and readout semantics.

## Non-Goals

- scalar-frame `DVR` compatibility
- legacy `BL` as a user-facing mode
- precomputed/stored DVR RGBA package products
- mesh extraction, scattering, gradient lighting, or preintegrated transfer
  functions
- mixed-LOD DVR

## Contract

- Active `DVR` emits a typed premultiplied display-RGBA product with coverage
  and completeness metadata.
- The app must not apply scalar display-window transfer to `DVR` output after
  raymarching.
- Color/display transfer, opacity transfer, channel color/opacity, density, and
  invert-LUT color behavior are renderer inputs.
- `DVR` display color is not scientific source data. Picking and readout remain
  source-value based.
- Validity and output contribution are separate. Invalid samples contribute no
  color, opacity, coverage, or range evidence.
- Missing occupied resident data is incomplete/loading, not completed
  transparent output.
- Opacity must be stable with respect to physical/world step length and LOD for
  equivalent content within documented tolerances.
- Same-ray multi-channel DVR combines visible channel contributions during one
  front-to-back traversal. Finished per-channel DVR frames are not the final
  multi-channel semantic model.
- Hidden channels must not be scheduled, decoded, uploaded, sampled,
  composited, picked, or reported for current-frame DVR work.
- Transfer-aware range skipping is allowed only when conservative over the
  active source interval and opacity-transfer state.
- Cache keys must include every state input that can change DVR output.

## Mode Boundary

| Mode | Renderer Product | App Scalar Transfer | Readout |
| --- | --- | --- | --- |
| `MIP` | scalar plus coverage | yes, for covered pixels | selected source value |
| `ISO` | typed surface-hit frame | no fake scalar transfer | selected surface-hit source value |
| `DVR` | premultiplied display RGBA plus coverage/completeness | no | source-value probe/readout |

## Failure Modes

- scalar-display-transfer fallback for DVR
- `float32` values windowed twice
- integer DVR normalized by dtype maximum when a display window is active
- invalid or missing data appearing as completed transparent output
- channel-order-dependent output
- hidden channels still doing DVR work
- pick/readout reporting DVR display RGBA as source intensity

## Testing Requirements

Coverage must include transfer placement, invert LUT, valid zero data, invalid
samples, missing resident data, early termination, same-ray multi-channel
determinism, physical step-length opacity, LOD opacity stability, cache
invalidation, CPU/GPU parity where available, and source-value readout.
