# Camera And View Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define Mirante4D's camera, projection, view-state, viewport-resize, and
geometric correctness requirements.

Orthographic projection was difficult to stabilize in the legacy viewer. This spec exists to prevent "looks right once" implementations that warp the volume during navigation.

## Scope

This spec covers:

- supported projection modes
- camera state model
- projection switching
- ray generation
- picking and overlays
- screen-space metrics for LOD/residency
- viewport-resize scale invariants
- orthographic correctness invariants
- required camera/projection tests

## Non-Goals

- VR/WebXR camera support.
- Fisheye, panoramic, nonlinear, cinematic, or arbitrary lens models.
- Continuous interpolation between perspective and orthographic as a product feature.
- Isometric or diagonal view presets as a required initial feature.
- Separate renderer or data-loading architectures per projection.

## Requirements

- Mirante4D should support exactly two projection modes by default:
  - `Perspective`
  - `Orthographic`
- Projection mode is camera/view state, not a render mode.
- Camera math must live in a dedicated camera/view module, not be reimplemented in UI panels, renderer code, data-engine policy, or overlay code.
- Switching projection must preserve the active target, orientation, and
  approximate apparent scale.
- Viewport resize must not change apparent volume scale. Resize changes how
  much world area is visible, not the camera zoom.
- Perspective performance must not regress materially because orthographic exists.
- Orthographic behavior must be verified with mathematical and visual tests before it is considered implemented.

## Camera Model

The durable camera/view model should be based on semantic view parameters rather than raw toolkit camera objects.

Candidate model:

```text
CameraView:
  projection: Perspective | Orthographic
  target: world-space point
  orientation: normalized quaternion
  orthographic_world_per_screen_point: positive scalar
  perspective_focal_length_screen_points: positive scalar
  perspective_view_distance_world: positive scalar, or equivalent eye position
  near_far_policy: clipping policy
```

Important semantics:

- `target` is the world-space point the view is centered around.
- `orientation` defines the view direction and image-plane axes.
- `orthographic_world_per_screen_point` is the orthographic apparent scale.
  If it is `1.0`, one logical screen point represents one world unit.
- `perspective_focal_length_screen_points` is the perspective apparent scale.
  It maps `world_offset / depth` into logical screen points.
- `perspective_view_distance_world` or an equivalent eye position is explicit
  navigation state. It is not derived from viewport height.
- Viewport width and height determine the visible projection rectangle around
  the view center. They are not hidden camera zoom references.

This model allows projection switching without destructive resets or arbitrary jumps.

## Projection Modes

### Perspective

Perspective projection uses diverging rays. Objects farther from the camera appear smaller.

Use cases:

- natural 3D exploration
- depth perception
- general free navigation

Perspective-specific state:

- focal length measured in logical screen points
- explicit eye distance or eye position
- optional field-of-view diagnostics derived from focal length and the current
  presentation size, not durable zoom authority

### Orthographic

Orthographic projection uses parallel rays. Object size is independent of camera distance.

Use cases:

- distortion-free inspection
- measurement-like views
- stable screenshots
- comparing positions and structures

Orthographic-specific state:

- world units per logical screen point
- camera position only insofar as it affects clipping and ray origins

## Viewport Resize Invariants

Viewport resize must be symmetric across width and height.

Required behavior:

- Increasing viewport width reveals more world left/right.
- Increasing viewport height reveals more world up/down.
- Decreasing viewport width or height reveals less world in that axis.
- Resize does not change apparent volume scale.
- Resize does not perform Fit Data.
- Resize does not move the perspective camera, change perspective focal length,
  or change orthographic world-per-point.
- Zoom changes apparent scale.
- Fit Data and Reset View intentionally choose a new apparent scale.

Invalid behavior:

- treating viewport height as the durable camera scale reference
- making a taller window enlarge the same rendered world span
- deriving perspective focal length continuously from current viewport height
- moving the perspective camera on resize to compensate for a vertical-FOV
  convention
- changing LOD because render target pixels changed rather than because camera
  scale or visible world coverage changed

## Orthographic Invariants

These are correctness requirements, not polish.

- Moving the camera forward/backward along the view direction must not change object size.
- Moving the camera forward/backward along the view direction must not warp the volume.
- Moving the camera forward/backward along the view direction must not introduce perspective-like distortion.
- Orthographic zoom changes scale; orthographic dolly does not.
- Parallel structures remain parallel in orthographic screenshots.
- Orthographic apparent geometry must be stable during panning, zooming, and rotation, except for intentional scale/orientation changes.
- Camera-distance-only LOD, quality, or residency decisions are invalid in orthographic mode.
- Clipping changes may occur if near/far policy clips the volume, but clipping must not masquerade as deformation.

Any implementation that makes the volume smoothly warp during orthographic navigation is wrong even if the initial static image looks plausible.

## Ray Generation

Renderer ray generation must follow the projection contract.

Perspective:

- ray origin is effectively the camera position
- ray direction varies by pixel
- rays diverge through the image plane
- focal length in logical screen points is stable across resize

Orthographic:

- ray direction is constant across the image
- ray origin varies by pixel across the image plane
- rays are parallel
- world-per-screen-point is stable across resize

The renderer may use inverse view/projection matrices or an equivalent explicitly tested formulation. The chosen implementation must have CPU tests that verify ray origins and directions for known camera states.

Avoid hot per-sample projection branches in volume shaders if benchmarks show a measurable cost. Projection-specific setup or shader/pipeline variants are acceptable when they keep perspective fast and simplify correctness.

## Projection Switching

Switching between perspective and orthographic should preserve:

- target
- orientation
- approximate apparent scale at the view center
- selected timepoint/layer/render mode
- loaded dataset/session state

Switching projection should not:

- reset to an unrelated camera view
- reload the dataset
- change render mode
- change data-loading architecture
- silently change channel/layer settings

## Picking, Hover, ROI, And Overlays

Picking and overlays must use the same camera model as the renderer.

Requirements:

- hover rays must be correct in both perspective and orthographic
- ROI tools must not assume perspective rays
- tracks, labels, axes, and overlays must project consistently with the volume
- depth/hit buffers must document their projection semantics
- screen-space annotations must not use a different projection from the rendered data

## Screen-Space Metrics, LOD, And Residency

Projection mode must inform screen-space policy, but must not select separate architectures.

The data engine and renderer should use projection-aware metrics such as:

- projected voxel size
- visible world extent
- frustum or view-volume intersection
- screen-space brick coverage
- ray/brick intersection estimates

Invalid assumptions:

- camera distance alone determines LOD
- render target size alone determines LOD
- viewport height is the durable camera scale
- orthographic implies a special direct-volume-only path
- projection mode alone chooses residency strategy

Correct direction:

```text
CameraView scale + presentation extent -> visible world coverage / projected voxel size -> shared LOD, cache, residency, and upload policy
```

## Performance Policy

Supporting orthographic should primarily add implementation and testing complexity, not baseline perspective runtime cost.

Requirements:

- inactive projection mode should have negligible per-frame cost
- perspective mode must not pay material dormant orthographic shader cost
- perspective non-regression is required when adding/changing orthographic support
- orthographic performance must be measured separately under comparable visible-coverage scenarios

Orthographic can be more or less expensive than perspective depending on visible volume coverage and sampling policy. That is a policy/benchmark question, not a reason to use a separate architecture.

## UI Behavior

The first UI should expose projection as a simple control:

- `Perspective`
- `Orthographic`

Projection control should be distinct from visual render mode controls.

Optional future controls:

- fit-to-view
- saved views
- axis-aligned presets such as `XY`, `XZ`, `YZ`
- constrained FOV adjustment for perspective

These optional controls are view/navigation features, not additional projection modes.

## Testing Requirements

Camera/projection work requires targeted tests before completion.

Unit/math tests:

- perspective ray generation for known pixels
- orthographic ray generation for known pixels
- orthographic rays are parallel
- perspective rays diverge
- resize preserves orthographic world-per-screen-point
- resize preserves perspective focal length and eye distance
- increasing viewport height reveals more world instead of enlarging the volume
- projection switching preserves target/orientation/apparent scale within tolerance
- orthographic dolly does not alter projected scale
- perspective visible scale changes with distance/FOV as expected

Renderer/screenshot tests:

- synthetic cube/grid volume remains stable under orthographic dolly
- parallel edges remain parallel in orthographic screenshots
- `MIP`, `DVR`, and `ISO` render nonblank in both projections
- overlays align with synthetic geometry in both projections
- clipping behavior is distinguishable from deformation

Interaction tests:

- hover/picking hits known synthetic points in both projections
- ROI/crosshair interactions use correct rays in both projections
- switching projection does not reset layer/render/time state

Policy/performance tests:

- orthographic zoom changes projected voxel size metrics
- resize does not change projected voxel size metrics
- render target resolution changes do not change projected voxel size metrics
- orthographic camera-distance changes do not change projected voxel size metrics
- brick prioritization responds to orthographic zoom and visible extent, not distance alone
- perspective benchmark scenarios do not regress materially when orthographic support is present

## Invariants

- Perspective and orthographic are the only baseline projection modes.
- Camera math is centralized and tested.
- Orthographic is not perspective with a different matrix bolted on at the end.
- Orthographic navigation must not warp data.
- Projection mode must not fork the data-loading architecture.
- Projection-specific code paths require tests proving both projections remain correct.

## Failure Modes

- volume appears correct at rest but warps during orthographic movement
- ray generation uses camera-position-to-sample direction in orthographic mode
- LOD/residency uses camera distance in orthographic mode
- projection switching changes framing or resets view state unexpectedly
- overlays/picking use perspective assumptions while the renderer is orthographic
- near/far clipping creates apparent deformation
- inactive orthographic support slows perspective rendering
- tests only prove nonblank rendering, not geometric correctness

## Open Questions

- Exact perspective FOV default and allowed range.
- Exact near/far clipping policy for huge and anisotropic volumes.
- Whether perspective FOV adjustment is exposed in v1.
- First UI placement for the projection control.
- First camera fixture set for geometric screenshot tests.
