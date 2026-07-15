# ADR-0001 — Foundation Program And Hard Cutovers

Status: ACCEPTED AND IMPLEMENTED
Accepted: 2026-07-09
Last reviewed: 2026-07-15
Decision source: OD-001 through OD-008 and PRG-001/002/003/008/015

## Context

The early application has valuable product behavior but its documentation,
ownership, runtime, persistence, verification, and release foundations grew
through incremental phase work. Local cleanup would preserve conflicting
authorities and make later features depend on accidental structure.

Mirante4D is still greenfield. The owner explicitly prefers deep replacement,
permits every repository document to change, rejects backward-compatibility
debris, and requires planning before implementation.

## Decision

- Execute one dependency-ordered foundation program rather than independent
  symptom patches.
- Treat hard cutover as the default: one replacement authority activates while
  its predecessor is deleted in the same product-facing checkpoint.
- Keep preparation code unreachable until its named activation/deletion gate;
  never expose old/new product routes, compatibility shims, legacy readers,
  fallback branches, re-export facades, or dormant feature flags.
- WP-02 removed the segmentation prototype before the general foundation
  rebuild. Segmentation remains absent and may return only through a separately
  approved capability plan.
- Separate planning approval from implementation authorization through PH-00,
  a promoted revision-stamped handoff, entry-stamped package briefs, and
  immutable exit evidence.
- Use revision/deployment rollback or fix-forward, never an in-product fallback
  that restores the predecessor beside the replacement.

## Alternatives Rejected

- Repairing individual large files, tests, or performance symptoms without
  changing authority ownership.
- A single long-lived big-bang rewrite branch.
- Keeping old paths “temporarily” reachable for safety.
- Preserving current experimental format/project identities merely to avoid a
  deliberate cutover.

## Consequences

- Every work package needs explicit predecessor deletion, stop conditions,
  proof classes, checkpoint boundaries, and rollback units.
- Current behavior remains authoritative until its owning cutover lands; target
  documentation cannot pretend implementation already exists.
- User data is never deleted merely because application support is removed.
- A failed cutover reopens its owning change instead of becoming lingering
  cleanup debt.

## Enforcement

[Current State](../CURRENT_STATE.md) and
[Current Architecture](../ARCHITECTURE.md) own the resulting product facts.
Git history and immutable tags preserve the individual cutover record.
