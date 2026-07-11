# Dataset Format Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the strict native dataset contract for Mirante4D.

## Scope

This spec covers the intended `mirante4d-v1` dataset profile, including:

- manifest requirements
- storage layout direction
- spatial geometry and transform metadata
- dense intensity representation
- acceleration metadata
- validation requirements

## Non-Goals

- Opening old `llsm_viewer` preprocessed datasets.
- Opening arbitrary OME-Zarr datasets.
- Supporting multiple historical native versions in the core reader.
- Hiding missing acceleration metadata behind runtime fallbacks.

## Format Identity

Every native dataset must declare a root format identity:

```json
{
  "format": "mirante4d-v1"
}
```

The core reader must reject every other value.

## Storage Direction

The format should use Zarr v3 storage concepts and OME-Zarr-compatible metadata where useful. Mirante4D-specific required metadata should live under an explicit Mirante4D namespace or manifest file.

The initial physical representation should be a directory package, not a compressed archive.

The concrete bootstrap schema is defined in `NATIVE_DATASET_V1_SCHEMA_SPEC.md`. That spec is the implementation target for the first fixture, format reader, data-engine read path, and renderer smoke path.

Production chunking, sharding, compression, multiscale, histogram, acceleration metadata, checksum, and OME metadata policy is defined in `DATASET_V1_STORAGE_POLICY_SPEC.md`.

The native dataset package on disk is the durable source of truth for viewing. Mirante4D should not create a browser-style OPFS equivalent or normal app-private copy as a separate viewing source. App caches may exist, but they are rebuildable runtime artifacts, not the canonical dataset.

## Required Metadata

The manifest must describe:

- dataset identity and format version
- channel and layer list
- timepoint count
- voxel size and coordinate transforms
- index-to-physical/world transform metadata
- source acquisition geometry metadata where known
- source and stored axis order
- source and stored dtype metadata for dense intensity layers
- explicit conversion/normalization metadata for dense intensity layers
- dense intensity scale list
- track sets where present
- preprocessing parameters
- storage codec policy
- acceleration metadata locations
- checksums or equivalent validation metadata for critical binary payloads

## Dense Intensity Requirements

Spatial geometry and resampling semantics are defined in `SPATIAL_GEOMETRY_RESAMPLING_SPEC.md`.

Dense intensity dtype and channel semantics are defined in `INTENSITY_DTYPE_CHANNEL_SPEC.md`.

Dense intensity no-data sentinel behavior is defined in `NO_DATA_MASK_POLICY_SPEC.md`.

- Dense intensity data must be stored as multiscale bricked arrays.
- Dense intensity data should preserve the source-like grid by default.
- Isotropic grids must be explicit derived artifacts, not silent replacement storage.
- Scale levels must be contiguous from base scale to terminal scale.
- Chunk/brick dimensions must be explicit per scale.
- Stored data type must be explicit.
- Source data type metadata must be recorded where available.
- Stored data type must be one of the current accepted dense intensity dtypes.
- Dtype conversion policy must be explicit.
- Normalization metadata must be explicit.
- Display normalization/windowing must be separate from stored scientific values.
- Optional no-data sentinel metadata must be explicit and validated.
- Channel metadata must be explicit; channels must not be inferred as RGB/RGBA components.
- Per-scale histogram metadata should be present unless explicitly rejected by a future decision.
- Per-brick min/max/occupancy metadata must be present for renderable scales.
- Valid/min/max range hierarchy metadata must be present for renderable scales.

## Invariants

- No missing required acceleration metadata.
- No implicit axis interpretation.
- No inferred data type from file extension.
- No inferred layer semantics from data type.
- No silent lossy dense intensity conversion.
- No hidden display normalization written as stored intensity data.
- No hidden no-data sentinel inference.
- No silent isotropic resampling as canonical storage.
- No unit-spacing assumption for spatial data.
- No old-format compatibility branch.
- All binary indexes must be bounds-checkable before use.
- The native dataset package is the canonical viewer source.
- App-owned caches are not authoritative.

## Failure Modes

- unknown format string
- unsupported version
- missing required manifest field
- invalid axis order
- non-contiguous scale levels
- chunk/index/shard mismatch
- checksum mismatch
- sparse directory references missing payload bytes
- acceleration metadata shape mismatch
- dataset package moved or partially deleted while open

## Testing Requirements

- Golden manifest validation tests.
- Golden binary index parsing tests.
- Tiny dense intensity dataset fixture.
- Corrupt/missing shard fixtures.
- Axis-order validation tests.
- Spatial transform validation tests.
- Anisotropic source-grid preservation tests.
- Dense intensity dtype/conversion validation tests.
- Multi-channel metadata validation tests.
- Round-trip tests for preprocessing output.

## Open Questions

- Whether native `int16` intensity storage is needed in a future schema/profile.
- Exact binary index layout.
- Whether indexes are stored as Zarr arrays, custom binary blobs, or both.
