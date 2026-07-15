# ADR-0006 — Clean Public Source Before Full Public Data

Status: ACCEPTED
Accepted: 2026-07-09
Last reviewed: 2026-07-14
Current-state effect: WP-04 PUBLIC CUTOVER COMPLETE

WP-04 completed the clean-root publication cutover. The public repository is
now canonical, protected `main` is operational, and the private predecessor
history remains outside the public object graph.

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
- Publish only small approved repository fixtures unless a separate dataset
  release is authorized.
- Defer full microscopy-dataset selection, licensing, hosting, DOI, candidate
  validation, upload, and publication to a separately approved open-data
  handoff.

## Consequences

Public source availability does not imply that a full dataset is licensed,
approved, reproducible, or available for redistribution. Target formats remain
experimental until their own release gates. Full-data delays do not block
ordinary source or software development.

## Enforcement

The WP-04 cutover passed its disclosure, credential, dependency, provenance,
license, workflow, clean-clone, and reproducibility gates. The public
repository must preserve those boundaries. Any new secret or rights uncertainty
blocks the affected release, and external dataset contributions remain closed
until a later governance decision.

## Owning Documents

- [Current State](../CURRENT_STATE.md)
- [Deferred open-data plan](../plans/deferred/OPEN_DATA_FOLLOW_ON.md)
