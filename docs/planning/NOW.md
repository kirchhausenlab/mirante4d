# Current Work

Last updated: 2026-07-13

## Current Checkpoint

WP-09A is accepted and immutable at `foundation-wp-09a-exit-1`
(`1b1e7d5534f29b010cc346d434811a3906fb40e1`). WP-10B's entry is accepted at
`4d0e4853637a0466ca37548134822c5ec83a240f`. Its B1 wire contract and
independent fixture are frozen; B2 transactional store implementation is
active off-product. Its first core checkpoint provides the exact crate/API
boundary, control-record wire, and immutable object primitive. Its current
checkpoint adds typed canonical generations, direct and deterministic paged
closure, generation-last immutable publication, process-held maintenance and
writer leases, an exact no-replace initial manual head, and crate-private
established manual recovery-before-head replacement with bounded store
inventory and recovery-ahead retry. The same transaction now creates and
advances an established-project autosave lane, including divergent-base and
lower-revision cases. A crate-private established-session actor now owns the
root and leases, serializes those manual/autosave primitives, and enforces the
frozen queue, coalescing, cancellation, close, and shutdown rules. One shared
private inspection core now opens and validates established stores for actor
startup and transaction preflight, with bounded object-metadata checks and no
eager bulk-payload hashing. A shared crate-private initial-package transaction
now gives future Create and Save As one destination-local sibling-stage,
full-tree-sync, no-clobber install path with a caller-bound fork tuple and
retained leases. The private established-session actor now authenticates Save
As against the live manual head and scientific identity, then changes its owned
root and leases only after that transaction durably installs the fork. B2
now also has one bounded metadata graph over established or provisional store
state, canonical generation/object namespace enumeration, exact ref/pin roots,
and capped orphan candidates. It performs no repair, payload hashing, or
deletion. A narrow accepted correction now makes successful Open and
OpenRecovery return both the held session and loaded projection and adds an
honest manual-branch recovery classification without changing persisted bytes.
A bounded private recovery reader and actor path now classify all four fixtures,
fall back across corrupt heads/targets, scan only after exhausted lane fallback,
and load a freshly selected projection without repair or promotion. B2 remains
active off-product: public Create/Open/Save As execution, provisional autosave
publication, timers, public/product garbage collection, Purge, and public actor
wiring remain later work, with product activation at B4.
Private Pin/Unpin now supplies durable checkpoint roots with fresh graph,
capacity, cancellation, and read-only enforcement. The accepted maintenance
transition correction names pin, unpin, and purge phases for the later
exhaustive failure matrix.
A private bounded FullVerify path now hashes every physical object in one
stable active-store snapshot, reconstructs paged logical objects, remains
cancellable and available read-only, and changes no store authority. Artifact
scientific semantics, repair, trash, durability, and public/product wiring are
outside this slice.
A private bounded PlanCompaction path now returns stable-snapshot metadata-only
recovery-review candidates for every orphan generation. It is cancellable,
available read-only, and non-mutating; Trash authorization, object/byte moves,
reclaim estimates, backup approval, durability, and public/product wiring stay
outside this slice.
The next B2 checkpoint is bound by a narrow Trash safety correction: only
freshly revalidated orphans declaring zero non-regenerable artifacts may enter
the mirrored quarantine. Shared objects stay active, work proceeds in synced
bounded batches, and every other selection fails with `ConfirmationRequired`.
The private actor now routes that subset using bounded admission, correlated
completion and cancellation, the corrected same-descriptor transition, exact
retry, and fail-closed inventory. Exact transition/fresh-process evidence is
the next slice; Purge remains later B2 work.

The unified runtime is the sole live interactive dataset-demand and CPU-byte
authority. Analysis execution remains unavailable until WP-12.

## WP-10B Entry Boundary

The accepted entry's sole correction permits `mirante4d-project-store` to
depend directly on the unchanged `mirante4d-domain` API so it can encode and
reconstruct the domain-owned values in `ProjectGenerationProjection`. It does
not authorize another persistence DTO owner or any other dependency change.

The entry must bind to tag `foundation-wp-09a-exit-1`, commit
`1b1e7d5534f29b010cc346d434811a3906fb40e1`, and tree
`42846232a04ec7548a3bb9b1b6598e79be29e72b`. It must freeze the final
directory-backed project-store contract, persistence-owned canonical records
and independent fixtures, background actor and lease ownership, exact
revision/autosave/recovery behavior, bounded object growth, and the declared
Linux durability/fault-injection matrix. Project binding remains gated on a
verified D-009 scientific identity.

WP-10B is a product persistence hard cutover: the private project-v15 bridge
must be deleted in the same accepted package, no legacy reader or converter may
remain, and real product-open validation is required. WP-11 is the next
protected-branch checkpoint, following the
[foundation handoff](../plans/active/FOUNDATION_REFACTOR_HANDOFF.md).
