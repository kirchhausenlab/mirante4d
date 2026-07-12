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
- Temporary private project bridge: `mirante4d-project-v15` schema version 1;
  normal project I/O is identity-gated
- Settings identity: `mirante4d-settings-v1`

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
- Writers stage output, validate it, and publish atomically under an explicit
  create or replacement policy; incomplete output must never appear complete.
- Persisted identities are strict. There are no compatibility readers or
  in-application migrations during pre-alpha development.
- Analysis results carry source and operation provenance. Preview,
  approximate, partial, and complete states are distinct; only complete
  results can be exported as final results.
- Public evidence must not expose private paths, dataset metadata, or raw
  qualification identities.

## Approved Replacement

WP-10A builds and freezes an off-product strict Mirante4D profile over released
OME-NGFF 0.5 and Zarr v3. The target adds storage-independent scientific
identity, distinct package/recipe/derivation/artifact identities, explicit
lifecycle states, mandatory object-count and amplification proof, and an
independent conformance corpus. WP-10C later activates it in the product and
deletes the current reader and writer.

The accepted WP-10A freeze now has a pure implementation core for its
experimental compatibility tuple, storage geometry, bounded counts,
amplification limits, sole portable package-path authority, scientific
hashing, exact object/package hashing, and domain-framed recipe,
derivation-record, and release hashing. Strict scalar grammars, restricted JCS,
the closed profile, canonical-value, scientific, and display-defaults
grammars, exact recipe bodies and verified RecipeId payloads, and the exact
compatibility tuple are implemented. Exact path-bound object descriptors,
canonical greedy manifest pages, authenticated manifest roots, and PackageId
derivation are also implemented. A checked control-wire specialization fixes
the ordinal, indexed-path, temporal, recipe-node, and manifest spellings
omitted from the accepted freeze, plus the closed portable-record and detached-
release scalar choices. Exact source, recipe, derivation, rights, citation, and
release DTOs are implemented. The fixed packed-index record and bounded
zstd/CRC32C inner-payload and end-index codecs implement the selected binary
layer in memory. Strict Zarr group/array metadata, closed OME image-group axes
and transforms, and bounded root-confined Unix object-range reads are
implemented. The product reader is not yet implemented; the off-product
independent conformance validator and corpus are promoted. A bounded local
catalog authenticates the manifest root/pages, verifies opening-critical
metadata bytes, parses the closed
control/Zarr/OME objects, and checks their layer, time, geometry, dtype, shape,
validity, and packed-index-count relationships. A separate cancellable
inventory enforces the exact finalized file and ancestor-directory closure,
safe object types, declared lengths, and globally bounded counts/fan-out
without reading payload bytes. It reports directory depth and reauthenticates
manifest authority around the scan. One-brick address planning validates
requested coordinates and derives exact pixel, validity, and packed-index
shard paths, inner slots, packed-record offsets, and edge extents. From that
plan, the bounded brick core fetches only the selected shard-index and inner-
payload ranges, validates index and inner CRC32C plus bounded zstd output, and
uses the packed record to authorize pixel or validity fill elision. It exposes
exact request/read/decode counters and enforces the frozen absolute ceilings.
Explicit caller-selected DS admission distinguishes arithmetic addressed shards
from actual files, validates every listed shard coordinate, requires complete
packed-index shard coverage, and applies the selected count ceilings without
enumerating logical bricks. It does not infer or persist a DS label. A crate-
private structural pass then verifies packed-index object digests and every
record's coordinates, edge capacity, validity mode, canonical padding, and
pixel/validity inner-slot presence without reading those large payloads. The
catalog exposes the root digest only as the declared PackageId. Consuming full
validation now stream-hashes the root, pages, and every descriptor object with
a fixed 64 KiB buffer, requires the structural and digest observations to name
the same shard versions, repeats inventory, and performs a final snapshot
sweep. Its owning capability is the only PackageId-authorized brick-read path;
it checks manifest authority and each consumed shard against that proof. A
separate consuming scan recomputes the base-scale scientific layer roots and
ScientificContentId with bounded memory and cancellation, then returns the
stronger verified-scientific-package capability. Lazy portable-record
semantics, atomic snapshotting of a concurrently mutable directory, and
product support remain outside these capabilities. A create-only off-product
writer derives canonical metadata, shards, descriptors, pages, and root bytes
from typed inputs; hashes objects while writing; performs DS admission,
structural reconciliation,
inventory, and snapshot checks in a private sibling stage; and publishes with
Linux `RENAME_NOREPLACE` followed by parent-directory sync. It does not replace
packages, derive scientific identity from source data, generate multiscales,
perform import, or activate the product. The exact official OME-NGFF 0.5.2
image artifacts, Zarr core 3.0, and selected codec specifications are retained
offline by immutable source revision, length, and SHA-256. A separately pinned
zarr-python reader decodes one hand-built selected-subset shard. This is an
interoperability stop/go result only; it is not T1, IO-3, official-schema,
complete-package, or generic OME-Zarr evidence. The promoted
`target-m4d-v1` authority contains three bounded deterministic USTAR archives,
independent expected facts, critical identity vectors, full-array independent
readback with pinned OME-schema results, 15 executed rejection mutations, and
byte-identical two-run reproduction. It covers only the frozen EXPERIMENTAL
profile.

The accepted WP-10A production path consumes all three promoted packages
through the production exact-to-scientific path, checks every full-array and
per-layer value/validity digest plus exact shard/object/depth/fan-out and
one-brick amplification facts, and rejects all 15 mutations.
Production-writer outputs
pass the pinned schema and independent reader with matching image and
scientific facts; encoded bytes and PackageId may differ. Isolated
2,750/5,500/11,000-descriptor opens prove the linear metadata-work contract,
with the largest bounded to 10 seconds and 64 MiB of post-open RSS.
`cargo xtask verify-local format-lifecycle` is the accepted lifecycle gate.
WP-10A is immutable at `foundation-wp-10a-exit-1`. The format stays
EXPERIMENTAL and off-product, with no stable, generic OME-Zarr, importer,
product-support, or product-activation claim.

WP-10B B1 freezes a new experimental project-store wire authority: canonical
immutable generations and exact-byte objects, checksummed 160-byte refs, and
deterministic 16 MiB paging only for large artifact payloads. Its promoted
independent fixture covers manual/autosave recovery, divergence, object reuse,
and corruption. B2 implements transactions; B4 deletes project-v15 and makes
the new store the sole product path.
