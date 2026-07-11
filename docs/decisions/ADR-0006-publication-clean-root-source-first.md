# ADR-0006 — Clean Public Source Before Full Public Data

Status: ACCEPTED
Accepted: 2026-07-09
Last reviewed: 2026-07-11

## Context

Mirante4D should be genuinely open source, while full microscopy-data release
requires separate artifact-level rights, privacy, hosting, permanence, cost,
integrity, and citation decisions.

## Decision

- License the source under standard MIT terms.
- Begin public development from one independently constructed, sanitized root
  commit. Pre-public Git objects, refs, workflow history, artifacts, and caches
  are not part of the public source repository.
- Record attribution, lineage, citation, retained-asset provenance, and the
  clean-root construction deliberately in the public source tree.
- Publish only small approved source fixtures during the foundation program.
- Defer full microscopy-dataset selection, licensing, hosting, DOI, candidate
  validation, upload, and publication to a separately approved open-data
  handoff.

## Consequences

Public source availability does not imply that a full dataset is licensed,
approved, reproducible, or available for redistribution. Target formats remain
experimental until their own cutovers. Full-data delays do not block source or
technical-foundation work.

## Enforcement

The public root must pass disclosure, credential, dependency, provenance,
license, workflow, clean-clone, and reproducibility gates. Any unresolved
secret or rights uncertainty blocks publication. External dataset
contributions remain closed until a later governance decision.

## Owning Documents

- [Foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
- [Deferred open-data plan](../plans/deferred/OPEN_DATA_FOLLOW_ON.md)
