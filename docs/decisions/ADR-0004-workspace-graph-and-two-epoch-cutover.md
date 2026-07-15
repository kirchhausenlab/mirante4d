# ADR-0004 — Ownership DAG And Gated Trunk

Status: ACCEPTED AND IMPLEMENTED
Accepted: 2026-07-09
Last reviewed: 2026-07-15

## Context

The prototype's broad crate ownership cannot mechanically enforce the target
model, state, resource, side-effect, and persistence boundaries. A long-lived
rewrite branch would create a second product authority and hide integration
failures.

## Decision

- Implement the sixteen-crate ownership DAG specified by the workspace brief,
  plus a dev-only CPU reference renderer.
- Crates exist for authority, dependency direction, persisted lifecycle, side
  effects, or resource ownership—not to meet a line-count aesthetic.
- Permit only the six named one-way migration bridges. Each has one product
  route, one resource/authority rule, and a mandatory deletion gate. Target
  crates never depend on predecessor crates.
- Use one protected `main`, short reviewed checkpoints, deterministic serial
  integration, squash merges, create-once attempt tags, and atomic
  activation/deletion revisions.
- Recovery is revision/deployment rollback or fix-forward. It never restores a
  predecessor beside its replacement or adds a hidden fallback.

## Consequences

The refactor incurred deliberate crate/API/deletion work instead of cosmetic
file movement. Architecture checks use Cargo dependency metadata, public
API/side-effect/resource ownership, and predecessor deletion—not source line
limits.

## Owning Documents

- [Current Architecture](../ARCHITECTURE.md)
