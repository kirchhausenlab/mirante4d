# Data Format

Mirante4D uses strict native packages so that scientific meaning, storage, and
runtime expectations are explicit before the viewer opens a dataset.

## Current Format

- Format identifier: `mirante4d-v1`
- Schema version: `1`
- Supported dense intensity dtypes: `uint8`, `uint16`, and `float32`
- Current project/session identity: `mirante4d-project-v14`
- Current preferences identity: `mirante4d-preferences-v1`

Packages contain validated metadata, multiscale intensity arrays, geometry,
validity/range information, and runtime data required by the current viewer.
Unsupported identities are rejected rather than guessed or migrated.

The detailed current contracts remain in:

- [dataset format](specs/DATASET_FORMAT_SPEC.md)
- [native schema](specs/NATIVE_DATASET_V1_SCHEMA_SPEC.md)
- [storage policy](specs/DATASET_V1_STORAGE_POLICY_SPEC.md)
- [project/session model](specs/PROJECT_SESSION_MODEL_SPEC.md)

## Approved Replacement Direction

The foundation refactor will replace the current package through a hard
cutover. The target is a strict Mirante4D profile over released OME-NGFF
0.5/Zarr v3 concepts, with:

- one physical `t,c,z,y,x` pixel pyramid;
- independent channel/time loading;
- mandatory indexed sharding and bounded filesystem object counts;
- explicit validity and scientific geometry;
- storage-independent scientific identity;
- separate package, derivation, release, and artifact identities;
- an experimental/candidate/stable/superseded lifecycle;
- immutable transactional project generations.

This target is not the current reader or writer. The active
[data-format brief](plans/active/foundation-refactor/DATA_FORMAT_IDENTITY_BRIEF.md)
owns its implementation.

## Compatibility

Current formats are experimental. The core application does not carry legacy
readers or compatibility shims. If a future stable format needs migration, it
will use an explicit external conversion/regeneration path rather than adding
old-format branches to the viewer.
