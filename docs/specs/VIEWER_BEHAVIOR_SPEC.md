# Viewer Behavior Specification

Status: ACCEPTED
Last updated: 2026-07-07

## Purpose

Define the current expected behavior for the interactive Mirante4D viewer.

## Scope

This spec covers:

- supported dimensionality
- default open behavior
- render modes
- camera/projection expectations
- fidelity/status behavior
- inspection and interaction expectations

## Non-Goals

- Browser runtime behavior.
- Legacy `llsm_viewer` mode compatibility.
- User-facing legacy render modes.
- WebGL2, WebXR, browser memory, or OPFS constraints.
- Public AWS experiment loading.

## Data Dimensionality

Mirante4D is named for 4D data: 3D volumes over time. The viewer also supports
lower-dimensional cases through the same model:

- 4D: 3D volume plus time
- 3D: single 3D volume
- 2D plus time: 2D movie
- 2D: single image

Current dense intensity arrays use `t,z,y,x` axes. A dataset with `z == 1` is a
single-plane dataset for first-view defaults.

## Render Modes

The current user-facing intensity render modes are:

- `MIP`: maximum intensity projection for fast overview and familiar
  fluorescence inspection.
- `DVR`: transfer-function direct volume rendering with source-value opacity
  transfer and typed RGBA output.
- `ISO`: display-space isosurface/surface-hit rendering.

Render mode is per-channel durable display state. Fresh native datasets open
visible intensity channels in `MIP`.

Voxel-exact or smooth sampling is a sampling/quality policy where applicable,
not a separate top-level legacy render mode.

## Mixed Display

The viewer builds one display graph from the visible channel state. A single
viewport can combine channels using different render modes.

Rules:

- hidden channels do no current-frame work
- `DVR` channels preserve same-ray multi-channel composition within their cohort
- `ISO` channels preserve display-space thresholding and depth-sorted
  compositing within their cohort
- final display compositing is deterministic
- picking and readout remain channel-aware

## Camera Projection Model

Projection is camera/view state, not a render mode.

Supported projections:

- `Perspective`: diverging rays for natural 3D exploration.
- `Orthographic`: parallel rays for scientific inspection, measurement-like
  views, and stable screenshots.

Orthographic correctness is strict: moving the camera forward/backward along the
view direction must not change projected object size or warp the volume.

Viewport resize is not zoom. Resizing the viewer changes the visible world area
in the resized axis while preserving apparent volume scale. A wider viewport
shows more world left/right. A taller viewport shows more world up/down. The
volume must not stretch, and the app must not use viewport height as a hidden
camera scale reference.

Perspective projection uses a stable focal length measured in logical screen
points. Resizing the viewport must not move the perspective camera or change
that focal length. Orthographic projection uses a stable world-units-per-screen
point scale. Fit Data, Reset View, and user zoom are the operations that change
apparent scale.

Detailed camera requirements live in `CAMERA_VIEW_SPEC.md`.

## Fidelity And Status

The UI must report what is actually displayed:

- shown LOD
- target LOD when different
- completeness or loading/budget reason
- backend
- presentation size and render target pixels when they differ
- render timing
- display freshness when available

Render timing is not actual interaction or presentation FPS unless that is what
was measured.

During active 3D movie playback, the viewer may temporarily target `s1` instead
of normal `s0` so time advances smoothly on large time series. This is a
runtime playback policy, not a saved camera or session setting. When playback
stops, the viewer immediately restores the normal LOD target for the current
view and continues showing the best complete frame while finer data loads.
Status labels must remain literal throughout that transition.

## Inspection

Hover, picking, and readout must preserve typed source values where possible:

- intensity values retain `uint8`, `uint16`, or `float32` identity
- label IDs remain exact and non-interpolated
- missing occupied data reports incomplete/loading state
- display color is not used as a substitute for scientific source value

## Testing Requirements

- Fresh open defaults visible intensity channels to `MIP`.
- Project save/reopen restores per-channel render mode and typed parameters.
- Render-mode switching preserves camera, timepoint, channel state, and honest
  displayed/target status.
- Mixed-mode visible channels render in one deterministic viewport composite.
- Orthographic and perspective projection tests cover geometry invariants.
- Product-open validation is required for renderer, viewport, GPU,
  data-loading, interaction, or large-dataset viewer changes unless waived.
