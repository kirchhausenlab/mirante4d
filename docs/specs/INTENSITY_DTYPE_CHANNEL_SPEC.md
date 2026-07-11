# Intensity Dtype And Channel Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define supported dense intensity source dtypes, stored dtype policy, conversion
rules, renderer representation, and channel semantics.

## Scope

This covers `uint8`, `uint16`, and `float32` dense intensity layers, channel
metadata, physical channel storage, multi-channel rendering, display mapping,
and analysis precision.

## Core Model

- Source dtype is scientific data identity.
- Stored dtype is package storage representation.
- Conversion is explicit metadata and is lossless by default.
- Display transfer maps source/stored values to display; it is not stored data.
- Channels are separate dense intensity layers, not a channel axis inside dense
  arrays.

## Dtype Policy

- Supported dense intensity source/stored dtypes are `uint8`, `uint16`, and
  finite `float32`.
- Integer data must not be widened to `float32` in resident paths merely to
  satisfy older renderer representations.
- `float32` payload values must be finite.
- Lossy quantization is forbidden by default.
- Multiscales preserve source semantics and record reduction/conversion policy.

## Rendering Policy

- `MIP`: project active channels with their own transfer/window state, then
  composite deterministic premultiplied color.
- `DVR`: sample active visible channels at each ray step and combine
  contributions according to `DVR_TRANSFER_RGBA_RENDERING_SPEC.md`.
- `ISO`: threshold semantics are channel-explicit and governed by the ISO specs.
- Hidden channels do no current-frame scheduling, decode, upload, render,
  composite, pick, or readout work.

Final 2D presentation filtering is not source sampling and must not affect
source requests or readout.

## Invariants

- Channel color is display metadata, not stored intensity data.
- Source-value readout remains tied to source dtype semantics.
- Display normalization must not rewrite source/stored values.
- Channel identity, visibility, order, and mode parameters are explicit display
  state.

## Failure Modes

- dtype maximum used as a display window when explicit range metadata exists
- valid zero-valued data treated as no-data
- integer data widened in performance-critical resident paths without reason
- channel axis hidden inside dense intensity arrays
- hidden channels still doing current-frame work

## Testing Requirements

Coverage must include dtype round-trips, finite `float32` validation, display
range mapping, multiscale dtype metadata, channel visibility/order, MIP/DVR/ISO
multi-channel behavior, source-value readout, and hidden-channel exclusion.
