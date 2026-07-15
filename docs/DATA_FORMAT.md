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

`mirante4d-storage` owns bounded catalog opening, exact-package and
scientific-content verification, runtime brick reads, and create-only package
publication. Packages open provisionally; project binding and project I/O stay
blocked until background verification succeeds.

`mirante4d-import-pipeline` writes validated target packages from TIFF and
OME-TIFF sources. Import never changes source data. It writes to an owned
stage, validates the result, and publishes only to a previously absent
destination.

Incomplete imports use one current, non-portable checkpoint schema with six
regular files: two fixed canonical-base files and four encoded-spool files.
The canonical file stores little-endian planes; checksummed state records name
only a durable plane prefix. Its batch triggers are 16 completed planes,
64 MiB of pending plane bytes, and a 15-second age check. The spool uses
payload, journal, and chained watermark durability batches triggered at 512
work units, 64 MiB of pending encoded payload, and the same age check. A stage
boundary or serialized decoder interval can force either authority to
synchronize earlier. A canonical plane above 64 MiB fails the checked capacity
boundary instead of expanding a batch. Recovery discards only an incomplete
bounded suffix, rejects complete corruption or a wrong source/plan binding,
and never migrates a predecessor checkpoint. Checkpoint names are opened
relative to a retained no-follow directory descriptor; canonical and spool
cleanup remove a name only while it still identifies the exact retained file.
Checkpoints are temporary implementation state, not part of the `.m4d`
profile.

Pixel and validity chunks are encoded once through the storage codec authority.
The checkpoint-to-writer boundary validates kind, encoded/decoded length,
checksum, single-frame extent, ordering, and profile before exact encoded bytes
enter an outer shard. Packed-index chunks continue to be constructed at
publication. Scientific and exact package identities retain their existing
contracts.

Scientific identity is independent of storage layout. Package identity covers
the exact package bytes. Recipe, derivation, rights, citation, and analysis
artifact identities remain explicit typed records rather than filenames or
informal metadata.

## Data Safety

- Import, validation, project recovery, and maintenance never modify source
  microscopy data.
- Writers stage output, validate it, and publish atomically under an explicit
  create-only or replacement policy. Incomplete output never appears complete.
- Source drift invalidates a verified binding and requires verification again.
- There are no compatibility readers or in-application migrations during
  pre-alpha development.
- Analysis results carry source and operation provenance. Preview,
  approximate, partial, and complete states are distinct; only complete
  results can be exported as final results.
- Public evidence must not expose private paths, dataset metadata, or raw
  qualification identities.

## Project Format And Scope

The accepted project-store service is the sole product route for New, Open,
Save, Save As, autosave, and recovery. Project I/O is identity-gated and the
project format remains experimental.

The active dataset profile does not promise backward compatibility, stable
public release support, or generic OME-Zarr compatibility. Its exact normative
standards are recorded in
[`architecture/wp10a-normative-standards.json`](../architecture/wp10a-normative-standards.json).
The closed control wire is implemented by `mirante4d-storage` and described
above. The bounded independent corpus is
[`fixtures/target/manifest.json`](../fixtures/target/manifest.json).
