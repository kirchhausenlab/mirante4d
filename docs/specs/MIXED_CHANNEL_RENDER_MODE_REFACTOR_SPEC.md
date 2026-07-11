# Mixed Channel Render Mode Specification

Status: ACCEPTED
Implementation: implemented, automated-verified, and product-validated for the
accepted first mixed-display scope
Last updated: 2026-06-26

## Purpose

Define the current viewer contract for per-channel render modes and mixed
display compositing.

## Current Product Behavior

- Fresh native datasets open visible intensity channels in `MIP`.
- Each visible intensity channel owns its render mode and typed mode parameters.
- Supported intensity render modes are `MIP`, `DVR`, and `ISO`.
- Changing one channel's render mode does not change other channels unless the
  user invokes an explicit apply-to-visible/all action.
- Project/session state persists per-channel render state in the current
  hard-cut project/session format, `mirante4d-project-v14`.
- Old global render-mode project/session fields are not accepted by the current
  reader.
- Channel presets include visibility, transfer state, render mode, and typed
  mode parameters.

## Display Graph

The app builds a display graph from visible channel state. The display graph is
the product render authority.

Graph rules:

- hidden channels do not schedule, decode, upload, render, composite, pick, or
  contribute current-frame intensity readout
- `DVR` channels in a visible graph preserve same-ray multi-channel composition
  within the `DVR` cohort
- `ISO` channels preserve display-space thresholding and depth-sorted
  compositing within the `ISO` cohort
- `MIP` channels remain a first-class fast overview path
- final mixed display compositing is deterministic
- fidelity/status is channel-aware and can report mixed fidelity
- picking/readout is channel-aware and must not infer scientific values from
  final display color

## Architecture Boundary

Viewer-global render mode is not product authority. Remaining global or
active-channel render-mode conveniences must be derived UI/runtime state only.

Product requests, residency planning, render dispatch, display identity,
status, and product automation should use per-channel render state and display
graph identity.

## Non-Goals

- Backward-compatible readers for old project/session formats.
- Legacy `BL` or `MIP-Voxel` user-facing modes.
- Per-channel cameras, timepoints, or source datasets.
- Hidden lower-quality fallbacks to make mixed modes appear fast.
- CPU display texture rebuilds as the normal mixed-mode product path.

## Verification Requirements

Changes touching this contract require tests for:

- per-channel render-state persistence
- display graph construction
- hidden-channel resource exclusion
- cohort behavior for `DVR` and `ISO`
- display identity/fidelity/status
- project/session hard-cut rejection
- product automation or product-open validation when the changed path is
  renderer, interaction, GPU, data-loading, or large-dataset relevant
