# Data Format And Safety

Mirante4D uses strict native packages. Scientific meaning, storage, and
runtime limits are explicit before a dataset opens.

## Active Dataset Profile

- Format family: `mirante4d`
- Lifecycle: `EXPERIMENTAL`
- Semantic schema: `m4d-science-1.0`
- Storage profile: `m4d-zarr3-local-1.0`
- Image metadata: OME-NGFF 0.5.2
- Array storage: Zarr 3.0 with indexed sharding
- Intensity dtypes: `uint8`, `uint16`, and finite `float32`
- Axes: explicit time and spatial axes; channels are logical layers
- Project store: `mirante4d-project-store-v1`
- Settings document: `mirante4d-settings-v1`

The product opens only this profile. Unsupported identities, malformed
metadata, inconsistent payloads, and unknown variants fail. They do not select
a repair, migration, or generic OME-Zarr path.

`0` is valid intensity unless a reviewed no-data policy says otherwise.
Missing data for an occupied region is incomplete state, not scientific zero.

## Storage Terms

- A **brick** is a semantic runtime spatial block.
- A **chunk** is a logical Zarr storage unit.
- A **shard** is one physical object that contains several chunks.

Indexed sharding keeps file count and I/O amplification bounded. A file or
sidecar for each brick is forbidden.

`mirante4d-storage` owns package paths, manifest limits, bounded admission,
reads, integrity checks, explicit audits, and create-only publication.
`mirante4d-import-pipeline` produces the profile from TIFF sources.

## Import Source Contract

The import source is an explicit list of one through 64 named channels. Each
channel declares exactly one interpretation:

- one 3D TIFF;
- immediate 3D TIFF children ordered as timepoints; or
- immediate single-page TIFF children ordered as Z.

Directory traversal is non-recursive. Lexical child order is canonical.
Filenames have no scientific coordinate meaning. Labels must be unique after
normalization. All channels require one common `T/Z/Y/X` shape and dtype.

New imports store optional canonical display labels. A package without that
optional field uses ordinal-derived labels. This does not require a second
format reader.

Import accepts grayscale `uint8`, `uint16`, and finite `float32` TIFF pages.
Compression can be uncompressed, LZW, current or old Deflate, or PackBits.
Other decoder workspaces are outside the audited memory boundary and fail as
unsupported.

Source inspection and decode bind the reviewed path to the opened descriptor
generation. Import checks that generation before, during, and after use. It
never writes source data.

## Spatial Pyramid

Time is never reduced. Starting at S0, each spatial dimension is ceil-divided
by two until both conditions are true:

```text
max(z, y, x) <= 64
z * y * x <= 262,144 voxels per layer and timepoint
```

A source that already meets both conditions remains S0-only. The profile
admits up to 64 spatial levels. This covers the maximum representable
`Shape4D` dimension.

The terminal geometry is the viewer navigation-floor contract. No fixed scale
ordinal has that role.

## No-Data And Validity

Reviewed no-data import resolves one immutable policy from channel zero at
timepoint zero.

Automatic value detection finds the first exact typed value whose samples
fill a `5 x 5 x 5` block in deterministic Z/Y/X order. A complete second scan
marks all such seeds and reconstructs every face-connected equal-value
component that contains a seed. Equal-valued voxels disconnected from all
seeds remain valid. No seed is a normal all-valid result.

Constant-plane detection records each exactly constant source Z plane from
the same volume. It is independent of automatic value detection. The fixed
mask and plane indices apply to every channel and timepoint. Later volumes do
not change the policy.

Manual `uint8` mode uses dataset-wide exact-value equality. It does not use
spatial reconstruction.

At S0, automatic mode classifies fixed spatial-mask membership. Manual mode
classifies value equality. Value-derived invalidity uses an in-bounds
Chebyshev-radius-one dilation. Its Z radius is zero for 2D data. Constant-plane
invalidity is plane-local and never dilates.

Each coarser value is the mean of final-valid samples in its aligned,
odd-tail-clipped factor-two parent block. Integer means use half-up rounding.
Float means use finite `f64` accumulation and canonical `float32` output.
Value-support dilation continues through coarse validity. A coarse sample is
hidden by the plane rule only when its complete contributing base-Z interval
is hidden.

A derived valid mean can equal the resolved no-data value. It remains valid.
Every final invalid sample uses typed canonical zero.

Validity-aware levels use centered OME transforms. For cumulative reduction
factor `F`, an affected axis uses:

```text
scale = base_spacing * F
translation = base_spacing * (F - 1) / 2
```

An axis of length one keeps factor one and zero translation.

The recipe records request mode, block edge, connectivity, fixed-mask scope,
mask digest, resolved typed value or no-match, constant-plane indices,
classification, morphology, reduction, rounding, support, and
canonicalization. An all-valid result emits no validity arrays.

## Checkpoint And Publication

An incomplete import uses one private final-layout stage beside the requested
destination. The stage contains:

- one current unit cache;
- at most one future decoded cache;
- one current bounded encoded spool;
- a compact shard and unit journal;
- fixed-position decoded digests and packed records;
- the resolved no-data policy; and
- one resumable scientific-hash frontier per channel.

The future cache has no scientific-hash, spool, shard, journal, or publication
authority. The canonical owner consumes it only at its exact unit ordinal.

Completed outer shards enter their final package-relative positions. Journal
records follow payload durability and name one valid prefix. Recovery removes
an incomplete suffix. It rejects corrupt state or the wrong source and plan
binding. It does not migrate an old checkpoint schema.

Disk admission reserves bounded unfinished-unit and finalization headroom.
The predicted full package size is guidance only. A safe capacity pause keeps
the checkpoint and offers Resume.

Pixel and validity chunks are numerically produced and inner-encoded once.
The spool validates kind, length, checksum, frame extent, order, and profile
before the exact inner bytes enter outer shards. Publication removes private
checkpoint control, validates the staged package, completes its durability
order, and renames only to an absent destination.

## Package Admission And Integrity

Ordinary open checks metadata, profile, paths, object types, declared lengths,
and bounded manifest structure. It does not wait for a complete encoded and
decoded content scan.

Each consumed object still receives checksum and pre/post-use mutation checks.
Packed payload facts from an external package are hints until the matching
payload passes its first required check. An import capability can transfer
facts already checked by the same create-only publication.

A separate explicit full audit checks encoded objects, S0 content addresses,
and packed-fact self-consistency. It is bounded and cancellable. It does not
promote or revoke the open source. Package self-agreement does not prove
authorship, provenance, or biological correctness.

## Identity

The scientific content address is independent of storage layout. The exact
package digest covers package bytes. These identities do not substitute for
derivation, rights, citation, or independent scientific validation.

Recipe, derivation, artifact, project, and source identities are typed. They
do not come from filenames or informal metadata.

## Data Safety

- Import, validation, recovery, and maintenance never modify source
  microscopy data.
- Writers stage, validate, and publish through an explicit atomic policy.
- Incomplete output never appears complete.
- A changing consumed object fails the dependent operation with its object and
  phase.
- There are no compatibility readers or in-product migrations during
  pre-alpha development.
- Preview, partial, approximate, and complete results remain distinct.
- Only complete analysis results can become final exported artifacts.
- Public evidence cannot contain private paths, credentials, or unpublished
  dataset metadata.

## Project Format

The project-store service is the sole product route for New, Open, Save, Save
As, autosave, and recovery. Project records retain typed content addresses and
optional exact-package pins. Project I/O does not wait for an unrelated full
package audit.

The dataset and project formats remain experimental. They do not promise
backward compatibility, stable release support, or generic OME-Zarr input.

Normative format sources are in
[`architecture/wp10a-normative-standards.json`](../architecture/wp10a-normative-standards.json).
The independent bounded target corpus is
[`fixtures/target/manifest.json`](../fixtures/target/manifest.json).
