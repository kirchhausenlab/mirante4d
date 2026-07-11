# Renderer Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the native GPU renderer requirements for Mirante4D.

## Scope

This spec covers:

- camera projection integration
- GPU backend direction
- renderable resource contracts
- dense intensity rendering
- multi-channel compositing
- scene-layer and overlay rendering
- diagnostics
- correctness and performance expectations

## Non-Goals

- WebGL2 compatibility.
- WebXR/VR.
- Browser canvas rendering.
- CUDA-only baseline.
- Treating stored dtype as layer semantics.
- Assuming intensity channels are RGB/RGBA components.
- Treating "overlay" as one generic renderer path.
- Legacy user-facing render modes.

## Backend Direction

The first renderer backend should use `wgpu`. The architecture should keep enough separation to permit future lower-level backends only if benchmarks justify them.

Concrete shader, resource, render-mode default, compositing, pass-order, picking, and value-representation policy is defined in `RENDERER_PIPELINE_SPEC.md`.

## Requirements

- Initialize and report GPU adapter/backend/features.
- Support perspective and orthographic camera projections through the shared camera/view contract.
- Render bricked multiscale dense intensity volumes.
- Render anisotropic volumes using explicit physical/world transforms.
- Support explicit GPU memory or resource budgets where available.
- Support asynchronous resource upload from data-engine-provided resources.
- Support typed scene-layer draw lists for tracks, ROIs, annotations, measurements, interaction visuals, and reference visuals.
- Expose frame diagnostics.
- Keep renderer resource lifetimes explicit.
- Avoid hidden global renderer state.
- Use the same renderer resource contracts for small fully-resident datasets and huge streaming datasets.

## Camera Projection Integration

The renderer must consume camera/view data from the centralized camera module described in `CAMERA_VIEW_SPEC.md`.

Requirements:

- perspective ray generation uses diverging rays
- orthographic ray generation uses parallel rays with per-pixel origins
- projection mode must not fork renderer resource ownership or data-loading architecture
- projection-specific shader/pipeline variants are allowed when they protect hot-path performance and simplify correctness
- perspective mode must not pay material dormant orthographic shader cost
- renderer diagnostics should include active projection mode and relevant camera metrics

## Dense Intensity Rendering

The renderer should support:

- multiscale sampling
- page-table or equivalent brick indirection
- empty-space skipping
- `DVR` direct volume rendering with transfer-function-based front-to-back compositing
- `MIP` maximum intensity projection as a first-class fast/familiar intensity-projection mode
- `ISO` isosurface/threshold rendering
- smooth/interpolated and voxel-exact/discrete representation policies where meaningful
- brightness/contrast/windowing
- channel/layer color controls
- active-channel subset rendering without forcing data-engine decode or GPU work for inactive channels when avoidable

Dense intensity dtype, value mapping, and channel semantics must follow `INTENSITY_DTYPE_CHANNEL_SPEC.md`.

Spatial geometry, anisotropy, and resampling semantics must follow `SPATIAL_GEOMETRY_RESAMPLING_SPEC.md`.

Current user-facing intensity render modes are `MIP`, `DVR`, and `ISO`.
Voxel-exact behavior is a sampling policy where meaningful, not a separate
top-level mode.

## Multi-Channel Compositing

Channels are first-class intensity layers, not fixed RGB components.

Renderer requirements:

- active visible channels determine sampling and compositing work
- per-channel display mapping is explicit
- channel color is display metadata, not stored intensity data
- compositing should use linear premultiplied color internally where practical
- exact inspection/readout should preserve source/stored value semantics rather than reporting only normalized display values
- renderer resource keys must include channel/layer identity and representation

## Plane Sampling

The renderer should support:

- orthogonal planes such as `XY`, `XZ`, and `YZ`
- arbitrary oblique planes at user-controlled angles
- plane transforms expressed in physical/world coordinates
- linear/smooth sampling for intensity channels when selected
- honest visual state for nonresident or missing bricks intersecting the plane

## Anisotropic Volume Rendering

The renderer must not require preprocessing-time isotropic inflation to display anisotropic volumes correctly.

Expected behavior:

- rays are defined in world/physical space
- physical bounds come from layer transform metadata
- sample positions are transformed from world space into the source/storage grid
- ray-march step size is chosen in physical units and informed by voxel spacing
- LOD, residency, clipping, and projected voxel metrics use physical dimensions where relevant
- hover/readout can report both voxel indices and physical/world coordinates

## Overlays

Overlay-like content must follow `SCENE_LAYER_OVERLAY_SPEC.md`.

"Overlay" is user-facing language, not the renderer architecture. The renderer should consume typed scene-layer draw lists produced by extraction from project/session state and viewer runtime state.

Expected scene-layer content over time:

- tracks
- ROI geometry
- hover/crosshair
- axes/grid/reference markers
- measurement visuals
- annotations
- selection handles
- tool previews
- labels and text

Scene-layer rendering should use explicit pass assignment:

- world-space object passes for tracks, ROIs, annotations, and measurement geometry
- interaction passes for hover, selected objects, handles, and active tool previews
- screen-space passes for labels, scale bars, timestamps, and tooltips

Each draw item should declare an occlusion policy, such as always-on-top, depth-tested geometry, volume-depth-cued, xray, or screen-space.

## Invariants

- Renderer must not infer semantic layer kind from data type alone.
- Renderer must not assume exactly three intensity channels.
- GPU resources must be recreated or updated when their source identity changes.
- View and data coordinate transforms must be explicit.
- Renderer must not assume unit voxel spacing.
- Renderer resources for scene layers are derived caches, not authoritative object state.
- Scene-layer draw items must have explicit coordinate space, time behavior, and occlusion policy.
- All shader loops/traversals must guarantee forward progress.
- Missing resident occupied data must be represented honestly and must not be rendered as empty.

## Failure Modes

- no compatible adapter
- required feature unavailable
- resource allocation failure
- upload queue overflow
- shader/pipeline creation failure
- dataset resource mismatch
- incomplete residency
- scene-layer draw list/resource mismatch
- unsupported occlusion policy

## Testing Requirements

- GPU initialization smoke test.
- Synthetic volume render smoke test.
- CPU model tests for traversal math.
- CPU model tests for camera ray generation.
- Orthographic no-warp screenshot or pixel tests with synthetic geometry.
- Golden image tests where stable enough.
- Dense intensity value-mapping tests.
- Multi-channel active-subset compositing tests.
- Anisotropic physical-bounds and ray-step tests.
- Oblique plane sampling tests on anisotropic fixtures.
- Scene-layer draw-list ordering tests.
- Typed picking/render-ID tests.
- Track/ROI/screen-text rendering smoke tests.
- Resource lifetime/drop tests.
- Benchmark frame-time tests with synthetic fixtures.

## Open Questions

- Exact GPU resource representation for intensity page tables.
- How to handle devices with weak feature sets.
- First UI/control model for arbitrary oblique plane movement and rotation.
