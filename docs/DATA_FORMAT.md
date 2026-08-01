# Data Format And Safety

Mirante4D uses strict native packages so scientific meaning, storage, and
runtime expectations are explicit before a dataset opens.

## Active Dataset Profile

- Format family: `mirante4d`
- Lifecycle: `EXPERIMENTAL`
- Semantic schema: `m4d-science-1.0`
- Storage profile: `m4d-zarr3-local-1.0`
- Image metadata: OME-NGFF 0.5.2
- Array storage: Zarr 3.0 with indexed sharding
- Intensity dtypes: `uint8`, `uint16`, and finite `float32`
- Axes: explicit time and spatial axes; channels are separate logical layers
- Project store: `mirante4d-project-store-v1`
- Settings document: `mirante4d-settings-v1`

The product opens only this strict target profile. Unsupported identities,
malformed metadata, inconsistent payloads, and unrecognized profile variants
are rejected rather than guessed or migrated.

`0` is valid intensity unless an explicit reviewed no-data policy says
otherwise. Validity metadata is shared by import, storage, rendering, and
analysis. Missing data for an occupied region is incomplete/loading state, not
empty scientific data.

## Storage And Identity

- A **brick** is the runtime spatial block used for loading and rendering.
- A **chunk** is the logical Zarr storage unit.
- A **shard** is one physical storage object containing multiple chunks.

Indexed sharding keeps file count and I/O amplification bounded. Creating one
physical file or sidecar per brick is forbidden.

`mirante4d-storage` owns bounded structural admission, strict runtime object
and brick reads, explicit package-integrity auditing, and create-only package
publication. Ordinary packages open after metadata/schema/profile, path,
object-type, declared-length, and manifest-structure admission. They do not
wait for a full encoded-object or decoded-content scan. Every consumed object
retains checksum and currentness checks, while externally supplied packed
facts remain hints until the corresponding payload has been decoded and
checked.

`mirante4d-import-pipeline` writes validated target packages from TIFF and
OME-TIFF sources. Import never changes source data. It writes to an owned
stage, validates the result, and publishes only to a previously absent
destination.

The import source is an explicit per-channel manifest. Each channel declares
its label and exactly one interpretation: a single 3D TIFF, immediate 3D TIFF
children ordered as timepoints, or immediate single-page TIFF children
ordered as Z. Directory traversal is non-recursive and filenames carry no
scientific coordinate semantics. All channels currently require the same
`T/Z/Y/X` geometry and dtype; mixed-dtype package layout and float16 remain
unsupported. New imports write channel labels into optional canonical display
layer metadata. Existing packages with the field absent retain ordinal-derived
runtime labels and need no compatibility reader or migration.

Imported spatial pyramids follow one deterministic geometry contract. Time is
never reduced. Starting at S0, each spatial dimension is ceil-divided by two
until the coarsest shape satisfies both:

```text
max(z, y, x) <= 64
z * y * x <= 262,144 voxels per layer and timepoint
```

A small source that already satisfies both conditions remains single-scale.
A larger or long-thin source receives however many distinct factor-two levels
its geometry requires. The closed two-digit profile admits up to 64 levels;
the largest possible `u64` dimension reaches the terminal contract in 59, so
this bound covers every representable `Shape4D` rather than imposing a
product LOD count. The terminal geometry—not an ordinal such as S6—is the
viewer navigation-floor contract.

`mirante4d-storage` owns these terminal constants and the sole pure factor-two
shape sequence. Import production consumes that authority, and representative
DS logical-brick and shard ceilings are checked against it so a geometry
change cannot leave admission on a predecessor scale count.

Reviewed no-data imports resolve one immutable policy from channel zero at
timepoint zero. Automatic value detection accepts the first exact typed value
whose samples fill a `5 x 5 x 5` block in deterministic Z/Y/X traversal; it
supports uint8, uint16, and finite float32 stored bits. A complete second scan
then marks every same-value `5 x 5 x 5` seed and performs face-six connected
reconstruction through exact-value voxels. The resulting immutable row-packed
spatial mask is the union of components containing at least one seed; equal-
valued voxels disconnected from every seed remain valid. An automatic no-match
is successful and uses the ordinary all-valid representation. The independent
constant-plane rule records every exactly constant source Z plane from that
same volume. Neither rule inspects or validates later channels/timepoints; the
fixed mask geometry and Z indices apply dataset-wide. Manual uint8 mode remains
dataset-wide exact-value equality and does not use spatial reconstruction.

At S0, automatic mode classifies fixed spatial-mask membership, while manual
uint8 mode classifies value equality. Classification occurs only outside
recorded constant planes and receives the in-bounds Chebyshev-radius-one
invalid dilation (zero Z radius for 2D). Constant-plane invalidity is strictly
plane-local and never dilates. Each later LOD averages only final-valid samples
in its aligned,
odd-tail-clipped factor-two parent block. Value-derived unsupported regions
retain the same one-child-voxel dilation, while a coarse sample is hidden by
the plane rule only when its complete contributing base-Z interval is hidden.
Thus a thin hidden plane may disappear at coarse resolution without its fill
intensity contaminating the mean. A derived mean numerically equal to the
resolved value remains valid. Every final invalid sample uses typed canonical
zero.

The recipe records request mode, automatic block edge, face-six connectivity,
fixed-mask scope and row-packed encoding, exact mask digest and voxel count,
exact typed resolution or no-match, constant-Z indices, classification,
morphology, reduction, rounding, support, and canonicalization under
`tiff-import-typed-first-volume-no-data` version `2.0.0`. A request that
resolves no mask or plane emits no validity arrays and retains the ordinary
point-decimated all-valid route.

Validity-aware mean levels use an axis-aware centered OME transform. An axis
reduced by cumulative factor `F` has scale `base_spacing * F` and translation
`base_spacing * (F - 1) / 2`; an axis already of length one keeps factor one
and zero translation. Existing complete packages retain their recorded arrays
and transforms and are not reinterpreted. Incomplete predecessor no-data
checkpoints do not resume under the current resolved-policy plan binding.

Incomplete imports use one current, non-portable final-layout stage beside the
requested destination. One `(timepoint, channel)` unit is decoded into bounded
little-endian cache scratch and encoded into bounded unit spool scratch.
Completed outer shards are committed directly to their final package-relative
paths; there is no complete decoded-base cache and no dataset-scale encoded
checkpoint copy. The private control directory holds a chained shard journal,
canonical unit journal, fixed-position decoded-unit digests, packed-index
records, resolved no-data policy, and one resumable scientific-hash frontier
per channel. Journal records follow the payload durability barrier and name
only a durable canonical prefix. Recovery removes an incomplete bounded
suffix, rejects corruption or a wrong source/plan binding, and never migrates
the predecessor checkpoint schema. A canonical plane above the explicit
64 MiB work-unit ceiling still fails before ingest. Checkpoint control is
removed before validation/publication and is never part of the `.m4d` profile.
The stage journal also retains a cumulative durable-payload prefix per input
ordinal. That index is private resume/accounting state: it lets preprocessing
deduct bytes already committed for the active temporal unit without treating
the package-wide output ceiling as a free-space reservation. It does not alter
package identity or the public format.

Pixel and validity chunks are numerically produced and inner-encoded once
through the storage codec authority. The bounded unit spool validates kind,
encoded/decoded length, checksum, single-frame extent, ordering, and profile
before those exact inner bytes enter final-layout outer shards; it is deleted
after the durable stage and unit journals advance. Packed-index chunks are
assembled after all unit records are present. Scientific and exact package
identities retain their existing contracts.

The scientific content address is independent of storage layout. The exact
package digest covers the package bytes. Either digest names content under its
specified algorithm; package self-agreement does not authenticate the producer
or prove scientific correctness. Recipe, derivation, rights, citation, and
analysis artifact identities remain explicit typed records rather than
filenames or informal metadata.

## Data Safety

- Import, validation, project recovery, and maintenance never modify source
  microscopy data.
- Writers stage output, validate it, and publish atomically under an explicit
  create-only or replacement policy. Incomplete output never appears complete.
- A consumed object that changes across its guarded read fails that dependent
  operation with its exact object and phase. An explicit full audit reports
  package self-consistency without promoting or revoking the open source or
  clearing already presented pixels.
- There are no compatibility readers or in-application migrations during
  pre-alpha development.
- Analysis results carry source and operation provenance. Preview,
  approximate, partial, and complete states are distinct; only complete
  results can be exported as final results.
- Public evidence must not expose private paths, dataset metadata, or raw
  qualification identities.

## Project Format And Scope

The accepted project-store service is the sole product route for New, Open,
Save, Save As, autosave, and recovery. Project records retain typed content
addresses and optional exact-package pins, but project I/O does not wait for an
unrelated whole-package integrity audit. The project format remains
experimental.

The active dataset profile does not promise backward compatibility, stable
public release support, or generic OME-Zarr compatibility. Its exact normative
standards are recorded in
[`architecture/wp10a-normative-standards.json`](../architecture/wp10a-normative-standards.json).
The closed control wire is implemented by `mirante4d-storage` and described
above. The bounded independent corpus is
[`fixtures/target/manifest.json`](../fixtures/target/manifest.json).
