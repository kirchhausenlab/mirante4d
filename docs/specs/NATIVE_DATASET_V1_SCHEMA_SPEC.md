# Native Dataset V1 Schema Specification

Status: ACCEPTED
Last updated: 2026-06-17

## Purpose

Define the first concrete `mirante4d-v1` dataset package schema used by bootstrap code and tests.

This spec narrows the broader dataset-format direction into a minimal accepted v1 subset. The subset is intentionally small, but it is not a temporary compatibility route and not an old-format bridge.

## Scope

This spec covers:

- root package layout
- root manifest shape
- dense intensity layer schema
- bootstrap Zarr v3 array placement
- axis, dtype, spatial transform, display, statistics, and checksum fields
- deterministic first fixtures

## Non-Goals

- Opening legacy `llsm_viewer` preprocessed datasets.
- Opening arbitrary OME-Zarr datasets.
- Supporting multiple historical Mirante4D schema variants.
- Defining analysis or track artifact schemas.
- Hiding missing metadata behind best-effort inference.

## Package Identity

A native dataset is a directory package.

Product-created packages should use the `.m4d` suffix:

```text
example_dataset.m4d/
```

The suffix is a user-experience convention. The authoritative identity is the root manifest:

```text
example_dataset.m4d/
  mirante4d.json
```

The reader must reject packages without `mirante4d.json`, even if the directory name ends in `.m4d`.

## Bootstrap Package Layout

The initial accepted dense-intensity package layout is:

```text
dataset.m4d/
  mirante4d.json
  arrays/
    intensity/
      <layer_id>/
        s0/
          zarr.json
          ...
```

Rules:

- `mirante4d.json` is the authoritative Mirante4D manifest.
- `arrays/intensity/<layer_id>/s<level>/` is a Zarr v3 array root.
- `mirante4d-format` owns strict manifest validation.
- `zarrs` owns Zarr v3 array metadata and chunk decoding unless dependency review finds a concrete blocker.
- The app does not accept generic Zarr/OME-Zarr datasets that lack the Mirante4D manifest and required metadata.

The current code supports required source scale `s0` plus optional additional dense intensity scales for LOD rendering.

## Root Manifest

The root manifest is UTF-8 JSON.

Required top-level fields:

```json
{
  "format": "mirante4d-v1",
  "schema_version": 1,
  "writer": {
    "name": "mirante4d",
    "version": "0.0.0-dev"
  },
  "dataset": {
    "id": "fixture-basic-u16",
    "name": "Basic uint16 fixture"
  },
  "axes": ["t", "z", "y", "x"],
  "world_space": {
    "name": "sample",
    "unit": "micrometer"
  },
  "layers": []
}
```

Rules:

- `format` must be exactly `mirante4d-v1`.
- `schema_version` must be exactly `1` for the first implementation.
- `axes` must be exactly `["t", "z", "y", "x"]` for bootstrap dense intensity arrays.
- `layers` must be non-empty for a viewable dataset package.
- Channels are separate layers; dense intensity arrays do not include a channel axis.
- 2D images use `z = 1`.
- Single images use `t = 1`.
- The app must not infer units, axis order, dtype, display range, or channel semantics from file names.

## Dense Intensity Layer Schema

Each dense intensity layer entry has this required shape:

```json
{
  "id": "ch0",
  "kind": "dense_intensity",
  "name": "Channel 0",
  "channel": {
    "index": 0,
    "color_rgba": [0.0, 1.0, 0.0, 1.0]
  },
  "shape": {
    "t": 1,
    "z": 16,
    "y": 16,
    "x": 16
  },
  "dtype": {
    "source": "uint16",
    "stored": "uint16",
    "conversion": "lossless"
  },
  "grid_to_world": {
    "matrix4x4_row_major": [
      0.20, 0.0, 0.0, 0.0,
      0.0, 0.20, 0.0, 0.0,
      0.0, 0.0, 0.50, 0.0,
      0.0, 0.0, 0.0, 1.0
    ]
  },
  "display": {
    "visible": true,
    "window": { "low": 0.0, "high": 65535.0 },
    "opacity": 1.0
  },
  "scales": []
}
```

Rules:

- `id` must be stable, ASCII, and unique within the package.
- `kind` must be `dense_intensity` for dense intensity layers.
- `channel.index` is metadata for ordering; it is not an array axis.
- `color_rgba` values are linear display values in `[0.0, 1.0]`.
- `shape` values are positive integers.
- `dtype.source` and `dtype.stored` must be one of `uint8`, `uint16`, or `float32` in the initial implementation.
- `dtype.conversion` must be explicit. The first accepted value is `lossless`.
- `display.window` is a display mapping, not stored-value normalization.
- Optional no-data sentinel metadata is defined by `NO_DATA_MASK_POLICY_SPEC.md`. When present, it must be explicit layer metadata and must not be inferred from filenames, paths, dtype, or display window.

## Spatial Transform Convention

`grid_to_world.matrix4x4_row_major` stores a row-major 4x4 matrix.

Mathematically, it maps a column vector `[x, y, z, 1]` from voxel-grid coordinates into the manifest `world_space` unit.

Rules:

- The array storage order is `t, z, y, x`.
- The spatial transform input order is `x, y, z`.
- Voxel centers use integer grid coordinates unless a later decision changes the convention.
- For physical bounds, a voxel-centered grid spans `[-0.5, shape_axis - 0.5]` along each spatial axis before applying `grid_to_world`.
- Downsampled multiscale transforms must preserve voxel-center registration. If a scale is reduced by factor `F` from its source scale on a spatial axis, output grid coordinate `i` maps to source-grid coordinate `F * i + (F - 1) / 2` on that axis before the source `grid_to_world` transform is applied. Axes with factor `1` have zero added offset.
- Scaling voxel spacing without the center offset is invalid. It shifts lower-resolution scales relative to source scale and must be rejected instead of tolerated.
- `world_to_grid` is derived by inverting `grid_to_world`; it is not stored separately in the bootstrap manifest.
- CPU metadata and measurement code should use `f64`.
- GPU uniforms may use `f32` conversions at the renderer boundary.

## Scale Schema

The first accepted scale entry is:

```json
{
  "level": 0,
  "array_path": "arrays/intensity/ch0/s0",
  "shape": { "t": 1, "z": 16, "y": 16, "x": 16 },
  "storage": {
    "kind": "zarr_v3_sharded",
    "array_path": "arrays/intensity/ch0/s0",
    "dtype": "uint16",
    "codec_chain": ["sharding", "bytes", "zstd"],
    "brick_shape": { "t": 1, "z": 16, "y": 16, "x": 16 },
    "brick_grid_shape": { "t": 1, "z": 1, "y": 1, "x": 1 },
    "subchunk_shape": { "t": 1, "z": 16, "y": 16, "x": 16 },
    "chunks_per_shard": { "t": 1, "z": 1, "y": 1, "x": 1 },
    "shard_shape": { "t": 1, "z": 16, "y": 16, "x": 16 },
    "shard_grid_shape": { "t": 1, "z": 1, "y": 1, "x": 1 },
    "checksum_scope": "zarr_shard_payload",
    "shard_records": [
      {
        "index": { "t": 0, "z": 0, "y": 0, "x": 0 },
        "payload_bytes": 8192,
        "payload_checksum": {
          "algorithm": "blake3",
          "scope": "zarr_shard_payload",
          "hex": "<hex digest>"
        }
      }
    ]
  },
  "grid_to_world": {
    "matrix4x4_row_major": [
      0.20, 0.0, 0.0, 0.0,
      0.0, 0.20, 0.0, 0.0,
      0.0, 0.0, 0.50, 0.0,
      0.0, 0.0, 0.0, 1.0
    ]
  },
  "source_scale": null,
  "reduction": "source",
  "statistics": {
    "min": 0.0,
    "max": 65535.0,
    "histogram": {
      "bin_count": 256,
      "range_min": 0,
      "range_max": 65535,
      "bins": ["<256 uint64 counts>"]
    },
    "percentiles": {
      "p0_1": 0.0,
      "p1": 0.0,
      "p50": 1024.0,
      "p99": 4095.0,
      "p99_9": 4095.0
    }
  },
  "bricks": {
    "grid_shape": { "t": 1, "z": 1, "y": 1, "x": 1 },
    "records": [
      {
        "index": { "t": 0, "z": 0, "y": 0, "x": 0 },
        "occupied": true,
        "valid_voxel_count": 4096,
        "min": 0.0,
        "max": 65535.0
      }
    ],
    "range_hierarchy": {
      "levels": [
        {
          "level": 0,
          "grid_shape": { "t": 1, "z": 1, "y": 1, "x": 1 },
          "records": [
            {
              "index": { "t": 0, "z": 0, "y": 0, "x": 0 },
              "has_valid_voxels": true,
              "valid_voxel_count": 4096,
              "min": 0.0,
              "max": 65535.0
            }
          ]
        }
      ]
    }
  }
}
```

Rules:

Current production hard-cutover rules:

- Production dense scale storage must use `storage.kind =
  "zarr_v3_sharded"`.
- Dense scale records must use explicit `storage` metadata with `kind`,
  `brick_shape`, `brick_grid_shape`, `subchunk_shape`, `chunks_per_shard`,
  `shard_shape`, and `shard_grid_shape`.
- Scale records must not serialize a top-level `chunk_shape`.
- `storage.brick_shape` is the viewer/data-engine logical brick shape.
- `storage.subchunk_shape` is the Zarr sharding inner chunk shape and must
  equal `storage.brick_shape`.
- `storage.shard_shape` is the Zarr outer chunk shape and must equal
  `storage.brick_shape * storage.chunks_per_shard` per axis.
- `storage.chunks_per_shard.t` must be `1` by default; production shards must
  not span multiple timepoints unless a future accepted policy explicitly
  changes this.
- `storage.shard_grid_shape` must cover the scale shape at `shard_shape`
  granularity.
- Production codec metadata must describe a sharded Zarr v3 array whose inner
  codec chain stores native numeric values through `bytes` plus the declared
  compression codec.
- Per-brick encoded `payload_bytes` and `payload_checksum` are not serialized
  for sharded production arrays. Shard-level `storage.shard_records` carry
  encoded payload bytes and `zarr_shard_payload` checksums.
- `storage.shard_records` are required for production dense intensity and
  validity arrays. Each record is keyed by logical shard coordinate, not by an
  ad hoc filesystem path.

- Dense intensity supports required source scale `level = 0` plus optional additional render scales.
- `array_path` is relative to the package root and must point to a Zarr v3 array root.
- `shape` must equal the parent layer shape for `level = 0`.
- Additional scales must preserve `t`, may reduce spatial dimensions, and must record their own `grid_to_world`.
- Scale levels must be contiguous from `0`.
- `statistics.histogram` uses fixed 256-bin integer histograms for current `uint8` and `uint16` native writes: `uint8` bins map one-to-one to `0..255`, and `uint16` bins cover the full `0..65535` range.
- `float32` native writes use `4096` bins over the finite stored value range. Non-finite `float32` payload values are invalid.
- `statistics.histogram.range_min` and `statistics.histogram.range_max` are numeric floating-point manifest fields so integer and float stored dtypes share the same schema shape.
- `statistics.percentiles` stores nearest-rank robust percentiles for display and diagnostics.
- For layers declaring no-data masking, statistics, brick min/max, valid counts, occupancy, and range hierarchy must be computed over render-valid voxels according to `NO_DATA_MASK_POLICY_SPEC.md`.
- For layers without no-data masking, every geometrically present source voxel is valid; zero-valued voxels are not no-data.
- `bricks.records[].valid_voxel_count` is required. Dense intensity `occupied` means the brick has at least one valid voxel, not at least one nonzero voxel.
- `bricks.range_hierarchy` is required and stores deterministic valid/min/max summaries derived from brick records.
- Range hierarchy `level = 0` mirrors the leaf brick grid; each coarser level groups spatial bricks by `2 x 2 x 2` per timepoint until the spatial hierarchy reaches `1 x 1 x 1`.
- Range hierarchy records must match the brick records exactly; stale or inconsistent hierarchy metadata is invalid.
- Scale `s0` must use `source_scale = null` and `reduction = "source"`.
- Additional scales must use `source_scale = level - 1` and `reduction != "source"`.
- `bricks.grid_shape` must match `storage.brick_grid_shape`.
- The first implementation may use inline brick records.
- Future large datasets may move brick records to external indexes, but that must be documented as the current schema when implemented.
- Arrays are sharded and compressed according to
  `DATASET_V1_STORAGE_POLICY_SPEC.md`.
- Full validation must verify shard byte counts and shard checksums against
  payloads resolved through the Zarr store layer.
- The checksum is associated with the logical shard coordinate, not with an ad
  hoc file path parsed by app or renderer code.
- `mirante4d-format` / `mirante4d-data` must resolve Zarr storage through the
  Zarr store layer; UI and renderer code must not inspect Zarr internals.

## Zarr V3 Storage Subset

The current implementation uses Zarr v3 storage machinery for dense intensity
arrays with a deliberately small codec set:

- dense numeric arrays
- array shape `t, z, y, x`
- production arrays use Zarr v3 sharding
- logical brick shape is declared as `storage.brick_shape`
- Zarr sharding inner chunk shape equals `storage.subchunk_shape`
- Zarr outer chunk shape equals `storage.shard_shape`
- stored dtypes limited to `uint8`, `uint16`, and `float32`
- production compression follows `DATASET_V1_STORAGE_POLICY_SPEC.md`

`zarrs` may decide the exact internal chunk-key layout. Mirante4D tests should
validate the semantic contract through the manifest, validator, and reader APIs
instead of depending on ad hoc path parsing in app or renderer code.

Checksum policy:

- production sharded arrays use shard-level encoded payload byte counts and
  `zarr_shard_payload` checksums
- explicitly scoped unsharded unit-test fixtures, if retained, may use
  per-chunk `zarr_chunk_payload` checksums
- production sharded arrays must use the digest mapping rules in
  `DATASET_V1_STORAGE_POLICY_SPEC.md`

## First Synthetic Fixtures

Committed fixtures must stay tiny. Larger real data remains outside the repo and is addressed through `MIRANTE4D_SAMPLE_DATA`.

The first fixture set should be generated by `xtask` into `target/mirante4d/fixtures/` and should include:

- `basic-u16-16cube.m4d`: one channel, one timepoint, `uint16`, `16x16x16`, isotropic spacing.
- `anisotropic-u16-16cube.m4d`: one channel, one timepoint, `uint16`, `16x16x16`, anisotropic spacing.
- `time-u16-8cube-3t.m4d`: one channel, three timepoints, `uint16`, `8x8x8`.
- `basic-f32-8cube.m4d`: one channel, one timepoint, `float32`, `8x8x8`, isotropic spacing.

The deterministic value pattern for `uint16` fixtures is:

```text
value(t, z, y, x) = t * 4096 + z * 257 + y * 17 + x
```

Native `float32` round-trip coverage includes strict writer/data-engine unit
tests and the generated `basic-f32-8cube.m4d` package fixture used by typed
measurement tests.

Fixture dimensions must keep this value within `uint16` range.

## Invariants

- No missing root manifest.
- No generic OME-Zarr or generic Zarr acceptance.
- No old-format reader.
- No channel axis inside bootstrap dense intensity arrays.
- No implicit axis order.
- No implicit dtype.
- No implicit unit spacing.
- No silent display normalization as stored data.
- No renderer file reads.
- No app/UI parsing of Zarr chunks or binary payloads.
- No unchecked payload length or checksum mismatch.

## Failure Modes

- missing `mirante4d.json`
- wrong `format`
- unsupported `schema_version`
- wrong bootstrap axis order
- duplicate layer IDs
- unsupported dtype
- unsupported layer kind
- invalid `grid_to_world` matrix
- scale `0` missing
- Zarr array shape mismatch
- logical brick, subchunk, or shard shape mismatch
- missing payload
- payload checksum mismatch
- missing or inconsistent range hierarchy

## Testing Requirements

- Golden manifest parse tests for each first fixture.
- Invalid manifest tests for each required field.
- Shape, brick, subchunk, and shard mismatch tests.
- Transform round-trip tests.
- Range hierarchy validation tests.
- Checksum mismatch tests.
- Known-value read tests using the deterministic pattern.
- Anisotropic transform validation tests.
- Timepoint read tests.
- Rejection tests for generic Zarr/OME-Zarr directories without `mirante4d.json`.

## Open Questions

- Exact external index format for large multiscale datasets.

Production compression and multiscale policy are defined in `DATASET_V1_STORAGE_POLICY_SPEC.md`. Project/sidecar ownership is defined in `PROJECT_SESSION_MODEL_SPEC.md`.
