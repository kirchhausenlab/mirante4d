# Import And Preprocessing Workflow Specification

Status: ACCEPTED
Last updated: 2026-06-17

## Purpose

Define the first user-facing source import workflow that creates native `.m4d` datasets.


## Scope

This spec covers:

- first source formats
- GUI workflow
- metadata validation
- preprocessing options
- no-data sentinel review where applicable
- output safety
- cancellation and progress

## Non-Goals

- User-facing CLI/headless preprocessing.
- Legacy `llsm_viewer` output generation.
- Proprietary microscopy formats in the first import milestone.
- Silent guessing of ambiguous spatial calibration.
- Default isotropic inflation.

## First Source Formats

First supported import sources:

- single TIFF stack
- OME-TIFF when metadata is available through the chosen TIFF reader
- directory of TIFF stacks grouped into timepoints/channels by filename pattern

Rationale: the current local sample data configured by `MIRANTE4D_SAMPLE_DATA` is TIFF-based, and TIFF/OME-TIFF covers a useful first workflow without adding a JVM/Bio-Formats packaging burden.

Deferred source formats:

- CZI
- ND2
- LIF
- proprietary vendor formats
- generic arbitrary OME-Zarr import

Deferred formats require a separate decision before implementation.

## Current Implemented Scope

As of 2026-06-12, the implemented importer is intentionally narrower than the long-term import product direction but is the current accepted implementation scope:

- source: single grayscale `uint16`, `uint8`, or IEEE `float32` TIFF/OME-TIFF stack, or directory of grayscale TIFF/OME-TIFF stacks where every stack has the same accepted dtype
- dtype conversion: `uint16` sources are stored as native `uint16`; `uint8` sources are stored as native `uint8`; IEEE `float32` sources are stored as native `float32`; no lossy conversion is exposed
- grouping: strict filename tokens `chN` and `stackNNNN` for non-reviewed library imports; explicit reviewed file grouping for GUI imports that need correction
- channels: stored as separate dense intensity layers, not a channel axis
- timepoints: one TIFF stack per channel/timepoint
- output: native `.m4d` package using the current strict `mirante4d-v1` writer
- multiscales: deterministic mean-reduced pyramid
- statistics: per-scale min/max, dtype-specific histogram, robust percentiles,
  per-brick valid/min/max metadata, and shard-level checksum/payload-byte
  metadata
- display defaults: layer display window derived from source `p0.1` and `p99.9`
- inspection: metadata-only scan for file count, channel count, timepoint count, stack shape, source dtype, and OME-TIFF physical voxel spacing metadata before writing output
- storage estimate: before source stack conversion begins, the importer estimates native source payload bytes, derived multiscale payload bytes, metadata overhead, total package bytes, and peak decoded stack bytes
- OME-TIFF metadata: complete `PhysicalSizeX/Y/Z` values with explicit convertible units are surfaced as an initial voxel-spacing suggestion; missing, incomplete, or conflicting metadata is reported and does not silently fill spacing
- streaming: each source stack is converted into its per-timepoint scale pyramid and written directly into the temporary native package
- app setup: explicit review of source, detected dimensions, output package, dataset name, voxel-spacing metadata status, and voxel spacing before starting
- app setup must also expose any accepted no-data sentinel policy before preprocessing starts when the source profile or user selection requires one
- app setup shows the estimated native package size and peak decoded stack size before the user starts import
- filename grouping correction: directory imports expose editable per-file channel/timepoint grouping and cannot start until grouping has been reviewed
- channel metadata correction: channel names and colors are editable during the review step and written into native layer metadata
- metadata safety: the app blocks starting import until voxel spacing has been reviewed, so the default values cannot be accepted silently
- app workflow: background import task with progress/status display, cancellation, and open-on-success

Current unsupported import scope:

- TIFF source dtypes outside grayscale `uint8`, grayscale `uint16`, and grayscale IEEE `float32`
- 32-bit unsigned grayscale TIFFs are rejected and are not treated as `float32`
- lossy dtype conversion is not exposed; the implemented policy is lossless-only
- page/chunk streaming within an individual source TIFF stack is not the current importer architecture; TIFF pages are decoded by TIFF chunk/strip into the current source stack buffer

## Workflow

User-facing flow:

1. choose `Import Source`
2. select one TIFF/OME-TIFF file or a directory of TIFF files
3. inspect detected dimensions, channels, timepoints, dtype, and metadata
4. resolve filename grouping if multiple files are selected
5. resolve missing required metadata
6. choose output `.m4d` package path
7. run preprocessing with progress and cancellation
8. validate output
9. open the new `.m4d` dataset in the viewer

The workflow is integrated into the GUI app. It is not a separate product surface.

## Metadata Policy

The importer must determine or request:

- spatial dimensions
- timepoint count
- channel count
- source dtype
- voxel spacing and units
- time spacing where available
- channel names/wavelengths where available
- axis order

If voxel spacing or units are missing, the GUI must ask the user to provide them or explicitly mark the dataset as uncalibrated. It must not silently assume unit spacing for spatially meaningful data.

## Filename Grouping Policy

For TIFF directories, the importer should detect common patterns:

- stack index
- channel index or wavelength
- time in milliseconds
- camera/channel tokens

Detected grouping must be shown to the user before writing output.

Ambiguous grouping is a validation error until resolved.

## Default Preprocessing Options

Defaults:

- preserve source-like grid
- preserve source dtype when it maps to accepted native dtype
- store ordinary integer microscopy data as `uint16`
- use lossless conversion by default
- apply explicit no-data sentinel policy from `NO_DATA_MASK_POLICY_SPEC.md` when enabled by the reviewed import plan
- generate display window metadata from histograms/percentiles
- generate multiscales according to `DATASET_V1_STORAGE_POLICY_SPEC.md`
- generate per-brick valid counts, min/max/occupancy, and range hierarchy
- use production storage defaults unless the dataset is tiny
- do not create isotropic derived grids by default

Lossy dtype conversion requires an explicit user option and provenance.

## Output Safety

Preprocessing writes to a sibling temporary package first. The current implementation uses a hidden sibling name:

```text
.output_name.m4d.tmp-import/
```

After successful validation, the temporary package is renamed to:

```text
output_name.m4d/
```

When explicit replace is used, the existing output is first moved to a hidden sibling backup after the new temporary package has validated:

```text
.output_name.m4d.replace-backup/
```

The backup is removed after the new package commits. If commit fails, the importer attempts to restore the backup.

Rules:

- source files are never modified
- existing `.m4d` output is not overwritten without explicit user confirmation
- storage estimates are emitted as progress before stack conversion and shown in the app review UI
- cancelled jobs remove temporary output and do not publish a completed dataset
- validation failure does not produce a completed dataset

## Progress And Cancellation

Progress stages:

- source scan
- metadata validation
- chunk planning
- scale generation
- histogram/statistics
- acceleration metadata
- payload writing
- output validation

Cancellation must be available during long stages. Cancelled jobs must not leave an output that appears valid.

The current implementation reports discovery, per-stack reads, scale writes, package writing, and completion. It checks cancellation before/after stack reads, during per-timepoint multiscale generation, after scale writes, before package writing, and before final commit.

## Invariants

- No source overwrite.
- No hidden compatibility output.
- No silent lossy conversion.
- No silent unit-spacing assumption.
- No default isotropic resampling.
- No completed output without validation.
- GUI import creates the same native package the viewer streams from.
- Import cancellation must not leave a valid-looking final package from a partial write.

## Failure Modes

- unsupported TIFF layout
- inconsistent dimensions across files
- ambiguous filename grouping
- missing required spatial metadata
- unsupported dtype
- insufficient disk space
- read failure
- write failure
- cancellation
- validation failure after write

## Testing Requirements

- single TIFF synthetic import test
- TIFF directory grouping test
- OME-TIFF physical voxel-spacing metadata test
- ambiguous grouping rejection test
- missing voxel spacing UI/state test
- no-source-mutation test
- cancellation cleanup test
- output validation test
- open-preprocessed-output integration test
- no-data sentinel review and preprocessing tests from `NO_DATA_MASK_POLICY_SPEC.md`

## Open Questions

- Exact UI for filename grouping correction.
- Whether uncalibrated datasets are allowed into the viewer with strong warnings or blocked by default.
