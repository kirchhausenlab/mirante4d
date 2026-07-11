# ADR-0004 — Ownership DAG And Gated Trunk

Status: ACCEPTED TARGET DECISION
Accepted: 2026-07-09
Last reviewed: 2026-07-11

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

The program incurs deliberate crate/API/deletion work instead of cosmetic file
movement. Missing a bridge deletion or allowing a duplicate authority reopens
the owning work package. Architecture checks use Cargo dependency metadata,
public API/side-effect/resource ownership, and predecessor deletion—not source
line limits.

## Owning Documents

- [Foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
- [Workspace architecture brief](../plans/active/foundation-refactor/WORKSPACE_ARCHITECTURE_BRIEF.md)
