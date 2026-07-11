# Preprocessing Specification

Status: ACCEPTED
Last updated: 2026-06-27

## Purpose

Define the pipeline that converts source microscopy data into the strict Mirante4D native dataset format.

## Scope

Preprocessing covers:

- source inspection
- validation
- dtype conversion policy
- display normalization metadata
- no-data sentinel policy and visibility metadata
- physical geometry and coordinate metadata
- optional explicit derived resampling artifacts
- multiscale generation
- dense intensity brick writing
- acceleration metadata generation
- track conversion
- output validation

Preprocessing is expected to be an integrated GUI application workflow, backed by reusable internal crates. It is not planned as a separate user-facing CLI or headless product mode.

The first concrete import workflow and source-format scope is defined in `IMPORT_PREPROCESSING_WORKFLOW_SPEC.md`.

## Current Implementation Status

As of 2026-06-17, preprocessing has an implemented strict TIFF source subset:

- single `uint16` grayscale TIFF stack import
- single `uint8` grayscale TIFF stack import stored as native `uint8`
- directory import of grayscale `uint16` TIFF stacks
- directory import of grayscale `uint8` TIFF stacks stored as native `uint8`
- rejection of mixed source dtypes within one TIFF import
- source/stored dtype metadata in the native layer manifest for TIFF imports
- directory filename-token grouping by `chN` and `stackNNNN`
- explicit reviewed directory file grouping for TIFF files whose names need correction
- native `.m4d` output through the shared incremental writer
- output validation before commit
- mean multiscale generation
- per-scale statistics and robust percentile display defaults
- metadata-only TIFF directory inspection before import execution
- OME-TIFF `ImageDescription` metadata extraction for complete XYZ physical voxel spacing with explicit units
- per-page TIFF chunk/strip decoding into the current source stack buffer instead of whole-page `read_image()` allocation
- streaming native `uint8`, `uint16`, and `float32` layer writing by timepoint and scale
- native dense intensity output uses sharded Zarr v3 storage with logical
  bricks bounded by the storage policy: `t=1,z=64,y=64,x=64` for 3D imports
  and `t=1,z=1,y=256,x=256` for 2D imports, clamped to smaller source
  dimensions
- production multiscale stop rules: tiny imports keep only source scale `s0`; larger imports downsample spatial axes until the scale is small enough by dimension or per-timepoint voxel count
- per-timepoint multiscale generation bounded to the current source stack and the current generated scale pair, not a retained full-scale pyramid
- background GUI execution with progress/status and cancellation
- explicit GUI review/edit step for detected dimensions, dataset name, voxel spacing, voxel-spacing metadata status, channel names, and channel colors before import starts
- explicit voxel-spacing review gate so default spacing cannot be accepted silently
- temporary output cleanup on cancellation or failure

The broader requirements below remain the target contract for the full preprocessing system. The current importer is no longer channel-buffered: it writes each generated timepoint/scale slab directly into a bounded-chunk native package. It also decodes each TIFF page by TIFF chunk/strip into the current source stack buffer. It still holds one source TIFF stack and one generated output scale at a time. The implemented dense TIFF dtype policy is lossless-only: `uint8`, `uint16`, and grayscale IEEE `float32` sources are accepted and preserved as native stored dtypes.

Current hard-cutover storage and dtype contract:

- Production preprocessing must write sharded Zarr v3 dense arrays, where the
  logical viewer brick is the sharding subchunk and the storage object is the
  outer shard.
- Source `uint8`, `uint16`, and grayscale IEEE `float32` inputs are preserved
  as native stored dtypes by default.
- Preprocessing must not widen `uint8` or `uint16` source data merely to satisfy
  an older viewer representation.

## Non-Goals

- Preserving old `llsm_viewer` output format.
- Silent repair of invalid source datasets.
- Writing compatibility variants.
- Viewer runtime fallbacks for missing preprocessing products.
- Making isotropic resampling the default canonical output.

## Requirements

- Preprocessing must be deterministic for the same inputs and options.
- Output must be validated before being marked complete.
- Partial outputs must be clearly marked incomplete or written into a temporary location.
- Source files must not be modified.
- Source intensity values must not be silently normalized into display values.
- No-data sentinel values must be explicit user-reviewed metadata, not silent source mutation.
- Lossy dtype conversion must require an explicit option and provenance.
- Source dtype and stored dtype metadata must be recorded where available.
- Source-like spatial grids should be preserved by default.
- Voxel spacing, units, and index-to-physical transforms must be recorded where available.
- Isotropic resampling must be an explicit derived-artifact operation, not a hidden preprocessing default.
- Jobs must be cancellable.
- Progress must be reportable by stage.
- The output format must be the current strict Mirante4D native format.
- Successful preprocessing should produce the same durable native dataset package that the viewer streams from.
- Production dense output must use the current sharded storage contract.

## Pipeline Stages

1. Inspect source files and metadata.
2. Validate dimensions, channels, timepoints, data types, and voxel sizes.
3. Resolve preprocessing options.
4. Resolve physical geometry metadata and index-to-physical transforms.
5. Resolve dense intensity dtype, conversion policy, and display normalization metadata.
6. Generate no-data validity metadata when a layer declares a sentinel policy.
7. Generate multiscale pyramid.
8. Generate histograms.
9. Generate per-brick valid counts, min/max, and occupancy.
10. Generate range hierarchy.
11. Convert tracks.
12. Write shards, indexes, and manifests.
13. Validate output.

## Invariants

- No valid output without validation.
- No missing required acceleration metadata.
- No hidden compatibility mode.
- No source overwrite.
- No best-effort guessing when required metadata is ambiguous.
- No separate app-private storage output for normal viewing.
- No silent lossy dtype conversion.
- No display normalization written as source-like intensity data.
- No runtime-only no-data masking that leaves statistics or multiscales polluted by sentinel values.
- No default isotropic inflation of source-like data.
- No missing physical geometry metadata for spatial datasets.

## Failure Modes

- unsupported source format
- inconsistent dimensions
- inconsistent timepoint count
- unsupported data type
- unsupported or unsafe dtype conversion
- missing or ambiguous spatial calibration
- unsupported spatial/acquisition transform
- insufficient disk space
- cancellation
- write failure
- validation failure after write

## Testing Requirements

- Tiny synthetic source-to-native round trip.
- `uint8`, `uint16`, and grayscale IEEE `float32` TIFF-to-native dtype round trips.
- No-silent-normalization tests.
- Explicit lossy-conversion metadata tests.
- Physical transform metadata validation tests.
- Default-no-isotropic-inflation tests.
- Determinism tests.
- Cancellation tests.
- Partial-output cleanup/recovery tests.
- Invalid source validation tests.
- Benchmark tests for preprocessing throughput.
- No-data sentinel tests described in `NO_DATA_MASK_POLICY_SPEC.md`.

## Open Questions

- CPU vs GPU preprocessing split.
- CUDA optional acceleration policy.
- Exact UI for explicit derived isotropic grid creation.
