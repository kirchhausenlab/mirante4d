# Spatial Geometry And Resampling Specification

Status: DRAFT
Last updated: 2026-06-10

## Purpose

Define how Mirante4D represents voxel spacing, anisotropy, acquisition geometry, physical/world coordinates, and optional isotropic resampling.

The central rule is that physical correctness must come from explicit coordinate transforms, not from silently inflating or rewriting the dataset into an isotropic grid.

## Scope

This spec covers:

- source grid versus physical/world space
- voxel spacing and units
- index-to-physical transforms
- anisotropic data rendering
- measurements in physical coordinates
- optional derived isotropic grids
- de-skew/acquisition geometry direction
- testing requirements

## Non-Goals

- Preserving the legacy "Make data isotropic" checkbox behavior.
- Making isotropic resampling the default preprocessing output.
- Hiding anisotropy by silently changing stored data.
- Treating isotropic resampling as a harmless metadata toggle.
- Using resampled isotropic data as if it were raw/source-like data.

## Core Model

Mirante4D should distinguish three related spaces:

```text
Source/storage grid:
  integer voxel indices
  source-like sampled values
  native bricked/multiscale storage

Physical/world space:
  real units such as nm or um
  correct distances, scale bars, planes, camera, measurements
  derived from explicit transforms

Derived resampled grid:
  optional analysis/render artifact
  explicit target spacing
  explicit interpolation policy
  explicit provenance
```

The default native dataset should preserve the source/storage grid and store enough transform metadata to make physical/world behavior correct.

## Default Policy

Mirante4D must not resample all data to isotropic spacing by default.

Default preprocessing should:

- preserve the source-like grid
- record voxel spacing and units
- record index-to-physical/world transform metadata
- record acquisition geometry where known
- generate multiscales and acceleration metadata against declared transforms
- validate that renderer/analysis code can interpret the transform

Isotropic resampling may exist as an explicit derived artifact, but not as the canonical hidden preprocessing step.

## Required Geometry Metadata

Each spatial layer should declare:

- axis order
- dimensions
- voxel spacing for each spatial axis
- spatial units
- origin
- orientation where known
- index-to-physical transform
- physical-to-index transform or enough metadata to derive it
- per-scale transform metadata
- source acquisition geometry where known

The transform model should allow at least affine transforms. Simple axis-aligned voxel spacing is a common case, not the whole design.

## Anisotropic Rendering

The renderer must handle anisotropic grids natively.

Expected rendering model:

```text
camera ray in world space
  -> intersect physical/world volume bounds
  -> march in physical units
  -> transform sample position world -> grid
  -> sample stored grid using the layer's sampling policy
```

Renderer requirements:

- physical bounds must use geometry metadata
- step size should be chosen in physical units and informed by voxel spacing
- camera, clipping, projected voxel metrics, LOD, and residency policy must use physical/world dimensions
- hover/readout should report both voxel index and physical/world coordinate where useful
- missing data must remain honest; transform ambiguity must not render as empty data

The native renderer should not require preprocessing-time isotropic inflation to make anisotropic volumes look physically correct.



Requirements:

- arbitrary oblique planes are sampled through world-to-grid transforms
- axis-aligned `XY`, `XZ`, and `YZ` planes are special UI presets, not separate data paths
- selected intensity sampling policy must not affect source-value readout
- plane movement and measurement should use physical units
- exported plane images should record the source layer, transform, plane

## Measurement And Analysis

Measurements must be performed in physical/world coordinates unless explicitly declared otherwise.

Examples:

- distances use physical units
- areas and volumes use physical spacing/transform metadata
- ROI geometry should have a world-space representation
- object size statistics must not assume unit voxel spacing
- line profiles should record whether sampling is grid-index based or physical-distance based

Analysis operations may request an isotropic working grid when mathematically justified, but that grid is an explicit derived representation with provenance.

## Derived Isotropic Grids

An isotropic grid is a derived artifact, not source-like storage.

An explicit isotropic artifact must record:

- source dataset/layer/timepoint/scale
- target spacing and units
- target dimensions
- transform from derived grid to source/world space
- interpolation policy
- dtype/conversion policy
- boundary policy
- whether the output is intended for rendering, analysis, or export
- provenance and tool version

Interpolation policy must depend on layer kind:

- intensity: linear/cubic/windowed-sinc or other declared intensity interpolation
- annotations/tracks: geometric transform of objects, not image interpolation

Isotropic derived grids may be useful for:

- algorithms that assume isotropic sampling
- publication/export products
- specific analysis operations
- performance experiments

They must not silently replace the native source-grid representation.

## De-Skew And Acquisition Geometry

Light-sheet/LLSM de-skew is an acquisition-geometry problem, not the same thing as making data isotropic.

The format and runtime should be designed to represent affine acquisition transforms, including skew/shear where known.

Potential product paths:

- view source grid through its declared physical/acquisition transform
- create an explicit de-skewed derived grid for algorithms or export
- create an explicit isotropic de-skewed derived grid only when requested

Each path must be named and recorded separately. "Isotropic" must not be used as a catch-all for de-skew, scale correction, and resampling.

## UI Language

Avoid ambiguous labels like "Make data isotropic" as a primary product control.

Prefer explicit wording:

- preserve source grid
- use physical voxel spacing
- create isotropic derived grid
- target spacing
- interpolation policy
- de-skew derived grid

The default should be physically correct viewing without resampling.

## Invariants

- Source-grid storage is preserved by default.
- Physical/world transforms are explicit.
- Isotropic resampling is an explicit derived artifact.
- Renderer and analysis code must not assume unit voxel spacing.
- Measurements use physical/world units by default.
- De-skew and isotropic resampling are separate concepts.
- Derived grids must carry provenance.

## Failure Modes

- source data silently inflated into isotropic storage
- anisotropic volume displayed with unit voxel spacing
- scale bar computed from voxel counts instead of physical units
- ROI/measurement tools ignore voxel spacing
- de-skew hidden behind an "isotropic" toggle
- derived isotropic data treated as raw/source-like data
- LOD/residency metrics computed from index dimensions only

## Testing Requirements

Tests should include:

- transform round-trip tests: grid -> world -> grid
- anisotropic physical bounds tests
- renderer CPU-mirror ray/step tests for anisotropic grids
- orthographic no-warp tests on anisotropic fixtures
- scale-bar tests using physical units
- oblique plane tests on anisotropic synthetic volumes
- measurement tests where voxel spacing is not 1:1:1
- provenance validation for derived isotropic artifacts
- de-skew/affine transform fixtures once de-skew support is designed
- regression tests proving default preprocessing does not inflate anisotropic data

## Open Questions

- Exact transform metadata schema.
- Whether all spatial transforms are affine in v1.
- Default physical unit representation.
- First UI workflow for explicit derived isotropic grid creation.
- First interpolation kernels for intensity-derived isotropic artifacts.
- Whether any renderer-specific acceleration metadata should be generated in physical-space bins.
- Exact de-skew metadata fields for LLSM acquisitions.
