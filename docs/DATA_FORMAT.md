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
public release support, or generic OME-Zarr compatibility. Its frozen technical
contract and independent target corpus are recorded in
[`architecture/wp10a-storage-contract.json`](../architecture/wp10a-storage-contract.json).
