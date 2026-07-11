# ADR-0003 — Immutable Content-Addressed Project Generations

Status: ACCEPTED TARGET DECISION
Accepted: 2026-07-09
Last reviewed: 2026-07-11
Decision ID: D-010
Implementation authorization: NONE INDEPENDENT; ACTIVE HANDOFF AND PACKAGE ENTRY ONLY

This ADR records owner-approved target policy. It carries no independent
implementation or project-format change. WP-02 cut project v13 to the
segmentation-free v14 predecessor. WP-07B later deleted v14 and installed the
private experimental `mirante4d-project-v15` bridge. Only WP-10B may replace
that bridge with this store. No existing project gains the transaction or
durability guarantees described here.

## Context

Mutable multi-file project saves cannot provide one clear committed revision
when state and potentially large artifacts change concurrently. They make
crash recovery, autosave, conflict detection, relocation, garbage collection,
and exact dirty-state reporting difficult to prove. The replacement must keep
filesystem behavior inspectable and incremental while never exposing a partial
project generation or blocking the UI thread on persistence work.

## Options

- Continue mutable files or copy the whole project per save. This is simple but
  leaves partial-state and scale problems.
- Use SQLite/WAL for all data. This provides transactions but makes large
  artifacts opaque and widens the persistence boundary.
- Use SQLite plus external blobs. This creates a two-authority, two-phase
  commit problem.
- Use copy-on-write generation directories or an append-only event log. These
  add duplication, portability, replay, snapshot, and compaction complexity.
- Select a transparent directory-backed content-addressed object store with
  complete immutable generations and tiny atomic refs. This is selected.

## Decision

The target project is a directory-backed store containing a fixed envelope and
project UUID, immutable content-addressed objects, immutable complete generation
snapshots, bounded refs, staging, leases, pins, and quarantined trash. One
atomic `head` ref identifies the current and previous manual generations; an
independently synced recovery ref preserves the prior manual tip. Autosave uses
equivalent independent head/recovery refs and records its base manual
generation. A ref never points to staging or a partial closure.

One background project-store actor owns serialization, hashing, file writes,
flushes, directory sync, and ref replacement. Each save captures an exact
domain revision and immutable snapshot. `saved_revision` advances only after
that revision is durably committed, so an intervening edit leaves the project
truthfully dirty. Persistence DTOs contain canonical durable state and typed
object descriptors, never renderer, GPU, worker, cache, scheduler, arbitrary
internal-path, or deleted-segmentation state.

A commit validates the expected parent, stages and durably publishes changed
objects, publishes and syncs one complete generation, durably updates recovery
to the old tip, then flushes and atomically replaces the tiny head and syncs its
directory. Publication is create-if-absent/no-replace. If final durability
cannot be established, the store returns typed `CommitIndeterminate`, keeps the
revision dirty, suspends further writes, and requires close/reopen recovery; a
visible renamed file alone is not durability proof.

Every opener holds a shared OS maintenance lease for its session. A writable
opener also holds the writer lease, and commits compare the expected parent. A
second process that cannot obtain the writer lease opens read-only; there is no
automatic merge. Compaction requires the exclusive maintenance lease.

Recovery and garbage collection fail closed. Open validates bounded control
records and the referenced generation closure before use. Corrupt heads may
offer only validated previous/recovery generations; bounded scans list
candidates but never auto-repair. Unknown files, a corrupt graph, or an active
lease block deletion. Unreachable data first moves into synced `trash`, and
purge is separate. Non-regenerable annotations, ROIs, tracks, measurements,
manual edits, imported material, and artifacts are never guessed or age-pruned
away.

Writable durability is claimed only for explicitly qualified local Linux
filesystem and mount-option tuples supporting same-filesystem staging,
no-replace publication, atomic ref replacement, file sync, and directory sync.
Unknown, network, FUSE, and overlay stores default to read-only. Internal path
operations must be descriptor-relative and no-follow.

Dataset bindings use verified D-009 scientific identity; package/release IDs
and locator hints remain distinct. The WP-10B replacement of transitional
project v14 is a hard cut: no v13/v14 core reader, in-place migration,
fallback, or converter is included afterward.

## Consequences

- A committed head always names one complete immutable generation, and prior
  accepted state remains available through explicit previous/recovery refs.
- Saves are incremental, relocation-safe, revision-aware, and inspectable, but
  require careful filesystem, locking, recovery, and growth controls.
- Unqualified filesystems cannot be advertised writable merely because a basic
  save appeared to work.
- Content-addressed orphans are harmless before explicit compaction, so storage
  may grow until retention and GC run.
- Existing experimental v13/v14 projects are regenerated unless a separately
  requested and authorized external converter is later approved.

## Enforcement

- WP-10B freezes canonical envelope/ref/generation/object schemas and independent
  vectors, implements the sole project store, switches save/open atomically,
  and deletes the temporary current-persistence bridge and old mutation paths.
- Acceptance injects failure before and after every write, flush, publish,
  directory sync, generation, ref, autosave, and GC transition; it also covers
  process kill, corruption, `ENOSPC`, short writes, permissions, concurrent
  writers, stale parents, relocation, Save As, symlink attacks, and recovery.
- Durability evidence names the exact filesystem and mount options and includes
  a VM/loopback/fault-injection power-loss harness capable of discarding
  unpersisted writes. Logic mocks and `SIGKILL` alone are insufficient.
- WP-10C verifies dataset/runtime integration but does not recut project
  persistence; WP-10B remains the project save/open authority.
- No mutable artifact authority, hybrid database/blob commit, UI-thread file
  transaction, silent merge, auto-repair, or legacy project reader may be added
  without a new owner-approved ADR and handoff change.
- Implementation follows the active handoff and begins only when the owning
  execution brief is revision-stamped and authorized.

## Owning Documents

- [Foundation Refactor Implementation Handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md)
- [Project Store And Durability Brief](../plans/active/foundation-refactor/PROJECT_STORE_DURABILITY_BRIEF.md)
