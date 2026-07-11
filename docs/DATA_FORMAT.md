# Data Format And Safety

Mirante4D uses strict native packages so scientific meaning, storage, and
runtime expectations are explicit before a dataset opens.

## Current Dataset Package

- Format identifier: `mirante4d-v1`
- Schema version: `1`
- Intensity dtypes: `uint8`, `uint16`, and finite `float32`
- Dense array axes: explicit time and spatial axes; channels are separate
  layers
- Storage: Zarr v3 indexed sharding with bounded groups of bricks per shard
- Project/session identity: `mirante4d-project-v14`
- Preferences identity: `mirante4d-preferences-v1`

Metadata records axes, units, geometry, multiscale reductions, intensity
ranges, checksums, and provenance. Unsupported identities, malformed metadata,
and inconsistent payloads are rejected rather than guessed or migrated.

`0` is valid intensity unless an explicit reviewed no-data policy says
otherwise. Validity metadata is shared by import, storage, rendering, and
analysis. Missing data for an occupied region is incomplete/loading state, not
empty scientific data.

Terms used here:

- A **brick** is the runtime spatial block used for loading and rendering.
- A **chunk** is the logical Zarr storage unit.
- A **shard** is one physical storage object containing multiple chunks, used
  to bound file count and I/O overhead.

The current package is already sharded; creating one file or sidecar per brick
is forbidden.

## Data Safety

- Import, validation, recovery, and maintenance never modify source microscopy
  data.
- Writers stage output, validate it, and publish by an explicit replacement;
  incomplete output must never appear complete.
- Persisted identities are strict. There are no compatibility readers or
  in-application migrations during pre-alpha development.
- Analysis results carry source and operation provenance. Preview,
  approximate, partial, and complete states are distinct; only complete
  results can be exported as final results.
- Public evidence must not expose private paths, dataset metadata, or raw
  qualification identities.

## Approved Replacement

WP-10A will hard-cut the current dataset package to a strict Mirante4D profile
over released OME-NGFF 0.5 and Zarr v3. The target adds storage-independent
scientific identity, distinct package/recipe/derivation/artifact identities,
explicit lifecycle states, mandatory object-count and amplification proof, and
an independent conformance corpus.

WP-10B separately installs immutable content-addressed project objects,
complete generations, atomic head/recovery refs, leases, autosave/recovery,
and conservative garbage collection.

These are approved targets, not current readers or writers. Their authorities
are the [data-format brief](plans/active/foundation-refactor/DATA_FORMAT_IDENTITY_BRIEF.md),
[project-store brief](plans/active/foundation-refactor/PROJECT_STORE_DURABILITY_BRIEF.md),
and their accepted work-package entries.
