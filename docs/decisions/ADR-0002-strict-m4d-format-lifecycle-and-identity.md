# ADR-0002: Use One Strict M4D Profile And Typed Identities

Status: Accepted

Accepted: 2026-07-09

Last reviewed: 2026-08-04

## Context

Mirante4D needs a bounded native dataset format that external bioimage tools
can interpret. Accepting arbitrary OME-Zarr would admit too many layouts,
codecs, and incomplete semantic combinations. A custom unrelated format would
duplicate pixels and weaken interoperability.

Scientific sameness, exact stored bytes, derivation, publication, and project
references also represent different facts. One mutable manifest digest cannot
identify all of them safely.

## Decision

- Admit one versioned M4D profile over pinned released OME-NGFF and Zarr v3
  standards.
- Put only missing Mirante4D semantics in a namespaced extension.
- Require sharded, byte-bounded production arrays. Per-brick files and
  sidecars are not a production layout.
- Bound object count, directory fan-out, shard size, and read amplification.
- Give persisted contracts an explicit lifecycle. A public version label does
  not imply stability.
- Use full typed and versioned SHA-256 identity families for scientific
  content, exact objects and packages, recipes, derivations, releases, and
  artifacts.
- Keep scientific identity independent of compression, sharding, paths,
  display state, and regenerable acceleration data.
- Change the identity scheme when its canonicalization, domain, tree, or
  algorithm changes. Do not reinterpret existing IDs.

## Consequences

Mirante4D rejects unsupported OME-Zarr variants instead of approximating them.
Repacking can preserve scientific identity while changing exact package
identity. Experimental packages may need regeneration after a hard cut.

This decision creates no permanent legacy reader, fallback, or converter
obligation.

## Guardrails

- Unknown required capabilities and bare persisted digests fail.
- Writer and reader agreement alone does not prove conformance.
- Independent schemas, facts, vectors, and readers verify the format boundary.
- A dual reader, unsharded production path, or changed identity meaning
  requires a new decision.

[Data format](../DATA_FORMAT.md) owns the current profile and safety rules.
[Current state](../CURRENT_STATE.md) owns implemented support.
