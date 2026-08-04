# ADR-0003: Store Projects As Immutable Generations

Status: Accepted

Accepted: 2026-07-09

Last reviewed: 2026-08-04

## Context

Mutable multi-file saves do not expose one clear committed revision when state
and artifacts change concurrently. They make crash recovery, autosave,
conflict detection, relocation, garbage collection, and dirty-state reporting
difficult to prove.

A single database would hide large artifacts. A database plus external blobs
would create a two-authority commit.

## Decision

- Use a directory-backed content-addressed object store.
- Store complete project snapshots as immutable generations.
- Publish only tiny atomic refs to complete, validated generation closures.
- Keep manual and autosave heads and recovery refs distinct.
- Let one background store actor own serialization, hashing, writes, flushes,
  directory sync, and ref replacement.
- Bind each save to an exact domain revision. Advance saved state only after
  that revision commits durably.
- Use leases and expected-parent checks to prevent conflicting writers.
- Fail closed on ambiguous durability, corrupt control state, active leases,
  unknown files, or an invalid object graph.
- Move unreachable objects to synchronized trash before a separate purge.
- Keep dataset scientific identity separate from package identity and locator
  hints.

## Consequences

A committed head names one complete immutable project generation. Saves are
incremental and inspectable. Previous accepted state can remain reachable
through explicit recovery refs.

Orphaned content is safe but can consume space until maintenance runs.
Unsupported filesystems cannot become writable because one simple save
appeared to work. Experimental predecessor formats may require external
regeneration after a hard cut.

## Guardrails

- No ref points to staging or an incomplete closure.
- No UI-thread file transaction, silent merge, automatic repair, or guessed
  deletion is allowed.
- Non-regenerable user material is never age-pruned.
- Writable support requires an explicit filesystem qualification.
- A mutable artifact authority, hybrid commit, or legacy reader requires a new
  decision.

[Architecture](../ARCHITECTURE.md) owns current persistence authority.
[Data format](../DATA_FORMAT.md) owns the current project contract.
