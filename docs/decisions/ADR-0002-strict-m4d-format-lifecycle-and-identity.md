# ADR-0002 — Strict M4D Format, Lifecycle, Sharding, And Identity

Status: ACCEPTED TARGET DECISION
Accepted: 2026-07-09
Last reviewed: 2026-07-10
Decision IDs: D-007, D-008, D-009
Implementation authorization: NO

This ADR records owner-approved target policy. It carries no independent implementation
or persisted-format change. The current `mirante4d-v1` schema-1 package,
reader/writer behavior, project identity, and existing identity fields remain
the factual implementation until an approved work package hard-cuts them. No
current persisted contract becomes OME-NGFF-compatible, candidate, or stable
merely because this ADR is accepted.

## Context

Mirante4D needs a strict, bounded native format that remains useful to external
bioimage tools without accepting the unconstrained surface of arbitrary
OME-Zarr. Production datasets must also avoid one physical file or sidecar per
logical brick. Scientific sameness, exact encoded bytes, derivation,
publication, and artifacts require distinct identities rather than one mutable
manifest fingerprint. Persisted compatibility promises must be explicit rather
than inferred from a `v1` label or public availability.

## Options

- Keep a custom M4D layout and produce a separate OME export. This preserves
  internal freedom but duplicates pixels and weakens the public native format.
- Treat generic OME-NGFF as the product input. This exposes too many layouts,
  codecs, and incomplete semantic combinations and would require fallback
  paths.
- Select one strict M4D profile over a pinned released OME-NGFF/Zarr v3 core,
  with namespaced extensions, mandatory sharding, explicit lifecycle states,
  and typed identities. This is selected.
- Infer stability from version labels and retain old-major readers indefinitely,
  or use one digest for every notion of identity. Both are rejected.

## Decision

The target dataset is admitted only by a strict, versioned M4D profile layered
on released OME-NGFF 0.5 and Zarr v3. OME-NGFF owns facts it represents exactly;
a small namespaced M4D extension owns only missing profile, validity, transform,
logical-layer, default, reference, and capability semantics. Large occupancy,
range, integrity, and acceleration indexes remain external, compact, lazy,
byte-bounded, and sharded. Mirante4D opens the exact profile, not arbitrary
OME-Zarr.

Co-registered multichannel pixels use physical `t,c,z,y,x` arrays. Inner chunks
and outer shards have `t=1,c=1`, so logical channels remain independently
loadable. Heterogeneous dtype, grid, or transform groups remain separate images
linked by the M4D logical graph.

Zarr v3 indexed sharding is mandatory for production pixel, validity, and
large-index arrays. Small logical bricks are packed into bounded outer shards;
per-brick files, manifest entries, and sidecars are forbidden. Import preflight
and acceptance must bound and verify logical bricks, shards, total physical
objects, directory fan-out, shard sizes, and one-brick read amplification. Only
tiny, explicitly scoped conformance fixtures may be unsharded.

Persisted contracts use explicit `EXPERIMENTAL`, `CANDIDATE`, `STABLE`, and
`DEPRECATED`/`SUPERSEDED` states. Current contracts are experimental with no
compatibility promise. Stability requires a separate owner-approved release
gate. Core code does not accumulate old-major readers; before a future stable
major is dropped, an independently verified external regeneration route or
side-by-side converter must exist and receive its own authorization.

Public identity uses full, typed, versioned SHA-256 families:

- a storage-independent scientific-content Merkle identity;
- exact raw-object and package-closure identities;
- reusable recipe and exact derivation-record identities;
- immutable dataset-release identity; and
- typed scientific-artifact/project-reference identities.

Scientific identity includes canonical values, validity, axes, units,
geometry, and logical-layer mapping. It excludes compression, chunks, shards,
paths, display state, provenance narrative, release metadata, and regenerable
acceleration. Recompression or resharding may preserve scientific identity but
must change exact package identity. Bare persisted digests are invalid, and a
canonicalization, domain, tree, or algorithm change creates a new identity
scheme rather than reinterpreting an old digest.

## Consequences

- Pixels are stored once and are externally interpretable only to the exact
  declared interoperability level; unsupported semantics are never silently
  approximated.
- Filesystem object growth is bounded independently of logical-brick count.
- Repacking and scientific sameness are distinguishable, while release,
  derivation, rights, and display concerns cannot masquerade as content.
- The profile, canonical encodings, schemas, object manifests, and independent
  vectors require substantial implementation and conformance work.
- Existing experimental packages may need regeneration after the hard cut.
  This ADR creates no legacy reader, fallback, or converter obligation.

## Enforcement

- WP-10A freezes the released standard/profile, normative schemas and domains,
  per-profile object/fan-out/read-amplification ceilings, independent producer/
  fact/reader fixtures, and canonical SHA-256/metamorphic vectors before any
  candidate claim.
- WP-10C activates one product reader/writer route and deletes the superseded
  format path in the same hard cutover.
- Validation checks the official OME schema, strict M4D profile, bounded index
  structure, object closure, and independent expected facts; production writer/
  reader agreement alone is insufficient.
- Unknown required capabilities and unsupported identities fail clearly. No
  dual reader, unsharded production path, per-brick file layout, or bare digest
  may be introduced without a new owner-approved ADR and handoff change.
- Implementation follows the active handoff and begins only when the owning
  execution brief is revision-stamped and authorized.

## Owning Documents

- [Foundation Refactor Implementation Handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
- [Data Format, Lifecycle, And Identity Brief](../plans/active/foundation-refactor/DATA_FORMAT_IDENTITY_BRIEF.md)
