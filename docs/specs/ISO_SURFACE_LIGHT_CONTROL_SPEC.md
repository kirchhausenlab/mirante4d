# ISO Surface Light Control Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the current ISO surface-light state and relighting behavior.

## Scope

This contract applies only to dense intensity `ISO` surface rendering. It does
not change ISO hit selection, thresholding, coverage, or source-value readout.

## Contract

- ISO uses camera-attached headlight shading by default.
- Users may detach the ISO light and drag a screen-space light direction.
- Light state affects only final ISO material shading.
- Light-only changes must reuse the compatible cached ISO surface frame when
  possible and rerun only surface compositing/relighting.
- Light state is display/session state and is not stored in native dataset
  packages.
- The current project/session format is `mirante4d-project-v14`; no legacy
  migration path is added by this spec.
- CPU and GPU ISO paths must expose normals in the same documented coordinate
  space for lighting parity.

## UI Requirements

- Provide an attach/detach control in ISO controls.
- Provide a compact two-dimensional light direction control when detached.
- Preserve deterministic defaults so switching into ISO does not require manual
  light setup.

## Failure Modes

- light changes alter selected ISO hits or source readout
- detached light state is lost from the current session model
- relighting forces unnecessary data reads or raymarching
- CPU/GPU normal-space mismatch causes inconsistent light direction

## Testing Requirements

Coverage must include default attached lighting, detached relighting,
multi-channel ISO compositing after relight, session persistence, unchanged hit
selection/readout, and timing evidence for light-only updates when performance
is claimed.
